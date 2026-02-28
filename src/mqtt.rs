use backoff::backoff::Backoff;
use comprehensive::health::{HealthReporter, HealthSignaller};
use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::Stream;
use pin_project_lite::pin_project;
use rumqttc::Event::Incoming;
use rumqttc::Packet::{ConnAck, Publish};
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
    tx: tokio::sync::watch::Sender<crate::parse::Message>,
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
        #[pin] rx: tokio_stream::wrappers::WatchStream<crate::parse::Message>,
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
        self.project().rx.poll_next(cx)
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
                let (tx, _) = tokio::sync::watch::channel(crate::parse::Message::Empty);
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

    pub async fn publish<T, V>(&self, topic: T, payload: V) -> Result<(), rumqttc::ClientError>
    where
        T: Into<String>,
        V: Into<Vec<u8>>,
    {
        self.client
            .publish(topic, QoS::AtMostOnce, false, payload)
            .await
    }

    async fn run(
        &self,
        mut eventloop: EventLoop,
        health_signaller: HealthSignaller,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut backoff = backoff::ExponentialBackoffBuilder::new()
            .with_max_elapsed_time(None) // Never completely give up.
            .build();
        let mut healthy = false;
        loop {
            let now_healthy = match eventloop.poll().await {
                Ok(Incoming(Publish(msg))) => {
                    self.got_message(msg).await;
                    true
                }
                Ok(Incoming(ConnAck(ack))) => {
                    if ack.code == rumqttc::ConnectReturnCode::Success {
                        true
                    } else {
                        error!("MQTT ConnAck code {:?}", ack.code);
                        false
                    }
                }
                Ok(Incoming(_)) => true,
                Ok(rumqttc::Event::Outgoing(_)) => true,
                Err(error) => {
                    error!("MQTT client error: {:?}", error);
                    false
                }
            };
            if now_healthy {
                if !healthy {
                    healthy = true;
                    health_signaller.set_healthy(true);
                    backoff.reset();
                }
            } else {
                if healthy {
                    healthy = false;
                    health_signaller.set_healthy(false);
                }
                if let Some(duration) = backoff.next_backoff() {
                    sleep(duration).await;
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
        (health_reporter,): (Arc<HealthReporter>,),
        a: MqttArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, comprehensive::ComprehensiveError> {
        let health_signaller = health_reporter.register(Self::NAME)?;
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
            let l = pin!(shared2.run(eventloop, health_signaller));
            let u = pin!(shared3.change_subscriptions(sub_unsub_rx));
            futures::future::select(l, u).await.factor_first().0
        });
        Ok(shared)
    }
}
