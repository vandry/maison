use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::future::Either;
use protobuf_well_known_types::{Timestamp, TimestampView};
use std::marker::PhantomData;
use std::pin::pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch::error::RecvError;
use tokio::time::sleep;

use crate::mqtt::Mqtt;
use crate::pb::{PersistentState, PersistentStateMut, PersistentStateView};
use crate::state::State;

pub trait OnOffTimerImpl: Send + Sync + 'static {
    const SET_TOPIC: &str;
    fn get_timer(state: PersistentStateView<'_>) -> protobuf::Optional<TimestampView<'_>>;
    fn set_timer(state: PersistentStateMut<'_>, ts: Timestamp);
    fn clear_timer(state: PersistentStateMut<'_>);
}

pub struct OnOffTimer<T> {
    mqtt: Arc<Mqtt>,
    state: Arc<State>,
    _t: PhantomData<T>,
}

#[resource]
impl<T: OnOffTimerImpl> Resource for OnOffTimer<T> {
    fn new(
        (mqtt, state): (Arc<Mqtt>, Arc<State>),
        _: comprehensive::NoArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        let state_rx = state.subscribe();
        let shared = Arc::new(Self {
            mqtt,
            state,
            _t: PhantomData,
        });
        let shared2 = Arc::clone(&shared);
        runtime.set_task(async move {
            let _ = shared.await_timer(state_rx).await;
            Err("exited unexpectedly".into())
        });
        Ok(shared2)
    }
}

impl<T: OnOffTimerImpl> OnOffTimer<T> {
    pub async fn duration_ms(&self, maybe_duration_ms: Option<u64>) -> Result<(), tonic::Status> {
        match maybe_duration_ms {
            None => self.on().await,
            Some(0) => self.off().await,
            Some(duration_ms) => {
                let end = crate::after_now(Duration::from_millis(duration_ms))
                    .ok_or_else(|| tonic::Status::out_of_range("silly diration"))?;
                T::set_timer(self.state.write().as_mut(), end);
                self.on().await
            }
        }
        .map_err(|e| tonic::Status::internal(e.to_string()))
    }

    pub async fn on(&self) -> Result<(), rumqttc::ClientError> {
        self.mqtt.publish(T::SET_TOPIC, b"ON").await
    }

    pub async fn off(&self) -> Result<(), rumqttc::ClientError> {
        self.mqtt.publish(T::SET_TOPIC, b"OFF").await
    }

    async fn await_timer(
        &self,
        mut rx: tokio::sync::watch::Receiver<Arc<PersistentState>>,
    ) -> Result<(), RecvError> {
        loop {
            let until = T::get_timer(rx.borrow_and_update().as_view())
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
                            Ok(()) => T::clear_timer(self.state.write().as_mut()),
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
}

pub struct Garden;

impl OnOffTimerImpl for Garden {
    const SET_TOPIC: &str = "zigbee/garden/set/state";

    fn get_timer(state: PersistentStateView<'_>) -> protobuf::Optional<TimestampView<'_>> {
        state.garden_light_until_opt()
    }

    fn set_timer(mut state: PersistentStateMut<'_>, ts: Timestamp) {
        state.set_garden_light_until(ts);
    }

    fn clear_timer(mut state: PersistentStateMut<'_>) {
        state.clear_garden_light_until();
    }
}

pub struct UnderfloorHeating;

impl OnOffTimerImpl for UnderfloorHeating {
    const SET_TOPIC: &str = "zigbee/underfloor_heating/set/state";

    fn get_timer(state: PersistentStateView<'_>) -> protobuf::Optional<TimestampView<'_>> {
        state.underfloor_heating_until_opt()
    }

    fn set_timer(mut state: PersistentStateMut<'_>, ts: Timestamp) {
        state.set_underfloor_heating_until(ts);
    }

    fn clear_timer(mut state: PersistentStateMut<'_>) {
        state.clear_underfloor_heating_until();
    }
}
