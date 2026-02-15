use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use std::marker::PhantomData;
use std::sync::Arc;

mod api;
mod grpcweb;
mod mqtt;
mod parse;
mod state;

mod pb {
    tonic::include_proto!("maison");
    pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("fdset");
    include!(concat!(env!("OUT_DIR"), "/protobuf_generated/generated.rs"));
}

struct TestState;

#[resource]
impl Resource for TestState {
    fn new(
        (state,): (Arc<state::State>,),
        _: comprehensive::NoArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<std::sync::Arc<Self>, std::convert::Infallible> {
        runtime.set_task(async move {
            loop {
                {
                    let lock = state.read();
                    tracing::info!(
                        "garden_light_until = {:?}",
                        lock.as_view().garden_light_until()
                    );
                }
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                {
                    let mut lock = state.write();
                    let mut s = lock.as_mut();
                    let mut ts = protobuf_well_known_types::Timestamp::new();
                    ts.set_nanos(s.garden_light_until().nanos() + 1);
                    s.set_garden_light_until(ts);
                }
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            }
        });
        Ok(std::sync::Arc::new(Self))
    }
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
        Arc<TestState>,
        PhantomData<comprehensive_spiffe::SpiffeTlsProvider>,
        PhantomData<api::Api>,
    )>::new()?
    .run()
    .await
}
