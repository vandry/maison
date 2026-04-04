use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::{Stream, StreamExt};
use humantime::parse_duration;
use pin_project_lite::pin_project;
use protobuf::{Optional, proto};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::mqtt::{Mqtt, SubscriptionStream};
use crate::pb::{
    Climate, HeatSchedule, HysteresisMut, PersistentState, SetPoint, SetPoint2, SetPoint2View,
};
use crate::state::State;

#[derive(clap::Args)]
pub struct ThermostatSetArgs {
    #[arg(long, help = "Amount of time to hold thermostat on", default_value = "45m", value_parser = parse_duration)]
    hysteresis_on_hold_time: Duration,
    #[arg(long, help = "Amount of time to hold thermostat off", default_value = "30m", value_parser = parse_duration)]
    hysteresis_off_hold_time: Duration,
    #[arg(long, help = "Amount of time to remember live temperatures", default_value = "4500s", value_parser = parse_duration)]
    live_temp_memory: Duration,
}

enum ClimateMonitor {
    Subscribing(Pin<Box<dyn Future<Output = Arc<crate::mqtt::Subscription>> + Send>>),
    Subscribed(Pin<Box<SubscriptionStream>>),
}

impl ClimateMonitor {
    fn new(mqtt: Arc<Mqtt>, zone: &str) -> Self {
        let topic = format!("zigbee/climate/{}", zone);
        Self::Subscribing(Box::pin(async move { mqtt.subscribe(topic).await }))
    }
}

impl Stream for ClimateMonitor {
    type Item = crate::parse::Message;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match *self.as_mut() {
            Self::Subscribing(ref mut fut) => {
                let mut inner = Box::pin(ready!(fut.as_mut().poll(cx)).into_stream());
                let r = inner.as_mut().poll_next(cx);
                *self = Self::Subscribed(inner);
                r
            }
            Self::Subscribed(ref mut s) => s.as_mut().poll_next(cx),
        }
    }
}

pin_project! {
    struct ThermostatSetFuture {
        config: ThermostatSetArgs,
        current_schedule: Arc<HeatSchedule>,
        current_state: Arc<PersistentState>,
        mqtt: Arc<Mqtt>,
        state: Arc<State>,
        #[pin] schedule_stream: tokio_stream::wrappers::WatchStream<Arc<HeatSchedule>>,
        #[pin] state_stream: tokio_stream::wrappers::WatchStream<Arc<PersistentState>>,
        monitors: HashMap<String, ClimateMonitor>,
        live_temp: HashMap<String, (f64, Instant)>,
        #[pin] sleep: Option<tokio::time::Sleep>,
    }
}

#[derive(Debug, Default)]
struct Thermostat<'a> {
    state: Option<SetPoint2View<'a>>,
    schedule: Option<&'a SetPoint>,
}

#[derive(Debug)]
enum Decision {
    Skip,
    Wait(Duration),
    Disable,
    Off(Option<u64>),
    On(Option<u64>),
}

fn proto_ts_to_delay(
    ts: Optional<protobuf_well_known_types::TimestampView>,
    now: SystemTime,
) -> Option<Duration> {
    if let Optional::Set(until_p) = ts {
        if let Ok(secs) = until_p.seconds().try_into() {
            if let Ok(nanos) = until_p.nanos().try_into() {
                if let Some(until) = UNIX_EPOCH.checked_add(Duration::new(secs, nanos)) {
                    if let Ok(delay) = until.duration_since(now) {
                        if !delay.is_zero() {
                            return Some(delay);
                        }
                    }
                }
            }
        }
    }
    None
}

impl Thermostat<'_> {
    fn setpoint_from_schedule(&self) -> Option<(f64, Option<u64>)> {
        self.schedule
            .and_then(|s| s.setpoint.map(|p| (p, s.unique)))
    }

    fn setpoint_from_state_or_schedule(&self) -> Option<(f64, Option<u64>)> {
        match self.state {
            Some(s) => s
                .setpoint_opt()
                .into_option()
                .map(|s| (s, None))
                .or_else(|| self.setpoint_from_schedule()),
            None => self.setpoint_from_schedule(),
        }
    }

    fn decide(&self, live: Option<f64>, now: SystemTime, is_heating_overridden: bool) -> Decision {
        let maybe_hysteresis = self.state.and_then(|s| s.hysteresis_opt().into_option());
        let default_decision = match maybe_hysteresis {
            Some(_) => Decision::Disable,
            None => Decision::Skip,
        };
        let Some(live_temp) = live else {
            return default_decision;
        };
        let sp = if is_heating_overridden {
            self.setpoint_from_state_or_schedule()
        } else {
            self.setpoint_from_schedule()
        };
        let Some((setpoint, unique)) = sp else {
            return default_decision;
        };
        if let Some(hysteresis) = maybe_hysteresis {
            if hysteresis.schedule_unique_opt().into_option() == unique {
                if hysteresis.state() == (live_temp < setpoint) {
                    return Decision::Skip;
                }
                if let Some(delay) = proto_ts_to_delay(hysteresis.until_opt(), now) {
                    return Decision::Wait(delay);
                }
            }
        }
        if live_temp < setpoint {
            Decision::On(unique)
        } else {
            Decision::Off(unique)
        }
    }
}

impl ThermostatSetFuture {
    fn hold(
        mut h: HysteresisMut<'_>,
        s: bool,
        d: Option<protobuf_well_known_types::Timestamp>,
        unique: Option<u64>,
    ) {
        if let Some(until) = d {
            h.set_until(until);
        }
        h.set_state(s);
        if let Some(u) = unique {
            h.set_schedule_unique(u);
        }
    }

    fn calculate(self: Pin<&mut Self>) -> bool {
        let mut this = self.project();
        let mut want: HashMap<_, Thermostat<'_>> = HashMap::new();
        for o in this.current_state.heating_override() {
            if let Optional::Set(zone) = o.zone_opt() {
                if let Ok(zone) = zone.to_str() {
                    want.entry(zone).or_default().state = Some(o);
                }
            }
        }
        for event in this.current_schedule.heating.iter() {
            if let Some(ref zone) = event.zone {
                want.entry(zone).or_default().schedule = Some(event);
            }
        }
        this.monitors
            .retain(|zone, _| match want.get(zone.as_str()) {
                None => false,
                Some(Thermostat { state, schedule }) => {
                    state.map(|s| s.has_setpoint()).unwrap_or_default()
                        || schedule.map(|s| s.setpoint.is_some()).unwrap_or_default()
                }
            });
        let any_added = want.keys().fold(false, |any_added, zone| {
            if !this.monitors.contains_key(*zone) {
                this.monitors.insert(
                    zone.to_string(),
                    ClimateMonitor::new(Arc::clone(&this.mqtt), zone),
                );
                true
            } else {
                any_added
            }
        });
        if any_added {
            return false;
        }

        let now_i = Instant::now();
        let now = SystemTime::now();
        let heating_overridden_remaining = proto_ts_to_delay(
            this.current_state.as_view().heating_override_until_opt(),
            now,
        );
        let mut decisions = want
            .into_iter()
            .map(|(zone, thermostat)| {
                let live = this
                    .live_temp
                    .get(zone)
                    .filter(|(_, when)| now_i.duration_since(*when) < this.config.live_temp_memory)
                    .map(|(t, _)| *t);
                (
                    zone,
                    thermostat.decide(live, now, heating_overridden_remaining.is_some()),
                )
            })
            .collect::<HashMap<_, _>>();

        let end_of_heating_override = match heating_overridden_remaining {
            Some(d) => Decision::Wait(d),
            None => Decision::Skip,
        };
        let min_next_timed_event = decisions
            .values()
            .chain([&end_of_heating_override].into_iter())
            .filter_map(|d| match d {
                Decision::Wait(d) if !d.is_zero() => Some(d),
                _ => None,
            })
            .min()
            .copied();
        if decisions.values().any(|d| match d {
            Decision::Disable | Decision::On(_) | Decision::Off(_) => true,
            Decision::Skip | Decision::Wait(_) => false,
        }) {
            let mut lock = this.state.write();
            let ho = lock
                .as_view()
                .heating_override()
                .iter()
                .filter_map(|ho| match ho.zone_opt() {
                    Optional::Set(zone_p) => match zone_p.to_str() {
                        Ok(zone) => match decisions.remove(zone) {
                            Some(Decision::Disable) => match ho.setpoint_opt() {
                                Optional::Set(setpoint)
                                    if heating_overridden_remaining.is_some() =>
                                {
                                    Some(proto!(SetPoint2 {
                                        zone: zone_p,
                                        setpoint,
                                    }))
                                }
                                _ => None,
                            },
                            Some(Decision::On(unique)) => {
                                let mut o = ho.to_owned();
                                if heating_overridden_remaining.is_none() {
                                    o.clear_setpoint();
                                }
                                o.clear_hysteresis();
                                Self::hold(
                                    o.hysteresis_mut(),
                                    true,
                                    crate::make_delay(now, this.config.hysteresis_on_hold_time),
                                    unique,
                                );
                                tracing::info!("Zone {zone:?} wants heat on");
                                Some(o)
                            }
                            Some(Decision::Off(unique)) => {
                                let mut o = ho.to_owned();
                                if heating_overridden_remaining.is_none() {
                                    o.clear_setpoint();
                                }
                                o.clear_hysteresis();
                                Self::hold(
                                    o.hysteresis_mut(),
                                    false,
                                    crate::make_delay(now, this.config.hysteresis_off_hold_time),
                                    unique,
                                );
                                tracing::info!("Zone {zone:?} wants heat off");
                                Some(o)
                            }
                            None | Some(Decision::Skip) | Some(Decision::Wait(_)) => {
                                let mut o = ho.to_owned();
                                if heating_overridden_remaining.is_none() {
                                    o.clear_setpoint();
                                    if o.has_hysteresis() { Some(o) } else { None }
                                } else {
                                    Some(o)
                                }
                            }
                        },
                        Err(_) => None,
                    },
                    Optional::Unset(_) => None,
                })
                .collect::<Vec<_>>();
            let mut rw = lock.as_mut();
            if heating_overridden_remaining.is_none() {
                rw.clear_heating_override_until();
            }
            let mut hom = rw.heating_override_mut();
            hom.clear();
            hom.extend(ho);
            for (zone, decision) in decisions {
                match decision {
                    Decision::Disable | Decision::Skip | Decision::Wait(_) => (),
                    Decision::On(unique) => {
                        let mut o = proto!(SetPoint2 { zone });
                        Self::hold(
                            o.hysteresis_mut(),
                            true,
                            crate::make_delay(now, this.config.hysteresis_on_hold_time),
                            unique,
                        );
                        hom.push(o);
                    }
                    Decision::Off(unique) => {
                        let mut o = proto!(SetPoint2 { zone });
                        Self::hold(
                            o.hysteresis_mut(),
                            false,
                            crate::make_delay(now, this.config.hysteresis_off_hold_time),
                            unique,
                        );
                        hom.push(o);
                    }
                }
            }
        }
        this.sleep.set(min_next_timed_event.map(tokio::time::sleep));
        true
    }
}

impl Future for ThermostatSetFuture {
    type Output = Result<(), Box<dyn std::error::Error>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            let mut something_happened = false;
            let mut this = self.as_mut().project();
            match this.schedule_stream.as_mut().poll_next(cx) {
                Poll::Ready(None) => {
                    return Poll::Ready(Err("schedule stream ended".into()));
                }
                Poll::Pending => (),
                Poll::Ready(Some(schedule)) => {
                    *this.current_schedule = schedule;
                    something_happened = true;
                }
            }
            match this.state_stream.as_mut().poll_next(cx) {
                Poll::Ready(None) => {
                    return Poll::Ready(Err("state stream ended".into()));
                }
                Poll::Pending => (),
                Poll::Ready(Some(state)) => {
                    *this.current_state = state;
                    something_happened = true;
                }
            }
            let now_i = Instant::now();
            this.monitors
                .retain(|zone, monitor| match monitor.poll_next_unpin(cx) {
                    Poll::Ready(None) => false,
                    Poll::Pending => true,
                    Poll::Ready(Some(m)) => {
                        match m {
                            crate::parse::Message::Climate(Climate {
                                temperature: Some(t),
                                ..
                            }) => {
                                this.live_temp.insert(zone.to_string(), (t, now_i));
                            }
                            crate::parse::Message::Climate(Climate {
                                temperature: None, ..
                            }) => {
                                this.live_temp.remove(zone);
                            }
                            _ => (),
                        };
                        something_happened = true;
                        true
                    }
                });
            this.live_temp
                .retain(|_, (_, when)| now_i.duration_since(*when) < this.config.live_temp_memory);

            if self.as_mut().calculate() && !something_happened {
                let mut this = self.as_mut().project();
                match this.sleep.as_mut().as_pin_mut() {
                    Some(s) => {
                        let _ = ready!(s.poll(cx));
                        this.sleep.set(None);
                    }
                    None => {
                        return Poll::Pending;
                    }
                }
            }
        }
    }
}

pub struct ThermostatSet;

#[resource]
impl Resource for ThermostatSet {
    fn new(
        (mqtt, state, schedule): (Arc<Mqtt>, Arc<State>, Arc<crate::schedule::Schedule>),
        config: ThermostatSetArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        let mut schedule_sub = schedule.subscribe();
        let current_schedule = Arc::clone(&schedule_sub.borrow_and_update());
        let mut state_sub = state.subscribe();
        let current_state = Arc::clone(&state_sub.borrow_and_update());
        runtime.set_task(ThermostatSetFuture {
            mqtt,
            state,
            config,
            current_schedule,
            current_state,
            schedule_stream: tokio_stream::wrappers::WatchStream::from_changes(schedule_sub),
            state_stream: tokio_stream::wrappers::WatchStream::from_changes(state_sub),
            monitors: HashMap::new(),
            live_temp: HashMap::new(),
            sleep: None,
        });
        Ok(Arc::new(Self))
    }
}
