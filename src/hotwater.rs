use futures::Stream;
use pin_project_lite::pin_project;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::boiler::ControlSignal;
use crate::mqtt::Mqtt;
use crate::schedule::Schedule;
use crate::state::State;

use crate::pb::HeatSchedule;

pub struct HotWater;

pin_project! {
    pub struct HotWaterIntentStream {
        #[pin] inner: tokio_stream::wrappers::WatchStream<Arc<HeatSchedule>>,
    }
}

impl Stream for HotWaterIntentStream {
    type Item = bool;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<bool>> {
        match self.project().inner.poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(x)) => Poll::Ready(Some(x.hot_water.unwrap_or_default())),
        }
    }
}

impl ControlSignal for HotWater {
    const NAME: &str = "Controller<HotWater>";
    const TOPIC: &str = "zigbee/boiler/set/state_l1";
    type IntentStream = HotWaterIntentStream;

    fn get_intent_stream(
        _: &Arc<Mqtt>,
        schedule: &Arc<Schedule>,
        _: &Arc<State>,
    ) -> Self::IntentStream {
        HotWaterIntentStream {
            inner: tokio_stream::wrappers::WatchStream::new(schedule.subscribe()),
        }
    }

    fn get_live(msg: crate::parse::Message) -> Option<bool> {
        match msg {
            crate::parse::Message::Boiler(x) => x.hot_water,
            _ => None,
        }
    }
}
