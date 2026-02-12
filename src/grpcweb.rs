use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures_util::TryFutureExt;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic_web::GrpcWebLayer;
use tower_http::cors::CorsLayer;

pub struct Server;

#[derive(clap::Args)]
pub struct GrpcwebServerArgs {
    #[arg(long, help = "Path of UNIX socket to listen on for grpcweb")]
    grpcweb_listen: PathBuf,
}

#[resource]
impl Resource for Server {
    fn new(
        (service,): (Arc<comprehensive_grpc::server::GrpcCommonService>,),
        a: GrpcwebServerArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::io::Error> {
        let listener = UnixListener::bind(&a.grpcweb_listen).or_else(|e| match e.kind() {
            std::io::ErrorKind::AddrInUse => {
                match std::os::unix::net::UnixStream::connect(&a.grpcweb_listen) {
                    Ok(_) => Err(e),
                    Err(e) => match e.kind() {
                        std::io::ErrorKind::ConnectionRefused => {
                            match std::fs::remove_file(&a.grpcweb_listen) {
                                Ok(()) => UnixListener::bind(&a.grpcweb_listen),
                                Err(_) => Err(e),
                            }
                        }
                        _ => Err(e),
                    },
                }
            }
            _ => Err(e),
        })?;
        let server = tonic::transport::Server::builder()
            .accept_http1(true)
            .layer(CorsLayer::permissive())
            .layer(GrpcWebLayer::new())
            .serve_with_incoming(
                Arc::unwrap_or_clone(service).into_service(),
                UnixListenerStream::new(listener),
            );
        runtime.set_task(server.err_into());
        Ok(Arc::new(Self))
    }
}
