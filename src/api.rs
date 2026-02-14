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
}
