use chrono::{Datelike, NaiveDate};
use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use humantime::parse_duration;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::time::sleep;

use crate::daylight::SunStatus;

#[derive(Clone, Debug)]
struct DayOfYear {
    month: u32,
    day: u32,
}

#[derive(Debug, Error)]
enum DayOfYearParseError {
    #[error("expected 4 digits (mmdd)")]
    WrongFormat,
    #[error("date should be mmdd but is out of range")]
    OutOfRange,
    #[error("{0}")]
    ParseIntError(#[from] std::num::ParseIntError),
}

impl FromStr for DayOfYear {
    type Err = DayOfYearParseError;

    fn from_str(s: &str) -> Result<Self, DayOfYearParseError> {
        if s.len() != 4 {
            return Err(DayOfYearParseError::WrongFormat);
        }
        let month = s[..2].parse()?;
        let day = s[2..].parse()?;
        if month < 1 || month > 12 || day < 1 || day > 31 {
            return Err(DayOfYearParseError::OutOfRange);
        }
        Ok(Self { month, day })
    }
}

#[derive(Debug)]
struct Season {
    start: DayOfYear,
    end: DayOfYear,
}

impl Season {
    fn within(&self) -> Option<bool> {
        let now_dt: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
        let now = now_dt.date_naive();
        let this_year = now.year();
        let t0 = NaiveDate::from_ymd_opt(this_year, self.start.month, self.start.day)?;
        let t1 = NaiveDate::from_ymd_opt(this_year, self.end.month, self.end.day)?;
        Some(if t1 > t0 {
            now > t0 && now < t1
        } else {
            now < t0 || now > t1
        })
    }
}

#[derive(clap::Args)]
pub struct AutoSprinklersArgs {
    #[arg(long, help = "Delay after sunset before running sprinklers", default_value = "30min", value_parser = parse_duration)]
    sprinklers_delay_after_sunset: Duration,
    #[arg(long, help = "Amount of time to run autosprinklers for", default_value = "10min", value_parser = parse_duration)]
    sprinklers_duration: Duration,
    #[arg(long, help = "Amount of time to run autosprinklers for in the off season", default_value = "20sec", value_parser = parse_duration)]
    off_season_sprinklers_duration: Duration,
    #[arg(
        long,
        help = "Date of start of sprinkler season as mmdd",
        default_value = "0301"
    )]
    season_start: DayOfYear,
    #[arg(
        long,
        help = "Date of end of sprinkler season as mmdd",
        default_value = "1001"
    )]
    season_end: DayOfYear,
}

pub struct AutoSprinklers;

#[resource]
impl Resource for AutoSprinklers {
    fn new(
        (sprinklers,): (Arc<crate::lights::OnOffTimer<crate::lights::GardenSprinklers>>,),
        a: AutoSprinklersArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::num::TryFromIntError> {
        let in_season_ms = a.sprinklers_duration.as_millis().try_into()?;
        let off_season_ms = a.off_season_sprinklers_duration.as_millis().try_into()?;
        if in_season_ms == 0 && off_season_ms == 0 {
            return Ok(Arc::new(Self));
        }
        let season = Season {
            start: a.season_start,
            end: a.season_end,
        };
        runtime.set_task(async move {
            loop {
                let d = match crate::daylight::state_and_len() {
                    None => Duration::from_hours(1),
                    Some(SunStatus {
                        is_day: true,
                        duration_until_change,
                        ..
                    }) => duration_until_change,
                    Some(SunStatus {
                        is_day: false,
                        duration_until_change,
                        duration_since_change: None,
                    }) => duration_until_change,
                    Some(SunStatus {
                        is_day: false,
                        duration_until_change,
                        duration_since_change: Some(d),
                        ..
                    }) => match a.sprinklers_delay_after_sunset.checked_sub(d) {
                        Some(duration_until_on) => duration_until_on,
                        None => {
                            let lateness = d - a.sprinklers_delay_after_sunset;
                            if lateness < a.sprinklers_duration {
                                let ms = match season.within() {
                                    Some(true) => in_season_ms,
                                    Some(false) => off_season_ms,
                                    None => {
                                        tracing::error!(
                                            "Unable to determine if we are in {season:?}"
                                        );
                                        0
                                    }
                                };
                                if ms > 0 {
                                    tracing::info!("Turn on sprinklers after sunset for {ms}");
                                    if let Err(e) = sprinklers.duration_ms(Some(ms)).await {
                                        tracing::error!("Unable to turn on garden sprinklers: {e}");
                                    }
                                } else {
                                    tracing::info!(
                                        "Not turning on sprinklers after sunset for zero time"
                                    );
                                }
                            } else {
                                tracing::warn!("Too late for today, we are {lateness:?} late");
                            }
                            duration_until_change
                        }
                    },
                };
                sleep(d).await;
            }
        });
        Ok(Arc::new(Self))
    }
}
