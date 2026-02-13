use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::Stream;
use pin_project_lite::pin_project;
use rumqttc::Event::Incoming;
use rumqttc::Packet::Publish;
use rumqttc::{AsyncClient, EventLoop, MqttOptions, QoS};
use std::net::SocketAddr;
use std::pin::{Pin, pin};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::sleep;
use tracing::error;
use weak_table::WeakValueHashMap;

#[derive(Debug)]
struct SubscriptionCancel {
    topic: String,
    unsub: UnboundedSender<SubUnsub>,
}

#[derive(Debug)]
pub struct Subscription {
    tx: tokio::sync::broadcast::Sender<crate::parse::Message>,
    cancel: Option<SubscriptionCancel>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(SubscriptionCancel { topic, unsub }) = self.cancel.take() {
            let _ = unsub.send(SubUnsub::Unsub(topic));
        }
    }
}

pin_project! {
    pub struct SubscriptionStream {
        _subscription: Arc<Subscription>,
        #[pin] rx: tokio_stream::wrappers::BroadcastStream<crate::parse::Message>,
    }
}

impl Subscription {
    pub fn into_stream(self: Arc<Self>) -> SubscriptionStream {
        let rx = self.tx.subscribe().into();
        SubscriptionStream {
            _subscription: self,
            rx,
        }
    }
}

impl Stream for SubscriptionStream {
    type Item = crate::parse::Message;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.project().rx.poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Err(_))) => Poll::Ready(None),
            Poll::Ready(Some(Ok(b))) => Poll::Ready(Some(b)),
        }
    }
}

enum SubUnsub {
    Sub(Weak<Subscription>),
    Unsub(String),
}

pub struct Mqtt {
    client: AsyncClient,
    topics: Mutex<WeakValueHashMap<String, Weak<Subscription>>>,
    sub_unsub: UnboundedSender<SubUnsub>,
}

impl Mqtt {
    async fn got_message(&self, msg: rumqttc::Publish) {
        let topics = self.topics.lock().unwrap();
        if let Some(s) = topics.get(&msg.topic) {
            let _ = s.tx.send(msg.into());
        }
    }

    pub async fn subscribe(&self, topic: String) -> Arc<Subscription> {
        let mut topics = self.topics.lock().unwrap();
        match topics.entry(topic) {
            weak_table::weak_value_hash_map::Entry::Vacant(e) => {
                let (tx, _) = tokio::sync::broadcast::channel(2);
                let s = Arc::new(Subscription {
                    tx,
                    cancel: Some(SubscriptionCancel {
                        topic: e.key().to_string(),
                        unsub: self.sub_unsub.clone(),
                    }),
                });
                let ret = e.insert(s);
                let _ = self.sub_unsub.send(SubUnsub::Sub(Arc::downgrade(&ret)));
                ret
            }
            weak_table::weak_value_hash_map::Entry::Occupied(e) => e.get_strong(),
        }
    }

    async fn run(&self, mut eventloop: EventLoop) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            match eventloop.poll().await {
                Ok(notification) => {
                    if let Incoming(Publish(msg)) = notification {
                        self.got_message(msg).await;
                    }
                }
                Err(error) => {
                    error!("MQTT client error: {:?}", error);
                    sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }

    async fn change_subscriptions(
        &self,
        mut rx: UnboundedReceiver<SubUnsub>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        while let Some(sub_unsub) = rx.recv().await {
            match sub_unsub {
                SubUnsub::Sub(w) => {
                    if let Some(s) = w.upgrade()
                        && let Some(c) = s.cancel.as_ref()
                        && self.topics.lock().unwrap().contains_key(&c.topic)
                        && let Err(e) = self
                            .client
                            .subscribe(c.topic.clone(), QoS::AtMostOnce)
                            .await
                    {
                        error!("Error unsubscribing to {}: {e}", c.topic);
                    }
                }
                SubUnsub::Unsub(topic) => {
                    if !self.topics.lock().unwrap().contains_key(&topic)
                        && let Err(e) = self.client.unsubscribe(topic).await
                    {
                        error!("Error unsubscribing: {e}");
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(clap::Args)]
pub struct MqttArgs {
    #[arg(long, help = "Address of MQTT broker")]
    mqtt_address: SocketAddr,
}

#[resource]
impl Resource for Mqtt {
    fn new(
        _: (),
        a: MqttArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::convert::Infallible> {
        let ephemeral = uuid::Uuid::new_v4();
        let mut mqttoptions = MqttOptions::new(
            format!("maison-{ephemeral}"),
            a.mqtt_address.ip().to_string(),
            a.mqtt_address.port(),
        );
        mqttoptions.set_keep_alive(Duration::from_secs(5));
        let (client, eventloop) = AsyncClient::new(mqttoptions, 10);
        let (sub_unsub_tx, sub_unsub_rx) = unbounded_channel();
        let shared = Arc::new(Self {
            client,
            topics: Mutex::new(WeakValueHashMap::new()),
            sub_unsub: sub_unsub_tx,
        });
        let shared2 = Arc::clone(&shared);
        let shared3 = Arc::clone(&shared);
        runtime.set_task(async move {
            let l = pin!(shared2.run(eventloop));
            let u = pin!(shared3.change_subscriptions(sub_unsub_rx));
            futures::future::select(l, u).await.factor_first().0
        });
        Ok(shared)
    }
}
