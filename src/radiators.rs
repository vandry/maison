use backoff::backoff::Backoff;
use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::future::Either;
use futures::stream::FuturesUnordered;
use futures::{Stream, StreamExt};
use humantime::parse_duration;
use pin_project_lite::pin_project;
use std::collections::HashMap;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use crate::RadiatorMapEntry;
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
            if let (Some(zone), Some(new)) =
                (ho.zone_opt().into_option(), ho.setpoint_opt().into_option())
            {
                known.insert(zone.to_string(), new);
            }
        }
    }
    known
}

#[derive(clap::Args)]
pub struct SetpointMirrorArgs {
    #[arg(long, help = "RadiatorName=setpoint_zone_name")]
    radiator_setpoint: Vec<RadiatorMapEntry>,
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
                this.deliveries
                    .entry(zone)
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
            let item = this
                .deliveries
                .iter_mut()
                .filter_map(|(zone, delivery)| {
                    delivery.due(now).then(|| {
                        delivery.deliver_time = None;
                        (zone.to_string(), delivery.value)
                    })
                })
                .next();
            if deliveries_changed || item.is_some() {
                this.timer.set(
                    this.deliveries
                        .values()
                        .filter_map(|d| d.deliver_time)
                        .min()
                        .map(|i| tokio::time::sleep_until(i.into())),
                );
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
        for RadiatorMapEntry {
            radiator_name,
            zone,
        } in a.radiator_setpoint
        {
            config
                .entry(zone)
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

#[derive(clap::Args)]
pub struct ExternalMirrorArgs {
    #[arg(long, help = "RadiatorName=setpoint_zone_name")]
    radiator_sensor: Vec<RadiatorMapEntry>,
}

pub struct ExternalMirror;

struct SensorMirrorPublish<'a> {
    value: f64,
    it: std::slice::Iter<'a, String>,
}

pin_project! {
    struct SensorMirror<'a> {
        name: &'a str,
        radiators: &'a Vec<String>,
        #[pin] sensor: crate::mqtt::SubscriptionStream,
        publish: Option<SensorMirrorPublish<'a>>,
    }
}

impl<'a> SensorMirror<'a> {
    fn new(mqtt: &Mqtt, name: &'a str, radiators: &'a Vec<String>) -> Self {
        Self {
            name,
            radiators,
            publish: None,
            sensor: mqtt
                .subscribe(format!("zigbee/climate/{name}"))
                .into_stream(),
        }
    }
}

struct PleasePublish {
    topic: String,
    payload: String,
}

impl<'a> Stream for SensorMirror<'a> {
    type Item = PleasePublish;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            if let Some(p) = this.publish {
                if let Some(radiator) = p.it.next() {
                    tracing::info!(
                        "Mirror temperature sensor {} value {} to radiator {radiator}",
                        this.name,
                        p.value
                    );
                    return Poll::Ready(Some(PleasePublish {
                        topic: format!("zigbee/radiator/{radiator}/set/external_temperature_input"),
                        payload: format!("{}", p.value),
                    }));
                }
                *this.publish = None;
            }
            match ready!(this.sensor.as_mut().poll_next(cx)) {
                None => {
                    return Poll::Ready(None);
                }
                Some(message) => {
                    if let crate::parse::Message::Climate(x) = message {
                        if let Some(value) = x.temperature {
                            *this.publish = Some(SensorMirrorPublish {
                                value,
                                it: this.radiators.iter(),
                            });
                        }
                    }
                }
            }
        }
    }
}

struct ExternalMirrorConfigIter<'a, 'b> {
    mqtt: &'b Mqtt,
    inner: std::collections::hash_map::Iter<'a, String, Vec<String>>,
}

impl<'a> Iterator for ExternalMirrorConfigIter<'a, '_> {
    type Item = SensorMirror<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|(sensor, radiators)| SensorMirror::new(self.mqtt, sensor, radiators))
    }
}

#[resource]
impl Resource for ExternalMirror {
    fn new(
        (mqtt,): (Arc<Mqtt>,),
        a: ExternalMirrorArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        let mut config = HashMap::new();
        for RadiatorMapEntry {
            radiator_name,
            zone,
        } in a.radiator_sensor
        {
            config
                .entry(zone)
                .or_insert_with(Vec::new)
                .push(radiator_name);
        }
        if config.is_empty() {
            return Ok(Arc::new(Self));
        }
        runtime.set_task(async move {
            let mut backoff = crate::new_backoff();
            loop {
                let mut readings = pin!(
                    futures::stream::iter(ExternalMirrorConfigIter {
                        inner: config.iter(),
                        mqtt: &*mqtt,
                    })
                    .flatten_unordered(None)
                );
                let mut writings = FuturesUnordered::new();
                loop {
                    let r = if writings.is_empty() {
                        readings.next().await
                    } else {
                        match futures::future::select(readings.next(), writings.next()).await {
                            Either::Left((x, _)) => x,
                            Either::Right((None, _)) => {
                                tracing::error!("FuturesUnordered returned nothing");
                                break;
                            }
                            Either::Right((Some(Ok(())), _)) => {
                                continue;
                            }
                            Either::Right((Some(Err(e)), _)) => {
                                tracing::error!("MQTT set: {e}");
                                continue;
                            }
                        }
                    };
                    match r {
                        None => {
                            tracing::warn!("stream ended");
                            break;
                        }
                        Some(PleasePublish { topic, payload }) => {
                            writings.push(mqtt.publish(topic, payload));
                            backoff.reset();
                        }
                    }
                }
                if let Some(duration) = backoff.next_backoff() {
                    tokio::time::sleep(duration).await;
                }
            }
        });
        Ok(Arc::new(Self))
    }
}
