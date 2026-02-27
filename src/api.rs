use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::{FutureExt, Stream, StreamExt};
use merge_streams::MergeStreams;
use std::pin::Pin;
use std::sync::Arc;
use tonic::Status;

use crate::mqtt::Mqtt;
use crate::pb::{MaisonState, MonitorResponse, PersistentStateView};

pub struct Api {
    mqtt: Arc<Mqtt>,
    state: Arc<crate::state::State>,
}

#[resource]
#[export_grpc(crate::pb::maison_server::MaisonServer)]
#[proto_descriptor(crate::pb::FILE_DESCRIPTOR_SET)]
impl Resource for Api {
    fn new(
        (mqtt, state): (Arc<Mqtt>, Arc<crate::state::State>),
        _: comprehensive::NoArgs,
        _: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        Ok(Arc::new(Self { mqtt, state }))
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
}

fn convert_temperature(x: crate::parse::Message) -> Option<MonitorResponse> {
    match x {
        crate::parse::Message::Climate(x) => Some(MonitorResponse {
            message: Some(crate::pb::monitor_response::Message::LiveTemperature(x)),
        }),
        _ => None,
    }
}

impl From<PersistentStateView<'_>> for MaisonState {
    fn from(s: PersistentStateView<'_>) -> MaisonState {
        MaisonState {
            garden_light_until: s.garden_light_until_opt().into_option().map(|ts| {
                prost_types::Timestamp {
                    seconds: ts.seconds(),
                    nanos: ts.nanos(),
                }
            }),
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
}
