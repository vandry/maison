use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::{Stream, StreamExt};
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
    type MqttTestStream =
        Pin<Box<dyn Stream<Item = Result<crate::pb::HelloResponse, Status>> + Send>>;

    async fn mqtt_test(
        &self,
        req: tonic::Request<crate::pb::HelloRequest>,
    ) -> Result<tonic::Response<Self::MqttTestStream>, Status> {
        let sub = self
            .mqtt
            .subscribe(req.into_inner().message.unwrap_or_default())
            .await;
        Ok(tonic::Response::new(Box::pin(sub.into_stream().map(|x| {
            Ok(crate::pb::HelloResponse {
                message: Some(format!("{x:?}")),
            })
        }))))
    }
}
