use protobuf::{Optional, proto};
use protobuf_well_known_types::Timestamp;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod api;
mod autogarden;
mod autokitchen;
mod boiler;
mod grpcweb;
mod heat;
mod hotwater;
mod lights;
mod mqtt;
mod parse;
mod schedule;
mod state;
mod thermostat;

mod pb {
    tonic::include_proto!("maison");
    pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("fdset");
    include!(concat!(env!("OUT_DIR"), "/protobuf_generated/generated.rs"));
}

fn new_backoff() -> backoff::ExponentialBackoff {
    backoff::ExponentialBackoffBuilder::new()
        .with_max_elapsed_time(None) // Never completely give up.
        .build()
}

fn make_delay(start: SystemTime, d: Duration) -> Option<Timestamp> {
    let tt = start.checked_add(d)?.duration_since(UNIX_EPOCH).ok()?;
    Some(proto!(Timestamp {
        seconds: tt.as_secs().try_into().ok()?,
        nanos: tt.subsec_nanos().try_into().ok()?,
    }))
}

fn after_now(d: Duration) -> Option<Timestamp> {
    make_delay(SystemTime::now(), d)
}

fn proto_ts_to_delay(
    ts: Optional<protobuf_well_known_types::TimestampView>,
    now: SystemTime,
) -> Option<Duration> {
    if let Optional::Set(until_p) = ts {
        if let Ok(secs) = until_p.seconds().try_into() {
            if let Ok(nanos) = until_p.nanos().try_into() {
                if let Some(until) = UNIX_EPOCH.checked_add(Duration::new(secs, nanos)) {
                    if let Ok(delay) = until.duration_since(now) {
                        if !delay.is_zero() {
                            return Some(delay);
                        }
                    }
                }
            }
        }
    }
    None
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
        Arc<autogarden::AutoGarden>,
        Arc<autokitchen::AutoKitchen>,
        Arc<boiler::Controller<hotwater::HotWater>>,
        Arc<boiler::Controller<heat::Heat>>,
        Arc<thermostat::ThermostatSet>,
    )>::new()?
    .run()
    .await
}
