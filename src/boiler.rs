use backoff::backoff::Backoff;
use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::{Stream, StreamExt};
use pin_project_lite::pin_project;
use std::marker::PhantomData;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::time::{Sleep, sleep};

use crate::mqtt::Mqtt;
use crate::schedule::Schedule;
use crate::state::State;

pub trait ControlSignal: Send + Sync + 'static {
    const NAME: &str;
    const TOPIC: &str;
    type IntentStream: Stream<Item = bool> + Send;

    fn get_intent_stream(
        mqtt: &Arc<Mqtt>,
        schedule: &Arc<Schedule>,
        state: &Arc<State>,
    ) -> Self::IntentStream;
    fn get_live(msg: crate::parse::Message) -> Option<bool>;
}

pub struct Controller<T> {
    _p: PhantomData<T>,
}

#[derive(Debug)]
enum Update {
    NewIntent(bool),
    NewLive(bool),
    HoldRelease,
    StreamEnded,
}

struct Ended;

impl Stream for Ended {
    type Item = Update;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Update>> {
        Poll::Ready(Some(Update::StreamEnded))
    }
}

pin_project! {
    struct Hold {
        #[pin] after_act: Option<Sleep>,
        #[pin] keep_intention: Option<Sleep>,
        intention_proposed: u64,
        intention_allowed: u64,
    }
}

impl Hold {
    fn new() -> Self {
        Self {
            after_act: None,
            keep_intention: None,
            intention_proposed: 0,
            intention_allowed: 0,
        }
    }

    fn act(self: Pin<&mut Self>, serial: u64, state: bool) -> bool {
        let mut this = self.project();
        if serial == *this.intention_allowed {
            if this.after_act.is_none() {
                this.after_act.set(Some(sleep(Duration::from_secs(20))));
                tracing::info!("Setting actuation to {state}");
                true
            } else {
                false
            }
        } else if serial != *this.intention_proposed {
            *this.intention_proposed = serial;
            this.keep_intention.set(Some(sleep(Duration::from_secs(6))));
            tracing::info!("Want to set actuation to {state}, waiting for stability");
            false
        } else {
            false
        }
    }
}

impl Stream for Hold {
    type Item = Update;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Update>> {
        let mut this = self.project();
        match this.after_act.as_mut().as_pin_mut() {
            None => (),
            Some(f) => match f.poll(cx) {
                Poll::Pending => (),
                Poll::Ready(()) => {
                    this.after_act.set(None);
                    return Poll::Ready(Some(Update::HoldRelease));
                }
            },
        }
        match this.keep_intention.as_mut().as_pin_mut() {
            None => (),
            Some(f) => match f.poll(cx) {
                Poll::Pending => (),
                Poll::Ready(()) => {
                    this.keep_intention.set(None);
                    *this.intention_allowed = *this.intention_proposed;
                    return Poll::Ready(Some(Update::HoldRelease));
                }
            },
        }
        Poll::Pending
    }
}

#[resource]
impl<T: ControlSignal> Resource for Controller<T> {
    const NAME: &str = T::NAME;

    fn new(
        (mqtt, schedule, state): (Arc<Mqtt>, Arc<Schedule>, Arc<State>),
        _: comprehensive::NoArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        runtime.set_task(async move {
            let mut backoff = crate::new_backoff();
            loop {
                let mut want = None;
                let mut got = None;
                let intent = T::get_intent_stream(&mqtt, &schedule, &state)
                    .map(|x| Update::NewIntent(x))
                    .chain(Ended);
                let live = mqtt
                    .subscribe(String::from("zigbee/boiler"))
                    .await
                    .into_stream()
                    .filter_map(|x| std::future::ready(T::get_live(x).map(Update::NewLive)))
                    .chain(Ended);
                let mixed = pin!(futures::stream::select(intent, live));
                let mut mixed_release = pin!(futures::stream::select(mixed, Hold::new()));
                while let Some(m) = mixed_release.next().await {
                    match m {
                        Update::NewIntent(x) => match want {
                            Some((old, i)) if old != x => {
                                want = Some((x, i + 1));
                            }
                            None => {
                                want = Some((x, 1));
                            }
                            _ => (),
                        },
                        Update::NewLive(x) => {
                            got = Some(x);
                        }
                        Update::HoldRelease => (),
                        Update::StreamEnded => {
                            break;
                        }
                    }
                    match (want, got) {
                        (Some((x, i)), Some(y)) if x != y => {
                            let (_, hold) = mixed_release.as_mut().get_pin_mut();
                            if hold.act(i, x) {
                                if let Err(e) = mqtt
                                    .publish(
                                        T::TOPIC,
                                        if x { b"ON".as_ref() } else { b"OFF".as_ref() },
                                    )
                                    .await
                                {
                                    tracing::error!("State change: {e}");
                                }
                            }
                        }
                        _ => (),
                    }
                }
                tracing::warn!("stream ended");
                if let Some(duration) = backoff.next_backoff() {
                    sleep(duration).await;
                }
            }
        });
        Ok(Arc::new(Self { _p: PhantomData }))
    }
}
