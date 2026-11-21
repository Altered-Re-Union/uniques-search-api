//! Standalone non-unique card index: CSV ingestion, compact records, and bitmaps.
//!
//! First slice per [`cli-indexer/plans/16-nonunique-index.md`]: faction, rarity, product,
//! serialization, and the 5 stats shared with uniques. Extended (see
//! `docs/non-unique-refonte-decisions.md`) with set/edition, card type, subtypes, and
//! banned/errated/suspended status — all resolved via a `CardFamilyId` join against
//! `CardFamilies.csv`/`CardFamilySubTypes.csv`. Still no effect search, no `PERMANENT`/`RESERVE`
//! stats — see the plan for what's deferred and why.
//!
//! Unlike uniques, a non-unique print isn't a numbered instance within a family span — each row is
//! its own print, addressed by its position (`print_index`) in a flat, sorted list. So this module
//! has no `CatalogBuilder`-equivalent bit-span logic, and no `CardJson` detour either: CSV rows go
//! straight to [`CompactNonUniqueFields`] (family-level facets like type/subtypes are resolved into
//! bitmaps directly at build time, not carried in the compact per-print record).
//!
//! [`cli-indexer/plans/16-nonunique-index.md`]: https://github.com/Altered-Re-Union/uniques-search-api/blob/main/cli-indexer/plans/16-nonunique-index.md

use crate::csv_util::read_all;
use anyhow::{Context, Result};
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const VALUE_BUCKETS: usize = 16;
const LOCALES: [&str; 5] = ["en_US", "fr_FR", "es_ES", "de_DE", "it_IT"];

/// Canonicalize `EffectFragments.KeywordReference` variants (`FLEETING_CHAR` -> `FLEETING`,
/// `RESUPPLIES` / `RESUPPLY_INF` / `EXHAUSTED_RESUPPLY` -> `RESUPPLY`, etc.) so keyword filtering
/// matches what users pick in the UI filter panel.
pub fn canonical_keyword(raw: &str) -> Option<String> {
    let k = raw.trim().to_ascii_uppercase();
    if k.is_empty() || k == "BB" {
        return None;
    }
    let mapped = match k.as_str() {
        s if s.starts_with("ANCHORED") => "ANCHORED",
        s if s.starts_with("ASLEEP") => "ASLEEP",
        s if s.starts_with("ASCEND") || s == "DUE_TO_ASCENSION" => "ASCEND",
        s if s.starts_with("BOOSTED") => "BOOSTED",
        s if s.starts_with("COOLDOWN") => "COOLDOWN",
        s if s.starts_with("DEFENDER") => "DEFENDER",
        s if s.starts_with("ETERNAL") => "ETERNAL",
        s if s.starts_with("FLEETING") => "FLEETING",
        s if s.starts_with("GIGANTIC") => "GIGANTIC",
        s if s.starts_with("RESUPPL") || s.starts_with("EXHAUSTED_RESUPPL") => "RESUPPLY",
        s if s.starts_with("SABOTAGE") => "SABOTAGE",
        s if s.starts_with("SCOUT") => "SCOUT",
        s if s.starts_with("SEASONED") => "SEASONED",
        s if s.starts_with("TOUGH") => "TOUGH",
        other => other,
    };
    Some(mapped.to_string())
}

/// Map `RarityReference` to an on-disk code (`0` = unknown/unmapped). `U` (Unique) isn't expected
/// on a non-unique row but isn't rejected here either — see `load_nonunique_prints`.
fn rarity_code_from_reference(reference: &str) -> u8 {
    match reference {
        "C" => 1,
        "R" => 2,
        "E" => 3,
        "U" => 4,
        _ => 0,
    }
}

pub fn rarity_reference_from_code(code: u8) -> Option<&'static str> {
    match code {
        1 => Some("C"),
        2 => Some("R"),
        3 => Some("E"),
        4 => Some("U"),
        _ => None,
    }
}

/// Map `ProductReference` to an on-disk code (`0` = unknown/unmapped). This is also where
/// "alt-art" already lives (`A`) — see decision D4, no separate alt-art flag needed.
fn product_code_from_reference(reference: &str) -> u8 {
    match reference {
        "B" => 1,
        "A" => 2,
        "P" => 3,
        _ => 0,
    }
}

pub fn product_reference_from_code(code: u8) -> Option<&'static str> {
    match code {
        1 => Some("B"),
        2 => Some("A"),
        3 => Some("P"),
        _ => None,
    }
}

// --- Compact per-print record ---

#[derive(Debug, Clone, Copy, Default)]
pub struct CompactNonUniqueFields {
    pub faction_code: u8,
    pub rarity_code: u8,
    pub product_code: u8,
    pub is_serialized: bool,
    pub main_cost: u8,
    pub recall_cost: u8,
    pub mountain_power: u8,
    pub ocean_power: u8,
    pub forest_power: u8,
}

pub const RECORD_SIZE: usize = 9;

pub fn encode_record(fields: &CompactNonUniqueFields) -> [u8; RECORD_SIZE] {
    [
        fields.faction_code,
        fields.rarity_code,
        fields.product_code,
        fields.is_serialized as u8,
        fields.main_cost,
        fields.recall_cost,
        fields.mountain_power,
        fields.ocean_power,
        fields.forest_power,
    ]
}

pub fn decode_record(buf: &[u8; RECORD_SIZE]) -> CompactNonUniqueFields {
    CompactNonUniqueFields {
        faction_code: buf[0],
        rarity_code: buf[1],
        product_code: buf[2],
        is_serialized: buf[3] != 0,
        main_cost: buf[4],
        recall_cost: buf[5],
        mountain_power: buf[6],
        ocean_power: buf[7],
        forest_power: buf[8],
    }
}

pub fn write_compact_records(path: &Path, records: &[CompactNonUniqueFields]) -> Result<()> {
    let mut bytes = Vec::with_capacity(records.len() * RECORD_SIZE);
    for record in records {
        bytes.extend_from_slice(&encode_record(record));
    }
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

pub struct CompactNonUniqueView<'a> {
    buf: &'a [u8; RECORD_SIZE],
}

impl<'a> CompactNonUniqueView<'a> {
    pub fn from_data(data: &'a [u8], print_index: u32) -> Option<Self> {
        let offset = print_index as usize * RECORD_SIZE;
        let slice = data.get(offset..offset + RECORD_SIZE)?;
        let buf: &[u8; RECORD_SIZE] = slice.try_into().ok()?;
        Some(Self { buf })
    }

    pub fn fields(&self) -> CompactNonUniqueFields {
        decode_record(self.buf)
    }
}

// --- Catalog: print_index -> reference (no family spans in this slice) ---

#[derive(Debug, Default)]
pub struct NonUniqueCatalogBuilder {
    references: Vec<String>,
}

impl NonUniqueCatalogBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a print; returns its `print_index` for bitmap insertion.
    pub fn push(&mut self, reference: String) -> u32 {
        let index = self.references.len() as u32;
        self.references.push(reference);
        index
    }

    pub fn into_catalog(self, set: impl Into<String>) -> NonUniqueCatalog {
        NonUniqueCatalog {
            set: set.into(),
            references: self.references,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NonUniqueCatalog {
    pub set: String,
    pub references: Vec<String>,
}

impl NonUniqueCatalog {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        fs::write(path, text).with_context(|| format!("write {}", path.display()))
    }

    pub fn reference_for_index(&self, print_index: u32) -> Option<&str> {
        self.references
            .get(print_index as usize)
            .map(String::as_str)
    }

    /// Reverse of [`reference_for_index`](Self::reference_for_index) — non-unique addressing is
    /// flat/sequential (unlike unique's bit-span arithmetic), so a print's `print_index` *is* its
    /// bit position and no parsing of the reference's structure is needed, just an exact match
    /// against the reference string CardsData already assigned it. Linear scan: catalogs here are
    /// hundreds to low thousands of entries, and this only runs at collection-creation time, not on
    /// the query hot path.
    pub fn index_for_reference(&self, reference: &str) -> Option<u32> {
        self.references
            .iter()
            .position(|r| r == reference)
            .map(|i| i as u32)
    }

    pub fn total_cards(&self) -> u32 {
        self.references.len() as u32
    }
}

// --- Bitmaps: faction / rarity / product / serialized / stats ---

#[derive(Debug, Default)]
pub struct NonUniqueIndexBuilder {
    faction: BTreeMap<String, RoaringBitmap>,
    rarity: BTreeMap<u8, RoaringBitmap>,
    product: BTreeMap<u8, RoaringBitmap>,
    serialized: RoaringBitmap,
    stats: BTreeMap<&'static str, [RoaringBitmap; VALUE_BUCKETS]>,
    set: BTreeMap<String, RoaringBitmap>,
    card_type: BTreeMap<String, RoaringBitmap>,
    subtype: BTreeMap<String, RoaringBitmap>,
    family: BTreeMap<String, RoaringBitmap>,
    keywords: BTreeMap<String, RoaringBitmap>,
    cost_equal: RoaringBitmap,
    cost_main_higher: RoaringBitmap,
    cost_recall_higher: RoaringBitmap,
    has_no_effect: RoaringBitmap,
    banned: RoaringBitmap,
    errated: RoaringBitmap,
    suspended: RoaringBitmap,
}

impl NonUniqueIndexBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_record(&mut self, print_index: u32, record: &NonUniquePrintRecord) {
        let fields = &record.fields;
        if !record.faction_reference.is_empty() {
            self.faction
                .entry(record.faction_reference.clone())
                .or_default()
                .insert(print_index);
        }
        if fields.rarity_code != 0 {
            self.rarity
                .entry(fields.rarity_code)
                .or_default()
                .insert(print_index);
        }
        if fields.product_code != 0 {
            self.product
                .entry(fields.product_code)
                .or_default()
                .insert(print_index);
        }
        if fields.is_serialized {
            self.serialized.insert(print_index);
        }
        for (name, opt_val) in [
            ("main_cost", record.main_cost),
            ("recall_cost", record.recall_cost),
            ("mountain_power", record.mountain_power),
            ("ocean_power", record.ocean_power),
            ("forest_power", record.forest_power),
        ] {
            if let Some(v) = opt_val {
                let value = v as usize;
                if value < VALUE_BUCKETS {
                    self.stats.entry(name).or_insert_with(empty_buckets)[value].insert(print_index);
                }
            }
        }

        if let (Some(mc), Some(rc)) = (record.main_cost, record.recall_cost) {
            if mc == rc {
                self.cost_equal.insert(print_index);
            } else if mc > rc {
                self.cost_main_higher.insert(print_index);
            } else {
                self.cost_recall_higher.insert(print_index);
            }
        }

        if record.main_effect.is_empty() && record.echo_effect.is_empty() {
            self.has_no_effect.insert(print_index);
        }

        if !record.set_reference.is_empty() {
            self.set
                .entry(record.set_reference.clone())
                .or_default()
                .insert(print_index);
        }
        if let Some(card_type) = record.card_type.as_deref() {
            self.card_type
                .entry(card_type.to_string())
                .or_default()
                .insert(print_index);
        }
        for subtype in &record.subtypes {
            self.subtype
                .entry(subtype.clone())
                .or_default()
                .insert(print_index);
        }
        for kw in &record.keywords {
            self.keywords
                .entry(kw.clone())
                .or_default()
                .insert(print_index);
        }
        self.family
            .entry(record.card_family_id.to_string())
            .or_default()
            .insert(print_index);
        if record.is_banned {
            self.banned.insert(print_index);
        }
        if record.is_errated {
            self.errated.insert(print_index);
        }
        if record.is_suspended {
            self.suspended.insert(print_index);
        }
    }

    pub fn into_index(self) -> NonUniqueIndex {
        NonUniqueIndex {
            faction: self.faction,
            rarity: self.rarity,
            product: self.product,
            serialized: self.serialized,
            stats: self.stats,
            set: self.set,
            card_type: self.card_type,
            subtype: self.subtype,
            family: self.family,
            keywords: self.keywords,
            cost_equal: self.cost_equal,
            cost_main_higher: self.cost_main_higher,
            cost_recall_higher: self.cost_recall_higher,
            has_no_effect: self.has_no_effect,
            banned: self.banned,
            errated: self.errated,
            suspended: self.suspended,
        }
    }
}

pub struct NonUniqueIndex {
    faction: BTreeMap<String, RoaringBitmap>,
    rarity: BTreeMap<u8, RoaringBitmap>,
    product: BTreeMap<u8, RoaringBitmap>,
    serialized: RoaringBitmap,
    stats: BTreeMap<&'static str, [RoaringBitmap; VALUE_BUCKETS]>,
    set: BTreeMap<String, RoaringBitmap>,
    card_type: BTreeMap<String, RoaringBitmap>,
    subtype: BTreeMap<String, RoaringBitmap>,
    family: BTreeMap<String, RoaringBitmap>,
    keywords: BTreeMap<String, RoaringBitmap>,
    cost_equal: RoaringBitmap,
    cost_main_higher: RoaringBitmap,
    cost_recall_higher: RoaringBitmap,
    has_no_effect: RoaringBitmap,
    banned: RoaringBitmap,
    errated: RoaringBitmap,
    suspended: RoaringBitmap,
}

impl NonUniqueIndex {
    /// Writes `<root>/nonunique/{factions,rarity,product,stats,set,type,subtype,family,keywords,cost_relations}/...` and
    /// `<root>/nonunique/{serialized,has_no_effect,status/{banned,errated,suspended}}.roar`.
    pub fn write_dir(&self, root: &Path) -> Result<()> {
        let nonunique_root = root.join("nonunique");
        fs::create_dir_all(&nonunique_root)?;

        let factions_dir = nonunique_root.join("factions");
        fs::create_dir_all(&factions_dir)?;
        for (reference, bitmap) in &self.faction {
            if !bitmap.is_empty() {
                write_single_bitmap(&factions_dir.join(format!("{reference}.roar")), bitmap)?;
            }
        }
        write_coded_bitmaps(
            &nonunique_root.join("rarity"),
            &self.rarity,
            rarity_reference_from_code,
        )?;
        write_coded_bitmaps(
            &nonunique_root.join("product"),
            &self.product,
            product_reference_from_code,
        )?;

        write_named_bitmaps(&nonunique_root.join("set"), &self.set)?;
        write_named_bitmaps(&nonunique_root.join("type"), &self.card_type)?;
        write_named_bitmaps(&nonunique_root.join("subtype"), &self.subtype)?;
        write_named_bitmaps(&nonunique_root.join("family"), &self.family)?;
        write_named_bitmaps(&nonunique_root.join("keywords"), &self.keywords)?;

        write_single_bitmap(&nonunique_root.join("serialized.roar"), &self.serialized)?;
        write_single_bitmap(&nonunique_root.join("has_no_effect.roar"), &self.has_no_effect)?;

        let cost_rel_root = nonunique_root.join("cost_relations");
        fs::create_dir_all(&cost_rel_root)?;
        write_single_bitmap(&cost_rel_root.join("equal.roar"), &self.cost_equal)?;
        write_single_bitmap(&cost_rel_root.join("main_higher.roar"), &self.cost_main_higher)?;
        write_single_bitmap(&cost_rel_root.join("recall_higher.roar"), &self.cost_recall_higher)?;

        let status_root = nonunique_root.join("status");
        fs::create_dir_all(&status_root)?;
        write_single_bitmap(&status_root.join("banned.roar"), &self.banned)?;
        write_single_bitmap(&status_root.join("errated.roar"), &self.errated)?;
        write_single_bitmap(&status_root.join("suspended.roar"), &self.suspended)?;

        let stats_root = nonunique_root.join("stats");
        fs::create_dir_all(&stats_root)?;
        for (name, buckets) in &self.stats {
            let field_dir = stats_root.join(name);
            fs::create_dir_all(&field_dir)?;
            for (value, bitmap) in buckets.iter().enumerate() {
                if bitmap.is_empty() {
                    continue;
                }
                let path = field_dir.join(format!("{value:02}.roar"));
                let mut bytes = Vec::new();
                bitmap
                    .serialize_into(&mut bytes)
                    .with_context(|| format!("serialize {name} value {value}"))?;
                fs::write(&path, bytes)?;
            }
        }

        Ok(())
    }
}

fn write_single_bitmap(path: &Path, bitmap: &RoaringBitmap) -> Result<()> {
    if bitmap.is_empty() {
        return Ok(());
    }
    let mut bytes = Vec::new();
    bitmap
        .serialize_into(&mut bytes)
        .with_context(|| format!("serialize {}", path.display()))?;
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

/// Writes one `.roar` per non-empty entry, plus an `_index.json` array of the names that got a
/// file. Unlike faction/rarity/product (a small fixed code space the HTTP loader can just probe
/// with `has_file`), set/type/subtype are open-ended — a new set release adds new values — so the
/// loader needs this list rather than guessing which files might exist.
fn write_named_bitmaps(dir: &Path, bitmaps: &BTreeMap<String, RoaringBitmap>) -> Result<()> {
    fs::create_dir_all(dir)?;
    let mut present: Vec<&str> = Vec::new();
    for (name, bitmap) in bitmaps {
        if bitmap.is_empty() {
            continue;
        }
        write_single_bitmap(&dir.join(format!("{name}.roar")), bitmap)?;
        present.push(name);
    }
    let index_json = serde_json::to_string_pretty(&present)?;
    fs::write(dir.join("_index.json"), index_json)
        .with_context(|| format!("write {}/_index.json", dir.display()))?;
    Ok(())
}

fn write_coded_bitmaps(
    dir: &Path,
    buckets: &BTreeMap<u8, RoaringBitmap>,
    reference_from_code: fn(u8) -> Option<&'static str>,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    for (code, bitmap) in buckets {
        if bitmap.is_empty() {
            continue;
        }
        let Some(reference) = reference_from_code(*code) else {
            continue;
        };
        let path = dir.join(format!("{reference}.roar"));
        let mut bytes = Vec::new();
        bitmap
            .serialize_into(&mut bytes)
            .with_context(|| format!("serialize {reference}"))?;
        fs::write(&path, bytes)?;
    }
    Ok(())
}

fn empty_buckets() -> [RoaringBitmap; VALUE_BUCKETS] {
    std::array::from_fn(|_| RoaringBitmap::new())
}

// --- CSV ingestion ---

#[derive(Debug, Deserialize)]
struct NonUniquePrintRow {
    #[serde(rename = "Id")]
    id: i64,
    #[serde(rename = "Reference")]
    reference: String,
    #[serde(rename = "CardFamilyId")]
    card_family_id: i64,
    #[serde(rename = "SetReference")]
    set_reference: String,
    #[serde(rename = "FactionReference")]
    faction_reference: String,
    #[serde(rename = "RarityReference")]
    rarity_reference: String,
    #[serde(rename = "ProductReference")]
    product_reference: String,
    #[serde(rename = "ArtistId", default, deserialize_with = "csv::invalid_option")]
    artist_id: Option<i64>,
    #[serde(rename = "CollectorNumberFormatted", default)]
    collector_number_formatted: String,
    #[serde(rename = "IsSerialized")]
    is_serialized: u8,
    #[serde(rename = "IsBanned")]
    is_banned: u8,
    #[serde(rename = "IsErrated")]
    is_errated: u8,
    #[serde(rename = "IsSuspended")]
    is_suspended: u8,
    // Raw strings: blank on rows whose card type doesn't carry this stat (e.g. a
    // LANDMARK_PERMANENT or HERO row leaves these blank).
    #[serde(rename = "MAIN_COST")]
    main_cost: String,
    #[serde(rename = "RECALL_COST")]
    recall_cost: String,
    #[serde(rename = "MOUNTAIN_POWER")]
    mountain_power: String,
    #[serde(rename = "OCEAN_POWER")]
    ocean_power: String,
    #[serde(rename = "FOREST_POWER")]
    forest_power: String,
    #[serde(rename = "PERMANENT", default)]
    permanent: String,
    #[serde(rename = "RESERVE", default)]
    reserve: String,
}

#[derive(Debug, Deserialize)]
struct ArtistCsvRow {
    #[serde(rename = "Id")]
    id: i64,
    #[serde(rename = "NickName")]
    nick_name: String,
}

#[derive(Debug, Deserialize)]
struct SetCsvRow {
    #[serde(rename = "Reference")]
    reference: String,
    #[serde(rename = "Name_en_US")]
    name_en_us: String,
}

#[derive(Debug, Deserialize)]
struct CardTypeCsvRow {
    #[serde(rename = "Reference")]
    reference: String,
    #[serde(rename = "Name_en_US")]
    name_en_us: String,
    #[serde(rename = "Name_fr_FR")]
    name_fr_fr: String,
    #[serde(rename = "Name_es_ES")]
    name_es_es: String,
    #[serde(rename = "Name_de_DE")]
    name_de_de: String,
    #[serde(rename = "Name_it_IT")]
    name_it_it: String,
}

#[derive(Debug, Deserialize)]
struct KeywordCsvRow {
    #[serde(rename = "Reference")]
    reference: String,
    #[serde(rename = "DisplayWeb_en_US", default)]
    display_web_en_us: String,
    #[serde(rename = "DisplayWeb_fr_FR", default)]
    display_web_fr_fr: String,
    #[serde(rename = "DisplayWeb_es_ES", default)]
    display_web_es_es: String,
    #[serde(rename = "DisplayWeb_de_DE", default)]
    display_web_de_de: String,
    #[serde(rename = "DisplayWeb_it_IT", default)]
    display_web_it_it: String,
}

#[derive(Debug, Deserialize)]
struct CardFamilyTypeRow {
    #[serde(rename = "Id")]
    id: i64,
    #[serde(rename = "CardTypeReference")]
    card_type_reference: String,
}

#[derive(Debug, Deserialize)]
struct CardFamilySubTypeRow {
    #[serde(rename = "CardFamilyId")]
    card_family_id: i64,
    #[serde(rename = "CardSubTypeReference")]
    card_sub_type_reference: String,
}

/// One row of `NonUnique/<set>/PrintEffectFragments.csv` — same shape as the unique side's
/// `UniquePrintEffectFragments.csv`, joined via the print's numeric `Id`.
#[derive(Debug, Deserialize)]
struct PrintEffectFragmentRow {
    #[serde(rename = "PrintId")]
    print_id: i64,
    #[serde(rename = "DisplayIndex", default)]
    display_index: i32,
    #[serde(rename = "TriggerId", deserialize_with = "csv::invalid_option")]
    trigger_id: Option<i64>,
    #[serde(rename = "ConditionId", deserialize_with = "csv::invalid_option")]
    condition_id: Option<i64>,
    #[serde(rename = "EffectId", deserialize_with = "csv::invalid_option")]
    effect_id: Option<i64>,
}

/// Full text + keyword + slot row from shared `EffectFragments.csv`.
#[derive(Debug, Deserialize)]
struct EffectFragmentTextRow {
    #[serde(rename = "IdGd")]
    id_gd: i64,
    #[serde(rename = "KeywordReference", default)]
    keyword_reference: String,
    #[serde(rename = "Slot", default)]
    slot: String,
    #[serde(rename = "Text_en_US")]
    text_en_us: String,
    #[serde(rename = "Text_fr_FR")]
    text_fr_fr: String,
    #[serde(rename = "Text_es_ES")]
    text_es_es: String,
    #[serde(rename = "Text_de_DE")]
    text_de_de: String,
    #[serde(rename = "Text_it_IT")]
    text_it_it: String,
}

pub struct NonUniquePrintRecord {
    pub reference: String,
    pub card_family_id: i64,
    pub fields: CompactNonUniqueFields,
    pub set_reference: String,
    pub set_name: String,
    pub set_code: Option<String>,
    pub faction_reference: String,
    pub rarity_reference: String,
    pub product_reference: String,
    pub artist: String,
    pub collector_number: Option<String>,
    pub card_type: Option<String>,
    pub subtypes: Vec<String>,
    pub keywords: Vec<String>,
    pub main_cost: Option<u8>,
    pub recall_cost: Option<u8>,
    pub mountain_power: Option<u8>,
    pub ocean_power: Option<u8>,
    pub forest_power: Option<u8>,
    pub permanent: Option<u8>,
    pub reserve: Option<u8>,
    pub is_banned: bool,
    pub is_errated: bool,
    pub is_suspended: bool,
    pub main_effect: BTreeMap<String, String>,
    pub echo_effect: BTreeMap<String, String>,
    /// Locale -> space-joined combined text for full-text search.
    pub effect_text: BTreeMap<String, String>,
}

/// Serializable per-print metadata stored at `nonunique/prints.json`, parallel to
/// `nonunique/catalog.json` (`prints[print_index]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NonUniquePrintMeta {
    pub reference: String,
    pub card_family_id: i64,
    pub set: String,
    pub set_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_code: Option<String>,
    pub faction: String,
    pub rarity: String,
    pub product: String,
    pub is_serialized: bool,
    pub is_banned: bool,
    pub is_errated: bool,
    pub is_suspended: bool,
    pub artist: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collector_number: Option<String>,
    pub card_type: String,
    #[serde(default)]
    pub card_sub_types: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub main_cost: Option<u8>,
    #[serde(default)]
    pub recall_cost: Option<u8>,
    #[serde(default)]
    pub forest_power: Option<u8>,
    #[serde(default)]
    pub mountain_power: Option<u8>,
    #[serde(default)]
    pub ocean_power: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permanent: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserve: Option<u8>,
    #[serde(default)]
    pub main_effect: BTreeMap<String, String>,
    #[serde(default)]
    pub echo_effect: BTreeMap<String, String>,
}

impl From<&NonUniquePrintRecord> for NonUniquePrintMeta {
    fn from(r: &NonUniquePrintRecord) -> Self {
        Self {
            reference: r.reference.clone(),
            card_family_id: r.card_family_id,
            set: r.set_reference.clone(),
            set_name: r.set_name.clone(),
            set_code: r.set_code.clone(),
            faction: r.faction_reference.clone(),
            rarity: r.rarity_reference.clone(),
            product: r.product_reference.clone(),
            is_serialized: r.fields.is_serialized,
            is_banned: r.is_banned,
            is_errated: r.is_errated,
            is_suspended: r.is_suspended,
            artist: r.artist.clone(),
            collector_number: r.collector_number.clone(),
            card_type: r.card_type.clone().unwrap_or_default(),
            card_sub_types: r.subtypes.clone(),
            keywords: r.keywords.clone(),
            main_cost: r.main_cost,
            recall_cost: r.recall_cost,
            forest_power: r.forest_power,
            mountain_power: r.mountain_power,
            ocean_power: r.ocean_power,
            permanent: r.permanent,
            reserve: r.reserve,
            main_effect: r.main_effect.clone(),
            echo_effect: r.echo_effect.clone(),
        }
    }
}

fn read_optional_all<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    read_all(path)
}

fn parse_optional_u8(raw: &str) -> Option<u8> {
    let s = raw.trim();
    if s.is_empty() {
        None
    } else {
        s.parse::<u8>().ok()
    }
}

fn substitute_keywords(
    raw: &str,
    locale: &str,
    keywords: &BTreeMap<String, KeywordCsvRow>,
) -> String {
    if !raw.contains('[') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        if let Some(close) = after_open.find(']') {
            let token = &after_open[..close];
            if let Some(kw) = keywords.get(token) {
                let localized = match locale {
                    "fr_FR" => &kw.display_web_fr_fr,
                    "es_ES" => &kw.display_web_es_es,
                    "de_DE" => &kw.display_web_de_de,
                    "it_IT" => &kw.display_web_it_it,
                    _ => &kw.display_web_en_us,
                };
                let replacement = if !localized.is_empty() {
                    localized.as_str()
                } else if !kw.display_web_en_us.is_empty() {
                    kw.display_web_en_us.as_str()
                } else {
                    token
                };
                out.push_str(replacement);
            } else {
                out.push('[');
                out.push_str(token);
                out.push(']');
            }
            rest = &after_open[close + 1..];
        } else {
            out.push('[');
            out.push_str(after_open);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

fn extract_bracket_keywords(raw: &str, out: &mut BTreeSet<String>) {
    let mut rest = raw;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        if let Some(close) = after.find(']') {
            let token = &after[..close];
            if !token.is_empty()
                && token
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                if let Some(canon) = canonical_keyword(token) {
                    out.insert(canon);
                }
            }
            rest = &after[close + 1..];
        } else {
            break;
        }
    }
}

/// Reads `<cardsdata_root>/data/csv/NonUnique/<set>/NonUniquePrints.csv` plus shared referentials.
/// Skips placeholder `FOILER` rows and sorts by `(card_family_id, reference)`.
pub fn load_nonunique_prints(
    cardsdata_root: &Path,
    set: &str,
) -> Result<Vec<NonUniquePrintRecord>> {
    let csv_root = cardsdata_root.join("data").join("csv");

    let card_types: BTreeMap<i64, String> = read_all::<CardFamilyTypeRow>(
        &csv_root.join("CardFamilies.csv"),
    )?
    .into_iter()
    .map(|r| (r.id, r.card_type_reference))
    .collect();

    let mut subtypes_by_family: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for row in read_all::<CardFamilySubTypeRow>(&csv_root.join("CardFamilySubTypes.csv"))? {
        let norm = row.card_sub_type_reference.trim().to_ascii_uppercase();
        if !norm.is_empty() {
            let list = subtypes_by_family.entry(row.card_family_id).or_default();
            if !list.contains(&norm) {
                list.push(norm);
            }
        }
    }

    let artists: BTreeMap<i64, String> =
        read_optional_all::<ArtistCsvRow>(&csv_root.join("Artists.csv"))?
            .into_iter()
            .map(|r| (r.id, r.nick_name))
            .collect();

    let sets: BTreeMap<String, String> =
        read_optional_all::<SetCsvRow>(&csv_root.join("Sets.csv"))?
            .into_iter()
            .map(|r| (r.reference, r.name_en_us))
            .collect();

    let keywords_map: BTreeMap<String, KeywordCsvRow> =
        read_optional_all::<KeywordCsvRow>(&csv_root.join("Keywords.csv"))?
            .into_iter()
            .map(|r| (r.reference.clone(), r))
            .collect();

    let effect_texts: BTreeMap<i64, EffectFragmentTextRow> =
        read_all::<EffectFragmentTextRow>(&csv_root.join("EffectFragments.csv"))?
            .into_iter()
            .map(|r| (r.id_gd, r))
            .collect();

    let set_dir = csv_root.join("NonUnique").join(set);
    let mut effect_rows_by_print: BTreeMap<i64, Vec<PrintEffectFragmentRow>> = BTreeMap::new();
    for row in read_all::<PrintEffectFragmentRow>(&set_dir.join("PrintEffectFragments.csv"))? {
        effect_rows_by_print
            .entry(row.print_id)
            .or_default()
            .push(row);
    }
    for rows in effect_rows_by_print.values_mut() {
        rows.sort_by_key(|r| r.display_index);
    }

    let path = set_dir.join("NonUniquePrints.csv");
    let rows: Vec<NonUniquePrintRow> = read_all(&path)?;

    let mut out: Vec<NonUniquePrintRecord> = rows
        .into_iter()
        .filter(|row| !row.reference.contains("FOILER"))
        .map(|row| {
            let effects = effect_rows_by_print
                .get(&row.id)
                .map(|r| build_print_effects(r, &effect_texts, &keywords_map))
                .unwrap_or_default();
            let main_cost = parse_optional_u8(&row.main_cost);
            let recall_cost = parse_optional_u8(&row.recall_cost);
            let mountain_power = parse_optional_u8(&row.mountain_power);
            let ocean_power = parse_optional_u8(&row.ocean_power);
            let forest_power = parse_optional_u8(&row.forest_power);
            let permanent = parse_optional_u8(&row.permanent);
            let reserve = parse_optional_u8(&row.reserve);
            let artist = row
                .artist_id
                .and_then(|id| artists.get(&id).cloned())
                .unwrap_or_default();
            let set_name = sets
                .get(&row.set_reference)
                .cloned()
                .unwrap_or_else(|| row.set_reference.clone());
            let set_code = crate::set_code(&row.set_reference).map(str::to_string);
            let collector_number = {
                let s = row.collector_number_formatted.trim();
                if s.is_empty() {
                    None
                } else {
                    Some(s.to_string())
                }
            };

            NonUniquePrintRecord {
                card_type: card_types.get(&row.card_family_id).cloned(),
                subtypes: subtypes_by_family
                    .get(&row.card_family_id)
                    .cloned()
                    .unwrap_or_default(),
                keywords: effects.keywords,
                main_effect: effects.main_effect,
                echo_effect: effects.echo_effect,
                effect_text: effects.combined_text,
                set_name,
                set_code,
                set_reference: row.set_reference.clone(),
                faction_reference: row.faction_reference.clone(),
                rarity_reference: row.rarity_reference.clone(),
                product_reference: row.product_reference.clone(),
                artist,
                collector_number,
                main_cost,
                recall_cost,
                mountain_power,
                ocean_power,
                forest_power,
                permanent,
                reserve,
                is_banned: row.is_banned != 0,
                is_errated: row.is_errated != 0,
                is_suspended: row.is_suspended != 0,
                reference: row.reference,
                card_family_id: row.card_family_id,
                fields: CompactNonUniqueFields {
                    faction_code: crate::compact::faction_code_from_reference(
                        &row.faction_reference,
                    ),
                    rarity_code: rarity_code_from_reference(&row.rarity_reference),
                    product_code: product_code_from_reference(&row.product_reference),
                    is_serialized: row.is_serialized != 0,
                    main_cost: main_cost.unwrap_or(0),
                    recall_cost: recall_cost.unwrap_or(0),
                    mountain_power: mountain_power.unwrap_or(0),
                    ocean_power: ocean_power.unwrap_or(0),
                    forest_power: forest_power.unwrap_or(0),
                },
            }
        })
        .collect();

    out.sort_by(|a, b| {
        (a.card_family_id, &a.reference).cmp(&(b.card_family_id, &b.reference))
    });
    Ok(out)
}

#[derive(Default)]
struct BuiltPrintEffects {
    main_effect: BTreeMap<String, String>,
    echo_effect: BTreeMap<String, String>,
    combined_text: BTreeMap<String, String>,
    keywords: Vec<String>,
}

fn fragment_text_for_locale<'a>(frag: &'a EffectFragmentTextRow, locale: &str) -> &'a str {
    match locale {
        "fr_FR" => &frag.text_fr_fr,
        "es_ES" => &frag.text_es_es,
        "de_DE" => &frag.text_de_de,
        "it_IT" => &frag.text_it_it,
        _ => &frag.text_en_us,
    }
}

fn build_print_effects(
    rows: &[PrintEffectFragmentRow],
    texts: &BTreeMap<i64, EffectFragmentTextRow>,
    keywords_map: &BTreeMap<String, KeywordCsvRow>,
) -> BuiltPrintEffects {
    let mut main_lines: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut echo_lines: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut combined_parts: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut kw_set: BTreeSet<String> = BTreeSet::new();

    for row in rows {
        let trigger_slot = row
            .trigger_id
            .and_then(|id| texts.get(&id))
            .map(|t| t.slot.as_str());

        let ids: Vec<i64> = [row.trigger_id, row.condition_id, row.effect_id]
            .into_iter()
            .flatten()
            .collect();

        // Check whether this row is an echo (support) ability: either explicit Trigger.Slot
        // == ECHO_EFFECT, or an Effect starting with `{D}` / `#{D}`.
        let is_support_by_text = ids.iter().any(|id| {
            texts.get(id).is_some_and(|f| {
                let s = f.text_en_us.trim_start();
                s.starts_with("{D}") || s.starts_with("#{D}")
            })
        });
        let is_echo =
            trigger_slot == Some("ECHO_EFFECT") || (trigger_slot.is_none() && is_support_by_text);
        if is_echo {
            kw_set.insert("ECHO".to_string());
        }

        for locale in LOCALES {
            let mut row_parts = Vec::new();
            for id in &ids {
                let Some(frag) = texts.get(id) else {
                    continue;
                };
                if !frag.keyword_reference.is_empty() {
                    if let Some(canon) = canonical_keyword(&frag.keyword_reference) {
                        kw_set.insert(canon);
                    }
                }
                let raw_t = fragment_text_for_locale(frag, locale);
                if raw_t.is_empty() {
                    continue;
                }
                if locale == "en_US" {
                    extract_bracket_keywords(raw_t, &mut kw_set);
                }
                let rendered = substitute_keywords(raw_t, locale, keywords_map);
                combined_parts.entry(locale).or_default().push(rendered.clone());
                row_parts.push(rendered);
            }
            if !row_parts.is_empty() {
                let joined = row_parts.join(" ");
                if is_echo {
                    echo_lines.entry(locale).or_default().push(joined);
                } else {
                    main_lines.entry(locale).or_default().push(joined);
                }
            }
        }
    }

    BuiltPrintEffects {
        main_effect: main_lines
            .into_iter()
            .map(|(l, lines)| (l.to_string(), lines.join("\n")))
            .collect(),
        echo_effect: echo_lines
            .into_iter()
            .map(|(l, lines)| (l.to_string(), lines.join("\n")))
            .collect(),
        combined_text: combined_parts
            .into_iter()
            .map(|(l, parts)| (l.to_string(), parts.join(" ")))
            .collect(),
        keywords: kw_set.into_iter().collect(),
    }
}

fn load_card_types_localized(
    csv_root: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
    let rows = read_optional_all::<CardTypeCsvRow>(&csv_root.join("CardTypes.csv"))?;
    let mut out = BTreeMap::new();
    for row in rows {
        let mut names = BTreeMap::new();
        for (locale, val) in [
            ("en_US", &row.name_en_us),
            ("fr_FR", &row.name_fr_fr),
            ("es_ES", &row.name_es_es),
            ("de_DE", &row.name_de_de),
            ("it_IT", &row.name_it_it),
        ] {
            if !val.is_empty() {
                names.insert(locale.to_string(), val.clone());
            }
        }
        out.insert(row.reference, names);
    }
    Ok(out)
}

// --- Build orchestration ---

#[derive(Debug, serde::Serialize)]
pub struct NonUniqueManifest {
    pub version: u32,
    pub set: String,
    pub built_at_secs: u64,
    pub card_count: u32,
    /// Present only for a multi-set build — see `build_nonunique_index`'s doc comment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_sets: Option<Vec<String>>,
}

pub struct NonUniqueBuildSummary {
    pub output_dir: PathBuf,
    pub cards_indexed: u32,
}

/// Reads a CardsData checkout's `NonUnique/<set>/NonUniquePrints.csv` for one or more sets,
/// builds the catalog + bitmaps + metadata (`prints.json`, `card_types.json`), and writes
/// everything under `nonunique/`.
pub fn build_nonunique_index(
    cardsdata_root: &Path,
    sets: &[String],
    out: &Path,
) -> Result<NonUniqueBuildSummary> {
    if sets.is_empty() {
        anyhow::bail!("build_nonunique_index requires at least one set");
    }

    let mut records = Vec::new();
    for set in sets {
        records.extend(load_nonunique_prints(cardsdata_root, set)?);
    }
    records.sort_by(|a, b| (a.card_family_id, &a.reference).cmp(&(b.card_family_id, &b.reference)));

    let merged_set_name = if sets.len() == 1 {
        sets[0].clone()
    } else {
        out.file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .with_context(|| "--out must end with a folder name for a multi-set build")?
    };

    let mut catalog_builder = NonUniqueCatalogBuilder::new();
    let mut index_builder = NonUniqueIndexBuilder::new();
    let mut compact_records = Vec::with_capacity(records.len());
    let mut effect_texts = Vec::with_capacity(records.len());
    let mut print_metas = Vec::with_capacity(records.len());

    for record in &records {
        let print_index = catalog_builder.push(record.reference.clone());
        index_builder.insert_record(print_index, record);
        compact_records.push(record.fields);
        effect_texts.push(record.effect_text.clone());
        print_metas.push(NonUniquePrintMeta::from(record));
    }

    let catalog = catalog_builder.into_catalog(&merged_set_name);
    let set_out = if sets.len() == 1 {
        out.join(&sets[0])
    } else {
        out.to_path_buf()
    };
    let nonunique_out = set_out.join("nonunique");
    fs::create_dir_all(&nonunique_out)
        .with_context(|| format!("create {}", nonunique_out.display()))?;

    catalog.save(&nonunique_out.join("catalog.json"))?;
    write_compact_records(&nonunique_out.join("cards.bin"), &compact_records)?;
    index_builder.into_index().write_dir(&set_out)?;

    let effect_text_json = serde_json::to_string(&effect_texts)?;
    fs::write(nonunique_out.join("effect_text.json"), effect_text_json)
        .with_context(|| format!("write {}", nonunique_out.join("effect_text.json").display()))?;

    let prints_json = serde_json::to_string(&print_metas)?;
    fs::write(nonunique_out.join("prints.json"), prints_json)
        .with_context(|| format!("write {}", nonunique_out.join("prints.json").display()))?;

    let card_types_map = load_card_types_localized(&cardsdata_root.join("data").join("csv"))?;
    let card_types_json = serde_json::to_string(&card_types_map)?;
    fs::write(nonunique_out.join("card_types.json"), card_types_json)
        .with_context(|| format!("write {}", nonunique_out.join("card_types.json").display()))?;

    let built_at_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let manifest = NonUniqueManifest {
        version: 1,
        set: merged_set_name,
        built_at_secs,
        card_count: catalog.total_cards(),
        source_sets: (sets.len() > 1).then(|| sets.to_vec()),
    };
    let manifest_text = serde_json::to_string_pretty(&manifest)?;
    fs::write(nonunique_out.join("manifest.json"), manifest_text)?;

    crate::family_catalog::build_family_catalog(cardsdata_root)?
        .save(&set_out.join("families.json"))?;
    crate::artist_credits::write_artist_credits(cardsdata_root, &set_out)?;

    Ok(NonUniqueBuildSummary {
        output_dir: nonunique_out,
        cards_indexed: catalog.total_cards(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::TempDir;

    fn read_roar(path: &Path) -> RoaringBitmap {
        let bytes = fs::read(path).expect("read roar file");
        RoaringBitmap::deserialize_from(Cursor::new(bytes)).expect("decode roar bitmap")
    }

    #[test]
    fn canonical_keyword_maps_variants() {
        assert_eq!(canonical_keyword("FLEETING_CHAR").as_deref(), Some("FLEETING"));
        assert_eq!(canonical_keyword("RESUPPLY_INF").as_deref(), Some("RESUPPLY"));
        assert_eq!(canonical_keyword("EXHAUSTED_RESUPPLY").as_deref(), Some("RESUPPLY"));
        assert_eq!(canonical_keyword("COOLDOWN_2").as_deref(), Some("COOLDOWN"));
        assert_eq!(canonical_keyword("BB"), None);
        assert_eq!(canonical_keyword(""), None);
    }

    #[test]
    fn build_nonunique_index_handles_ne_nullable_stats_zero_power_and_effects() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("cardsdata");
        let csv_root = root.join("data").join("csv");
        let set_dir = csv_root.join("NonUnique").join("CORE");
        fs::create_dir_all(&set_dir).unwrap();

        fs::write(
            csv_root.join("Sets.csv"),
            "Reference,Name_en_US,Name_fr_FR,Name_es_ES,Name_de_DE,Name_it_IT\nCORE,Beyond the Gates,Au-dela des Portes,,,\n",
        )
        .unwrap();
        fs::write(
            csv_root.join("Artists.csv"),
            "Id,NickName\n7,Alice Artist\n",
        )
        .unwrap();
        fs::write(
            csv_root.join("CardTypes.csv"),
            "Reference,Name_en_US,Name_fr_FR,Name_es_ES,Name_de_DE,Name_it_IT\nHERO,Hero,Heros,Heroe,Held,Eroe\nCHARACTER,Character,Personnage,Personaje,Charakter,Personaggio\n",
        )
        .unwrap();
        fs::write(
            csv_root.join("CardFamilies.csv"),
            "Id,Reference,CardTypeReference,Name_en_US,Name_fr_FR,Name_es_ES,Name_de_DE,Name_it_IT\n10,FAM_HERO,HERO,Hero Name,Nom Heros,,,\n20,FAM_CHAR,CHARACTER,Zero Guy,Homme Zero,,,\n30,FAM_NE,CHARACTER,Neutral Token,Jeton Neutre,,,\n",
        )
        .unwrap();
        fs::write(
            csv_root.join("CardFamilySubTypes.csv"),
            "CardFamilyId,CardSubTypeReference\n20,HUMANKIND\n",
        )
        .unwrap();
        fs::write(
            csv_root.join("Keywords.csv"),
            "Reference,DisplayWeb_en_US,DisplayWeb_fr_FR,DisplayWeb_es_ES,DisplayWeb_de_DE,DisplayWeb_it_IT\nFLEETING,Fleeting,Fugace,,,\n",
        )
        .unwrap();
        fs::write(
            csv_root.join("EffectFragments.csv"),
            "IdGd,KeywordReference,Slot,Text_en_US,Text_fr_FR,Text_es_ES,Text_de_DE,Text_it_IT\n101,FLEETING_CHAR,,[FLEETING].,[FLEETING].,,,\n102,,,{D} Support text.,{D} Texte soutien.,,,\n",
        )
        .unwrap();
        fs::write(
            csv_root.join("Factions.csv"),
            "Reference,Name_en_US\nAX,Axiom\nNE,Neutral\n",
        )
        .unwrap();
        fs::write(
            csv_root.join("CardSubTypes.csv"),
            "Reference,Name_en_US\nHUMANKIND,Human\n",
        )
        .unwrap();

        fs::write(
            set_dir.join("NonUniquePrints.csv"),
            "Id,Reference,CardFamilyId,SetReference,FactionReference,RarityReference,ProductReference,IsSerialized,ArtistId,CollectorNumberFormatted,MAIN_COST,RECALL_COST,MOUNTAIN_POWER,OCEAN_POWER,FOREST_POWER,PERMANENT,RESERVE,IsBanned,IsErrated,IsSuspended\n\
1,ALT_CORE_B_AX_01_C,10,CORE,AX,C,B,0,7,BTG-001-C,,,,, ,2,,0,0,0\n\
2,ALT_CORE_B_AX_02_R,20,CORE,AX,R,B,0,7,BTG-002-R,2,2,0,3,0,,,0,1,0\n\
3,ALT_CORE_B_NE_03_C,30,CORE,NE,C,P,0,,BTG-003-C,0,0,0,0,0,,,0,0,0\n",
        )
        .unwrap();
        fs::write(
            set_dir.join("PrintEffectFragments.csv"),
            "PrintId,EffectId,DisplayIndex,TriggerId,ConditionId\n\
2,101,1,,\n\
2,102,2,,\n",
        )
        .unwrap();

        let out_dir = tmp.path().join("out");
        let summary = build_nonunique_index(
            &root,
            &["CORE".to_string()],
            &out_dir,
        )
        .expect("build_nonunique_index succeeds");
        assert_eq!(summary.cards_indexed, 3);

        let nu_dir = out_dir.join("CORE").join("nonunique");

        // Faction NE must be preserved in its own bitmap.
        let ne_bmp = read_roar(&nu_dir.join("factions/NE.roar"));
        assert_eq!(ne_bmp.len(), 1);
        assert!(ne_bmp.contains(2));

        // Print 0 (HERO) has blank stats -> NOT in bucket 00!
        // Print 1 (zero_guy) has Mountain=0, Forest=0 -> IN bucket 00!
        // Print 2 (NE token) has MainCost=0, Mountain=0 -> IN bucket 00!
        let main_00 = read_roar(&nu_dir.join("stats/main_cost/00.roar"));
        assert!(!main_00.contains(0), "Hero blank mainCost must not pollute bucket 00");
        assert!(main_00.contains(2));
        let forest_00 = read_roar(&nu_dir.join("stats/forest_power/00.roar"));
        assert!(!forest_00.contains(0), "Hero blank forestPower must not pollute bucket 00");
        assert!(forest_00.contains(1));

        // Hero & NE token have no effects; Character has FLEETING + echo split.
        let has_no_eff = read_roar(&nu_dir.join("has_no_effect.roar"));
        assert!(has_no_eff.contains(0));
        assert!(!has_no_eff.contains(1));
        assert!(has_no_eff.contains(2));

        let prints_raw = fs::read_to_string(nu_dir.join("prints.json")).unwrap();
        let prints: Vec<NonUniquePrintMeta> = serde_json::from_str(&prints_raw).unwrap();
        assert_eq!(prints.len(), 3);
        assert_eq!(prints[0].main_cost, None);
        assert_eq!(prints[0].permanent, Some(2));
        assert_eq!(prints[1].forest_power, Some(0));
        assert_eq!(prints[1].main_effect.get("en_US").map(String::as_str), Some("Fleeting."));
        assert_eq!(prints[1].echo_effect.get("fr_FR").map(String::as_str), Some("{D} Texte soutien."));
        assert_eq!(prints[1].keywords, vec!["ECHO".to_string(), "FLEETING".to_string()]);
        assert_eq!(prints[2].faction, "NE");
        assert!(prints[2].is_banned == false && prints[1].is_errated == true);

        let card_types_raw = fs::read_to_string(nu_dir.join("card_types.json")).unwrap();
        let card_types: BTreeMap<String, BTreeMap<String, String>> =
            serde_json::from_str(&card_types_raw).unwrap();
        assert_eq!(
            card_types.get("HERO").and_then(|m| m.get("fr_FR")).map(String::as_str),
            Some("Heros")
        );
    }
}
