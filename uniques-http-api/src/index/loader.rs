use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use index_core::bitmap::EffectLine;
use index_core::idgd_collapse::IdGdAliasMap;
use index_core::catalog::Catalog;
use index_core::compact::RECORD_SIZE;
use index_core::faction_index::Faction;
use index_core::idgd_catalog::IdGdCatalog;
use index_core::path::ParsedCardPath;
use index_core::stat_index::StatField;
use roaring::RoaringBitmap;
use serde::Deserialize;
use unicode_normalization::UnicodeNormalization;

use crate::collections::CollectionStore;
use crate::config::Settings;
use crate::formats::{load_format_index, FormatIndex};
use crate::http::api::effects::{build_effects_list, serialize_effects_list};
use crate::http::state::{AppState, QuerySnapshot};
use crate::index::UniquesIndex;

pub mod archive;
pub mod disk;
pub mod http;
pub mod nonunique;
pub mod object_store;
pub mod storage;

pub use archive::TarZstIndexStorage;
pub use disk::DiskIndexStorage;
pub use http::{load_app_state_from_http, load_index_from_http, HttpIndexClient};
pub use nonunique::{load_nonunique_index, NonUniqueQueryIndex};
pub use object_store::{load_app_state_from_object_store, load_index_from_object_store, ObjectStoreIndexClient};
pub use storage::IndexStorage;

use storage::{read_json, read_roar, read_roar_id_gd};

#[derive(Debug, Clone, Deserialize)]
pub struct IndexManifest {
    pub version: u32,
    pub set: String,
    #[serde(default)]
    pub kind: Option<String>,
    pub built_at_secs: u64,
    pub card_count: u32,
    pub id_gd_count: u64,
    pub total_bit_span: u32,
    pub family_count: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StatsSummary {
    pub version: u32,
    pub set: String,
    pub total_cards_indexed: u32,
    pub fields: Vec<StatFieldSummary>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StatFieldSummary {
    pub field: String,
    pub element_reference: String,
    pub counts: BTreeMap<u8, u64>,
    pub bitmap_dir: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FactionsSummary {
    pub version: u32,
    pub set: String,
    pub total_cards_indexed: u32,
    pub source: String,
    pub factions: Vec<FactionSummary>,
    pub unknown_count: u64,
    pub bitmap_dir: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FactionSummary {
    pub reference: String,
    pub card_count: u64,
    pub bitmap_file: String,
}

pub const SET_CORE: &str = "CORE";
pub const SET_COREKS: &str = "COREKS";

#[derive(Debug, Clone)]
pub struct SetBitmaps {
    pub by_set: BTreeMap<String, RoaringBitmap>,
    /// `CORE | COREKS` when both sets exist in `by_set`; built once at load.
    pub core_and_coreks: Option<RoaringBitmap>,
}

pub fn build_set_bitmaps(catalog: &Catalog) -> SetBitmaps {
    let mut by_set: BTreeMap<String, RoaringBitmap> = BTreeMap::new();
    for family in &catalog.families {
        let code = family
            .source_set
            .as_deref()
            .unwrap_or(&catalog.set)
            .to_string();
        let end = family.start_bit.saturating_add(family.max_unique_id);
        by_set
            .entry(code)
            .or_insert_with(RoaringBitmap::new)
            .insert_range(family.start_bit..end);
    }

    let core_and_coreks = match (by_set.get(SET_CORE), by_set.get(SET_COREKS)) {
        (Some(a), Some(b)) => Some(a.clone() | b.clone()),
        _ => None,
    };

    SetBitmaps {
        by_set,
        core_and_coreks,
    }
}

/// One bitmap per `CardSubTypeReference`, expanded from each family's bit-span — mirrors
/// `build_set_bitmaps` exactly, but a family can carry more than one subtype, so a print can end
/// up in more than one bitmap here (unlike sets, which partition the cards).
///
/// Derived entirely from `catalog.json` (`family.card_sub_types` is already there for display) —
/// no new bitmap files, no `cli-indexer` changes needed.
pub fn build_subtype_bitmaps(catalog: &Catalog) -> BTreeMap<String, RoaringBitmap> {
    let mut by_subtype: BTreeMap<String, RoaringBitmap> = BTreeMap::new();
    for family in &catalog.families {
        if family.card_sub_types.is_empty() || family.max_unique_id == 0 {
            continue;
        }
        let end = family.start_bit.saturating_add(family.max_unique_id);
        for sub_type in &family.card_sub_types {
            let key = sub_type.reference.trim().to_ascii_uppercase();
            if key.is_empty() {
                continue;
            }
            by_subtype
                .entry(key)
                .or_insert_with(RoaringBitmap::new)
                .insert_range(family.start_bit..end);
        }
    }
    by_subtype
}

/// `Ok(empty bitmap)` when `relative_path` isn't present — a status flag with zero matches on
/// this set, or an index built before status flags existed, are indistinguishable and both fine.
fn load_status_flag(storage: &impl IndexStorage, relative_path: &str) -> Result<RoaringBitmap> {
    if !storage.has_file(relative_path) {
        return Ok(RoaringBitmap::new());
    }
    read_roar(storage, relative_path)
}

/// Lowercased locale names per catalog family row, built once at index load.
#[derive(Debug, Clone)]
pub struct NameSearchIndex {
    by_family: Vec<Vec<String>>,
}

impl NameSearchIndex {
    pub fn by_family(&self) -> &[Vec<String>] {
        &self.by_family
    }

    pub fn bitmap_for_contains(&self, catalog: &Catalog, query: &str) -> RoaringBitmap {
        let needle = normalize_name_for_search(query);
        let mut bmp = RoaringBitmap::new();
        for (family, names) in catalog.families.iter().zip(&self.by_family) {
            if names.iter().any(|name| name.contains(&needle)) {
                let end = family.start_bit.saturating_add(family.max_unique_id);
                bmp.insert_range(family.start_bit..end);
            }
        }
        bmp
    }
}

/// Lowercase and strip combining marks so `elementaire` matches `Élémentaire`.
pub fn normalize_name_for_search(name: &str) -> String {
    name.nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FamilyKey {
    pub set: String,
    pub faction: String,
    pub family_number: String,
}

#[derive(Debug, Clone, Copy)]
pub struct FamilySpan {
    pub start_bit: u32,
    pub max_unique_id: u32,
}

#[derive(Debug, Clone)]
pub struct FamilyLookupIndex {
    by_key: HashMap<FamilyKey, FamilySpan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FamilyResolveError {
    NotFound,
    Padding,
}

impl FamilyLookupIndex {
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn max_unique_id(&self, parsed: &ParsedCardPath) -> Option<u32> {
        let key = FamilyKey {
            set: parsed.set.clone(),
            faction: parsed.faction.clone(),
            family_number: parsed.family_number.clone(),
        };
        self.by_key.get(&key).map(|s| s.max_unique_id)
    }

    pub fn resolve(&self, parsed: &ParsedCardPath) -> Result<u32, FamilyResolveError> {
        let key = FamilyKey {
            set: parsed.set.clone(),
            faction: parsed.faction.clone(),
            family_number: parsed.family_number.clone(),
        };
        let span = self.by_key.get(&key).ok_or(FamilyResolveError::NotFound)?;
        if parsed.unique_id > span.max_unique_id {
            return Err(FamilyResolveError::Padding);
        }
        Ok(span.start_bit + parsed.unique_id - 1)
    }
}

/// Precomputed contiguous `card_index` half-open range `[start, end)` for one catalog family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilySpanRange {
    pub start: u32,
    pub end: u32,
}

/// Pre-sorted catalog family ranges built once at index load so family-level
/// sorts (`setDate`, `collector`, `reference`, `name`) paginate without scanning
/// or sorting millions of individual cards per request.
#[derive(Debug, Clone, Default)]
pub struct FamilySortIndex {
    pub set_date_asc: Vec<FamilySpanRange>,
    pub set_date_desc: Vec<FamilySpanRange>,
    pub collector_asc: Vec<FamilySpanRange>,
    pub reference_asc: Vec<FamilySpanRange>,
    pub name_asc_by_locale: BTreeMap<String, Vec<FamilySpanRange>>,
}

fn family_set_code<'a>(catalog: &'a Catalog, family: &'a index_core::catalog::FamilyEntry) -> &'a str {
    family.source_set.as_deref().unwrap_or(&catalog.set)
}

pub(crate) fn set_release_rank(set_ref: &str) -> (usize, &str) {
    let rank = match set_ref {
        "CORE" => 0,
        "COREKS" => 1,
        "ALIZE" => 2,
        "BISE" => 3,
        "JUDGE" => 4,
        "WCQ25" => 5,
        "WCF25" => 6,
        "WCS25" => 7,
        "CYCLONE" => 8,
        "DUSTER" => 9,
        "DUSTERCB" => 10,
        "DUSTEROP" => 11,
        "DUSTERTOP" => 12,
        "TCS3" => 13,
        "MUSUBI" => 14,
        "EOLE" => 15,
        "EOLECB" => 16,
        "EOLEOP" => 17,
        "EOLETOP" => 18,
        "FUGUE" => 19,
        "WCS26" => 20,
        _ => usize::MAX,
    };
    (rank, set_ref)
}

pub(crate) fn faction_sort_rank(faction: &str) -> (usize, &str) {
    let rank = match faction {
        "AX" => 0,
        "BR" => 1,
        "LY" => 2,
        "MU" => 3,
        "OR" => 4,
        "YZ" => 5,
        "NE" => 6,
        _ => usize::MAX,
    };
    (rank, faction)
}

fn numeric_family_key(family_number: &str) -> (u32, &str) {
    (family_number.parse::<u32>().unwrap_or(u32::MAX), family_number)
}

fn family_range(family: &index_core::catalog::FamilyEntry) -> FamilySpanRange {
    FamilySpanRange {
        start: family.start_bit,
        end: family.start_bit.saturating_add(family.max_unique_id),
    }
}

pub fn build_family_sort_index(catalog: &Catalog) -> FamilySortIndex {
    let count = catalog.families.len();
    let mut indices: Vec<usize> = (0..count).collect();

    indices.sort_by(|&a, &b| {
        let fa = &catalog.families[a];
        let fb = &catalog.families[b];
        let ka = (
            set_release_rank(family_set_code(catalog, fa)),
            faction_sort_rank(&fa.faction),
            numeric_family_key(&fa.family_number),
            fa.start_bit,
        );
        let kb = (
            set_release_rank(family_set_code(catalog, fb)),
            faction_sort_rank(&fb.faction),
            numeric_family_key(&fb.family_number),
            fb.start_bit,
        );
        ka.cmp(&kb)
    });
    let set_date_asc = indices.iter().map(|&i| family_range(&catalog.families[i])).collect();

    indices.sort_by(|&a, &b| {
        let fa = &catalog.families[a];
        let fb = &catalog.families[b];
        let ka = (
            Reverse(set_release_rank(family_set_code(catalog, fa))),
            faction_sort_rank(&fa.faction),
            numeric_family_key(&fa.family_number),
            fa.start_bit,
        );
        let kb = (
            Reverse(set_release_rank(family_set_code(catalog, fb))),
            faction_sort_rank(&fb.faction),
            numeric_family_key(&fb.family_number),
            fb.start_bit,
        );
        ka.cmp(&kb)
    });
    let set_date_desc = indices.iter().map(|&i| family_range(&catalog.families[i])).collect();

    indices.sort_by(|&a, &b| {
        let fa = &catalog.families[a];
        let fb = &catalog.families[b];
        let set_a = family_set_code(catalog, fa);
        let set_b = family_set_code(catalog, fb);
        let code_a = fa.set.code.as_deref().unwrap_or(set_a);
        let code_b = fb.set.code.as_deref().unwrap_or(set_b);
        let ka = (
            code_a,
            faction_sort_rank(&fa.faction),
            numeric_family_key(&fa.family_number),
            set_release_rank(set_a),
            fa.start_bit,
        );
        let kb = (
            code_b,
            faction_sort_rank(&fb.faction),
            numeric_family_key(&fb.family_number),
            set_release_rank(set_b),
            fb.start_bit,
        );
        ka.cmp(&kb)
    });
    let collector_asc = indices.iter().map(|&i| family_range(&catalog.families[i])).collect();

    indices.sort_by(|&a, &b| {
        let fa = &catalog.families[a];
        let fb = &catalog.families[b];
        let ka = (
            family_set_code(catalog, fa),
            fa.faction.as_str(),
            fa.family_number.as_str(),
            fa.start_bit,
        );
        let kb = (
            family_set_code(catalog, fb),
            fb.faction.as_str(),
            fb.family_number.as_str(),
            fb.start_bit,
        );
        ka.cmp(&kb)
    });
    let reference_asc = indices.iter().map(|&i| family_range(&catalog.families[i])).collect();

    let mut locales = BTreeSet::new();
    locales.insert("en_US".to_string());
    for family in &catalog.families {
        for loc in family.name.keys() {
            locales.insert(loc.clone());
        }
    }

    let mut name_asc_by_locale = BTreeMap::new();
    for locale in locales {
        let normalized: Vec<String> = catalog
            .families
            .iter()
            .map(|f| {
                let raw = f
                    .name
                    .get(&locale)
                    .or_else(|| f.name.get("en_US"))
                    .or_else(|| f.name.values().next())
                    .map(String::as_str)
                    .unwrap_or("");
                normalize_name_for_search(raw)
            })
            .collect();

        let mut loc_indices: Vec<usize> = (0..count).collect();
        loc_indices.sort_by(|&a, &b| {
            let fa = &catalog.families[a];
            let fb = &catalog.families[b];
            (
                normalized[a].as_str(),
                set_release_rank(family_set_code(catalog, fa)),
                fa.start_bit,
            )
                .cmp(&(
                    normalized[b].as_str(),
                    set_release_rank(family_set_code(catalog, fb)),
                    fb.start_bit,
                ))
        });
        name_asc_by_locale.insert(
            locale,
            loc_indices.iter().map(|&i| family_range(&catalog.families[i])).collect(),
        );
    }

    FamilySortIndex {
        set_date_asc,
        set_date_desc,
        collector_asc,
        reference_asc,
        name_asc_by_locale,
    }
}

/// One logical family in merge order; CORE+COREKS rows with the same `family_id` share one range.
#[derive(Debug, Clone)]
pub struct FamilySpanGroup {
    pub family_id: String,
    pub faction: String,
    pub family_number: String,
    pub range_start: u32,
    /// Exclusive end: last row `start_bit + max_unique_id`.
    pub range_end: u32,
}

pub fn build_family_span_groups(catalog: &Catalog) -> Vec<FamilySpanGroup> {
    let mut out: Vec<FamilySpanGroup> = Vec::new();
    for family in &catalog.families {
        let end = family.start_bit.saturating_add(family.max_unique_id);
        if let Some(last) = out.last_mut() {
            if last.family_id == family.family_id && last.range_end == family.start_bit {
                last.range_end = end;
                continue;
            }
        }
        out.push(FamilySpanGroup {
            family_id: family.family_id.clone(),
            faction: family.faction.clone(),
            family_number: family.family_number.clone(),
            range_start: family.start_bit,
            range_end: end,
        });
    }
    out
}

pub fn build_family_lookup_index(catalog: &Catalog) -> FamilyLookupIndex {
    let mut by_key = HashMap::new();
    for family in &catalog.families {
        let set = family
            .source_set
            .as_deref()
            .unwrap_or(&catalog.set)
            .to_string();
        let key = FamilyKey {
            set,
            faction: family.faction.clone(),
            family_number: family.family_number.clone(),
        };
        by_key.insert(
            key,
            FamilySpan {
                start_bit: family.start_bit,
                max_unique_id: family.max_unique_id,
            },
        );
    }
    FamilyLookupIndex { by_key }
}

/// Keyed on the collector number's own `SET-FAMILY` segments (e.g. `("BTG", "011")`), which use a
/// **different** namespace than `FamilyKey`'s `(set reference, faction, family_number)` — the set
/// segment is the display `code` (`BTG`), not the internal set reference (`COREKS`), and the family
/// segment is `collector_family_number`, unrelated to `family_number`. See
/// `cli-indexer/plans/24-collector-number-ingestion.md`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CollectorNumberKey {
    pub set_code: String,
    pub family_collector_number: String,
}

#[derive(Debug, Clone)]
pub struct CollectorNumberLookupIndex {
    by_key: HashMap<CollectorNumberKey, FamilySpan>,
}

impl CollectorNumberLookupIndex {
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn resolve(
        &self,
        set_code: &str,
        family_collector_number: &str,
        unique_id: u32,
    ) -> Result<u32, FamilyResolveError> {
        let key = CollectorNumberKey {
            set_code: set_code.to_string(),
            family_collector_number: family_collector_number.to_string(),
        };
        let span = self.by_key.get(&key).ok_or(FamilyResolveError::NotFound)?;
        if unique_id > span.max_unique_id {
            return Err(FamilyResolveError::Padding);
        }
        Ok(span.start_bit + unique_id - 1)
    }
}

/// Families missing either `set.code` or `collector_family_number` (older index, or the deferred
/// `cardsdata.rs` path) are simply absent from this index — resolution for them falls through to
/// `NotFound`, matching the `Option`-all-the-way-down tolerance the rest of this field uses.
pub fn build_collector_number_lookup_index(catalog: &Catalog) -> CollectorNumberLookupIndex {
    let mut by_key = HashMap::new();
    for family in &catalog.families {
        let (Some(set_code), Some(family_collector_number)) = (
            family.set.code.as_deref(),
            family.collector_family_number.as_deref(),
        ) else {
            continue;
        };
        let key = CollectorNumberKey {
            set_code: set_code.to_string(),
            family_collector_number: family_collector_number.to_string(),
        };
        by_key.insert(
            key,
            FamilySpan {
                start_bit: family.start_bit,
                max_unique_id: family.max_unique_id,
            },
        );
    }
    CollectorNumberLookupIndex { by_key }
}

pub fn build_name_search_index(catalog: &Catalog) -> NameSearchIndex {
    let by_family = catalog
        .families
        .iter()
        .map(|family| {
            family
                .name
                .values()
                .map(|name| normalize_name_for_search(name))
                .collect()
        })
        .collect();
    NameSearchIndex { by_family }
}

/// Precomputed bitmaps comparing `MainCost` and `RecallCost` (`stats[MainCost]` vs `stats[RecallCost]`).
#[derive(Debug, Clone, Default)]
pub struct CostRelationBitmaps {
    pub equal: RoaringBitmap,
    pub main_higher: RoaringBitmap,
    pub recall_higher: RoaringBitmap,
}

/// Build cost-relation bitmaps from the 16-bucket stat bitmaps without scanning `cards.bin`.
pub fn build_cost_relation_bitmaps(
    stats: &BTreeMap<StatField, [RoaringBitmap; 16]>,
) -> CostRelationBitmaps {
    let (Some(main_by_cost), Some(recall_by_cost)) = (
        stats.get(&StatField::MainCost),
        stats.get(&StatField::RecallCost),
    ) else {
        return CostRelationBitmaps::default();
    };

    let mut out = CostRelationBitmaps::default();
    let mut recall_lt = RoaringBitmap::new();
    for m in 0..16 {
        let main_bm = &main_by_cost[m];
        if !main_bm.is_empty() {
            if !recall_by_cost[m].is_empty() {
                out.equal |= main_bm & &recall_by_cost[m];
            }
            if !recall_lt.is_empty() {
                out.main_higher |= main_bm & &recall_lt;
            }
        }
        recall_lt |= &recall_by_cost[m];
    }

    let mut main_lt = RoaringBitmap::new();
    for r in 0..16 {
        let recall_bm = &recall_by_cost[r];
        if !recall_bm.is_empty() && !main_lt.is_empty() {
            out.recall_higher |= recall_bm & &main_lt;
        }
        main_lt |= &main_by_cost[r];
    }

    out
}

/// Build bitmap of cards with zero effects across `id_gd_whole`, `id_gd_per_line`, and `cards.bin`.
pub fn build_no_effect_bitmap(
    catalog: &Catalog,
    cards: &[u8],
    id_gd_whole: &BTreeMap<u32, RoaringBitmap>,
    id_gd_per_line: &BTreeMap<(u32, EffectLine), RoaringBitmap>,
) -> RoaringBitmap {
    let mut all_cards = RoaringBitmap::new();
    for family in &catalog.families {
        all_cards.insert_range(family.start_bit..family.start_bit + family.max_unique_id);
    }

    let mut has_effect = RoaringBitmap::new();
    for bm in id_gd_whole.values() {
        has_effect |= bm;
    }
    for bm in id_gd_per_line.values() {
        has_effect |= bm;
    }
    for (idx, rec) in cards.chunks_exact(RECORD_SIZE).enumerate() {
        if rec.len() > 6 && rec[6..].iter().any(|&b| b != 0) {
            has_effect.insert(idx as u32);
        }
    }

    all_cards - has_effect
}

pub enum AnyIndexStorage {
    Disk(DiskIndexStorage),
    Archive(TarZstIndexStorage),
}

impl IndexStorage for AnyIndexStorage {
    fn source_path(&self) -> &Path {
        match self {
            Self::Disk(storage) => storage.source_path(),
            Self::Archive(storage) => storage.source_path(),
        }
    }

    fn read_bytes(&self, relative_path: &str) -> Result<Vec<u8>> {
        match self {
            Self::Disk(storage) => storage.read_bytes(relative_path),
            Self::Archive(storage) => storage.read_bytes(relative_path),
        }
    }

    fn has_file(&self, relative_path: &str) -> bool {
        match self {
            Self::Disk(storage) => storage.has_file(relative_path),
            Self::Archive(storage) => storage.has_file(relative_path),
        }
    }
}

pub fn open_index_storage(path: &Path) -> Result<AnyIndexStorage> {
    if path.is_dir() {
        Ok(AnyIndexStorage::Disk(DiskIndexStorage::new(path)?))
    } else if is_tar_zst(path) {
        Ok(AnyIndexStorage::Archive(TarZstIndexStorage::open(path)?))
    } else {
        bail!(
            "INDEX_PATH must be a directory or a .tar.zst file (got {})",
            path.display()
        )
    }
}

fn is_tar_zst(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "zst")
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.ends_with(".tar"))
}

/// Read only `manifest.json` (cheap hot-reload poll step).
pub fn read_manifest_from(storage: &impl IndexStorage) -> Result<IndexManifest> {
    read_json(storage, "manifest.json")
}

/// Read only `manifest.json` from an on-disk index directory.
pub fn read_manifest(index_dir: &Path) -> Result<IndexManifest> {
    read_manifest_from(&DiskIndexStorage::new(index_dir)?)
}

/// Eagerly load the full index from storage into memory.
pub fn load_uniques_index_from(storage: &impl IndexStorage) -> Result<UniquesIndex> {
    let index_dir = storage.source_path().to_path_buf();

    eprintln!("loading index from {}", index_dir.display());

    let catalog: Catalog = read_json(storage, "catalog.json").with_context(|| "load catalog.json")?;
    eprintln!(
        "  catalog: {} families, total_bit_span={}",
        catalog.families.len(),
        catalog.total_bit_span
    );

    let manifest: IndexManifest = read_json(storage, "manifest.json")?;
    let idgd_catalog: IdGdCatalog = read_json(storage, "idgd_catalog.json")?;
    let stats_summary: StatsSummary = read_json(storage, "stats_summary.json")?;
    let factions_summary: FactionsSummary = read_json(storage, "factions_summary.json")?;

    let cards = storage
        .read_bytes("cards.bin")
        .with_context(|| format!("read cards.bin from {}", index_dir.display()))?;
    let expected_len = UniquesIndex::expected_cards_len(catalog.total_bit_span);
    if cards.len() as u64 != expected_len {
        bail!(
            "cards.bin size mismatch: expected {expected_len} bytes (total_bit_span {} * {RECORD_SIZE}), got {}",
            catalog.total_bit_span,
            cards.len()
        );
    }
    eprintln!("  cards.bin: {} bytes", cards.len());

    let id_gd_whole = load_id_gd_whole(storage, &idgd_catalog)?;
    eprintln!("  id_gd whole-card bitmaps: {}", id_gd_whole.len());

    let id_gd_per_line = load_id_gd_per_line(storage, &idgd_catalog)?;
    eprintln!("  id_gd per-line bitmaps: {}", id_gd_per_line.len());

    let stats = load_stats(storage, &stats_summary)?;
    eprintln!("  stats field groups: {}", stats.len());

    let factions = load_factions(storage, &factions_summary)?;
    eprintln!("  faction bitmaps: {}", factions.len());

    let set_bitmaps = build_set_bitmaps(&catalog);
    let subtype_bitmaps = build_subtype_bitmaps(&catalog);
    eprintln!("  subtype bitmaps: {} subtypes", subtype_bitmaps.len());

    let banned = load_status_flag(storage, "status/banned.roar")?;
    let errated = load_status_flag(storage, "status/errated.roar")?;
    let suspended = load_status_flag(storage, "status/suspended.roar")?;
    eprintln!(
        "  status flags: banned={} errated={} suspended={}",
        banned.len(),
        errated.len(),
        suspended.len()
    );

    eprintln!(
        "  set bitmaps: {} sets{}",
        set_bitmaps.by_set.len(),
        if set_bitmaps.core_and_coreks.is_some() {
            ", CORE+COREKS combined"
        } else {
            ""
        }
    );

    let name_search_index = build_name_search_index(&catalog);
    eprintln!(
        "  name search index: {} families",
        name_search_index.by_family().len()
    );

    let family_lookup_index = build_family_lookup_index(&catalog);
    eprintln!(
        "  family lookup index: {} families",
        family_lookup_index.len()
    );

    let family_span_groups = build_family_span_groups(&catalog);
    eprintln!(
        "  family span groups: {} ({} catalog rows)",
        family_span_groups.len(),
        catalog.families.len()
    );

    let family_sort_index = build_family_sort_index(&catalog);

    let cost_relations = build_cost_relation_bitmaps(&stats);
    let no_effect_bitmap = build_no_effect_bitmap(&catalog, &cards, &id_gd_whole, &id_gd_per_line);
    eprintln!(
        "  derived bitmaps: {} subtypes, {} no_effect cards",
        subtype_bitmaps.len(),
        no_effect_bitmap.len()
    );

    let collector_number_lookup_index = build_collector_number_lookup_index(&catalog);
    eprintln!(
        "  collector number lookup index: {} families",
        collector_number_lookup_index.len()
    );

    let id_gd_aliases = IdGdAliasMap::from_catalog(&idgd_catalog);

    let effects_list = build_effects_list(&idgd_catalog);
    let effects_body = Arc::new(serialize_effects_list(&effects_list)?);
    eprintln!(
        "  effects list: {} triggers, {} conditions, {} outputs ({} bytes JSON)",
        effects_list.triggers.len(),
        effects_list.conditions.len(),
        effects_list.output.len(),
        effects_body.len()
    );

    eprintln!("index load complete");

    Ok(UniquesIndex {
        index_dir,
        catalog,
        manifest,
        idgd_catalog,
        id_gd_aliases,
        stats_summary,
        factions_summary,
        cards,
        id_gd_whole,
        id_gd_per_line,
        stats,
        factions,
        set_bitmaps,
        cost_relations,
        subtype_bitmaps,
        no_effect_bitmap,
        banned,
        errated,
        suspended,
        name_search_index,
        family_lookup_index,
        family_span_groups,
        family_sort_index,
        collector_number_lookup_index,
        effects_body,
    })
}

/// Eagerly load the full merged index directory into memory.
pub fn load_uniques_index(index_dir: &Path) -> Result<UniquesIndex> {
    load_uniques_index_from(&DiskIndexStorage::new(index_dir)?)
}

/// `Ok(empty)` when this storage root has no `families.json` — older build, or nothing built via
/// the CardsData path yet.
pub fn load_family_catalog(
    storage: &impl IndexStorage,
) -> Result<BTreeMap<i64, index_core::family_catalog::FamilyCatalogEntry>> {
    if !storage.has_file("families.json") {
        return Ok(BTreeMap::new());
    }
    let catalog: index_core::family_catalog::FamilyCatalog = read_json(storage, "families.json")?;
    Ok(catalog.families.into_iter().map(|f| (f.id, f)).collect())
}

/// Load index and format filters; wrap in [`AppState`].
pub fn load_app_state(settings: &Settings) -> Result<AppState> {
    let (index, nonunique, families) = match settings.index.source {
        crate::config::IndexSourceKind::Disk | crate::config::IndexSourceKind::Archive => {
            let index_path = settings.index_path()?;
            let storage = open_index_storage(&index_path)?;
            let index = load_uniques_index_from(&storage)?;
            let mut nonunique = load_nonunique_index(&storage)?;
            let mut families = load_family_catalog(&storage)?;
            if index_path.is_file() {
                if let Some(parent_dir) = index_path.parent().filter(|p| p.is_dir()) {
                    if let Ok(disk_storage) = DiskIndexStorage::new(parent_dir) {
                        if nonunique.is_none() {
                            nonunique = load_nonunique_index(&disk_storage)?;
                        }
                        if families.is_empty() {
                            families = load_family_catalog(&disk_storage)?;
                        }
                    }
                }
            }
            (index, nonunique, families)
        }
        crate::config::IndexSourceKind::ObjectStore => {
            bail!("load_app_state for object_store: use load_index_from_object_store in main")
        }
        crate::config::IndexSourceKind::Http => {
            bail!("load_app_state for http: use load_index_from_http in main")
        }
    };
    Ok(build_app_state(index, nonunique, families, settings))
}

pub fn build_app_state(
    index: UniquesIndex,
    mut nonunique: Option<NonUniqueQueryIndex>,
    families: BTreeMap<i64, index_core::family_catalog::FamilyCatalogEntry>,
    settings: &Settings,
) -> AppState {
    if let Some(ref mut nu) = nonunique {
        nu.bind_families(&families);
    }
    let formats = match settings.formats.as_ref().filter(|f| f.is_enabled()) {
        Some(formats_settings) => Arc::new(load_format_index(&index, formats_settings)),
        None => Arc::new(FormatIndex::empty()),
    };
    AppState::new(QuerySnapshot {
        index: Arc::new(index),
        families: Arc::new(families),
        nonunique: nonunique.map(Arc::new),
        formats,
        collections: CollectionStore::new(&settings.collections),
    })
}

/// Load index only (empty format catalog) for tests.
pub fn load_index(index_path: &Path) -> Result<AppState> {
    let index = load_uniques_index_from(&open_index_storage(index_path)?)?;
    Ok(AppState::new_with_index(index))
}

fn load_id_gd_whole(
    storage: &impl IndexStorage,
    catalog: &IdGdCatalog,
) -> Result<BTreeMap<u32, RoaringBitmap>> {
    let mut map = BTreeMap::new();

    for entry in &catalog.entries {
        if entry.bitmap_bytes == 0 {
            continue;
        }
        let relative_path = format!("id_gd/{}", entry.bitmap_file);
        if !storage.has_file(&relative_path) {
            continue;
        }
        let bmp = read_roar_id_gd(storage, entry.id_gd, &relative_path)?;
        if !bmp.is_empty() {
            map.insert(entry.id_gd, bmp);
        }
    }

    Ok(map)
}

fn load_id_gd_per_line(
    storage: &impl IndexStorage,
    catalog: &IdGdCatalog,
) -> Result<BTreeMap<(u32, EffectLine), RoaringBitmap>> {
    let mut map = BTreeMap::new();

    for entry in &catalog.entries {
        for line in EffectLine::ALL {
            let meta = match line {
                EffectLine::M1 => entry.m1.as_ref(),
                EffectLine::M2 => entry.m2.as_ref(),
                EffectLine::M3 => entry.m3.as_ref(),
                EffectLine::Ec => entry.ec.as_ref(),
            };
            let Some(meta) = meta else { continue };
            if meta.bitmap_bytes == 0 {
                continue;
            }
            let relative_path = format!("id_gd/{}", meta.bitmap_file);
            if !storage.has_file(&relative_path) {
                continue;
            }
            let bmp = read_roar_id_gd(storage, entry.id_gd, &relative_path)?;
            if !bmp.is_empty() {
                map.insert((entry.id_gd, line), bmp);
            }
        }
    }

    Ok(map)
}

fn stat_field_from_dir_name(name: &str) -> Option<StatField> {
    StatField::ALL.into_iter().find(|f| f.dir_name() == name)
}

fn load_stats(
    storage: &impl IndexStorage,
    summary: &StatsSummary,
) -> Result<BTreeMap<StatField, [RoaringBitmap; 16]>> {
    let mut out = BTreeMap::new();

    for field_summary in &summary.fields {
        let Some(field) = stat_field_from_dir_name(&field_summary.field) else {
            eprintln!("  warning: unknown stat field {}", field_summary.field);
            continue;
        };

        let mut buckets: [RoaringBitmap; 16] = std::array::from_fn(|_| RoaringBitmap::new());

        for (&value, &count) in &field_summary.counts {
            if count == 0 {
                continue;
            }
            let relative_path = format!("{}/{}", field_summary.bitmap_dir, format!("{value:02}.roar"));
            if !storage.has_file(&relative_path) {
                eprintln!(
                    "  warning: missing stat bitmap {relative_path} (count={count})"
                );
                continue;
            }
            let bmp = read_roar(storage, &relative_path)?;
            buckets[value as usize] = bmp;
        }

        out.insert(field, buckets);
    }

    Ok(out)
}

fn faction_from_reference(reference: &str) -> Option<Faction> {
    Faction::ALL
        .into_iter()
        .find(|f| f.reference() == reference)
}

fn load_factions(
    storage: &impl IndexStorage,
    summary: &FactionsSummary,
) -> Result<BTreeMap<Faction, RoaringBitmap>> {
    let mut out = BTreeMap::new();

    for entry in &summary.factions {
        if entry.card_count == 0 {
            continue;
        }
        let Some(faction) = faction_from_reference(&entry.reference) else {
            eprintln!("  warning: unknown faction {}", entry.reference);
            continue;
        };
        if !storage.has_file(&entry.bitmap_file) {
            eprintln!(
                "  warning: missing faction bitmap {} (card_count={})",
                entry.bitmap_file, entry.card_count
            );
            continue;
        }
        let bmp = read_roar(storage, &entry.bitmap_file)?;
        if !bmp.is_empty() {
            out.insert(faction, bmp);
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use index_core::catalog::{FamilyEntry, FamilySet};
    use index_core::path::parse_card_reference;

    fn test_family_entry(start_bit: u32, max_unique_id: u32) -> FamilyEntry {
        FamilyEntry {
            start_bit,
            faction: "AX".to_string(),
            family_number: "04".to_string(),
            family_id: "AX_04".to_string(),
            source_set: None,
            max_unique_id,
            card_count: max_unique_id,
            first_reference: "ALT_TEST_B_AX_04_U_1".to_string(),
            name: Default::default(),
            artist: String::new(),
            card_sub_types: vec![],
            set: FamilySet {
                reference: "TEST".to_string(),
                name: String::new(),
                code: None,
            },
            card_family_id: None,
            collector_family_number: None,
        }
    }

    #[test]
    fn family_lookup_index_has_one_entry_per_catalog_family() {
        let catalog = Catalog {
            set: "TEST".to_string(),
            faction_order: vec![],
            families: vec![test_family_entry(0, 3)],
            total_bit_span: 3,
        };
        let index = build_family_lookup_index(&catalog);
        assert_eq!(index.len(), 1);
        let parsed = parse_card_reference("ALT_TEST_B_AX_04_U_2").unwrap();
        assert_eq!(index.resolve(&parsed).unwrap(), 1);
    }

    #[test]
    fn family_lookup_index_rejects_padding_uid() {
        let catalog = Catalog {
            set: "TEST".to_string(),
            faction_order: vec![],
            families: vec![test_family_entry(10, 1)],
            total_bit_span: 11,
        };
        let index = build_family_lookup_index(&catalog);
        let parsed = parse_card_reference("ALT_TEST_B_AX_04_U_99").unwrap();
        assert_eq!(index.resolve(&parsed), Err(FamilyResolveError::Padding));
    }

    fn test_family_entry_with_set(
        start_bit: u32,
        max_unique_id: u32,
        family_id: &str,
        source_set: &str,
    ) -> FamilyEntry {
        let mut entry = test_family_entry(start_bit, max_unique_id);
        entry.family_id = family_id.to_string();
        entry.source_set = Some(source_set.to_string());
        entry
    }

    #[test]
    fn family_span_groups_merges_adjacent_core_coreks_rows() {
        let catalog = Catalog {
            set: "ALL_SETS".to_string(),
            faction_order: vec![],
            families: vec![
                test_family_entry_with_set(0, 3, "AX_04", SET_COREKS),
                test_family_entry_with_set(3, 3, "AX_04", SET_CORE),
                test_family_entry_with_set(6, 2, "AX_05", SET_COREKS),
            ],
            total_bit_span: 8,
        };
        let groups = build_family_span_groups(&catalog);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].family_id, "AX_04");
        assert_eq!(groups[0].range_start, 0);
        assert_eq!(groups[0].range_end, 6);
        assert_eq!(groups[1].family_id, "AX_05");
        assert_eq!(groups[1].range_start, 6);
        assert_eq!(groups[1].range_end, 8);

        let mut bmp = RoaringBitmap::new();
        bmp.insert(1);
        bmp.insert(4);
        assert_eq!(bmp.range_cardinality(0..6), 2);
        assert_eq!(bmp.range_cardinality(0..3), 1);
        assert_eq!(bmp.range_cardinality(3..6), 1);
    }

    #[test]
    fn normalize_name_for_search_folds_diacritics_and_lowercases() {
        assert_eq!(normalize_name_for_search("Élémentaire"), "elementaire");
        assert_eq!(normalize_name_for_search("elementaire"), "elementaire");
        assert_eq!(
            normalize_name_for_search("Élémentaire de Kélon"),
            "elementaire de kelon"
        );
    }
}
