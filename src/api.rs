use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::{Stream, StreamExt};
use merge_streams::MergeStreams;
use std::pin::Pin;
use std::sync::Arc;
use tonic::Status;

use crate::mqtt::Mqtt;

pub struct Api {
    mqtt: Arc<Mqtt>,
}

#[resource]
#[export_grpc(crate::pb::maison_server::MaisonServer)]
#[proto_descriptor(crate::pb::FILE_DESCRIPTOR_SET)]
impl Resource for Api {
    fn new(
        (mqtt,): (Arc<Mqtt>,),
        _: comprehensive::NoArgs,
        _: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        Ok(Arc::new(Self { mqtt }))
    }
}

type MonitorSwitchStream =
    Pin<Box<dyn Stream<Item = Result<crate::pb::SimpleSwitch, Status>> + Send>>;

impl Api {
    async fn monitor_switch(
        &self,
        topic: &str,
    ) -> Result<tonic::Response<MonitorSwitchStream>, Status> {
        let stream = self
            .mqtt
            .subscribe(String::from(topic.to_string()))
            .await
            .into_stream()
            .filter_map(|x| {
                std::future::ready(match x {
                    crate::parse::Message::SimpleSwitch(x) => Some(Ok(x)),
                    _ => None,
                })
            });
        Ok(tonic::Response::new(Box::pin(stream)))
    }
}

#[tonic::async_trait]
impl crate::pb::maison_server::Maison for Api {
    type MonitorLiveTemperaturesStream =
        Pin<Box<dyn Stream<Item = Result<crate::pb::Climate, Status>> + Send>>;

    async fn monitor_live_temperatures(
        &self,
        _: tonic::Request<()>,
    ) -> Result<tonic::Response<Self::MonitorLiveTemperaturesStream>, Status> {
        let stream = futures::future::join_all([
            self.mqtt.subscribe(String::from("zigbee/climate/bottom")),
            self.mqtt.subscribe(String::from("zigbee/climate/middle")),
            self.mqtt.subscribe(String::from("zigbee/climate/top")),
            self.mqtt.subscribe(String::from("zigbee/climate/main")),
            self.mqtt.subscribe(String::from("zigbee/climate/kitchen")),
        ])
        .await
        .into_iter()
        .map(|s| s.into_stream())
        .collect::<Vec<_>>()
        .merge();
        Ok(tonic::Response::new(Box::pin(stream.filter_map(|x| {
            std::future::ready(match x {
                crate::parse::Message::Climate(x) => Some(Ok(x)),
                _ => None,
            })
        }))))
    }

    type MonitorKitchenCeilingStream = MonitorSwitchStream;

    async fn monitor_kitchen_ceiling(
        &self,
        _: tonic::Request<()>,
    ) -> Result<tonic::Response<MonitorSwitchStream>, Status> {
        self.monitor_switch("zigbee/kitchen_ceiling").await
    }

    type MonitorKitchenUnderCupboardsStream = MonitorSwitchStream;

    async fn monitor_kitchen_under_cupboards(
        &self,
        _: tonic::Request<()>,
    ) -> Result<tonic::Response<MonitorSwitchStream>, Status> {
        self.monitor_switch("zigbee/kitchen_under_cupboards").await
    }

    type MonitorKitchenUnderStairsStream = MonitorSwitchStream;

    async fn monitor_kitchen_under_stairs(
        &self,
        _: tonic::Request<()>,
    ) -> Result<tonic::Response<MonitorSwitchStream>, Status> {
        self.monitor_switch("zigbee/kitchen_under_stairs").await
    }
}
