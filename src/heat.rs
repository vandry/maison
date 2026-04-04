use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::Stream;
use pin_project_lite::pin_project;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::boiler::ControlSignal;
use crate::state::State;

use crate::pb::PersistentState;

pub struct Heat(Arc<State>);

pin_project! {
    pub struct HeatIntentStream {
        #[pin] inner: tokio_stream::wrappers::WatchStream<Arc<PersistentState>>,
    }
}

impl Stream for HeatIntentStream {
    type Item = bool;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<bool>> {
        match self.project().inner.poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(x)) => Poll::Ready(Some(
                x.heating_override().iter().any(|o| o.hysteresis().state()),
            )),
        }
    }
}

#[resource]
impl Resource for Heat {
    fn new(
        (state,): (Arc<State>,),
        _: comprehensive::NoArgs,
        _: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        Ok(Arc::new(Self(state)))
    }
}

impl ControlSignal for Heat {
    const CONTROLLER_NAME: &str = "Controller<Heat>";
    const TOPIC: &str = "zigbee/boiler/set/state_l2";
    type IntentStream = HeatIntentStream;

    fn get_intent_stream(&self) -> Self::IntentStream {
        HeatIntentStream {
            inner: tokio_stream::wrappers::WatchStream::new(self.0.subscribe()),
        }
    }

    fn get_live(msg: crate::parse::Message) -> Option<bool> {
        match msg {
            crate::parse::Message::Boiler(x) => x.heating,
            _ => None,
        }
    }
}
