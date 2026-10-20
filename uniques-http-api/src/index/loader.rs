use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use index_core::bitmap::EffectLine;
use index_core::idgd_collapse::IdGdAliasMap;
use index_core::catalog::Catalog;
use index_core::compact::RECORD_SIZE;
use index_core::faction_index::Faction;
use index_core::idgd_catalog::IdGdCatalog;
use index_core::keyword_catalog::{KeywordCatalog, KEYWORDS_FILE};
use index_core::path::ParsedCardPath;
use index_core::stat_index::StatField;
use roaring::RoaringBitmap;
use serde::Deserialize;
use unicode_normalization::UnicodeNormalization;

use crate::collections::CollectionStore;
use crate::config::Settings;
use crate::formats::{load_format_index, FormatIndex};
use crate::http::api::effect_text::unresolved_keywords;
use crate::http::api::effects::{build_effects_list, gzip_effects_body, serialize_effects_list};
use crate::http::state::{AppState, QuerySnapshot};
use crate::index::UniquesIndex;

pub mod archive;
pub mod disk;
pub mod http;
pub mod object_store;
pub mod storage;

pub use archive::TarZstIndexStorage;
pub use disk::DiskIndexStorage;
pub use http::{load_app_state_from_http, load_index_from_http, HttpIndexClient};
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

    let id_gd_aliases = IdGdAliasMap::from_catalog(&idgd_catalog);

    let keywords: KeywordCatalog = if storage.has_file(KEYWORDS_FILE) {
        read_json(storage, KEYWORDS_FILE)?
    } else {
        KeywordCatalog::default()
    };
    eprintln!(
        "  keywords: {} printed keyword names",
        keywords.keywords.len()
    );
    let unresolved = unresolved_keywords(&idgd_catalog, &keywords);
    if !unresolved.unknown.is_empty() {
        let codes = unresolved.unknown.iter().cloned().collect::<Vec<_>>();
        eprintln!(
            "  warning: keyword codes without a printed name (left as [CODE]): {}",
            codes.join(", ")
        );
    }
    for (code, locales) in &unresolved.english_fallback {
        let locales = locales.iter().cloned().collect::<Vec<_>>().join(", ");
        eprintln!("  warning: keyword {code} printed in English for {locales}");
    }

    let effects_list = build_effects_list(&idgd_catalog, &keywords);
    let effects_body = Arc::new(serialize_effects_list(&effects_list)?);
    let effects_body_gzip = Arc::new(gzip_effects_body(&effects_body)?);
    eprintln!(
        "  effects list: {} triggers, {} conditions, {} outputs ({} bytes JSON, {} bytes gzip)",
        effects_list.triggers.len(),
        effects_list.conditions.len(),
        effects_list.output.len(),
        effects_body.len(),
        effects_body_gzip.len()
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
        name_search_index,
        family_lookup_index,
        family_span_groups,
        effects_body,
        effects_body_gzip: Some(effects_body_gzip),
    })
}

/// Eagerly load the full merged index directory into memory.
pub fn load_uniques_index(index_dir: &Path) -> Result<UniquesIndex> {
    load_uniques_index_from(&DiskIndexStorage::new(index_dir)?)
}

/// Load index and format filters; wrap in [`AppState`].
pub fn load_app_state(settings: &Settings) -> Result<AppState> {
    let index = match settings.index.source {
        crate::config::IndexSourceKind::Disk | crate::config::IndexSourceKind::Archive => {
            load_uniques_index_from(&open_index_storage(&settings.index_path()?)?)?
        }
        crate::config::IndexSourceKind::ObjectStore => {
            bail!("load_app_state for object_store: use load_index_from_object_store in main")
        }
        crate::config::IndexSourceKind::Http => {
            bail!("load_app_state for http: use load_index_from_http in main")
        }
    };
    Ok(build_app_state(index, settings))
}

pub fn build_app_state(index: UniquesIndex, settings: &Settings) -> AppState {
    let formats = match settings.formats.as_ref().filter(|f| f.is_enabled()) {
        Some(formats_settings) => Arc::new(load_format_index(&index, formats_settings)),
        None => Arc::new(FormatIndex::empty()),
    };
    AppState::new(QuerySnapshot {
        index: Arc::new(index),
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
