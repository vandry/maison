use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use std::sync::Arc;
use tonic::Status;

mod pb {
    tonic::include_proto!("maison");
    pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("fdset");
}

pub struct Api;

#[resource]
#[export_grpc(pb::maison_server::MaisonServer)]
#[proto_descriptor(pb::FILE_DESCRIPTOR_SET)]
impl Resource for Api {
    fn new(
        _: (),
        _: comprehensive::NoArgs,
        _: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        Ok(Arc::new(Self))
    }
}

#[tonic::async_trait]
impl pb::maison_server::Maison for Api {
    async fn hello(
        &self,
        req: tonic::Request<pb::HelloRequest>,
    ) -> Result<tonic::Response<pb::HelloResponse>, Status> {
        Ok(tonic::Response::new(pb::HelloResponse {
            message: req.into_inner().message,
        }))
    }
}
