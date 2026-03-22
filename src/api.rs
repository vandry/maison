use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::{FutureExt, Stream, StreamExt};
use merge_streams::MergeStreams;
use protobuf::proto;
use protobuf_well_known_types::TimestampView;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tonic::Status;

use crate::mqtt::Mqtt;
use crate::pb::{
    MaisonState, MonitorResponse, PersistentStateView, SetPoint, SetPoint2, SetPoint2View,
};

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
            heating_override_until: ts_to_ts(s.heating_override_until_opt()),
            heating_override: s.heating_override().iter().map(Into::into).collect(),
        }
    }
}

impl From<SetPoint2View<'_>> for SetPoint {
    fn from(s: SetPoint2View<'_>) -> SetPoint {
        SetPoint {
            zone: s.zone_opt().into_option().map(|s| s.to_string()),
            setpoint: s.setpoint_opt().into(),
        }
    }
}

fn maybe_expire_heating_override(state: &mut crate::state::Guard<'_>) {
    if let Ok(now) = SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        let ro = state.as_view().heating_override_until();
        if let Ok(secs) = ro.seconds().try_into() {
            if now.as_secs() < secs {
                return;
            }
            if now.as_secs() == secs {
                if let Ok(nanos) = ro.nanos().try_into() {
                    if now.subsec_nanos() < nanos {
                        return;
                    }
                }
            }
        }
        let mut rw = state.as_mut();
        rw.clear_heating_override_until();
        rw.heating_override_mut().clear();
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

    async fn set_heating(
        &self,
        req: tonic::Request<crate::pb::SetLightsRequest>,
    ) -> Result<tonic::Response<()>, Status> {
        match req.into_inner().duration_ms {
            Some(0) | None => {
                return Err(tonic::Status::out_of_range("duration_ms must be positive"));
            }
            Some(duration_ms) => {
                let end = crate::after_now(Duration::from_millis(duration_ms))
                    .ok_or_else(|| tonic::Status::out_of_range("silly diration"))?;
                let mut lock = self.state.write();
                maybe_expire_heating_override(&mut lock);
                if !lock.as_view().has_heating_override_until() {
                    return Err(tonic::Status::failed_precondition(
                        "heating override must be already set",
                    ));
                }
                lock.as_mut().set_heating_override_until(end);
            }
        }
        Ok(tonic::Response::new(()))
    }

    async fn cancel_heating_override(
        &self,
        _: tonic::Request<()>,
    ) -> Result<tonic::Response<()>, Status> {
        let mut lock = self.state.write();
        if lock.as_view().has_heating_override_until() {
            let mut rw = lock.as_mut();
            rw.clear_heating_override_until();
            rw.heating_override_mut().clear();
        }
        Ok(tonic::Response::new(()))
    }

    async fn increment_set_point(
        &self,
        req: tonic::Request<crate::pb::IncrementSetPointRequest>,
    ) -> Result<tonic::Response<()>, Status> {
        let req = req.into_inner();
        if let (Some(zone), Some(increment)) = (req.zone, req.increment) {
            let mut lock = self.state.write();
            let state = lock.as_view();
            let existing = state
                .heating_override()
                .iter()
                .enumerate()
                .find(|(_, o)| match o.zone_opt() {
                    protobuf::Optional::Set(z) if *z == *zone => true,
                    _ => false,
                })
                .map(|(i, _)| i);
            let has_heating_override_until = state.has_heating_override_until();
            let mut state_mut = lock.as_mut();
            let i = match (existing, req.starting_value_if_unset) {
                (None, None) => Err(tonic::Status::not_found("no setpoint for zone")),
                (None, Some(starting_value)) => {
                    match (
                        req.duration_ms_if_not_already_set,
                        has_heating_override_until,
                    ) {
                        (Some(duration_ms), false) => {
                            let end = crate::after_now(Duration::from_millis(duration_ms))
                                .ok_or_else(|| tonic::Status::out_of_range("silly diration"))?;
                            state_mut.set_heating_override_until(end);
                        }
                        (None, false) => {
                            return Err(tonic::Status::invalid_argument(
                                "need duration_ms_if_not_already_set",
                            ));
                        }
                        (_, true) => (),
                    }
                    state_mut.heating_override_mut().push(proto!(SetPoint2 {
                        zone,
                        setpoint: starting_value,
                    }));
                    Ok(state_mut.heating_override().len() - 1)
                }
                (Some(i), _) => Ok(i),
            }?;
            let mut heating_override_mut = state_mut.heating_override_mut();
            let mut setting = heating_override_mut.get_mut(i).unwrap();
            setting.set_setpoint(setting.setpoint() + increment);
        }
        Ok(tonic::Response::new(()))
    }
}
