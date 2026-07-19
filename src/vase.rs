use async_stream::stream;
use backoff::backoff::Backoff;
use chrono::{DateTime, NaiveTime, Utc};
use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use futures::StreamExt;
use humantime::parse_duration;
use pin_project_lite::pin_project;
use serde::Serialize;
use std::collections::VecDeque;
use std::f64::consts::PI;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::{Duration, SystemTime};
use tokio::time::sleep;
use tzfile::Tz;

use crate::daylight::SunStatus;
use crate::mqtt::Mqtt;

const NOON: NaiveTime = NaiveTime::from_hms_opt(12, 0, 0).unwrap();

#[derive(clap::Args)]
pub struct VaseArgs {
    #[arg(long, help = "Schedule timezone", default_value = "Europe/London")]
    vase_tz: String,
    #[arg(long, help = "Vase bedtime", default_value = "23:30")]
    vase_bedtime: NaiveTime,
    #[arg(long, help = "Fade in length at sunset", default_value = "600s", value_parser = parse_duration)]
    vase_fade_in: Duration,
    #[arg(long, help = "Fade out length at bedtime", default_value = "600s", value_parser = parse_duration)]
    vase_fade_out: Duration,
    #[arg(
        long,
        help = "Attractor colour wheel circle radius",
        default_value = "0.9"
    )]
    vase_colour_radius: f64,
    #[arg(long, help = "Vase resolution", default_value = "200")]
    vase_smear: usize,
    #[arg(long, help = "Vase attraction attenuation", default_value = "80.0")]
    vase_attenuate: f64,
    #[arg(long, help = "Vase brightness", default_value = "0.5")]
    vase_brightness: f64,
    #[arg(long, help = "Vase tick interval", default_value = "10s", value_parser = parse_duration)]
    vase_tick: Duration,
}

struct Setter<'a> {
    mqtt: &'a crate::mqtt::Mqtt,
    tz: Tz,
    bedtime: NaiveTime,
    fade_in: Duration,
    fade_out: Duration,
    max_brightness: f64,
    quiet: bool,
}

#[derive(Debug, Default, Serialize)]
struct HueColour {
    hue: f64,
    saturation: f64,
}

#[derive(Serialize)]
struct HueOut {
    color: HueColour,
    color_mode: &'static str,
    brightness: f64,
    state: &'static str,
}

impl<'a> Setter<'a> {
    fn new(
        mqtt: &'a crate::mqtt::Mqtt,
        tz: Tz,
        bedtime: NaiveTime,
        fade_in: Duration,
        fade_out: Duration,
        max_brightness: f64,
    ) -> Self {
        Self {
            mqtt,
            tz,
            bedtime,
            fade_in,
            fade_out,
            max_brightness,
            quiet: false,
        }
    }

    async fn export(&mut self, x: f64, y: f64) {
        let hue = y.atan2(x) + if y.is_sign_negative() { 2.0 * PI } else { 0.0 };
        let saturation = (x * x + y * y).sqrt().clamp(0.0, 1.0);
        let brightness = match crate::daylight::state_and_len() {
            None => None,
            Some(SunStatus {
                is_day: true,
                duration_until_change,
                ..
            }) => {
                if duration_until_change > self.fade_in {
                    None
                } else {
                    Some(1.0 - (duration_until_change.as_secs_f64() / self.fade_in.as_secs_f64()))
                }
            }
            Some(SunStatus { is_day: false, .. }) => {
                let now = DateTime::<Utc>::from(SystemTime::now())
                    .with_timezone(&&self.tz)
                    .time();
                if (self.bedtime > NOON && now < NOON) || now >= self.bedtime {
                    None
                } else {
                    let left = self.bedtime.signed_duration_since(now).to_std().unwrap();
                    if left > self.fade_out {
                        Some(1.0)
                    } else {
                        Some(left.as_secs_f64() / self.fade_out.as_secs_f64())
                    }
                }
            }
        };
        if brightness.is_none() && self.quiet {
            return;
        }
        let j = serde_json::to_vec(&HueOut {
            color: HueColour {
                hue: hue.to_degrees(),
                saturation: saturation * 100.0,
            },
            color_mode: "hs",
            brightness: brightness.map(|b| b * self.max_brightness).unwrap_or(1.0) * 256.0,
            state: brightness.map(|_| "ON").unwrap_or("OFF"),
        })
        .unwrap();
        if let Err(e) = self.mqtt.publish("zigbee/vase_lamp/set", j).await {
            tracing::error!("MQTT set: {e}");
        }
        self.quiet = brightness.is_none();
    }
}

struct Move {
    x: f64,
    y: f64,
}

impl Move {
    fn new(radius: f64) -> Self {
        let angle = rand::random::<f64>() * PI * 2.0;
        let (s, c) = angle.sin_cos();
        Self {
            x: c * radius,
            y: s * radius,
        }
    }
}

async fn walk<S>(
    mut setter: Setter<'_>,
    mut c: HueColour,
    mut stream: Pin<&mut S>,
    radius: f64,
    smear: usize,
    attenuate: f64,
) where
    S: futures::Stream<Item = Option<HueColour>>,
{
    let mut moves = VecDeque::with_capacity(smear * 2);
    loop {
        let saturation = c.saturation / 100.0;
        let hue = c.hue.to_radians();
        let (si, co) = hue.sin_cos();
        let mut x = saturation * co;
        let mut y = saturation * si;
        moves.push_front(Move::new(radius));
        for (step, m) in moves.iter().enumerate() {
            let norm = smear as f64;
            let effect = (step as f64) - norm;
            let vec_x = m.x - x;
            let vec_y = m.y - y;
            let d = (vec_x * vec_x + vec_y * vec_y).sqrt();
            let frac = ((effect * effect * -1.0) / (norm * norm) + 1.0) / attenuate;
            if frac.is_sign_positive() {
                x += frac * vec_x / d * (2.0 - d);
                y += frac * vec_y / d * (2.0 - d);
            }
        }
        setter.export(x, y).await;
        loop {
            match stream.next().await {
                None => {
                    return;
                }
                Some(Some(new_c)) => {
                    c = new_c;
                }
                Some(None) => {
                    break;
                }
            }
        }
        while moves.len() >= smear * 2 {
            moves.pop_back();
        }
    }
}

fn current_value(mqtt: &Mqtt) -> impl futures::Stream<Item = Option<HueColour>> {
    stream! {
        let mut backoff = crate::new_backoff();
        loop {
            let mut sub = mqtt
                .subscribe(String::from("zigbee/vase_lamp"))
                .into_stream();
            while let Some(m) = sub.next().await {
                if let crate::parse::Message::Hue(h) = m {
                    let colour = h.color.unwrap_or_default();
                    yield Some(HueColour {
                        hue: colour.hue.unwrap_or_default(),
                        saturation: colour.saturation.unwrap_or_default(),
                    });
                }
            }
            tracing::warn!("stream ended");
            if let Some(duration) = backoff.next_backoff() {
                sleep(duration).await;
            }
        }
    }
}

pin_project! {
    struct Tick {
        #[pin] sleeper: tokio::time::Sleep,
        interval: Duration,
    }
}

impl Tick {
    fn new(interval: Duration) -> Self {
        Self {
            sleeper: sleep(Duration::from_secs(30)),
            interval,
        }
    }

    fn tick(self: Pin<&mut Self>) {
        let i = self.interval;
        self.project().sleeper.set(sleep(i));
    }
}

impl futures::Stream for Tick {
    type Item = Option<HueColour>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Option<HueColour>>> {
        ready!(self.as_mut().project().sleeper.poll(cx));
        self.tick();
        Poll::Ready(Some(None))
    }
}

pub struct VaseLamp;

#[resource]
impl Resource for VaseLamp {
    fn new(
        (mqtt,): (Arc<Mqtt>,),
        a: VaseArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::io::Error> {
        let tz = Tz::named(&a.vase_tz)?;
        runtime.set_task(async move {
            let mut stream = pin!(futures::stream::select(
                current_value(&mqtt),
                Tick::new(a.vase_tick)
            ));
            let c = match stream.next().await {
                None => {
                    return Err("stream ended".into());
                }
                Some(None) => HueColour::default(),
                Some(Some(c)) => c,
            };
            stream.as_mut().get_pin_mut().1.tick();
            walk(
                Setter::new(
                    &mqtt,
                    tz,
                    a.vase_bedtime,
                    a.vase_fade_in,
                    a.vase_fade_out,
                    a.vase_brightness,
                ),
                c,
                stream,
                a.vase_colour_radius,
                a.vase_smear,
                a.vase_attenuate,
            )
            .await;
            Err("walk ended".into())
        });
        Ok(Arc::new(Self))
    }
}
