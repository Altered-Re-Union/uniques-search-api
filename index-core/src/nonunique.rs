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
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const VALUE_BUCKETS: usize = 16;
const STAT_FIELDS: [(&str, fn(&CompactNonUniqueFields) -> u8); 5] = [
    ("main_cost", |f| f.main_cost),
    ("recall_cost", |f| f.recall_cost),
    ("mountain_power", |f| f.mountain_power),
    ("ocean_power", |f| f.ocean_power),
    ("forest_power", |f| f.forest_power),
];

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
    faction: BTreeMap<u8, RoaringBitmap>,
    rarity: BTreeMap<u8, RoaringBitmap>,
    product: BTreeMap<u8, RoaringBitmap>,
    serialized: RoaringBitmap,
    stats: BTreeMap<&'static str, [RoaringBitmap; VALUE_BUCKETS]>,
    set: BTreeMap<String, RoaringBitmap>,
    card_type: BTreeMap<String, RoaringBitmap>,
    subtype: BTreeMap<String, RoaringBitmap>,
    family: BTreeMap<String, RoaringBitmap>,
    banned: RoaringBitmap,
    errated: RoaringBitmap,
    suspended: RoaringBitmap,
}

impl NonUniqueIndexBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &mut self,
        print_index: u32,
        fields: &CompactNonUniqueFields,
        set_reference: &str,
        card_type: Option<&str>,
        subtypes: &[String],
        card_family_id: i64,
        is_banned: bool,
        is_errated: bool,
        is_suspended: bool,
    ) {
        if fields.faction_code != 0 {
            self.faction
                .entry(fields.faction_code)
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
        for (name, get) in STAT_FIELDS {
            let value = get(fields) as usize;
            if value >= VALUE_BUCKETS {
                continue;
            }
            self.stats.entry(name).or_insert_with(empty_buckets)[value].insert(print_index);
        }

        if !set_reference.is_empty() {
            self.set
                .entry(set_reference.to_string())
                .or_default()
                .insert(print_index);
        }
        if let Some(card_type) = card_type {
            self.card_type
                .entry(card_type.to_string())
                .or_default()
                .insert(print_index);
        }
        for subtype in subtypes {
            self.subtype
                .entry(subtype.clone())
                .or_default()
                .insert(print_index);
        }
        self.family
            .entry(card_family_id.to_string())
            .or_default()
            .insert(print_index);
        if is_banned {
            self.banned.insert(print_index);
        }
        if is_errated {
            self.errated.insert(print_index);
        }
        if is_suspended {
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
            banned: self.banned,
            errated: self.errated,
            suspended: self.suspended,
        }
    }
}

pub struct NonUniqueIndex {
    faction: BTreeMap<u8, RoaringBitmap>,
    rarity: BTreeMap<u8, RoaringBitmap>,
    product: BTreeMap<u8, RoaringBitmap>,
    serialized: RoaringBitmap,
    stats: BTreeMap<&'static str, [RoaringBitmap; VALUE_BUCKETS]>,
    set: BTreeMap<String, RoaringBitmap>,
    card_type: BTreeMap<String, RoaringBitmap>,
    subtype: BTreeMap<String, RoaringBitmap>,
    family: BTreeMap<String, RoaringBitmap>,
    banned: RoaringBitmap,
    errated: RoaringBitmap,
    suspended: RoaringBitmap,
}

impl NonUniqueIndex {
    /// Writes `<root>/nonunique/{factions,rarity,product,stats,set,type,subtype,family}/...` and
    /// `<root>/nonunique/{serialized,status/{banned,errated,suspended}}.roar`.
    pub fn write_dir(&self, root: &Path) -> Result<()> {
        let nonunique_root = root.join("nonunique");
        fs::create_dir_all(&nonunique_root)?;

        write_coded_bitmaps(
            &nonunique_root.join("factions"),
            &self.faction,
            crate::compact::faction_reference_from_code,
        )?;
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

        write_single_bitmap(&nonunique_root.join("serialized.roar"), &self.serialized)?;

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
    #[serde(rename = "IsSerialized")]
    is_serialized: u8,
    #[serde(rename = "IsBanned")]
    is_banned: u8,
    #[serde(rename = "IsErrated")]
    is_errated: u8,
    #[serde(rename = "IsSuspended")]
    is_suspended: u8,
    // Raw strings: blank on rows whose card type doesn't carry this stat (e.g. a
    // LANDMARK_PERMANENT row leaves these blank). Parsed tolerantly below, same as the unique
    // adapter does via `compact_fields_from_card`.
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
/// `UniquePrintEffectFragments.csv`, joined via the print's numeric `Id` (not `Reference`). Ids can
/// be negative (freeform, undecomposed text — see D6); that's fine here, we only need the text
/// each id resolves to, not a shared/stable id space.
#[derive(Debug, Deserialize)]
struct PrintEffectFragmentRow {
    #[serde(rename = "PrintId")]
    print_id: i64,
    #[serde(rename = "TriggerId", deserialize_with = "csv::invalid_option")]
    trigger_id: Option<i64>,
    #[serde(rename = "ConditionId", deserialize_with = "csv::invalid_option")]
    condition_id: Option<i64>,
    #[serde(rename = "EffectId", deserialize_with = "csv::invalid_option")]
    effect_id: Option<i64>,
}

/// Just the text this slice needs from the shared `EffectFragments.csv` referential — `IdGd` can
/// be negative here (freeform per-print text) as well as the shared positive ids uniques use.
#[derive(Debug, Deserialize)]
struct EffectFragmentTextRow {
    #[serde(rename = "IdGd")]
    id_gd: i64,
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
    /// From `CardFamilies.CardTypeReference`, joined via `card_family_id` — `None` if the family
    /// id isn't found in `CardFamilies.csv` (shouldn't happen, but this is CSV data, not a type
    /// system: better to drop the type facet for that one print than fail the whole build).
    pub card_type: Option<String>,
    pub subtypes: Vec<String>,
    pub is_banned: bool,
    pub is_errated: bool,
    pub is_suspended: bool,
    /// Locale -> every fragment's text for this print, space-joined — interim full-text search
    /// substrate (see D6/plan 24), not a display field. Empty map if the print has no effect rows.
    pub effect_text: BTreeMap<String, String>,
}

/// Reads `<cardsdata_root>/data/csv/NonUnique/<set>/NonUniquePrints.csv` plus the shared
/// `CardFamilies.csv` / `CardFamilySubTypes.csv` referentials (joined via `CardFamilyId`, same
/// join `cardsdata.rs` already does for uniques — duplicated here rather than shared, per D10: two
/// small independent readers beat forcing one abstraction across record types that don't fully
/// overlap). Skips the per-set `FOILER` placeholder row (same artifact as on the unique side, see
/// D9), and sorts by `(card_family_id, reference)` — keeps a family's prints adjacent as free
/// groundwork for Lot 5, though nothing in this slice reads family boundaries yet.
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
        subtypes_by_family
            .entry(row.card_family_id)
            .or_default()
            .push(row.card_sub_type_reference);
    }

    let effect_texts: BTreeMap<i64, EffectFragmentTextRow> =
        read_all::<EffectFragmentTextRow>(&csv_root.join("EffectFragments.csv"))?
            .into_iter()
            .map(|r| (r.id_gd, r))
            .collect();

    let set_dir = csv_root.join("NonUnique").join(set);
    let mut effect_ids_by_print: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for row in read_all::<PrintEffectFragmentRow>(&set_dir.join("PrintEffectFragments.csv"))? {
        let entry = effect_ids_by_print.entry(row.print_id).or_default();
        entry.extend([row.trigger_id, row.condition_id, row.effect_id].into_iter().flatten());
    }

    let path = set_dir.join("NonUniquePrints.csv");
    let rows: Vec<NonUniquePrintRow> = read_all(&path)?;

    let mut out: Vec<NonUniquePrintRecord> = rows
        .into_iter()
        .filter(|row| !row.reference.contains("FOILER"))
        .map(|row| {
            let parse_stat = |s: &str| s.parse::<u8>().unwrap_or(0);
            let effect_text = effect_ids_by_print
                .get(&row.id)
                .map(|ids| build_effect_text(ids, &effect_texts))
                .unwrap_or_default();
            NonUniquePrintRecord {
                card_type: card_types.get(&row.card_family_id).cloned(),
                subtypes: subtypes_by_family
                    .get(&row.card_family_id)
                    .cloned()
                    .unwrap_or_default(),
                set_reference: row.set_reference.clone(),
                is_banned: row.is_banned != 0,
                is_errated: row.is_errated != 0,
                is_suspended: row.is_suspended != 0,
                effect_text,
                reference: row.reference,
                card_family_id: row.card_family_id,
                fields: CompactNonUniqueFields {
                    faction_code: crate::compact::faction_code_from_reference(
                        &row.faction_reference,
                    ),
                    rarity_code: rarity_code_from_reference(&row.rarity_reference),
                    product_code: product_code_from_reference(&row.product_reference),
                    is_serialized: row.is_serialized != 0,
                    main_cost: parse_stat(&row.main_cost),
                    recall_cost: parse_stat(&row.recall_cost),
                    mountain_power: parse_stat(&row.mountain_power),
                    ocean_power: parse_stat(&row.ocean_power),
                    forest_power: parse_stat(&row.forest_power),
                },
            }
        })
        .collect();

    out.sort_by(|a, b| {
        (a.card_family_id, &a.reference).cmp(&(b.card_family_id, &b.reference))
    });
    Ok(out)
}

/// Concatenates (space-joined) every fragment's text, per locale, for one print's effect ids —
/// works the same whether an id is a shared positive template or a freeform negative one (D6),
/// since this only needs the *text* an id resolves to, not a stable/shared id space.
fn build_effect_text(
    ids: &[i64],
    texts: &BTreeMap<i64, EffectFragmentTextRow>,
) -> BTreeMap<String, String> {
    let mut by_locale: BTreeMap<&'static str, Vec<&str>> = BTreeMap::new();
    for id in ids {
        let Some(fragment) = texts.get(id) else {
            continue;
        };
        for (locale, text) in [
            ("en_US", &fragment.text_en_us),
            ("fr_FR", &fragment.text_fr_fr),
            ("es_ES", &fragment.text_es_es),
            ("de_DE", &fragment.text_de_de),
            ("it_IT", &fragment.text_it_it),
        ] {
            if !text.is_empty() {
                by_locale.entry(locale).or_default().push(text);
            }
        }
    }
    by_locale
        .into_iter()
        .map(|(locale, parts)| (locale.to_string(), parts.join(" ")))
        .collect()
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
/// builds the catalog + bitmaps, and writes everything under `nonunique/` —
/// `{catalog.json,cards.bin,manifest.json,factions,rarity,product,stats,set,type,subtype,family,
/// serialized.roar,status/...}`.
///
/// **Single set** (`sets.len() == 1`, the common case): writes to `<out>/<set>/nonunique/`, so it
/// never collides with the unique index's own `catalog.json`/`cards.bin`/`manifest.json` when both
/// are built into the same `<out>/<set>/` directory (needed once the HTTP API loads both from one
/// index root — see Lot 2 / `uniques-http-api/plans/19-merged-search.md`).
///
/// **Multiple sets**: unlike uniques (`merge_indexes` in `merge.rs`), this doesn't merge already-
/// built binary indexes — a non-unique print isn't a numbered instance within a bit-span the way a
/// unique is (see this module's top doc comment), it's just one more row in a flat, sorted list. So
/// "merging" sets is exactly the same operation as building one, just reading more CSVs first and
/// sorting the concatenated result — no remapping, no overlap-group logic. Writes directly to
/// `<out>/nonunique/` (`out`'s own folder name becomes the merged set name, same convention
/// `merge_indexes` uses for `--out`), and every print naturally gets its own `SetReference` in the
/// `set` bitmap dimension already built per print — this is what actually makes `edition` a useful
/// filter once more than one set is loaded together (see plan 22/D16's caveat).
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

    for record in &records {
        let print_index = catalog_builder.push(record.reference.clone());
        index_builder.insert(
            print_index,
            &record.fields,
            &record.set_reference,
            record.card_type.as_deref(),
            &record.subtypes,
            record.card_family_id,
            record.is_banned,
            record.is_errated,
            record.is_suspended,
        );
        compact_records.push(record.fields);
        effect_texts.push(record.effect_text.clone());
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

    // Raw (unnormalized) per-locale effect text, one entry per `print_index`, parallel to
    // `catalog.json`'s reference list — the HTTP loader normalizes at load time, same pattern
    // `catalog.json`'s family names already use for unique name search.
    let effect_text_json = serde_json::to_string(&effect_texts)?;
    fs::write(nonunique_out.join("effect_text.json"), effect_text_json)
        .with_context(|| format!("write {}", nonunique_out.join("effect_text.json").display()))?;

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

    // Global (not per-set), but written alongside this build's own files so the HTTP server finds
    // it via the same storage root — see plan 23. Idempotent: harmless if `build` (the unique
    // side) already wrote the same content here.
    crate::family_catalog::build_family_catalog(cardsdata_root)?
        .save(&set_out.join("families.json"))?;

    Ok(NonUniqueBuildSummary {
        output_dir: nonunique_out,
        cards_indexed: catalog.total_cards(),
    })
}
