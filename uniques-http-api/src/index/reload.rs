mod disk;
mod remote;
mod source;
mod tick;

use std::sync::Arc;
use std::time::Duration;

use crate::config::Settings;
use crate::http::state::AppState;
use tokio::time::MissedTickBehavior;

pub use disk::DiskIndexSource;
pub use remote::RemoteIndexSource;
pub use source::IndexSource;

#[derive(Clone)]
pub enum AnyIndexSource {
    Disk(DiskIndexSource),
    Remote(RemoteIndexSource),
}

impl IndexSource for AnyIndexSource {
    fn read_version(&self) -> anyhow::Result<u64> {
        match self {
            Self::Disk(source) => source.read_version(),
            Self::Remote(source) => source.read_version(),
        }
    }

    fn load_bundle(&self) -> anyhow::Result<crate::index::loader::LoadedIndexBundle> {
        match self {
            Self::Disk(source) => source.load_bundle(),
            Self::Remote(source) => source.load_bundle(),
        }
    }
}

pub fn spawn_hot_reload(
    state: Arc<AppState>,
    settings: Arc<Settings>,
    source: impl IndexSource + 'static,
    interval_secs: u64,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        interval.tick().await;

        loop {
            interval.tick().await;
            if let Err(e) = tick::reload_tick(&state, &settings, &source).await {
                eprintln!("index hot-reload tick failed: {e:#}");
            }
        }
    });
}
