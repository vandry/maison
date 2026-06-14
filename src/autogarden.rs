use backoff::backoff::Backoff;
use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::{Stream, StreamExt};
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::time::sleep;

use crate::mqtt::Mqtt;

pub struct AutoGarden;

#[derive(Debug)]
enum Update {
    StreamEnded,
    LightState(Option<bool>),
    Contact(Option<bool>),
}

struct Ended;

impl Stream for Ended {
    type Item = Update;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Update>> {
        Poll::Ready(Some(Update::StreamEnded))
    }
}

#[resource]
impl Resource for AutoGarden {
    fn new(
        (mqtt, lights): (
            Arc<Mqtt>,
            Arc<crate::lights::OnOffTimer<crate::lights::Garden>>,
        ),
        _: comprehensive::NoArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        runtime.set_task(async move {
            let mut backoff = crate::new_backoff();
            loop {
                let light = mqtt
                    .subscribe(String::from("zigbee/garden"))
                    .into_stream()
                    .filter_map(|m| std::future::ready(match m {
                        crate::parse::Message::SimpleSwitch(crate::pb::SimpleSwitch {
                            state,
                        }) => Some(Update::LightState(state)),
                        _ => None,
                    }))
                    .chain(Ended);
                let door = mqtt
                    .subscribe(String::from("zigbee/garden_door"))
                    .into_stream()
                    .filter_map(|m| std::future::ready(match m {
                        crate::parse::Message::Contact(c) => Some(Update::Contact(c)),
                        _ => None,
                    }))
                    .chain(Ended);
                let mut updates = pin!(futures::stream::select(light, door));
                let mut live_lights = None;
                let mut contact = None;
                while let Some(update) = updates.next().await {
                    match update {
                        Update::StreamEnded => {
                            break;
                        }
                        Update::LightState(x) => {
                            live_lights = x;
                        }
                        Update::Contact(Some(false)) => {
                            if matches!(contact, Some(true)) && matches!(live_lights, Some(false)) {
                                match crate::daylight::is_night() {
                                    None => {
                                        tracing::info!("Garden door opened while garden lights are off but I do not know if it is night");
                                    }
                                    Some(false) => {
                                        tracing::info!("Garden door opened while garden lights are off in the daytime");
                                    }
                                    Some(true) => {
                                        tracing::info!("Garden door opened while garden lights are off at night");
                                        if let Err(e) = lights.duration_ms(Some(600000)).await {
                                            tracing::error!("Unable to turn on garden lights: {e}");
                                        }
                                    }
                                }
                            }
                            contact = Some(false);
                        }
                        Update::Contact(x) => {
                            contact = x;
                        }
                    }
                }
                tracing::warn!("stream ended");
                if let Some(duration) = backoff.next_backoff() {
                    sleep(duration).await;
                }
            }
        });
        Ok(Arc::new(Self))
    }
}
