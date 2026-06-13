use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::Stream;
use pin_project_lite::pin_project;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::SystemTime;
use tokio::time::Sleep;

use crate::boiler::ControlSignal;
use crate::schedule::Schedule;
use crate::state::State;

use crate::pb::{HeatSchedule, PersistentState};

pub struct HotWater(Arc<Schedule>, Arc<State>);

pin_project! {
    pub struct HotWaterIntentStream {
        reported: bool,
        programmed: Option<bool>,
        #[pin] override_countdown: Option<Sleep>,
        #[pin] schedule: tokio_stream::wrappers::WatchStream<Arc<HeatSchedule>>,
        #[pin] state: tokio_stream::wrappers::WatchStream<Arc<PersistentState>>,
    }
}

impl Stream for HotWaterIntentStream {
    type Item = bool;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<bool>> {
        let mut this = self.project();
        loop {
            match this.state.as_mut().poll_next(cx) {
                Poll::Ready(Some(x)) => {
                    this.override_countdown.set(
                        crate::proto_ts_to_delay(
                            x.hot_water_override_until_opt(),
                            SystemTime::now(),
                        )
                        .map(tokio::time::sleep),
                    );
                }
                Poll::Ready(None) => {
                    tracing::error!("State stream ended");
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    break;
                }
            }
        }
        if let Some(s) = this.override_countdown.as_mut().as_pin_mut() {
            match s.poll(cx) {
                Poll::Ready(()) => {
                    this.override_countdown.set(None);
                }
                Poll::Pending => {
                    if !*this.reported {
                        *this.reported = true;
                        return Poll::Ready(Some(true));
                    }
                    return Poll::Pending;
                }
            }
        }
        loop {
            match this.schedule.as_mut().poll_next(cx) {
                Poll::Ready(Some(x)) => {
                    let now = x.hot_water.unwrap_or_default();
                    *this.programmed = Some(now);
                }
                Poll::Ready(None) => {
                    tracing::error!("Schedule stream ended");
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    break;
                }
            }
        }
        if let Some(programmed) = this.programmed {
            if *programmed != *this.reported {
                *this.reported = *programmed;
                return Poll::Ready(Some(*programmed));
            }
        }
        Poll::Pending
    }
}

#[resource]
impl Resource for HotWater {
    fn new(
        (schedule, state): (Arc<Schedule>, Arc<State>),
        _: comprehensive::NoArgs,
        _: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        Ok(Arc::new(Self(schedule, state)))
    }
}

impl ControlSignal for HotWater {
    const CONTROLLER_NAME: &str = "Controller<HotWater>";
    const TOPIC: &str = "zigbee/boiler/set/state_l1";
    type IntentStream = HotWaterIntentStream;

    fn get_intent_stream(&self) -> Self::IntentStream {
        HotWaterIntentStream {
            reported: false,
            programmed: None,
            override_countdown: None,
            schedule: tokio_stream::wrappers::WatchStream::new(self.0.subscribe()),
            state: tokio_stream::wrappers::WatchStream::new(self.1.subscribe()),
        }
    }

    fn get_live(msg: crate::parse::Message) -> Option<bool> {
        match msg {
            crate::parse::Message::Boiler(x) => x.hot_water,
            _ => None,
        }
    }
}
