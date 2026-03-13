use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::{FutureExt, Stream, StreamExt};
use merge_streams::MergeStreams;
use protobuf_well_known_types::TimestampView;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tonic::Status;

use crate::mqtt::Mqtt;
use crate::pb::{MaisonState, MonitorResponse, PersistentStateView};

pub struct Api {
    mqtt: Arc<Mqtt>,
    state: Arc<crate::state::State>,
    garden_lights: Arc<crate::lights::Garden>,
    autokitchen: Arc<crate::autokitchen::AutoKitchen>,
}

#[resource]
#[export_grpc(crate::pb::maison_server::MaisonServer)]
#[proto_descriptor(crate::pb::FILE_DESCRIPTOR_SET)]
impl Resource for Api {
    fn new(
        (mqtt, state, garden_lights, autokitchen): (
            Arc<Mqtt>,
            Arc<crate::state::State>,
            Arc<crate::lights::Garden>,
            Arc<crate::autokitchen::AutoKitchen>,
        ),
        _: comprehensive::NoArgs,
        _: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        Ok(Arc::new(Self {
            mqtt,
            state,
            garden_lights,
            autokitchen,
        }))
    }
}

impl Api {
    fn maybe_subscribe(
        &self,
        want: Option<bool>,
        topic: &str,
        convert: &'static (dyn Fn(crate::parse::Message) -> Option<MonitorResponse> + Sync),
    ) -> Option<impl Future<Output = impl Stream<Item = Result<MonitorResponse, Status>> + 'static>>
    {
        want.unwrap_or_default().then(move || {
            self.mqtt.subscribe(String::from(topic)).map(move |sub| {
                sub.into_stream()
                    .filter_map(move |m| std::future::ready(convert(m).map(|x| Ok(x))))
            })
        })
    }

    async fn maybe_publish<T>(&self, topic: T, v: Option<bool>) -> Result<(), Status>
    where
        T: Into<String>,
    {
        if let Some(l) = v {
            self.mqtt
                .publish(topic, if l { b"ON".as_ref() } else { b"OFF".as_ref() })
                .await
                .map_err(|e| tonic::Status::internal(e.to_string()))?;
        }
        Ok(())
    }
}

fn convert_temperature(x: crate::parse::Message) -> Option<MonitorResponse> {
    match x {
        crate::parse::Message::Climate(x) => Some(MonitorResponse {
            message: Some(crate::pb::monitor_response::Message::LiveTemperature(x)),
        }),
        _ => None,
    }
}

fn now_timestamp() -> Option<prost_types::Timestamp> {
    let tt = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    Some(prost_types::Timestamp {
        seconds: tt.as_secs().try_into().ok()?,
        nanos: tt.subsec_nanos().try_into().ok()?,
    })
}

fn ts_to_ts(ts: protobuf::Optional<TimestampView<'_>>) -> Option<prost_types::Timestamp> {
    ts.into_option().map(|ts| prost_types::Timestamp {
        seconds: ts.seconds(),
        nanos: ts.nanos(),
    })
}

impl From<PersistentStateView<'_>> for MaisonState {
    fn from(s: PersistentStateView<'_>) -> MaisonState {
        MaisonState {
            now: now_timestamp(),
            garden_light_until: ts_to_ts(s.garden_light_until_opt()),
            hot_water_override_until: ts_to_ts(s.hot_water_override_until_opt()),
        }
    }
}

#[tonic::async_trait]
impl crate::pb::maison_server::Maison for Api {
    type MonitorEverythingStream =
        Pin<Box<dyn Stream<Item = Result<MonitorResponse, Status>> + Send>>;

    async fn monitor_everything(
        &self,
        req: tonic::Request<crate::pb::MonitorEverythingRequest>,
    ) -> Result<tonic::Response<Self::MonitorEverythingStream>, Status> {
        let req = req.into_inner();
        let mqtt_stream = futures::future::join_all(
            [
                self.maybe_subscribe(
                    req.want_live_temperatures,
                    "zigbee/climate/bottom",
                    &convert_temperature,
                ),
                self.maybe_subscribe(
                    req.want_live_temperatures,
                    "zigbee/climate/middle",
                    &convert_temperature,
                ),
                self.maybe_subscribe(
                    req.want_live_temperatures,
                    "zigbee/climate/top",
                    &convert_temperature,
                ),
                self.maybe_subscribe(
                    req.want_live_temperatures,
                    "zigbee/climate/main",
                    &convert_temperature,
                ),
                self.maybe_subscribe(
                    req.want_live_temperatures,
                    "zigbee/climate/kitchen",
                    &convert_temperature,
                ),
                self.maybe_subscribe(req.want_kitchen_ceiling, "zigbee/kitchen_ceiling", &|x| {
                    match x {
                        crate::parse::Message::SimpleSwitch(x) => Some(MonitorResponse {
                            message: Some(crate::pb::monitor_response::Message::KitchenCeiling(x)),
                        }),
                        _ => None,
                    }
                }),
                self.maybe_subscribe(
                    req.want_kitchen_under_cupboards,
                    "zigbee/kitchen_under_cupboards",
                    &|x| match x {
                        crate::parse::Message::SimpleSwitch(x) => Some(MonitorResponse {
                            message: Some(
                                crate::pb::monitor_response::Message::KitchenUnderCupboards(x),
                            ),
                        }),
                        _ => None,
                    },
                ),
                self.maybe_subscribe(
                    req.want_kitchen_under_stairs,
                    "zigbee/kitchen_under_stairs",
                    &|x| match x {
                        crate::parse::Message::SimpleSwitch(x) => Some(MonitorResponse {
                            message: Some(
                                crate::pb::monitor_response::Message::KitchenUnderStairs(x),
                            ),
                        }),
                        _ => None,
                    },
                ),
                self.maybe_subscribe(req.want_boiler, "zigbee/boiler", &|x| match x {
                    crate::parse::Message::Boiler(x) => Some(MonitorResponse {
                        message: Some(crate::pb::monitor_response::Message::Boiler(x)),
                    }),
                    _ => None,
                }),
                self.maybe_subscribe(req.want_garden_lights, "zigbee/garden", &|x| match x {
                    crate::parse::Message::SimpleSwitch(x) => Some(MonitorResponse {
                        message: Some(crate::pb::monitor_response::Message::GardenLights(x)),
                    }),
                    _ => None,
                }),
            ]
            .into_iter()
            .filter_map(|maybe_subscription| maybe_subscription),
        )
        .await
        .merge();
        let stream: Self::MonitorEverythingStream = if req.want_maison.unwrap_or_default() {
            Box::pin(futures::stream::select(
                mqtt_stream,
                tokio_stream::wrappers::WatchStream::new(self.state.subscribe()).map(|snapshot| {
                    Ok(MonitorResponse {
                        message: Some(crate::pb::monitor_response::Message::Maison(
                            snapshot.as_view().into(),
                        )),
                    })
                }),
            ))
        } else {
            Box::pin(mqtt_stream)
        };
        Ok(tonic::Response::new(stream))
    }

    async fn set_garden_lights(
        &self,
        req: tonic::Request<crate::pb::SetLightsRequest>,
    ) -> Result<tonic::Response<()>, Status> {
        self.garden_lights
            .duration_ms(req.into_inner().duration_ms)
            .await?;
        Ok(tonic::Response::new(()))
    }

    async fn set_many(
        &self,
        req: tonic::Request<crate::pb::SetManyRequest>,
    ) -> Result<tonic::Response<()>, Status> {
        let req = req.into_inner();
        if req.kitchen_ceiling.is_some() {
            self.autokitchen.suppress();
        }
        let (r1, r2, r3) = futures::join!(
            self.maybe_publish("zigbee/kitchen_ceiling/set/state", req.kitchen_ceiling),
            self.maybe_publish(
                "zigbee/kitchen_under_cupboards/set/state",
                req.kitchen_under_cupboards
            ),
            self.maybe_publish(
                "zigbee/kitchen_under_stairs/set/state",
                req.kitchen_under_stairs
            ),
        );
        r1?;
        r2?;
        r3?;
        Ok(tonic::Response::new(()))
    }

    async fn set_hot_water(
        &self,
        req: tonic::Request<crate::pb::SetLightsRequest>,
    ) -> Result<tonic::Response<()>, Status> {
        match req.into_inner().duration_ms {
            Some(duration_ms) => {
                let end = crate::after_now(Duration::from_millis(duration_ms))
                    .ok_or_else(|| tonic::Status::out_of_range("silly diration"))?;
                self.state
                    .write()
                    .as_mut()
                    .set_hot_water_override_until(end);
            }
            None => {
                self.state.write().as_mut().clear_hot_water_override_until();
            }
        }
        Ok(tonic::Response::new(()))
    }
}
