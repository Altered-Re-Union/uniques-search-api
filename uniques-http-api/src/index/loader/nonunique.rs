//! Loads the standalone non-unique index (see `index-core/src/nonunique.rs`) from the same
//! [`IndexStorage`] root as the unique index, if present. Best-effort: an index directory built
//! before Lot 2 simply has no `nonunique/` files, and callers get `Ok(None)`.

use std::collections::BTreeMap;

use anyhow::Result;
use index_core::nonunique::{CompactNonUniqueFields, CompactNonUniqueView, NonUniqueCatalog};
use roaring::RoaringBitmap;

use super::normalize_name_for_search;
use super::storage::{read_json, read_roar, IndexStorage};

const FACTIONS: [&str; 6] = ["AX", "BR", "LY", "MU", "OR", "YZ"];
const RARITIES: [&str; 3] = ["C", "R", "E"];
const PRODUCTS: [&str; 3] = ["B", "A", "P"];
const STAT_FIELDS: [&str; 5] = [
    "main_cost",
    "recall_cost",
    "mountain_power",
    "ocean_power",
    "forest_power",
];

pub struct NonUniqueQueryIndex {
    pub catalog: NonUniqueCatalog,
    cards: Vec<u8>,
    pub faction: BTreeMap<String, RoaringBitmap>,
    pub rarity: BTreeMap<String, RoaringBitmap>,
    pub product: BTreeMap<String, RoaringBitmap>,
    pub serialized: Option<RoaringBitmap>,
    pub stats: BTreeMap<String, BTreeMap<u8, RoaringBitmap>>,
    /// Open-ended (a new set release adds values) — see `set`/`type`/`subtype` in
    /// `nonunique.rs`'s `write_named_bitmaps` doc comment for why this needs `_index.json`
    /// instead of the fixed-code-space `has_file` probing used for faction/rarity/product.
    pub set: BTreeMap<String, RoaringBitmap>,
    pub card_type: BTreeMap<String, RoaringBitmap>,
    pub subtype: BTreeMap<String, RoaringBitmap>,
    /// Keyed by `CardFamilyId` formatted as a string (open-ended, same `_index.json` mechanism as
    /// set/type/subtype) — see plan 23.
    pub family: BTreeMap<String, RoaringBitmap>,
    pub banned: RoaringBitmap,
    pub errated: RoaringBitmap,
    pub suspended: RoaringBitmap,
    /// Normalized (lowercased, accent-folded) per-print, per-locale effect text — interim
    /// full-text search substrate (see D6/plan 24). One entry per `print_index`, empty `Vec` for a
    /// print with no effect rows. Not persisted normalized — raw text is normalized once here at
    /// load time, same pattern `NameSearchIndex` already uses for unique family names.
    effect_text_search: Vec<Vec<String>>,
}

impl NonUniqueQueryIndex {
    pub fn fields_for(&self, print_index: u32) -> Option<CompactNonUniqueFields> {
        CompactNonUniqueView::from_data(&self.cards, print_index).map(|v| v.fields())
    }

    pub fn reference_for(&self, print_index: u32) -> Option<&str> {
        self.catalog.reference_for_index(print_index)
    }

    pub fn total(&self) -> u32 {
        self.catalog.total_cards()
    }

    /// Prints whose effect text contains `query` in any locale (case/accent-insensitive) — same
    /// matching rule `NameSearchIndex::bitmap_for_contains` uses for unique names.
    pub fn bitmap_for_effect_contains(&self, query: &str) -> RoaringBitmap {
        let needle = normalize_name_for_search(query);
        let mut bitmap = RoaringBitmap::new();
        for (print_index, texts) in self.effect_text_search.iter().enumerate() {
            if texts.iter().any(|t| t.contains(&needle)) {
                bitmap.insert(print_index as u32);
            }
        }
        bitmap
    }
}

/// `Ok(None)` when this storage root has no `nonunique/catalog.json` (nothing built yet for this
/// index — not an error).
pub fn load_nonunique_index(storage: &impl IndexStorage) -> Result<Option<NonUniqueQueryIndex>> {
    if !storage.has_file("nonunique/catalog.json") {
        return Ok(None);
    }

    let catalog: NonUniqueCatalog = read_json(storage, "nonunique/catalog.json")?;
    let cards = storage.read_bytes("nonunique/cards.bin")?;

    let faction = load_coded_bitmaps(storage, "nonunique/factions", &FACTIONS)?;
    let rarity = load_coded_bitmaps(storage, "nonunique/rarity", &RARITIES)?;
    let product = load_coded_bitmaps(storage, "nonunique/product", &PRODUCTS)?;

    let serialized_rel = "nonunique/serialized.roar";
    let serialized = storage
        .has_file(serialized_rel)
        .then(|| read_roar(storage, serialized_rel))
        .transpose()?;

    let mut stats = BTreeMap::new();
    for field in STAT_FIELDS {
        let field_dir = format!("nonunique/stats/{field}");
        let mut buckets = BTreeMap::new();
        for value in 0u8..16 {
            let rel = format!("{field_dir}/{value:02}.roar");
            if storage.has_file(&rel) {
                buckets.insert(value, read_roar(storage, &rel)?);
            }
        }
        if !buckets.is_empty() {
            stats.insert(field.to_string(), buckets);
        }
    }

    let set = load_named_bitmaps(storage, "nonunique/set")?;
    let card_type = load_named_bitmaps(storage, "nonunique/type")?;
    let subtype = load_named_bitmaps(storage, "nonunique/subtype")?;
    let family = load_named_bitmaps(storage, "nonunique/family")?;

    let banned = load_status_flag(storage, "nonunique/status/banned.roar")?;
    let errated = load_status_flag(storage, "nonunique/status/errated.roar")?;
    let suspended = load_status_flag(storage, "nonunique/status/suspended.roar")?;

    let effect_text_rel = "nonunique/effect_text.json";
    let effect_text_search = if storage.has_file(effect_text_rel) {
        let raw: Vec<BTreeMap<String, String>> = read_json(storage, effect_text_rel)?;
        raw.into_iter()
            .map(|by_locale| by_locale.into_values().map(|t| normalize_name_for_search(&t)).collect())
            .collect()
    } else {
        Vec::new()
    };

    Ok(Some(NonUniqueQueryIndex {
        catalog,
        cards,
        faction,
        rarity,
        product,
        serialized,
        stats,
        set,
        card_type,
        subtype,
        family,
        banned,
        errated,
        suspended,
        effect_text_search,
    }))
}

fn load_coded_bitmaps(
    storage: &impl IndexStorage,
    dir: &str,
    codes: &[&str],
) -> Result<BTreeMap<String, RoaringBitmap>> {
    let mut out = BTreeMap::new();
    for code in codes {
        let rel = format!("{dir}/{code}.roar");
        if storage.has_file(&rel) {
            out.insert((*code).to_string(), read_roar(storage, &rel)?);
        }
    }
    Ok(out)
}

/// For open-ended value spaces (set/type/subtype): reads `{dir}/_index.json` (the list of names
/// that actually got a bitmap file, written by `nonunique.rs`'s `write_named_bitmaps`) first,
/// rather than guessing candidates — `Ok(empty)` if the index itself is absent (older build, or
/// nothing matched).
fn load_named_bitmaps(
    storage: &impl IndexStorage,
    dir: &str,
) -> Result<BTreeMap<String, RoaringBitmap>> {
    let index_path = format!("{dir}/_index.json");
    if !storage.has_file(&index_path) {
        return Ok(BTreeMap::new());
    }
    let names: Vec<String> = read_json(storage, &index_path)?;
    let mut out = BTreeMap::new();
    for name in names {
        let rel = format!("{dir}/{name}.roar");
        if storage.has_file(&rel) {
            out.insert(name.clone(), read_roar(storage, &rel)?);
        }
    }
    Ok(out)
}

/// `Ok(empty bitmap)` when `relative_path` isn't present.
fn load_status_flag(storage: &impl IndexStorage, relative_path: &str) -> Result<RoaringBitmap> {
    if !storage.has_file(relative_path) {
        return Ok(RoaringBitmap::new());
    }
    read_roar(storage, relative_path)
}
