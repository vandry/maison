use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::{Stream, StreamExt};
use humantime::parse_duration;
use pin_project_lite::pin_project;
use std::collections::HashMap;
use std::pin::{Pin, pin};
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use crate::mqtt::Mqtt;
use crate::schedule::Schedule;
use crate::state::State;

use crate::pb::{HeatSchedule, PersistentState};

fn calc(state: Option<&PersistentState>, schedule: Option<&HeatSchedule>) -> HashMap<String, f64> {
    let mut known = HashMap::new();
    if let Some(s) = schedule {
        for se in &s.heating {
            if let (Some(zone), Some(new)) = (se.zone.as_ref(), se.setpoint) {
                known.insert(zone.to_string(), new);
            }
        }
    }
    if let Some(s) = state {
        for ho in s.heating_override() {
            if let (Some(zone), Some(new)) = (ho.zone_opt().into_option(), ho.setpoint_opt().into_option()) {
                known.insert(zone.to_string(), new);
            }
        }
    }
    known
}

#[derive(Clone, Debug)]
struct RadiatorSetpointMapEntry {
    radiator_name: String,
    zone: String,
}

#[derive(Debug)]
struct RadiatorSetpointMapEntryParseError;

impl std::fmt::Display for RadiatorSetpointMapEntryParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "argument to --radiator_setpoint=RadiatorName=setpoint_zone_name")
    }
}

impl std::error::Error for RadiatorSetpointMapEntryParseError {}

impl FromStr for RadiatorSetpointMapEntry {
    type Err = RadiatorSetpointMapEntryParseError;

    fn from_str(s: &str) -> Result<Self, RadiatorSetpointMapEntryParseError> {
        match s.find('=') {
            None => Err(RadiatorSetpointMapEntryParseError),
            Some(i) => Ok(RadiatorSetpointMapEntry {
                radiator_name: s[..i].into(),
                zone: s[i+1..].into(),
            }),
        }
    }
}

#[derive(clap::Args)]
pub struct SetpointMirrorArgs {
    #[arg(long, help = "RadiatorName=setpoint_zone_name")]
    radiator_setpoint: Vec<RadiatorSetpointMapEntry>,
    #[arg(long, help = "Delay before mirroring radiator setpoint", default_value = "10s", value_parser = parse_duration)]
    radiator_setpoint_delay: Duration,
}

pub struct SetpointMirror;

struct Delivery {
    value: f64,
    deliver_time: Option<Instant>,
}

impl Delivery {
    fn due(&self, now: Instant) -> bool {
        self.deliver_time.map(|i| i <= now).unwrap_or_default()
    }
}

pin_project! {
    struct SetpointMirrorStream<ST, SC> {
        #[pin] state_fut: ST,
        #[pin] schedule_fut: SC,
        #[pin] timer: Option<tokio::time::Sleep>,
        deliveries: HashMap<String, Delivery>,
        persistent_state: Option<Arc<PersistentState>>,
        schedule: Option<Arc<HeatSchedule>>,
        delay: Duration,
    }
}

impl<ST, SC> SetpointMirrorStream<ST, SC> {
    fn new(state_fut: ST, schedule_fut: SC, delay: Duration) -> Self {
        Self {
            state_fut,
            schedule_fut,
            delay,
            timer: None,
            deliveries: HashMap::new(),
            persistent_state: None,
            schedule: None,
        }
    }
}

impl<ST, SC> Stream for SetpointMirrorStream<ST, SC>
where
    ST: Stream<Item = Arc<PersistentState>>,
    SC: Stream<Item = Arc<HeatSchedule>>,
{
    type Item = (String, f64);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            let mut something = false;
            match this.state_fut.as_mut().poll_next(cx) {
                Poll::Pending => (),
                Poll::Ready(None) => {
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(snapshot)) => {
                    something = true;
                    *this.persistent_state = Some(snapshot);
                }
            }
            match this.schedule_fut.as_mut().poll_next(cx) {
                Poll::Pending => (),
                Poll::Ready(None) => {
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(snapshot)) => {
                    something = true;
                    *this.schedule = Some(snapshot);
                }
            }
            if something {
                continue;
            }
            let now = Instant::now();
            let mut deliveries_changed = false;
            for (zone, v) in calc(this.persistent_state.as_deref(), this.schedule.as_deref()) {
                this.deliveries.entry(zone)
                    .and_modify(|d| {
                        if d.value != v {
                            d.value = v;
                            if d.deliver_time.is_none() {
                                d.deliver_time = Some(now + *this.delay);
                                deliveries_changed = true;
                            }
                        }
                    })
                    .or_insert_with(|| {
                        deliveries_changed = true;
                        Delivery {
                            value: v,
                            deliver_time: Some(now + *this.delay),
                        }
                    });
            }
            let item = this.deliveries.iter_mut().filter_map(|(zone, delivery)| delivery.due(now).then(|| {
                delivery.deliver_time = None;
                (zone.to_string(), delivery.value)
            })).next();
            if deliveries_changed || item.is_some() {
                this.timer.set(this.deliveries.values().filter_map(|d| d.deliver_time).min().map(|i| tokio::time::sleep_until(i.into())));
            }
            if item.is_some() {
                return Poll::Ready(item);
            }
            if let Some(t) = this.timer.as_mut().as_pin_mut() {
                if t.poll(cx).is_ready() {
                    this.timer.set(None);
                    continue;
                }
            }
            return Poll::Pending;
        }
    }
}

#[resource]
impl Resource for SetpointMirror {
    fn new(
        (mqtt, schedule, state): (Arc<Mqtt>, Arc<Schedule>, Arc<State>),
        a: SetpointMirrorArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        let mut config = HashMap::new();
        for RadiatorSetpointMapEntry { radiator_name, zone } in a.radiator_setpoint {
            config.entry(zone)
                .or_insert_with(Vec::new)
                .push(radiator_name);
        }
        runtime.set_task(async move {
            loop {
                let mut s = pin!(SetpointMirrorStream::new(
                    tokio_stream::wrappers::WatchStream::new(state.subscribe()),
                    tokio_stream::wrappers::WatchStream::new(schedule.subscribe()),
                    a.radiator_setpoint_delay,
                ));
                while let Some((zone, setpoint)) = s.next().await {
                    if let Some(radiators) = config.get(&zone) {
                        for radiator_name in radiators {
                            tracing::info!("Mirror setpoint {setpoint} to radiator {radiator_name} from zone {zone}");
                            if let Err(e) = mqtt.publish(format!("zigbee/radiator/{radiator_name}/set/occupied_heating_setpoint"), format!("{setpoint}")).await {
                                tracing::error!("MQTT set: {e}");
                            }
                        }
                    }
                }
            }
        });
        Ok(Arc::new(Self))
    }
}
