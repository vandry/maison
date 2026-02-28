use backoff::backoff::Backoff;
use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::StreamExt;
use futures::future::Either;
use protobuf::proto;
use protobuf_well_known_types::Timestamp;
use std::pin::pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch::error::RecvError;
use tokio::time::sleep;

use crate::mqtt::Mqtt;
use crate::pb::PersistentState;
use crate::state::State;

pub struct Garden {
    mqtt: Arc<Mqtt>,
    state: Arc<State>,
}

#[resource]
impl Resource for Garden {
    fn new(
        (mqtt, state): (Arc<Mqtt>, Arc<State>),
        _: comprehensive::NoArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        let state_rx = state.subscribe();
        let shared = Arc::new(Self { mqtt, state });
        let shared2 = Arc::clone(&shared);
        runtime.set_task(async move {
            let f1 = pin!(shared.await_timer(state_rx));
            let f2 = pin!(shared.cancel_timer_on_off());
            match futures::future::select(f1, f2).await {
                Either::Left((r, _)) => r?,
                Either::Right((_, _)) => (),
            }
            Err("exited unexpectedly".into())
        });
        Ok(shared2)
    }
}

fn after_now(d: Duration) -> Option<protobuf_well_known_types::Timestamp> {
    let tt = SystemTime::now()
        .checked_add(d)?
        .duration_since(UNIX_EPOCH)
        .ok()?;
    Some(proto!(Timestamp {
        seconds: tt.as_secs().try_into().ok()?,
        nanos: tt.subsec_nanos().try_into().ok()?,
    }))
}

impl Garden {
    pub async fn duration_ms(&self, maybe_duration_ms: Option<u64>) -> Result<(), tonic::Status> {
        match maybe_duration_ms {
            None => self.on().await,
            Some(0) => self.off().await,
            Some(duration_ms) => {
                let end = after_now(Duration::from_millis(duration_ms))
                    .ok_or_else(|| tonic::Status::out_of_range("silly diration"))?;
                self.state.write().as_mut().set_garden_light_until(end);
                self.on().await
            }
        }
        .map_err(|e| tonic::Status::internal(e.to_string()))
    }

    pub async fn on(&self) -> Result<(), rumqttc::ClientError> {
        self.mqtt.publish("zigbee/garden/set/state", b"ON").await
    }

    pub async fn off(&self) -> Result<(), rumqttc::ClientError> {
        self.mqtt.publish("zigbee/garden/set/state", b"OFF").await
    }

    async fn await_timer(
        &self,
        mut rx: tokio::sync::watch::Receiver<Arc<PersistentState>>,
    ) -> Result<(), RecvError> {
        loop {
            let until = rx
                .borrow_and_update()
                .as_view()
                .garden_light_until_opt()
                .into_option()
                .and_then(|u| {
                    UNIX_EPOCH.checked_add(Duration::new(
                        u.seconds().try_into().ok()?,
                        u.nanos().try_into().ok()?,
                    ))
                });
            match until {
                None => rx.changed().await?,
                Some(u) => match u.duration_since(SystemTime::now()) {
                    Ok(remaining) => {
                        let f1 = pin!(rx.changed());
                        let f2 = pin!(sleep(remaining));
                        match futures::future::select(f1, f2).await {
                            Either::Left((watch, _)) => watch?,
                            Either::Right((_, _)) => (),
                        }
                    }
                    Err(_) => {
                        match self.off().await {
                            Ok(()) => {
                                self.state.write().as_mut().clear_garden_light_until();
                            }
                            Err(e) => {
                                tracing::error!("Cannot turn light off: {e}");
                            }
                        }
                        rx.changed().await?
                    }
                },
            }
        }
    }

    async fn cancel_timer_on_off(&self) -> Result<(), RecvError> {
        let mut backoff = backoff::ExponentialBackoffBuilder::new()
            .with_max_elapsed_time(None) // Never completely give up.
            .build();
        loop {
            self.mqtt
                .subscribe(String::from("zigbee/garden"))
                .await
                .into_stream()
                .for_each(|m| {
                    if matches!(
                        m,
                        crate::parse::Message::SimpleSwitch(crate::pb::SimpleSwitch {
                            state: Some(false)
                        })
                    ) {
                        let mut lock = self.state.write();
                        if lock.as_view().has_garden_light_until() {
                            lock.as_mut().clear_garden_light_until();
                        }
                    }
                    backoff.reset();
                    std::future::ready(())
                })
                .await;
            tracing::warn!("subscribe to light stream ended");
            if let Some(duration) = backoff.next_backoff() {
                sleep(duration).await;
            }
        }
    }
}
