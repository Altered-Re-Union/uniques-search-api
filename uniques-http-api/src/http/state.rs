use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use index_core::family_catalog::FamilyCatalogEntry;

use crate::collections::CollectionStore;
use crate::config::{ArtistsSettings, CollectionsSettings, FormatsSettings, Settings};
use crate::formats::{load_format_index, FormatIndex};
use crate::http::api::cards::CardV2;
use crate::http::api::effects::UnifiedEffectIndex;
use crate::http::api::error::{bad_request, map_query_error, not_found, ApiResult};
use crate::index::{
    card_v2_from_index, enrich_unique_card_v2, nonunique_card_v2, subtype_names_from_index,
    CardResolveError, NonUniqueQueryIndex, QueryError, UniquesIndex,
};

#[derive(Debug, Clone)]
pub(crate) struct FamilyPage {
    pub(crate) family_id: i64,
    pub(crate) name: BTreeMap<String, String>,
    pub(crate) card_type: String,
    pub(crate) subtypes: Vec<String>,
    pub(crate) unique_print_total: u32,
    pub(crate) unique_prints: Vec<String>,
    pub(crate) non_unique_prints: Vec<String>,
}

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

    /// Hydrate one unique `CardV2` by bit index, optionally enriching with unified-search metadata
    /// (`rarity`, `cardType`, status flags, `legalFormats`).
    pub(crate) fn hydrate_unique(
        &self,
        card_index: u32,
        enrich: bool,
        debug_bga_trigram: bool,
    ) -> Result<CardV2, QueryError> {
        let index = self.index.as_ref();
        let mut card = card_v2_from_index(index, card_index, debug_bga_trigram)?;
        if enrich {
            enrich_unique_card_v2(
                &mut card,
                card_index,
                index,
                self.nonunique.as_deref(),
                self.formats.as_ref(),
            );
        }
        Ok(card)
    }

    /// Hydrate one non-unique `CardV2` by print index.
    pub(crate) fn hydrate_nonunique(&self, print_index: u32) -> Option<CardV2> {
        let nu = self.nonunique.as_deref()?;
        nonunique_card_v2(
            nu,
            print_index,
            &self.families,
            &self.subtype_names,
            self.formats.as_ref(),
            self.index.id_gd_aliases(),
        )
    }

    /// Resolve a card reference across both non-unique and unique indexes and hydrate its full
    /// enriched `CardV2` representation.
    pub(crate) fn card_by_reference(
        &self,
        reference: &str,
        debug_bga_trigram: bool,
    ) -> ApiResult<CardV2> {
        if let Some(nu) = self.nonunique.as_deref() {
            if let Some(print_index) = nu.resolve_reference(reference) {
                if let Some(card) = self.hydrate_nonunique(print_index) {
                    return Ok(card);
                }
            }
        }

        let index = self.index.as_ref();
        let card_index = index.resolve_card_index(reference).map_err(|e| match e {
            CardResolveError::BadRequest { message } => bad_request(message),
            CardResolveError::NotFound { message } => not_found(message),
        })?;

        let view = index.card_view(card_index).ok_or_else(|| {
            not_found(format!("missing compact record for card_index {card_index}"))
        })?;
        if view.faction_code() == 0 {
            return Err(not_found(format!("card not indexed at {reference}")));
        }

        self.hydrate_unique(card_index, true, debug_bga_trigram)
            .map_err(map_query_error)
    }

    /// Build a paginated view of one family in O(family_spans + limit) time: counts matching
    /// unique prints via `range_cardinality` and formats only the `<= limit` references on the
    /// requested page.
    pub(crate) fn family_page(&self, family_id: i64, page: u32, limit: usize) -> Option<FamilyPage> {
        let catalog_entry = self.families.get(&family_id);
        let default_set = &self.index.catalog().set;
        let present = self.index.present_cards();

        let matching_families: Vec<_> = self
            .index
            .catalog()
            .families
            .iter()
            .filter(|f| f.card_family_id == Some(family_id))
            .collect();

        let mut unique_print_total: u64 = 0;
        let mut remaining_skip = (page.saturating_sub(1) as u64).saturating_mul(limit as u64);
        let mut unique_prints = Vec::with_capacity(limit);

        for f in &matching_families {
            let end = f.start_bit.saturating_add(f.max_unique_id);
            let span_count = present.range_cardinality(f.start_bit..end);
            unique_print_total += span_count;
            if span_count == 0 || unique_prints.len() >= limit {
                continue;
            }
            if remaining_skip >= span_count {
                remaining_skip -= span_count;
                continue;
            }
            let set = f.source_set.as_deref().unwrap_or(default_set);
            let need = limit - unique_prints.len();
            let skip = remaining_skip as usize;
            remaining_skip = 0;
            for bit in present.range(f.start_bit..end).skip(skip).take(need) {
                let uid = bit - f.start_bit + 1;
                unique_prints.push(format!(
                    "ALT_{set}_B_{}_{}_U_{uid}",
                    f.faction, f.family_number
                ));
            }
        }

        let non_unique_prints: Vec<String> = self
            .nonunique
            .as_ref()
            .and_then(|nu| {
                nu.family.get(&family_id.to_string()).map(|bitmap| {
                    bitmap
                        .iter()
                        .filter_map(|idx| nu.reference_for(idx).map(str::to_string))
                        .collect()
                })
            })
            .unwrap_or_default();

        if catalog_entry.is_none() && matching_families.is_empty() && non_unique_prints.is_empty() {
            return None;
        }

        Some(FamilyPage {
            family_id,
            name: catalog_entry.map(|e| e.name.clone()).unwrap_or_default(),
            card_type: catalog_entry
                .map(|e| e.card_type.clone())
                .unwrap_or_default(),
            subtypes: catalog_entry.map(|e| e.subtypes.clone()).unwrap_or_default(),
            unique_print_total: unique_print_total as u32,
            unique_prints,
            non_unique_prints,
        })
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
    use std::sync::Arc;

    use index_core::catalog::{Catalog, FamilyEntry, FamilySet};
    use roaring::RoaringBitmap;

    use crate::collections::CollectionStore;
    use crate::config::CollectionsSettings;
    use crate::formats::FormatIndex;
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
    fn commit_assembled_if_newer_replaces_when_strictly_newer() {
        let state = AppState::new(snapshot_with_index(10));
        let swapped = state.commit_assembled_if_newer(snapshot_with_index(20));
        assert_eq!(swapped, Some((10, 20)));
        assert_eq!(state.current_built_at_secs(), 20);
    }

    #[test]
    fn commit_assembled_if_newer_skips_when_equal_or_older() {
        let state = AppState::new(snapshot_with_index(20));
        assert_eq!(state.commit_assembled_if_newer(snapshot_with_index(20)), None);
        assert_eq!(state.commit_assembled_if_newer(snapshot_with_index(10)), None);
        assert_eq!(state.current_built_at_secs(), 20);
    }

    #[test]
    fn commit_assembled_if_newer_discards_collections() {
        use crate::collections::CollectionBitmaps;

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
            .commit_assembled_if_newer(snapshot_with_index(20))
            .expect("swapped");

        assert!(!state.snapshot().collections.contains("deck"));
    }

    #[test]
    fn swap_formats_if_same_index_rejects_stale_formats_reload() {
        let state = AppState::new(snapshot_with_index(10));

        // Concurrent index hot-reload advances built_at_secs from 10 to 20.
        state
            .commit_assembled_if_newer(snapshot_with_index(20))
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

    #[test]
    fn family_page_paginates_across_multiple_spans_skipping_gaps() {
        let catalog = Catalog {
            set: "ALL_SETS".to_string(),
            faction_order: vec![],
            families: vec![
                FamilyEntry {
                    start_bit: 0,
                    faction: "AX".to_string(),
                    family_number: "01".to_string(),
                    family_id: "AX_01".to_string(),
                    source_set: Some("CORE".to_string()),
                    max_unique_id: 4,
                    card_count: 2,
                    first_reference: "ALT_CORE_B_AX_01_U_1".to_string(),
                    name: BTreeMap::new(),
                    artist: String::new(),
                    card_sub_types: vec![],
                    set: FamilySet {
                        reference: "CORE".to_string(),
                        name: "CORE".to_string(),
                        code: Some("BTG".to_string()),
                    },
                    card_family_id: Some(42),
                    collector_family_number: Some("001".to_string()),
                },
                FamilyEntry {
                    start_bit: 10,
                    faction: "AX".to_string(),
                    family_number: "01".to_string(),
                    family_id: "AX_01".to_string(),
                    source_set: Some("COREKS".to_string()),
                    max_unique_id: 3,
                    card_count: 2,
                    first_reference: "ALT_COREKS_B_AX_01_U_1".to_string(),
                    name: BTreeMap::new(),
                    artist: String::new(),
                    card_sub_types: vec![],
                    set: FamilySet {
                        reference: "COREKS".to_string(),
                        name: "COREKS".to_string(),
                        code: Some("BTG".to_string()),
                    },
                    card_family_id: Some(42),
                    collector_family_number: Some("001".to_string()),
                },
            ],
            total_bit_span: 13,
        };
        let mut index = UniquesIndex::minimal_for_test(catalog, 1);
        // Populated bits: CORE uid 1 (bit 0), CORE uid 4 (bit 3), COREKS uid 2 (bit 11), COREKS uid 3 (bit 12)
        index.present_cards = RoaringBitmap::from_iter([0, 3, 11, 12]);
        let snapshot = QuerySnapshot::from_parts(
            Arc::new(index),
            None,
            Arc::new(BTreeMap::new()),
            Arc::new(FormatIndex::empty()),
            CollectionStore::new(&test_collections_settings()),
        );

        let page1 = snapshot.family_page(42, 1, 2).expect("family exists");
        assert_eq!(page1.unique_print_total, 4);
        assert_eq!(
            page1.unique_prints,
            vec!["ALT_CORE_B_AX_01_U_1", "ALT_CORE_B_AX_01_U_4"]
        );

        let page2 = snapshot.family_page(42, 2, 2).expect("family exists");
        assert_eq!(page2.unique_print_total, 4);
        assert_eq!(
            page2.unique_prints,
            vec!["ALT_COREKS_B_AX_01_U_2", "ALT_COREKS_B_AX_01_U_3"]
        );

        let page3 = snapshot.family_page(42, 3, 2).expect("family exists");
        assert_eq!(page3.unique_print_total, 4);
        assert!(page3.unique_prints.is_empty());

        assert!(snapshot.family_page(999, 1, 10).is_none());
    }
}
