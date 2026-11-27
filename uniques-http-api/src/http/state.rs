use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use index_core::family_catalog::FamilyCatalogEntry;

use crate::collections::CollectionStore;
use crate::config::{ArtistsSettings, CollectionsSettings, FormatsSettings, Settings};
use crate::formats::{load_format_index, FormatIndex};
use crate::http::api::effects::UnifiedEffectIndex;
use crate::index::{subtype_names_from_index, NonUniqueQueryIndex, UniquesIndex};

#[derive(Clone)]
pub struct ServerState {
    pub app: Arc<AppState>,
    pub settings: Arc<Settings>,
}

#[derive(Clone)]
pub struct QuerySnapshot {
    pub index: Arc<UniquesIndex>,
    /// `None` when this index root has no `nonunique/` files yet (see Lot 2 / plan 19) — every
    /// index built before that lot still loads fine, just without `/api/v2/search` results.
    pub nonunique: Option<Arc<NonUniqueQueryIndex>>,
    /// Shared family name/type/subtypes, by `CardFamilyId` — empty when `families.json` is absent
    /// (see Lot 5 / plan 23).
    pub families: Arc<BTreeMap<i64, FamilyCatalogEntry>>,
    pub formats: Arc<FormatIndex>,
    pub collections: CollectionStore,
    /// Deep unified effect index (canonical `idGd` types, pre-canonicalized non-unique per-line
    /// bitmaps, pre-serialized `/api/v2/effects` body, and reachability).
    pub effects: Arc<UnifiedEffectIndex>,
    /// Precomputed `CardSubTypeReference` -> localized names map, built once per snapshot.
    pub subtype_names: Arc<BTreeMap<String, BTreeMap<String, String>>>,
}

impl QuerySnapshot {
    /// Single assembly seam for turning a loaded index bundle into a fully bound `QuerySnapshot`:
    /// binds artist credits, families, localized effect-text keywords, formats, unified effects,
    /// and derived lookup caches so unbound snapshots are unrepresentable.
    pub fn assemble(
        mut bundle: crate::index::loader::LoadedIndexBundle,
        artists_settings: Option<&ArtistsSettings>,
        formats_settings: Option<&FormatsSettings>,
        collections_settings: &CollectionsSettings,
    ) -> Result<Self> {
        match artists_settings {
            Some(settings) => bundle.bind_artist_credits(settings)?,
            None => {
                let credits = bundle
                    .artist_credits
                    .as_ref()
                    .map(index_core::artist_credits::ArtistCredits::from_file)
                    .unwrap_or_default();
                crate::index::loader::bind_artist_credits(
                    &mut bundle.index,
                    bundle.nonunique.as_mut(),
                    credits,
                );
            }
        }
        let crate::index::loader::LoadedIndexBundle {
            mut index,
            mut nonunique,
            families,
            ..
        } = bundle;
        if let Some(ref mut nu) = nonunique {
            nu.bind_families(&families);
            if !nu.keywords_i18n.is_empty() {
                index.bind_effect_text_keywords(&nu.keywords_i18n);
            }
        }
        let formats = match formats_settings.filter(|f| f.is_enabled()) {
            Some(settings) => Arc::new(load_format_index(&index, nonunique.as_ref(), settings)),
            None => Arc::new(FormatIndex::empty()),
        };
        Ok(Self::from_parts(
            Arc::new(index),
            nonunique.map(Arc::new),
            Arc::new(families),
            formats,
            CollectionStore::new(collections_settings),
        ))
    }

    /// Assemble a `QuerySnapshot` from already-bound parts, always deriving `effects` and
    /// `subtype_names` in one place.
    pub(crate) fn from_parts(
        index: Arc<UniquesIndex>,
        nonunique: Option<Arc<NonUniqueQueryIndex>>,
        families: Arc<BTreeMap<i64, FamilyCatalogEntry>>,
        formats: Arc<FormatIndex>,
        collections: CollectionStore,
    ) -> Self {
        let effects = Arc::new(UnifiedEffectIndex::build(index.as_ref(), nonunique.as_deref()));
        let subtype_names = Arc::new(subtype_names_from_index(index.as_ref()));
        Self {
            index,
            nonunique,
            families,
            formats,
            collections,
            effects,
            subtype_names,
        }
    }
}

/// Axum shared state; holds the loaded index, format filters, and collections.
pub struct AppState {
    query: RwLock<Arc<QuerySnapshot>>,
}

impl AppState {
    pub(crate) fn new(snapshot: QuerySnapshot) -> Self {
        Self {
            query: RwLock::new(Arc::new(snapshot)),
        }
    }

    /// Build state with an index and empty format catalog (tests / formats disabled).
    pub(crate) fn new_with_index(index: UniquesIndex) -> Self {
        Self::new_with_index_and_settings(index, &CollectionsSettings::default())
    }

    pub(crate) fn new_with_index_and_settings(
        index: UniquesIndex,
        collections_settings: &CollectionsSettings,
    ) -> Self {
        Self::new(QuerySnapshot::from_parts(
            Arc::new(index),
            None,
            Arc::new(BTreeMap::new()),
            Arc::new(FormatIndex::empty()),
            CollectionStore::new(collections_settings),
        ))
    }

    pub fn snapshot(&self) -> Arc<QuerySnapshot> {
        self.query
            .read()
            .expect("query lock poisoned")
            .clone()
    }

    pub fn index(&self) -> Arc<UniquesIndex> {
        Arc::clone(&self.snapshot().index)
    }

    pub fn formats(&self) -> Arc<FormatIndex> {
        Arc::clone(&self.snapshot().formats)
    }

    pub fn current_built_at_secs(&self) -> u64 {
        self.snapshot().index.manifest.built_at_secs
    }

    pub(crate) fn commit(&self, snapshot: Arc<QuerySnapshot>) {
        *self.query.write().expect("query lock poisoned") = snapshot;
    }

    /// Atomically commit an assembled snapshot under the write lock iff its `built_at_secs` is
    /// strictly newer than the currently active snapshot.
    pub(crate) fn commit_assembled_if_newer(&self, snapshot: QuerySnapshot) -> Option<(u64, u64)> {
        let mut guard = self.query.write().expect("query lock poisoned");
        let old_secs = guard.index.manifest.built_at_secs;
        let new_secs = snapshot.index.manifest.built_at_secs;
        if new_secs > old_secs {
            *guard = Arc::new(snapshot);
            Some((old_secs, new_secs))
        } else {
            None
        }
    }

    /// Atomically swap `formats` under the write lock iff the active index still matches
    /// `expected_built_at_secs`, preventing a formats hot-reload from clobbering a concurrent
    /// index hot-reload.
    pub(crate) fn swap_formats_if_same_index(
        &self,
        expected_built_at_secs: u64,
        new_formats: Arc<FormatIndex>,
    ) -> bool {
        let mut guard = self.query.write().expect("query lock poisoned");
        if guard.index.manifest.built_at_secs != expected_built_at_secs {
            return false;
        }
        let mut next = (**guard).clone();
        next.formats = new_formats;
        *guard = Arc::new(next);
        true
    }

    /// Swap index+formats (keeping existing nonunique/families) when newer.
    pub(crate) fn commit_if_newer(
        &self,
        new_index: Arc<UniquesIndex>,
        new_formats: Arc<FormatIndex>,
        collections_settings: &CollectionsSettings,
    ) -> Option<(u64, u64)> {
        let current = self.snapshot();
        self.commit_snapshot_if_newer(
            new_index,
            current.nonunique.clone(),
            current.families.clone(),
            new_formats,
            collections_settings,
        )
    }

    /// Swap a full loaded index bundle (unique + non-unique + family catalog + formats) when
    /// `bundle.index.manifest.built_at_secs` is strictly higher than the active snapshot.
    #[allow(dead_code)]
    pub(crate) fn commit_bundle_if_newer(
        &self,
        bundle: crate::index::loader::LoadedIndexBundle,
        new_formats: Arc<FormatIndex>,
        collections_settings: &CollectionsSettings,
    ) -> Option<(u64, u64)> {
        let mut snapshot =
            QuerySnapshot::assemble(bundle, None, None, collections_settings).ok()?;
        snapshot.formats = new_formats;
        self.commit_assembled_if_newer(snapshot)
    }

    pub(crate) fn commit_snapshot_if_newer(
        &self,
        new_index: Arc<UniquesIndex>,
        nonunique: Option<Arc<NonUniqueQueryIndex>>,
        families: Arc<BTreeMap<i64, FamilyCatalogEntry>>,
        new_formats: Arc<FormatIndex>,
        collections_settings: &CollectionsSettings,
    ) -> Option<(u64, u64)> {
        let snapshot = QuerySnapshot::from_parts(
            new_index,
            nonunique,
            families,
            new_formats,
            CollectionStore::new(collections_settings),
        );
        self.commit_assembled_if_newer(snapshot)
    }
}

impl ServerState {
    /// Wrap loaded app state with minimal settings for integration tests (formats disabled).
    pub fn for_test(app: AppState) -> Self {
        use crate::config::{
            ArtistsSettings, CardsSettings, CollectionsSettings, HttpIndexSettings, IndexSettings, IndexSourceKind,
            ObjectStoreSettings, ReloadSettings, ServerSettings, Settings,
        };

        Self {
            app: Arc::new(app),
            settings: Arc::new(Settings {
                server: ServerSettings { port: 8080 },
                index: IndexSettings {
                    source: IndexSourceKind::Disk,
                    path: None,
                    reload: ReloadSettings {
                        enabled: false,
                        interval_secs: None,
                    },
                    object_store: ObjectStoreSettings::default(),
                    http: HttpIndexSettings::default(),
                },
                formats: None,
                collections: CollectionsSettings {
                    max_memory_bytes: 1024 * 1024,
                    ..CollectionsSettings::default()
                },
                cards: CardsSettings::default(),
                artists: ArtistsSettings::default(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use axum::body::Bytes;

    use index_core::catalog::Catalog;
    use index_core::idgd_catalog::IdGdCatalog;
    use index_core::idgd_collapse::IdGdAliasMap;

    use crate::collections::CollectionStore;
    use crate::config::CollectionsSettings;
    use crate::formats::FormatIndex;
    use crate::index::loader::{
        build_collector_number_lookup_index, build_family_lookup_index, build_name_search_index,
        FactionsSummary, IndexManifest, StatsSummary,
    };
    use crate::index::UniquesIndex;

    use super::{AppState, QuerySnapshot};

    fn test_collections_settings() -> CollectionsSettings {
        CollectionsSettings {
            max_memory_bytes: 1024 * 1024,
            ..CollectionsSettings::default()
        }
    }

    fn minimal_index(built_at_secs: u64) -> UniquesIndex {
        UniquesIndex::minimal_for_test(
            Catalog {
                set: "TEST".to_string(),
                faction_order: vec![],
                families: vec![],
                total_bit_span: 0,
            },
            built_at_secs,
        )
    }

    fn snapshot_with_index(built_at_secs: u64) -> QuerySnapshot {
        QuerySnapshot::from_parts(
            Arc::new(minimal_index(built_at_secs)),
            None,
            Arc::new(BTreeMap::new()),
            Arc::new(FormatIndex::empty()),
            CollectionStore::new(&test_collections_settings()),
        )
    }

    #[test]
    fn commit_if_newer_replaces_when_strictly_newer() {
        let settings = test_collections_settings();
        let state = AppState::new(snapshot_with_index(10));
        let swapped = state.commit_if_newer(
            Arc::new(minimal_index(20)),
            Arc::new(FormatIndex::empty()),
            &settings,
        );
        assert_eq!(swapped, Some((10, 20)));
        assert_eq!(state.current_built_at_secs(), 20);
    }

    #[test]
    fn commit_if_newer_skips_when_equal_or_older() {
        let settings = test_collections_settings();
        let state = AppState::new(snapshot_with_index(20));
        assert_eq!(
            state.commit_if_newer(
                Arc::new(minimal_index(20)),
                Arc::new(FormatIndex::empty()),
                &settings,
            ),
            None
        );
        assert_eq!(
            state.commit_if_newer(
                Arc::new(minimal_index(10)),
                Arc::new(FormatIndex::empty()),
                &settings,
            ),
            None
        );
        assert_eq!(state.current_built_at_secs(), 20);
    }

    #[test]
    fn commit_if_newer_discards_collections() {
        use roaring::RoaringBitmap;

        use crate::collections::CollectionBitmaps;

        let settings = test_collections_settings();
        let state = AppState::new(snapshot_with_index(10));
        state.snapshot().collections.insert(
            "deck",
            Arc::new(CollectionBitmaps {
                unique: RoaringBitmap::from_iter([1]),
                nonunique: RoaringBitmap::new(),
            }),
        );
        assert!(state.snapshot().collections.contains("deck"));

        state
            .commit_if_newer(
                Arc::new(minimal_index(20)),
                Arc::new(FormatIndex::empty()),
                &settings,
            )
            .expect("swapped");

        assert!(!state.snapshot().collections.contains("deck"));
    }

    #[test]
    fn swap_formats_if_same_index_rejects_stale_formats_reload() {
        let settings = test_collections_settings();
        let state = AppState::new(snapshot_with_index(10));

        // Concurrent index hot-reload advances built_at_secs from 10 to 20.
        state
            .commit_if_newer(
                Arc::new(minimal_index(20)),
                Arc::new(FormatIndex::empty()),
                &settings,
            )
            .expect("swapped to 20");

        let mut stale_formats = FormatIndex::empty();
        stale_formats
            .manifest_versions
            .insert("frontier".to_string(), 99);
        assert!(
            !state.swap_formats_if_same_index(10, Arc::new(stale_formats)),
            "formats built against index v10 must not clobber index v20"
        );
        assert_eq!(state.current_built_at_secs(), 20);
        assert!(state.formats().manifest_versions.is_empty());

        let mut fresh_formats = FormatIndex::empty();
        fresh_formats
            .manifest_versions
            .insert("frontier".to_string(), 100);
        assert!(state.swap_formats_if_same_index(20, Arc::new(fresh_formats)));
        assert_eq!(state.formats().manifest_versions.get("frontier"), Some(&100));
    }
}
