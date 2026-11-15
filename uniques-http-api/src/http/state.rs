use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use index_core::family_catalog::FamilyCatalogEntry;

use crate::collections::CollectionStore;
use crate::config::{CollectionsSettings, Settings};
use crate::formats::FormatIndex;
use crate::index::{NonUniqueQueryIndex, UniquesIndex};

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
        Self::new(QuerySnapshot {
            index: Arc::new(index),
            nonunique: None,
            families: Arc::new(BTreeMap::new()),
            formats: Arc::new(FormatIndex::empty()),
            collections: CollectionStore::new(collections_settings),
        })
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

    /// Swap index+formats when the new index has a strictly higher `built_at_secs`.
    /// Collections are discarded as incompatible with the new index layout.
    pub(crate) fn commit_if_newer(
        &self,
        new_index: Arc<UniquesIndex>,
        new_formats: Arc<FormatIndex>,
        collections_settings: &CollectionsSettings,
    ) -> Option<(u64, u64)> {
        let current = self.snapshot();
        let old_secs = current.index.manifest.built_at_secs;
        let new_secs = new_index.manifest.built_at_secs;
        if new_secs > old_secs {
            self.commit(Arc::new(QuerySnapshot {
                index: new_index,
                // Non-unique hot-reload isn't wired yet (Lot 2 first slice); carry the current
                // value forward unchanged rather than dropping it on every unique-index reload.
                nonunique: current.nonunique.clone(),
                families: current.families.clone(),
                formats: new_formats,
                collections: CollectionStore::new(collections_settings),
            }));
            Some((old_secs, new_secs))
        } else {
            None
        }
    }
}

impl ServerState {
    /// Wrap loaded app state with minimal settings for integration tests (formats disabled).
    pub fn for_test(app: AppState) -> Self {
        use crate::config::{
            CardsSettings, CollectionsSettings, HttpIndexSettings, IndexSettings, IndexSourceKind,
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
        let catalog = Catalog {
            set: "TEST".to_string(),
            faction_order: vec![],
            families: vec![],
            total_bit_span: 0,
        };
        UniquesIndex {
            index_dir: PathBuf::from("/test"),
            catalog: catalog.clone(),
            manifest: IndexManifest {
                version: 1,
                set: "TEST".to_string(),
                kind: None,
                built_at_secs,
                card_count: 0,
                id_gd_count: 0,
                total_bit_span: 0,
                family_count: 0,
            },
            idgd_catalog: IdGdCatalog {
                set: "TEST".to_string(),
                entries: vec![],
            },
            id_gd_aliases: IdGdAliasMap::default(),
            stats_summary: StatsSummary {
                version: 1,
                set: "TEST".to_string(),
                total_cards_indexed: 0,
                fields: vec![],
            },
            factions_summary: FactionsSummary {
                version: 1,
                set: "TEST".to_string(),
                total_cards_indexed: 0,
                source: String::new(),
                factions: vec![],
                unknown_count: 0,
                bitmap_dir: String::new(),
            },
            cards: vec![],
            id_gd_whole: Default::default(),
            id_gd_per_line: Default::default(),
            stats: Default::default(),
            factions: Default::default(),
            set_bitmaps: crate::index::loader::SetBitmaps {
                by_set: Default::default(),
                core_and_coreks: None,
            },
            cost_relations: Default::default(),
            subtype_bitmaps: Default::default(),
            no_effect_bitmap: Default::default(),
            banned: Default::default(),
            errated: Default::default(),
            suspended: Default::default(),
            name_search_index: build_name_search_index(&catalog),
            family_lookup_index: build_family_lookup_index(&catalog),
            family_span_groups: vec![],
            family_sort_index: Default::default(),
            collector_number_lookup_index: build_collector_number_lookup_index(&catalog),
            effects_body: Arc::new(Bytes::from_static(b"[]")),
        }
    }

    fn snapshot_with_index(built_at_secs: u64) -> QuerySnapshot {
        QuerySnapshot {
            index: Arc::new(minimal_index(built_at_secs)),
            nonunique: None,
            families: Arc::new(BTreeMap::new()),
            formats: Arc::new(FormatIndex::empty()),
            collections: CollectionStore::new(&test_collections_settings()),
        }
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
}
