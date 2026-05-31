use async_stream::stream;
use chrono::NaiveDate;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use sunrise::{Coordinates, SolarDay, SolarEvent};
use tokio::time::sleep;

fn state_and_len() -> Option<(bool, Duration)> {
    let now = SystemTime::now();
    let today = NaiveDate::from_epoch_days(
        (now.duration_since(UNIX_EPOCH).ok()?.as_secs() / 86400)
            .try_into()
            .ok()?,
    )?;
    let solar = SolarDay::new(Coordinates::new(51.5, -0.1).unwrap(), today);
    let rise: SystemTime = solar.event_time(SolarEvent::Sunrise)?.into();
    match rise.duration_since(now) {
        Ok(duration_until_rise) => Some((false, duration_until_rise)),
        Err(_) => {
            let set: SystemTime = solar.event_time(SolarEvent::Sunset)?.into();
            match set.duration_since(now) {
                Ok(duration_until_set) => Some((true, duration_until_set)),
                Err(_) => {
                    let tomorrow = today.succ_opt()?;
                    let solar = SolarDay::new(Coordinates::new(51.5, -0.1).unwrap(), tomorrow);
                    let rise: SystemTime = solar.event_time(SolarEvent::Sunrise)?.into();
                    Some((false, rise.duration_since(now).ok()?))
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
                Some((new, d)) => {
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
    state_and_len().map(|(day, _)| !day)
}
