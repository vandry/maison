use std::marker::PhantomData;
use std::sync::Arc;

mod api;
mod grpcweb;
mod lights;
mod mqtt;
mod parse;
mod state;

mod pb {
    tonic::include_proto!("maison");
    pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("fdset");
    include!(concat!(env!("OUT_DIR"), "/protobuf_generated/generated.rs"));
}

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = pb::__unstable::PERSISTENT_DESCRIPTOR_INFO; // Silence warning
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    comprehensive::Assembly::<(
        Arc<grpcweb::Server>,
        Arc<comprehensive_http::diag::HttpServer>,
        PhantomData<comprehensive_spiffe::SpiffeTlsProvider>,
        PhantomData<api::Api>,
    )>::new()?
    .run()
    .await
}
