use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use index_core::bitmap::EffectLine;
use index_core::catalog::Catalog;
use index_core::compact::{CompactCardView, RECORD_SIZE};
use index_core::faction_index::Faction;
use index_core::idgd_catalog::IdGdCatalog;
use index_core::idgd_collapse::IdGdAliasMap;
use index_core::path::parse_card_reference;
use index_core::stat_index::StatField;
use roaring::RoaringBitmap;

use super::loader::{
    CollectorNumberLookupIndex, FamilyLookupIndex, FamilyResolveError, FamilySortIndex,
    FamilySpanGroup, FactionsSummary, IndexManifest, NameSearchIndex, SetBitmaps, StatsSummary,
};

/// In-memory representation of a loaded index directory.
pub struct UniquesIndex {
    pub index_dir: PathBuf,
    pub catalog: Catalog,
    pub manifest: IndexManifest,
    pub idgd_catalog: IdGdCatalog,
    pub id_gd_aliases: IdGdAliasMap,
    pub stats_summary: StatsSummary,
    pub factions_summary: FactionsSummary,
    pub cards: Vec<u8>,
    /// Whole-card idGd bitmaps: `id_gd/{id}.roar`
    pub id_gd_whole: BTreeMap<u32, RoaringBitmap>,
    /// Per-line idGd bitmaps: `id_gd/{id}_m1.roar`, etc.
    pub id_gd_per_line: BTreeMap<(u32, EffectLine), RoaringBitmap>,
    /// Stat bucket bitmaps keyed by field and value 0..15.
    pub stats: BTreeMap<StatField, [RoaringBitmap; 16]>,
    pub factions: BTreeMap<Faction, RoaringBitmap>,
    pub set_bitmaps: SetBitmaps,
    pub cost_relations: crate::index::loader::CostRelationBitmaps,
    /// Derived at load time from `catalog.json`'s per-family `card_sub_types` (no separate
    /// bitmap files on disk) — a print can belong to more than one subtype.
    pub subtype_bitmaps: BTreeMap<String, RoaringBitmap>,
    pub no_effect_bitmap: RoaringBitmap,
    /// `status/{banned,errated,suspended}.roar` — absent (empty) for an index built before this
    /// was added, or for a set with none of a given flag.
    pub banned: RoaringBitmap,
    pub errated: RoaringBitmap,
    pub suspended: RoaringBitmap,
    pub name_search_index: NameSearchIndex,
    pub family_lookup_index: FamilyLookupIndex,
    pub family_span_groups: Vec<FamilySpanGroup>,
    pub family_sort_index: FamilySortIndex,
    /// Uniques only — see `cli-indexer/plans/24-collector-number-ingestion.md`.
    pub collector_number_lookup_index: CollectorNumberLookupIndex,
    /// Card indices that have a populated record in `cards.bin`, excluding unindexed/gap slots
    /// inside family spans.
    pub present_cards: RoaringBitmap,
    pub effects_body: Arc<Bytes>,
}

#[derive(Debug)]
pub enum CardResolveError {
    BadRequest { message: String },
    NotFound { message: String },
}

impl UniquesIndex {
    pub fn expected_cards_len(total_bit_span: u32) -> u64 {
        total_bit_span as u64 * RECORD_SIZE as u64
    }

    pub fn index_dir(&self) -> &PathBuf {
        &self.index_dir
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn manifest(&self) -> &IndexManifest {
        &self.manifest
    }

    pub fn idgd_catalog(&self) -> &IdGdCatalog {
        &self.idgd_catalog
    }

    pub fn id_gd_aliases(&self) -> &IdGdAliasMap {
        &self.id_gd_aliases
    }

    pub fn resolve_id_gd(&self, id: u32) -> u32 {
        self.id_gd_aliases.resolve(id)
    }

    pub fn stats_summary(&self) -> &StatsSummary {
        &self.stats_summary
    }

    pub fn factions_summary(&self) -> &FactionsSummary {
        &self.factions_summary
    }

    pub fn cards(&self) -> &[u8] {
        &self.cards
    }

    pub fn present_cards(&self) -> &RoaringBitmap {
        &self.present_cards
    }

    pub fn id_gd_whole(&self) -> &BTreeMap<u32, RoaringBitmap> {
        &self.id_gd_whole
    }

    pub fn id_gd_per_line(&self) -> &BTreeMap<(u32, EffectLine), RoaringBitmap> {
        &self.id_gd_per_line
    }

    pub fn stats(&self) -> &BTreeMap<StatField, [RoaringBitmap; 16]> {
        &self.stats
    }

    pub fn factions(&self) -> &BTreeMap<Faction, RoaringBitmap> {
        &self.factions
    }

    pub fn set_bitmaps(&self) -> &SetBitmaps {
        &self.set_bitmaps
    }

    pub fn cost_relations(&self) -> &crate::index::loader::CostRelationBitmaps {
        &self.cost_relations
    }

    pub fn subtype_bitmaps(&self) -> &BTreeMap<String, RoaringBitmap> {
        &self.subtype_bitmaps
    }

    pub fn no_effect_bitmap(&self) -> &RoaringBitmap {
        &self.no_effect_bitmap
    }

    pub fn banned(&self) -> &RoaringBitmap {
        &self.banned
    }

    pub fn errated(&self) -> &RoaringBitmap {
        &self.errated
    }

    pub fn suspended(&self) -> &RoaringBitmap {
        &self.suspended
    }

    pub fn name_search_index(&self) -> &NameSearchIndex {
        &self.name_search_index
    }

    pub fn family_lookup_index(&self) -> &FamilyLookupIndex {
        &self.family_lookup_index
    }

    pub fn family_span_groups(&self) -> &[FamilySpanGroup] {
        &self.family_span_groups
    }

    pub fn family_sort_index(&self) -> &FamilySortIndex {
        &self.family_sort_index
    }

    pub fn collector_number_lookup_index(&self) -> &CollectorNumberLookupIndex {
        &self.collector_number_lookup_index
    }

    pub fn effects_body(&self) -> &Arc<Bytes> {
        &self.effects_body
    }

    pub fn card_view(&self, card_index: u32) -> Option<CompactCardView<'_>> {
        CompactCardView::from_data(&self.cards, card_index)
    }

    pub fn decode_reference(&self, card_index: u32) -> anyhow::Result<String> {
        Ok(self.catalog.decode_bit(card_index)?.reference)
    }

    pub fn resolve_card_index(&self, reference: &str) -> Result<u32, CardResolveError> {
        let parsed = parse_card_reference(reference).map_err(|e| CardResolveError::BadRequest {
            message: e.to_string(),
        })?;
        self.family_lookup_index
            .resolve(&parsed)
            .map_err(|e| match e {
                FamilyResolveError::NotFound => CardResolveError::NotFound {
                    message: format!("reference not found: {}", parsed.reference()),
                },
                FamilyResolveError::Padding => CardResolveError::NotFound {
                    message: format!(
                        "reference {} falls in padding (max UniqueID {})",
                        parsed.reference(),
                        self.family_lookup_index
                            .max_unique_id(&parsed)
                            .unwrap_or(0)
                    ),
                },
            })
    }

    /// Resolves an exact `collectorNumberFormatted` string (`SET-FAMILY-U-UID`, e.g.
    /// `BTG-011-U-5`, case-insensitive) to a populated `card_index`. Uniques only — see
    /// `cli-indexer/plans/24-collector-number-ingestion.md`.
    pub fn resolve_card_index_by_collector_number(&self, s: &str) -> Result<u32, CardResolveError> {
        let norm = s.trim().to_ascii_uppercase();
        let parts: Vec<&str> = norm.split('-').collect();
        let [set_code, family_collector_number, rarity, unique_id_str] = parts.as_slice() else {
            return Err(CardResolveError::BadRequest {
                message: format!("invalid collector number '{s}': expected SET-FAMILY-U-UID"),
            });
        };
        if *rarity != "U" {
            return Err(CardResolveError::BadRequest {
                message: format!(
                    "invalid collector number '{s}': expected 'U' rarity marker (uniques only)"
                ),
            });
        }
        let unique_id: u32 = unique_id_str.parse().map_err(|_| CardResolveError::BadRequest {
            message: format!("invalid collector number '{s}': non-numeric unique id"),
        })?;
        if unique_id == 0 {
            return Err(CardResolveError::NotFound {
                message: format!("collector number {s} falls in padding"),
            });
        }

        let candidates = self
            .collector_number_lookup_index
            .resolve_all(set_code, family_collector_number, unique_id)
            .map_err(|e| match e {
                FamilyResolveError::NotFound => CardResolveError::NotFound {
                    message: format!("collector number not found: {s}"),
                },
                FamilyResolveError::Padding => CardResolveError::NotFound {
                    message: format!("collector number {s} falls in padding"),
                },
            })?;

        candidates
            .into_iter()
            .find(|&idx| self.present_cards.contains(idx))
            .ok_or_else(|| CardResolveError::NotFound {
                message: format!("collector number {s} falls in padding"),
            })
    }

    /// Resolves a full collector number (`SET-FAMILY-U-UID`) or a 2-part family collector prefix
    /// (`SET-FAMILY`, case-insensitive) to all populated matching `card_index` values across
    /// spans sharing the same `(set.code, collector_family_number)` (e.g. `CORE` + `COREKS`).
    pub fn resolve_card_indices_by_collector_number(&self, s: &str) -> Vec<u32> {
        let norm = s.trim().to_ascii_uppercase();
        let parts: Vec<&str> = norm.split('-').collect();
        match parts.as_slice() {
            [set_code, family_collector_number, "U", unique_id_str] => {
                let Ok(unique_id) = unique_id_str.parse::<u32>() else {
                    return Vec::new();
                };
                if unique_id == 0 {
                    return Vec::new();
                }
                self.collector_number_lookup_index
                    .resolve_all(set_code, family_collector_number, unique_id)
                    .map(|idxs| {
                        idxs.into_iter()
                            .filter(|&idx| self.present_cards.contains(idx))
                            .collect()
                    })
                    .unwrap_or_default()
            }
            [set_code, family_collector_number] => {
                let Some(spans) = self
                    .collector_number_lookup_index
                    .spans(set_code, family_collector_number)
                else {
                    return Vec::new();
                };
                let mut out = Vec::new();
                for span in spans {
                    for idx in span.start_bit..span.start_bit + span.max_unique_id {
                        if self.present_cards.contains(idx) {
                            out.push(idx);
                        }
                    }
                }
                out
            }
            _ => Vec::new(),
        }
    }
}

impl Drop for UniquesIndex {
    fn drop(&mut self) {
        eprintln!(
            "UniquesIndex deallocated (built_at_secs={}, dir={})",
            self.manifest.built_at_secs,
            self.index_dir.display(),
        );
    }
}
