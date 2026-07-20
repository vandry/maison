use futures::Stream;
use pin_project_lite::pin_project;
use std::pin::Pin;
use std::task::{Context, Poll};

pin_project! {
    pub struct OptionStream<T> {
        #[pin] inner: Option<T>,
    }
}

impl<T> OptionStream<T> {
    pub fn new(inner: Option<T>) -> Self {
        Self { inner }
    }
}

impl<T: Stream> Stream for OptionStream<T> {
    type Item = T::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.project().inner.as_pin_mut() {
            Some(inner) => inner.poll_next(cx),
            None => Poll::Ready(None),
        }
    }
}
