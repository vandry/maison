use futures::Stream;
use itertools::Itertools;
use merge_streams::MergeStreams;
use pin_project_lite::pin_project;
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use crate::mqtt::SubscriptionStream;
use crate::parse::{OutdoorSensor, OutdoorSensorJson};
use crate::pb::OutdoorStats;

const SENSORS: [&str; 4] = [
    "zigbee/garden/dais",
    "zigbee/garden/east",
    "zigbee/garden/south",
    "zigbee/garden/west",
];

const MAX_STALENESS: Duration = Duration::from_mins(15);

pin_project! {
    pub struct StatsStream {
        #[pin] inner: <[SubscriptionStream; SENSORS.len()] as MergeStreams>::Stream,
        #[pin] sleep: tokio::time::Sleep,
        readings: HashMap<String, OutdoorSensorJson>,
    }
}

impl StatsStream {
    pub fn new(mqtt: &crate::mqtt::Mqtt) -> Self {
        Self {
            inner: SENSORS
                .map(|topic| mqtt.subscribe((*topic).into()).into_stream())
                .merge(),
            readings: HashMap::new(),
            sleep: tokio::time::sleep(Duration::default()),
        }
    }
}

impl Stream for StatsStream {
    type Item = OutdoorStats;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        let mut changes = false;
        loop {
            match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => {
                    match this.sleep.as_mut().poll(cx) {
                        Poll::Pending => {
                            if !changes {
                                return Poll::Pending;
                            }
                        }
                        Poll::Ready(()) => (),
                    }
                    break;
                }
                Poll::Ready(None) => {
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(crate::parse::Message::Outdoor(OutdoorSensor {
                    unit,
                    value,
                }))) => {
                    this.readings.insert(unit, value);
                    changes = true;
                }
                Poll::Ready(Some(_)) => (),
            }
        }
        let now = SystemTime::now();
        let mut soonest_expiry = None;
        let data = this
            .readings
            .values()
            .filter(|v| {
                match std::time::UNIX_EPOCH.checked_add(Duration::from_millis(v.last_seen)) {
                    None => false,
                    Some(last_seen) => match last_seen.checked_add(MAX_STALENESS) {
                        None => false,
                        Some(expiry) => {
                            if expiry <= now {
                                false
                            } else {
                                match soonest_expiry {
                                    Some(t) if t <= expiry => (),
                                    _ => {
                                        soonest_expiry = Some(expiry);
                                    }
                                }
                                true
                            }
                        }
                    },
                }
            })
            .collect::<Vec<_>>();
        this.sleep.as_mut().reset(
            tokio::time::Instant::now()
                + soonest_expiry
                    .and_then(|t| t.duration_since(now).ok())
                    .unwrap_or(Duration::from_hours(1)),
        );

        let count = data.len();
        let new = if count > 0 {
            let average: f64 = data.iter().map(|v| v.temperature).sum::<f64>() / count as f64;
            let temperature = if count > 2 {
                // Remove an outlier because one sensor or another might be
                // directly in the sun at different times of day.
                let outlier = data
                    .iter()
                    .fold(0.0, |m: f64, v| m.max((v.temperature - average).abs()));
                let (count, sum) = data
                    .iter()
                    .filter(|v| (v.temperature - average).abs() < outlier)
                    .fold((0, 0.0), |(count, sum), v| (count + 1, sum + v.temperature));
                if count > 0 {
                    sum / count as f64
                } else {
                    average
                }
            } else {
                average
            };
            let (sm0, sm1) = data
                .iter()
                .map(|v| v.soil_moisture)
                .minmax()
                .into_option()
                .unwrap();
            OutdoorStats {
                temperature: Some(temperature),
                min_soil_moisture: Some(sm0),
                max_soil_moisture: Some(sm1),
            }
        } else {
            OutdoorStats::default()
        };
        Poll::Ready(Some(new))
    }
}
