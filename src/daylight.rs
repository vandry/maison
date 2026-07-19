use async_stream::stream;
use chrono::NaiveDate;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use sunrise::{Coordinates, SolarDay, SolarEvent};
use tokio::time::sleep;

pub struct SunStatus {
    pub is_day: bool,
    pub duration_until_change: Duration,
    pub duration_since_change: Option<Duration>,
}

pub fn state_and_len() -> Option<SunStatus> {
    let now = SystemTime::now();
    let today = NaiveDate::from_epoch_days(
        (now.duration_since(UNIX_EPOCH).ok()?.as_secs() / 86400)
            .try_into()
            .ok()?,
    )?;
    let solar = SolarDay::new(Coordinates::new(51.5, -0.1).unwrap(), today);
    let rise: SystemTime = solar.event_time(SolarEvent::Sunrise)?.into();
    match rise.duration_since(now) {
        Ok(duration_until_rise) => Some(SunStatus {
            is_day: false,
            duration_until_change: duration_until_rise,
            duration_since_change: None,
        }),
        Err(e) => {
            let set: SystemTime = solar.event_time(SolarEvent::Sunset)?.into();
            match set.duration_since(now) {
                Ok(duration_until_set) => Some(SunStatus {
                    is_day: true,
                    duration_until_change: duration_until_set,
                    duration_since_change: Some(e.duration()),
                }),
                Err(e) => {
                    let tomorrow = today.succ_opt()?;
                    let solar = SolarDay::new(Coordinates::new(51.5, -0.1).unwrap(), tomorrow);
                    let rise: SystemTime = solar.event_time(SolarEvent::Sunrise)?.into();
                    Some(SunStatus {
                        is_day: false,
                        duration_until_change: rise.duration_since(now).ok()?,
                        duration_since_change: Some(e.duration()),
                    })
                }
            }
        }
    }
}

pub fn stream() -> impl futures::Stream<Item = bool> {
    stream! {
        let mut present = None;
        loop {
            match state_and_len() {
                None => {
                    sleep(Duration::from_hours(1)).await;
                }
                Some(SunStatus { is_day: new, duration_until_change: d, .. }) => {
                    if Some(new) != present {
                        present = Some(new);
                        yield new;
                    }
                    sleep(d).await;
                }
            }
        }
    }
}

pub fn is_night() -> Option<bool> {
    state_and_len().map(|SunStatus { is_day, .. }| !is_day)
}
