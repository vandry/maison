use comprehensive::v1::{AssemblyRuntime, Resource, resource};
use humantime::parse_duration;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use crate::daylight::SunStatus;

#[derive(clap::Args)]
pub struct AutoSprinklersArgs {
    #[arg(long, help = "Delay after sunset before running sprinklers", default_value = "30min", value_parser = parse_duration)]
    sprinklers_delay_after_sunset: Duration,
    #[arg(long, help = "Amount of time to run autosprinklers for", default_value = "10min", value_parser = parse_duration)]
    sprinklers_duration: Duration,
}

pub struct AutoSprinklers;

#[resource]
impl Resource for AutoSprinklers {
    fn new(
        (sprinklers,): (Arc<crate::lights::OnOffTimer<crate::lights::GardenSprinklers>>,),
        a: AutoSprinklersArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, std::num::TryFromIntError> {
        let ms = a.sprinklers_duration.as_millis().try_into()?;
        if ms == 0 {
            return Ok(Arc::new(Self));
        }
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
                                tracing::info!("Turn on sprinklers after sunset");
                                if let Err(e) = sprinklers.duration_ms(Some(ms)).await {
                                    tracing::error!("Unable to turn on garden sprinklers: {e}");
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
