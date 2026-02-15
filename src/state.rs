use comprehensive::health::{HealthReporter, HealthSignaller};
use comprehensive::v1::{AssemblyRuntime, Resource, TaskWithCleanup, resource};
use humantime::parse_duration;
use protobuf::{Parse, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Notify;
use tracing::{error, info};

use crate::pb::PersistentState;

pub struct State {
    inner: RwLock<(PersistentState, bool)>,
    notify: Notify,
}

#[derive(clap::Args)]
pub struct StateArgs {
    #[arg(long, help = "Pathname to persistent state")]
    persistent_state: PathBuf,
    #[arg(long, help = "Delay before flushing changed persistent state", default_value = "1h", value_parser = parse_duration)]
    state_write_interval: Duration,
}

#[derive(Debug, Error)]
pub enum StateCreateError {
    #[error("{0}")]
    IOError(#[from] std::io::Error),
    #[error("{0}")]
    ParseError(#[from] protobuf::ParseError),
    #[error("{0}")]
    ComprehensiveError(#[from] comprehensive::ComprehensiveError),
}

struct StateFlusher {
    shared: Arc<State>,
    path: PathBuf,
    new_path: PathBuf,
    health_signaller: HealthSignaller,
    state_write_interval: Duration,
}

impl StateFlusher {
    fn maybe_flush(&self) -> tokio::sync::futures::Notified<'_> {
        if let Ok(mut lock) = self.shared.inner.write() {
            if lock.1 {
                match lock.0.serialize() {
                    Ok(b) => match std::fs::write(&self.new_path, b) {
                        Ok(()) => match std::fs::rename(&self.new_path, &self.path) {
                            Ok(()) => {
                                info!("Wrote changed state");
                                lock.1 = false;
                                self.health_signaller.set_healthy(true);
                            }
                            Err(e) => {
                                error!("State install error to {:?}, error {e}", self.path);
                                self.health_signaller.set_healthy(false);
                            }
                        },
                        Err(e) => {
                            error!("State write error to {:?}, error {e}", self.new_path);
                            self.health_signaller.set_healthy(false);
                        }
                    },
                    Err(e) => {
                        error!("Serialisation error {e}");
                        lock.1 = false;
                        self.health_signaller.set_healthy(false);
                    }
                }
            } else {
                info!("No changes; skip write");
            }
            return self.shared.notify.notified();
        }
        return self.shared.notify.notified();
    }
}

#[allow(refining_impl_trait)]
impl TaskWithCleanup for StateFlusher {
    async fn main_task(&mut self) -> Result<(), std::convert::Infallible> {
        let mut n = self.shared.notify.notified();
        loop {
            n.await;
            info!("State may be dirty, will write it");
            tokio::time::sleep(self.state_write_interval).await;
            n = self.maybe_flush();
        }
    }

    async fn cleanup(self) -> Result<(), std::convert::Infallible> {
        let _ = self.maybe_flush();
        Ok(())
    }
}

#[resource]
impl Resource for State {
    fn new(
        (health_reporter,): (Arc<HealthReporter>,),
        a: StateArgs,
        runtime: &mut AssemblyRuntime<'_>,
    ) -> Result<Arc<Self>, StateCreateError> {
        let health_signaller = health_reporter.register(Self::NAME)?;
        let path = a.persistent_state;
        let r = std::fs::read(&path);
        if r.is_err() {
            tracing::warn!("HELP: To bootstrap with empty state, create an empty file {path:?}");
        }
        let serialised = r?;
        let state = PersistentState::parse(&serialised)?;
        let shared = Arc::new(Self {
            inner: RwLock::new((state, false)),
            notify: Notify::new(),
        });
        let mut new_path = path.clone();
        new_path.add_extension("new");
        health_signaller.set_healthy(true);
        runtime.set_task_with_cleanup(StateFlusher {
            shared: Arc::clone(&shared),
            path,
            new_path,
            health_signaller,
            state_write_interval: a.state_write_interval,
        });
        Ok(shared)
    }
}

pub struct ReadGuard<'a>(std::sync::RwLockReadGuard<'a, (PersistentState, bool)>);

impl<'a> ReadGuard<'a> {
    pub fn as_view(&'a self) -> crate::pb::PersistentStateView<'a> {
        self.0.0.as_view()
    }
}

pub struct WriteGuard<'a>(
    std::sync::RwLockWriteGuard<'a, (PersistentState, bool)>,
    &'a State,
);

impl WriteGuard<'_> {
    pub fn as_mut(&mut self) -> crate::pb::PersistentStateMut<'_> {
        self.0.1 = true;
        self.0.0.as_mut()
    }
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        if self.0.1 {
            self.1.notify.notify_waiters();
        }
    }
}

impl State {
    pub fn read(&self) -> ReadGuard<'_> {
        ReadGuard(self.inner.read().unwrap())
    }

    pub fn write(&self) -> WriteGuard<'_> {
        WriteGuard(self.inner.write().unwrap(), self)
    }
}
