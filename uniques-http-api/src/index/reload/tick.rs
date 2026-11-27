use crate::config::Settings;
use crate::http::state::{AppState, QuerySnapshot};

use super::source::IndexSource;

pub(super) async fn reload_tick(
    state: &AppState,
    settings: &Settings,
    source: &(impl IndexSource + Clone + 'static),
) -> anyhow::Result<()> {
    let remote_version = source.read_version()?;
    let current = state.current_built_at_secs();
    if remote_version <= current {
        return Ok(());
    }

    eprintln!(
        "index hot-reload starting: version {current} -> {remote_version} (loading index...)"
    );
    let started = std::time::Instant::now();

    let source = source.clone();
    let formats_settings = settings.formats.clone();
    let artists_settings = settings.artists.clone();
    let collections_settings = settings.collections.clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        let bundle = source.load_bundle()?;
        QuerySnapshot::assemble(
            bundle,
            Some(&artists_settings),
            formats_settings.as_ref(),
            &collections_settings,
        )
    })
    .await??;

    let elapsed = started.elapsed();

    match state.commit_assembled_if_newer(snapshot) {
        Some((old_secs, new_secs)) => {
            eprintln!(
                "index hot-reloaded: version {old_secs} -> {new_secs} (loaded in {:.2}s)",
                elapsed.as_secs_f64()
            );
        }
        None => {
            eprintln!(
                "index hot-reload skipped after load: version still {current} (loaded in {:.2}s, another swap won)",
                elapsed.as_secs_f64()
            );
        }
    }

    Ok(())
}
