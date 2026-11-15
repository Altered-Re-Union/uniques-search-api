use std::sync::Arc;
use std::time::Duration;

use crate::config::{FormatsSettings, Settings};
use crate::formats::loader::{load_format_index, read_manifest_versions, FormatIndex};
use crate::formats::source::FormatsSource;
use crate::http::state::{AppState, QuerySnapshot};
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
    let state = Arc::clone(state);

    let new_formats = tokio::task::spawn_blocking(move || {
        load_format_index(index.as_ref(), &settings)
    })
    .await?;

    let snapshot = QuerySnapshot {
        index: Arc::clone(&current.index),
        nonunique: current.nonunique.clone(),
        families: current.families.clone(),
        formats: Arc::new(new_formats),
        collections: current.collections.clone(),
    };
    state.commit(Arc::new(snapshot));
    eprintln!("formats hot-reloaded");
    Ok(())
}

pub fn rebuild_formats_for_index(
    index: Arc<crate::index::UniquesIndex>,
    formats_settings: Option<&FormatsSettings>,
) -> Arc<FormatIndex> {
    match formats_settings.filter(|f| f.is_enabled()) {
        Some(settings) => Arc::new(load_format_index(index.as_ref(), settings)),
        None => Arc::new(FormatIndex::empty()),
    }
}
