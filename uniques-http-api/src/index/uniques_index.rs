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
    FamilyLookupIndex, FamilyResolveError, FamilySpanGroup, FactionsSummary, IndexManifest,
    NameSearchIndex, SetBitmaps, StatsSummary,
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
    pub name_search_index: NameSearchIndex,
    pub family_lookup_index: FamilyLookupIndex,
    pub family_span_groups: Vec<FamilySpanGroup>,
    /// Pre-serialized `GET /api/v2/effects` JSON body.
    pub effects_body: Arc<Bytes>,
    /// `effects_body` gzipped once at load; `None` serves the plain body.
    pub effects_body_gzip: Option<Arc<Bytes>>,
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

    pub fn name_search_index(&self) -> &NameSearchIndex {
        &self.name_search_index
    }

    pub fn family_lookup_index(&self) -> &FamilyLookupIndex {
        &self.family_lookup_index
    }

    pub fn family_span_groups(&self) -> &[FamilySpanGroup] {
        &self.family_span_groups
    }

    pub fn effects_body(&self) -> &Arc<Bytes> {
        &self.effects_body
    }

    pub fn effects_body_gzip(&self) -> Option<&Arc<Bytes>> {
        self.effects_body_gzip.as_ref()
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
