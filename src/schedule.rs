use chrono::{
    DateTime, Datelike, Days, LocalResult, NaiveDateTime, NaiveTime, TimeDelta, Utc, Weekday,
};
use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use thiserror::Error;
use tzfile::Tz;

use crate::pb::{HeatSchedule, SetPoint};

#[derive(Debug, Default)]
struct SchedulePoint {
    water: bool,
    heat: HashMap<String, f64>,
}

impl From<&SchedulePoint> for HeatSchedule {
    fn from(h: &SchedulePoint) -> Self {
        let mut heating = h
            .heat
            .iter()
            .map(|(z, v)| SetPoint {
                zone: Some(z.to_string()),
                setpoint: Some(*v),
            })
            .collect::<Vec<_>>();
        heating.sort_by(|a, b| {
            if a.zone < b.zone {
                std::cmp::Ordering::Less
            } else if a.zone == b.zone {
                std::cmp::Ordering::Equal
            } else {
                std::cmp::Ordering::Greater
            }
        });
        Self {
            hot_water: Some(h.water),
            heating,
        }
    }
}

pub struct Schedule {
    tx: tokio::sync::watch::Sender<Arc<HeatSchedule>>,
}

impl Schedule {
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<Arc<HeatSchedule>> {
        self.tx.subscribe()
    }
}

#[derive(Debug, Error)]
pub enum ScheduleReadError {
    #[error("{0}")]
    IOError(#[from] std::io::Error),
    #[error("{0}")]
    ParseWeekdayError(#[from] chrono::ParseWeekdayError),
    #[error("{0}")]
    ChronoParseError(#[from] chrono::ParseError),
    #[error("Unknown event {0}")]
    UnknownEvent(String),
    #[error("No time in schedule line")]
    MissingTime,
    #[error("{0}")]
    BadTemperature(#[from] std::num::ParseFloatError),
}

#[derive(Clone, Debug)]
enum ProgrammedEventDesc {
    Water(bool),
    HeatZone(String, Option<f64>),
}

#[derive(Clone, Debug)]
struct ProgrammedEvent(NaiveTime, ProgrammedEventDesc);

#[derive(Debug, Default)]
struct ScheduleTracker {
    daily: [Vec<ProgrammedEvent>; 7],
    now: SchedulePoint,
    now_weekday: usize,
    now_time: usize,
}

impl ScheduleTracker {
    fn execute_next_today(&mut self, tx: Option<&tokio::sync::watch::Sender<Arc<HeatSchedule>>>) {
        if self.daily[self.now_weekday].len() > self.now_time {
            match self.daily[self.now_weekday][self.now_time].1 {
                ProgrammedEventDesc::Water(water) => {
                    self.now.water = water;
                }
                ProgrammedEventDesc::HeatZone(ref z, None) => {
                    self.now.heat.remove(z);
                }
                ProgrammedEventDesc::HeatZone(ref z, Some(t)) => {
                    self.now.heat.insert(z.to_string(), t);
                }
            }
            if let Some(tx) = tx {
                tracing::info!(
                    "Executing {} {:?}",
                    Weekday::try_from(u8::try_from(self.now_weekday).unwrap()).unwrap(),
                    self.daily[self.now_weekday][self.now_time]
                );
                let _ = tx.send(Arc::new((&self.now).into()));
            }
        }
        self.now_time += 1;
        if self.now_time >= self.daily[self.now_weekday].len() {
            self.now_weekday = (self.now_weekday + 1) % 7;
            self.now_time = 0;
        }
    }

    fn run_forward(&mut self, weekday: usize) {
        while self.now_weekday != weekday {
            self.execute_next_today(None);
        }
    }

    fn run_forward_to_now_and_get_wait(
        &mut self,
        tz: &Tz,
        tx: Option<&tokio::sync::watch::Sender<Arc<HeatSchedule>>>,
    ) -> Duration {
        let now = DateTime::<Utc>::from(SystemTime::now()).with_timezone(&tz);
        let now_wd = usize::try_from(now.date_naive().weekday().num_days_from_monday()).unwrap();
        let now_d = now.date_naive();
        while self.now_weekday != now_wd {
            self.execute_next_today(tx);
        }
        loop {
            let candidate = if self.now_weekday == now_wd
                && self.now_time < self.daily[self.now_weekday].len()
            {
                // There are events left today. Pick the next one.
                NaiveDateTime::new(now_d, self.daily[now_wd][self.now_time].0)
            } else if !self.daily[(self.now_weekday + 1) % 7].is_empty() {
                // Else there is at least one event tomorrow.
                NaiveDateTime::new(now_d + Days::new(1), self.daily[(now_wd + 1) % 7][0].0)
            } else {
                // Try again later.
                return Duration::from_hours(24);
            };
            let delta = match candidate.and_local_timezone(tz) {
                LocalResult::Single(t) => t.signed_duration_since(&now),
                // Not great, but anything fancier gets quickly out of hand
                LocalResult::Ambiguous(t_earlier, _) => t_earlier.signed_duration_since(&now),
                LocalResult::None => TimeDelta::zero(),
            };
            if delta > TimeDelta::zero() {
                let d = delta.to_std().unwrap();
                tracing::info!("Next scheduled event at {} happens in {:?}", candidate, d);
                return d;
            }
            self.execute_next_today(tx);
        }
    }

    async fn evolve(mut self, tz: Tz, tx: tokio::sync::watch::Sender<Arc<HeatSchedule>>) {
        loop {
            tokio::time::sleep(self.run_forward_to_now_and_get_wait(&tz, Some(&tx))).await;
        }
    }
}

#[derive(clap::Args)]
pub struct ScheduleArgs {
    #[arg(
        long,
        help = "Path to schedule",
        default_value = "/etc/boiler_schedule"
    )]
    schedule: PathBuf,
    #[arg(long, help = "Schedule timezone", default_value = "Europe/London")]
    tz: String,
}

#[resource]
impl Resource for Schedule {
    fn new(
        (): (),
        a: ScheduleArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, ScheduleReadError> {
        let tz = Tz::named(&a.tz)?;
        let mut s = ScheduleTracker::default();
        for l in std::io::BufReader::new(std::fs::File::open(a.schedule)?).lines() {
            let l = l?;
            let mut parts = l.split_whitespace().take_while(|p| !p.starts_with('#'));
            if let Some(weekdays) = parts.next() {
                let weekdays = weekdays
                    .split(',')
                    .map(|s| s.parse::<Weekday>())
                    .collect::<Result<Vec<_>, _>>()?;
                let tod = parts
                    .next()
                    .ok_or(ScheduleReadError::MissingTime)?
                    .parse::<NaiveTime>()?;
                for desc_s in parts {
                    let desc = match desc_s {
                        "water" => Ok(ProgrammedEventDesc::Water(true)),
                        "nowater" => Ok(ProgrammedEventDesc::Water(false)),
                        x => match x.split_once('=') {
                            None => Err(ScheduleReadError::UnknownEvent(x.to_string())),
                            Some((zone, temp_s)) => match temp_s {
                                "off" => Ok(ProgrammedEventDesc::HeatZone(zone.to_string(), None)),
                                x => Ok(ProgrammedEventDesc::HeatZone(
                                    zone.to_string(),
                                    Some(x.parse()?),
                                )),
                            },
                        },
                    }?;
                    for w in weekdays.iter() {
                        s.daily[usize::try_from(w.num_days_from_monday()).unwrap()]
                            .push(ProgrammedEvent(tod, desc.clone()));
                    }
                }
            }
        }
        for d in s.daily.iter_mut() {
            d.sort_by_key(|e| e.0);
        }
        s.run_forward(1);
        s.run_forward(0);
        s.run_forward_to_now_and_get_wait(&tz, None);
        tracing::info!("Schedule start state: {:?}", s.now);
        let (tx, _) = tokio::sync::watch::channel(Arc::new((&s.now).into()));
        let tx2 = tx.clone();
        runtime.set_task(async move {
            s.evolve(tz, tx2).await;
            Err("schedule exited".into())
        });
        Ok(Arc::new(Self { tx }))
    }
}
