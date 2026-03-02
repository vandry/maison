use backoff::backoff::Backoff;
use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::{Stream, StreamExt};
use std::pin::{Pin, pin};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::mqtt::Mqtt;

pub struct AutoKitchen {
    suppress: Mutex<Option<Instant>>,
}

impl AutoKitchen {
    pub fn suppress(&self) {
        if let Ok(mut lock) = self.suppress.lock() {
            *lock = Some(Instant::now());
        }
    }
}

#[derive(Debug)]
enum Update {
    StreamEnded,
    L0(Option<bool>),
    L1(Option<bool>),
    L2(Option<bool>),
}

struct Ended;

impl Stream for Ended {
    type Item = Update;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Update>> {
        Poll::Ready(Some(Update::StreamEnded))
    }
}

#[resource]
impl Resource for AutoKitchen {
    fn new(
        (mqtt,): (Arc<Mqtt>,),
        _: comprehensive::NoArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        let shared = Arc::new(Self {
            suppress: Mutex::new(None),
        });
        let shared2 = Arc::clone(&shared);
        runtime.set_task(async move {
            let mut backoff = crate::new_backoff();
            loop {
                let (l0, l1, l2) = futures::join!(
                    mqtt.subscribe(String::from("zigbee/kitchen_ceiling")),
                    mqtt.subscribe(String::from("zigbee/kitchen_under_cupboards")),
                    mqtt.subscribe(String::from("zigbee/kitchen_under_stairs")),
                );
                let l0 = l0
                    .into_stream()
                    .filter_map(|m| {
                        std::future::ready(match m {
                            crate::parse::Message::SimpleSwitch(crate::pb::SimpleSwitch {
                                state,
                            }) => Some(Update::L0(state)),
                            _ => None,
                        })
                    })
                    .chain(Ended);
                let l1 = l1
                    .into_stream()
                    .filter_map(|m| {
                        std::future::ready(match m {
                            crate::parse::Message::SimpleSwitch(crate::pb::SimpleSwitch {
                                state,
                            }) => Some(Update::L1(state)),
                            _ => None,
                        })
                    })
                    .chain(Ended);
                let l2 = l2
                    .into_stream()
                    .filter_map(|m| {
                        std::future::ready(match m {
                            crate::parse::Message::SimpleSwitch(crate::pb::SimpleSwitch {
                                state,
                            }) => Some(Update::L2(state)),
                            _ => None,
                        })
                    })
                    .chain(Ended);
                let l1l2 = futures::stream::select(l1, l2);
                let mut updates = pin!(futures::stream::select(l0, l1l2));
                let mut l0 = None;
                let mut l1 = None;
                let mut l2 = None;
                while let Some(update) = updates.next().await {
                    match update {
                        Update::StreamEnded => {
                            break;
                        }
                        Update::L1(x) => {
                            l1 = x;
                        }
                        Update::L2(x) => {
                            l2 = x;
                        }
                        Update::L0(None) => {
                            l0 = None;
                        }
                        Update::L0(Some(new)) => {
                            if l0 == Some(!new) && l1 == Some(!new) && l2 == Some(!new) {
                                // All the lights were in the same state and now the
                                // ceiling light has flipped. The others can follow.
                                let proceed = match shared.suppress.lock() {
                                    Err(_) => true,
                                    Ok(mut suppress) => match *suppress {
                                        None => true,
                                        Some(earlier) => {
                                            *suppress = None;
                                            earlier.elapsed() > Duration::from_secs(2)
                                        }
                                    },
                                };
                                if proceed {
                                    let msg = if new { b"ON".as_ref() } else { b"OFF".as_ref() };
                                    tracing::info!("Toggling kitchen LED strips to {new}");
                                    let (r1, r2) = futures::join!(
                                        mqtt.publish(
                                            "zigbee/kitchen_under_cupboards/set/state",
                                            msg
                                        ),
                                        mqtt.publish("zigbee/kitchen_under_stairs/set/state", msg),
                                    );
                                    if let Err(e) = r1 {
                                        tracing::error!(
                                            "Set kitchen_under_cupboards to {new}: {e}"
                                        );
                                    }
                                    if let Err(e) = r2 {
                                        tracing::error!("Set kitchen_under_stairs to {new}: {e}");
                                    }
                                }
                            }
                            l0 = Some(new);
                        }
                    }
                }
                tracing::warn!("stream ended");
                if let Some(duration) = backoff.next_backoff() {
                    sleep(duration).await;
                }
            }
        });
        Ok(shared2)
    }
}
