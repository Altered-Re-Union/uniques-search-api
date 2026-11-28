use std::sync::Arc;
use std::time::Duration;

use crate::config::Settings;
use crate::formats::loader::{load_format_index, read_manifest_versions};
use crate::formats::source::FormatsSource;
use crate::http::state::AppState;
use tokio::time::MissedTickBehavior;

pub fn spawn_formats_hot_reload(
    state: Arc<AppState>,
    settings: Arc<Settings>,
    interval_secs: u64,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        interval.tick().await;

        loop {
            interval.tick().await;
            if let Err(e) = formats_reload_tick(&state, &settings).await {
                eprintln!("formats hot-reload tick failed: {e:#}");
            }
        }
    });
}

async fn formats_reload_tick(state: &Arc<AppState>, settings: &Settings) -> anyhow::Result<()> {
    let Some(formats_settings) = settings.formats.as_ref().filter(|f| f.is_enabled()) else {
        return Ok(());
    };

    let source = FormatsSource::from_config(&formats_settings.source);
    let Some(new_versions) =
        tokio::task::spawn_blocking(move || read_manifest_versions(&source)).await?
    else {
        return Ok(());
    };

    let current = state.snapshot();
    if current.formats.manifest_versions == new_versions {
        return Ok(());
    }

    eprintln!("formats hot-reload starting: manifest version snapshot changed");
    let settings = formats_settings.clone();
    let index = Arc::clone(&current.index);
    let nonunique = current.nonunique.clone();
    let state = Arc::clone(state);

    let expected_built_at_secs = current.index.manifest.built_at_secs;
    let new_formats = tokio::task::spawn_blocking(move || {
        load_format_index(index.as_ref(), nonunique.as_deref(), &settings)
    })
    .await?;

    if state.swap_formats_if_same_index(expected_built_at_secs, Arc::new(new_formats)) {
        eprintln!("formats hot-reloaded");
    } else {
        eprintln!("formats hot-reload skipped: index reloaded concurrently");
    }
    Ok(())
}
