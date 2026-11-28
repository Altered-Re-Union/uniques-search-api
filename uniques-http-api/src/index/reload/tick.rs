use crate::config::Settings;
use crate::http::state::{AppState, QuerySnapshot};

use super::source::IndexSource;

pub(super) async fn reload_tick(
    state: &AppState,
    settings: &Settings,
    source: &(impl IndexSource + Clone + 'static),
) -> anyhow::Result<()> {
    let version_source = source.clone();
    let remote_version = tokio::task::spawn_blocking(move || version_source.read_version()).await??;
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;

    use index_core::catalog::Catalog;

    use crate::http::ServerState;
    use crate::index::loader::LoadedIndexBundle;
    use crate::index::UniquesIndex;

    use super::*;

    #[derive(Clone)]
    struct FakeIndexSource {
        remote_version: Arc<AtomicU64>,
        bundle_version: Arc<AtomicU64>,
        load_calls: Arc<AtomicUsize>,
    }

    impl FakeIndexSource {
        fn new(remote_version: u64, bundle_version: u64) -> Self {
            Self {
                remote_version: Arc::new(AtomicU64::new(remote_version)),
                bundle_version: Arc::new(AtomicU64::new(bundle_version)),
                load_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl IndexSource for FakeIndexSource {
        fn read_version(&self) -> anyhow::Result<u64> {
            Ok(self.remote_version.load(Ordering::SeqCst))
        }

        fn load_bundle(&self) -> anyhow::Result<LoadedIndexBundle> {
            self.load_calls.fetch_add(1, Ordering::SeqCst);
            let built_at_secs = self.bundle_version.load(Ordering::SeqCst);
            Ok(LoadedIndexBundle {
                index: UniquesIndex::minimal_for_test(
                    Catalog {
                        set: "TEST".to_string(),
                        faction_order: vec![],
                        families: vec![],
                        total_bit_span: 0,
                    },
                    built_at_secs,
                ),
                nonunique: None,
                families: BTreeMap::new(),
                artist_credits: None,
            })
        }
    }

    fn test_server(initial_version: u64) -> ServerState {
        let index = UniquesIndex::minimal_for_test(
            Catalog {
                set: "TEST".to_string(),
                faction_order: vec![],
                families: vec![],
                total_bit_span: 0,
            },
            initial_version,
        );
        ServerState::for_test(AppState::new_with_index(index))
    }

    #[tokio::test]
    async fn reload_tick_skips_load_when_version_unchanged() {
        let server = test_server(10);
        let source = FakeIndexSource::new(10, 10);
        reload_tick(&server.app, &server.settings, &source)
            .await
            .unwrap();
        assert_eq!(source.load_calls.load(Ordering::SeqCst), 0);
        assert_eq!(server.app.current_built_at_secs(), 10);
    }

    #[tokio::test]
    async fn reload_tick_swaps_when_remote_version_is_newer() {
        let server = test_server(10);
        let source = FakeIndexSource::new(20, 20);
        reload_tick(&server.app, &server.settings, &source)
            .await
            .unwrap();
        assert_eq!(source.load_calls.load(Ordering::SeqCst), 1);
        assert_eq!(server.app.current_built_at_secs(), 20);
    }

    #[tokio::test]
    async fn reload_tick_ignores_stale_bundle_when_loaded_version_is_not_newer() {
        let server = test_server(10);
        // Remote manifest claims v20, but the loaded bundle turns out to be v10.
        let source = FakeIndexSource::new(20, 10);
        reload_tick(&server.app, &server.settings, &source)
            .await
            .unwrap();
        assert_eq!(source.load_calls.load(Ordering::SeqCst), 1);
        assert_eq!(server.app.current_built_at_secs(), 10);
    }
}
