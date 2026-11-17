//! Loader and queryable in-memory index for non-unique cards (`<out>/nonunique/`).
//!
//! Reads the bitmap index + compact records emitted by `index_core::nonunique::build_nonunique_index`
//! plus optional rich metadata (`prints.json`, `card_types.json`, `keywords/`, `cost_relations/`,
//! `has_no_effect.roar`) so unified search (`/api/v2/search`) and `/api/v2/card/{reference}` can
//! filter, sort, and serialize non-unique prints alongside uniques with zero disk I/O per request.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use index_core::family_catalog::FamilyCatalogEntry;
use index_core::nonunique::{
    CompactNonUniqueFields, CompactNonUniqueView, NonUniqueCatalog, NonUniquePrintMeta,
};
use roaring::RoaringBitmap;

use super::storage::{read_json, read_roar, IndexStorage};
use super::{faction_sort_rank, normalize_name_for_search, set_release_rank};
use crate::index::text_search::{search_key, SearchQuery};

const VALUE_BUCKETS: usize = 16;
const STAT_NAMES: [&str; 5] = [
    "main_cost",
    "recall_cost",
    "mountain_power",
    "ocean_power",
    "forest_power",
];
const FACTION_CODES: [&str; 7] = ["AX", "BR", "LY", "MU", "OR", "YZ", "NE"];

#[derive(Debug, Clone, Default)]
pub struct NonUniqueCostRelationBitmaps {
    pub equal: RoaringBitmap,
    pub main_higher: RoaringBitmap,
    pub recall_higher: RoaringBitmap,
}

#[derive(Debug, Clone, Default)]
pub struct NonUniqueSortIndex {
    pub set_date_asc: Vec<u32>,
    pub set_date_desc: Vec<u32>,
    pub collector_asc: Vec<u32>,
    pub reference_asc: Vec<u32>,
    pub name_asc_by_locale: BTreeMap<String, Vec<u32>>,
}

#[derive(Debug, Clone)]
pub struct NonUniqueQueryIndex {
    pub catalog: NonUniqueCatalog,
    cards: Vec<u8>,
    pub faction: BTreeMap<String, RoaringBitmap>,
    pub rarity: BTreeMap<String, RoaringBitmap>,
    pub product: BTreeMap<String, RoaringBitmap>,
    pub serialized: Option<RoaringBitmap>,
    pub stats: BTreeMap<String, BTreeMap<u8, RoaringBitmap>>,
    /// Per-stat bitmap of prints where the stat is missing (`None` / N/A, e.g. Heroes, Spells,
    /// Landmarks) so numeric stat sorts push NULLS LAST cleanly.
    pub stat_missing: BTreeMap<String, RoaringBitmap>,
    pub set: BTreeMap<String, RoaringBitmap>,
    pub card_type: BTreeMap<String, RoaringBitmap>,
    pub subtype: BTreeMap<String, RoaringBitmap>,
    pub keywords: BTreeMap<String, RoaringBitmap>,
    pub has_no_effect: RoaringBitmap,
    pub cost_relations: NonUniqueCostRelationBitmaps,
    pub family: BTreeMap<String, RoaringBitmap>,
    pub banned: RoaringBitmap,
    pub errated: RoaringBitmap,
    pub suspended: RoaringBitmap,
    /// Full per-print rich metadata (`prints[print_index]`), when `prints.json` is present.
    pub prints: Vec<NonUniquePrintMeta>,
    /// `CardTypeReference` -> localized names (`{"en_US": "Hero", "fr_FR": "Héros", ...}`).
    pub card_types_i18n: BTreeMap<String, BTreeMap<String, String>>,
    /// Reference (uppercased & exact-case insensitive) -> `print_index`.
    pub ref_lookup: HashMap<String, u32>,
    /// Normalized collector number keys (e.g. `BTG-001-C`, `SDU-001-C-EN`, `SDU-001-C`, `BTG-001`)
    /// -> matching `print_index` list.
    pub collector_lookup: HashMap<String, Vec<u32>>,
    /// Effect text per locale for each print, as [`search_key`]s (text search).
    pub effect_search_keys: Vec<BTreeMap<String, String>>,
    /// Normalized localized names per print, used for name sorting (populated via `bind_families`).
    pub normalized_names: Vec<BTreeMap<String, String>>,
    /// Localized names per print, as [`search_key`]s (text search; populated via `bind_families`).
    pub name_search_keys: Vec<BTreeMap<String, String>>,
    /// Pre-sorted `print_index` vectors for global sorting & pagination.
    pub sort_index: NonUniqueSortIndex,
}

impl NonUniqueQueryIndex {
    pub fn fields_for(&self, print_index: u32) -> Option<CompactNonUniqueFields> {
        CompactNonUniqueView::from_data(&self.cards, print_index).map(|v| v.fields())
    }

    pub fn print_for(&self, print_index: u32) -> Option<&NonUniquePrintMeta> {
        self.prints.get(print_index as usize)
    }

    pub fn reference_for(&self, print_index: u32) -> Option<&str> {
        self.catalog.reference_for_index(print_index)
    }

    pub fn total(&self) -> u32 {
        self.catalog.total_cards()
    }

    pub fn resolve_reference(&self, reference: &str) -> Option<u32> {
        self.ref_lookup
            .get(&reference.trim().to_ascii_uppercase())
            .copied()
    }

    /// Recompute localized names and `NonUniqueSortIndex` once `FamilyCatalog` is attached.
    pub fn bind_families(&mut self, families: &BTreeMap<i64, FamilyCatalogEntry>) {
        let total = self.total() as usize;
        let mut norm_names = Vec::with_capacity(total);
        let mut search_keys = Vec::with_capacity(total);

        for i in 0..total {
            let mut per_loc = BTreeMap::new();
            let mut keys_per_loc = BTreeMap::new();
            if let Some(meta) = self.prints.get(i) {
                if let Some(fam) = families.get(&meta.card_family_id) {
                    for (loc, raw) in &fam.name {
                        per_loc.insert(loc.clone(), normalize_name_for_search(raw));
                        keys_per_loc.insert(loc.clone(), search_key(raw));
                    }
                }
            }
            norm_names.push(per_loc);
            search_keys.push(keys_per_loc);
        }

        self.normalized_names = norm_names;
        self.name_search_keys = search_keys;
        self.sort_index =
            build_nonunique_sort_index(&self.catalog, &self.prints, &self.normalized_names, families);
    }

    /// Prints whose effect text, in at least one locale, contains every term of `query` (see
    /// [`SearchQuery`]). A query with nothing searchable in it matches every print.
    pub fn bitmap_for_effect_contains(&self, query: &str) -> RoaringBitmap {
        let Some(q) = SearchQuery::parse(query) else {
            return self.all_prints();
        };
        self.bitmap_where(|i| {
            self.effect_search_keys
                .get(i)
                .is_some_and(|per_loc| per_loc.values().any(|key| q.matches(key)))
        })
    }

    /// Prints whose name, in at least one locale, contains every term of `query`, or whose
    /// reference / collector number contains `query`. A query with nothing searchable in it
    /// matches every print.
    pub fn bitmap_for_name_or_ident(&self, query: &str) -> RoaringBitmap {
        let Some(q) = SearchQuery::parse(query) else {
            return self.all_prints();
        };
        let ident = normalize_name_for_search(query.trim());
        self.bitmap_where(|i| {
            self.name_search_keys
                .get(i)
                .is_some_and(|per_loc| per_loc.values().any(|key| q.matches(key)))
                || self.ident_matches(i, &ident)
        })
    }

    /// Free-text `q`: prints where every term of `query` is found in the name or effect text of
    /// one same locale (terms may be split between name and effect), or whose reference /
    /// collector number contains `query`. A query with nothing searchable in it matches every
    /// print.
    pub fn bitmap_for_free_text(&self, query: &str) -> RoaringBitmap {
        let Some(q) = SearchQuery::parse(query) else {
            return self.all_prints();
        };
        let ident = normalize_name_for_search(query.trim());
        let empty = BTreeMap::new();
        self.bitmap_where(|i| {
            let names = self.name_search_keys.get(i).unwrap_or(&empty);
            let effects = self.effect_search_keys.get(i).unwrap_or(&empty);
            let text_matches = names.keys().chain(effects.keys()).any(|loc| {
                let name = names.get(loc).map_or("", String::as_str);
                let effect = effects.get(loc).map_or("", String::as_str);
                q.matches_any_field(&[name, effect])
            });
            text_matches || self.ident_matches(i, &ident)
        })
    }

    /// Reference or collector number contains `needle` (lowercased, as `normalize_name_for_search`).
    fn ident_matches(&self, i: usize, needle: &str) -> bool {
        if needle.is_empty() {
            return false;
        }
        let ref_matches = self
            .catalog
            .reference_for_index(i as u32)
            .is_some_and(|r| r.to_ascii_lowercase().contains(needle));
        let col_matches = self
            .prints
            .get(i)
            .and_then(|p| p.collector_number.as_deref())
            .is_some_and(|c| c.to_ascii_lowercase().contains(needle));
        ref_matches || col_matches
    }

    fn all_prints(&self) -> RoaringBitmap {
        let mut out = RoaringBitmap::new();
        out.insert_range(0..self.total());
        out
    }

    fn bitmap_where(&self, mut pred: impl FnMut(usize) -> bool) -> RoaringBitmap {
        let mut out = RoaringBitmap::new();
        for i in 0..(self.total() as usize) {
            if pred(i) {
                out.insert(i as u32);
            }
        }
        out
    }
}

pub fn load_nonunique_index(storage: &impl IndexStorage) -> Result<Option<NonUniqueQueryIndex>> {
    if !storage.has_file("nonunique/catalog.json") {
        return Ok(None);
    }
    let catalog: NonUniqueCatalog = read_json(storage, "nonunique/catalog.json")?;
    let cards = storage.read_bytes("nonunique/cards.bin")?;

    let faction = load_named_bitmap_list(storage, "nonunique/factions", &FACTION_CODES)?;
    let rarity = load_named_bitmap_list(storage, "nonunique/rarity", &["C", "R", "E", "U"])?;
    let product = load_named_bitmap_list(storage, "nonunique/product", &["B", "A", "P"])?;

    let serialized_rel = "nonunique/serialized.roar";
    let serialized = storage
        .has_file(serialized_rel)
        .then(|| read_roar(storage, serialized_rel))
        .transpose()?;

    let mut stats = BTreeMap::new();
    for stat in STAT_NAMES {
        let mut buckets = BTreeMap::new();
        for value in 0..VALUE_BUCKETS as u8 {
            let rel = format!("nonunique/stats/{stat}/{value:02}.roar");
            if storage.has_file(&rel) {
                buckets.insert(value, read_roar(storage, &rel)?);
            }
        }
        stats.insert(stat.to_string(), buckets);
    }

    let set = load_indexed_bitmaps(storage, "nonunique/set")?;
    let card_type = load_indexed_bitmaps(storage, "nonunique/type")?;
    let subtype = load_indexed_bitmaps(storage, "nonunique/subtype")?;
    let keywords = load_indexed_bitmaps(storage, "nonunique/keywords")?;
    let family = load_indexed_bitmaps(storage, "nonunique/family")?;

    let banned = load_optional_bitmap(storage, "nonunique/status/banned.roar")?;
    let errated = load_optional_bitmap(storage, "nonunique/status/errated.roar")?;
    let suspended = load_optional_bitmap(storage, "nonunique/status/suspended.roar")?;
    let has_no_effect = load_optional_bitmap(storage, "nonunique/has_no_effect.roar")?;

    let effect_text: Vec<BTreeMap<String, String>> = if storage.has_file("nonunique/effect_text.json")
    {
        read_json(storage, "nonunique/effect_text.json")?
    } else {
        Vec::new()
    };

    let prints: Vec<NonUniquePrintMeta> = if storage.has_file("nonunique/prints.json") {
        read_json(storage, "nonunique/prints.json")?
    } else {
        Vec::new()
    };

    let card_types_i18n: BTreeMap<String, BTreeMap<String, String>> =
        if storage.has_file("nonunique/card_types.json") {
            read_json(storage, "nonunique/card_types.json")?
        } else {
            BTreeMap::new()
        };

    let total = catalog.total_cards();
    let mut all_prints = RoaringBitmap::new();
    all_prints.insert_range(0..total);

    let cost_relations = if storage.has_file("nonunique/cost_relations/equal.roar")
        || storage.has_file("nonunique/cost_relations/main_higher.roar")
        || storage.has_file("nonunique/cost_relations/recall_higher.roar")
    {
        NonUniqueCostRelationBitmaps {
            equal: load_optional_bitmap(storage, "nonunique/cost_relations/equal.roar")?,
            main_higher: load_optional_bitmap(storage, "nonunique/cost_relations/main_higher.roar")?,
            recall_higher: load_optional_bitmap(
                storage,
                "nonunique/cost_relations/recall_higher.roar",
            )?,
        }
    } else {
        derive_cost_relations(&prints)
    };

    let stat_missing = compute_stat_missing(&all_prints, &stats, &prints);

    let mut ref_lookup = HashMap::with_capacity(total as usize);
    for idx in 0..total {
        if let Some(reference) = catalog.reference_for_index(idx) {
            ref_lookup.insert(reference.to_ascii_uppercase(), idx);
        }
    }

    let collector_lookup = build_collector_lookup(&prints);

    let effect_search_keys: Vec<BTreeMap<String, String>> = effect_text
        .iter()
        .map(|per_loc| {
            per_loc
                .iter()
                .map(|(loc, text)| (loc.clone(), search_key(text)))
                .collect()
        })
        .collect();

    let empty_families = BTreeMap::new();
    let normalized_names = vec![BTreeMap::new(); total as usize];
    let name_search_keys = vec![BTreeMap::new(); total as usize];
    let sort_index =
        build_nonunique_sort_index(&catalog, &prints, &normalized_names, &empty_families);

    Ok(Some(NonUniqueQueryIndex {
        catalog,
        cards,
        faction,
        rarity,
        product,
        serialized,
        stats,
        stat_missing,
        set,
        card_type,
        subtype,
        keywords,
        has_no_effect,
        cost_relations,
        family,
        banned,
        errated,
        suspended,
        prints,
        card_types_i18n,
        ref_lookup,
        collector_lookup,
        effect_search_keys,
        normalized_names,
        name_search_keys,
        sort_index,
    }))
}

fn derive_cost_relations(prints: &[NonUniquePrintMeta]) -> NonUniqueCostRelationBitmaps {
    let mut rel = NonUniqueCostRelationBitmaps::default();
    for (idx, meta) in prints.iter().enumerate() {
        if let (Some(mc), Some(rc)) = (meta.main_cost, meta.recall_cost) {
            let i = idx as u32;
            match mc.cmp(&rc) {
                std::cmp::Ordering::Equal => {
                    rel.equal.insert(i);
                }
                std::cmp::Ordering::Greater => {
                    rel.main_higher.insert(i);
                }
                std::cmp::Ordering::Less => {
                    rel.recall_higher.insert(i);
                }
            }
        }
    }
    rel
}

fn compute_stat_missing(
    all_prints: &RoaringBitmap,
    stats: &BTreeMap<String, BTreeMap<u8, RoaringBitmap>>,
    prints: &[NonUniquePrintMeta],
) -> BTreeMap<String, RoaringBitmap> {
    let mut missing = BTreeMap::new();
    for stat in STAT_NAMES {
        let bmp = if prints.len() as u32 == all_prints.len() as u32 && !prints.is_empty() {
            let mut m = RoaringBitmap::new();
            for (idx, meta) in prints.iter().enumerate() {
                let present = match stat {
                    "main_cost" => meta.main_cost.is_some(),
                    "recall_cost" => meta.recall_cost.is_some(),
                    "mountain_power" => meta.mountain_power.is_some(),
                    "ocean_power" => meta.ocean_power.is_some(),
                    "forest_power" => meta.forest_power.is_some(),
                    _ => false,
                };
                if !present {
                    m.insert(idx as u32);
                }
            }
            m
        } else {
            let mut present = RoaringBitmap::new();
            if let Some(buckets) = stats.get(stat) {
                for b in buckets.values() {
                    present |= b;
                }
            }
            all_prints - present
        };
        missing.insert(stat.to_string(), bmp);
    }
    missing
}

fn build_collector_lookup(prints: &[NonUniquePrintMeta]) -> HashMap<String, Vec<u32>> {
    let mut map: HashMap<String, Vec<u32>> = HashMap::new();
    for (idx, meta) in prints.iter().enumerate() {
        let Some(raw) = meta.collector_number.as_deref() else {
            continue;
        };
        let norm = raw.trim().to_ascii_uppercase();
        if norm.is_empty() {
            continue;
        }
        let i = idx as u32;
        map.entry(norm.clone()).or_default().push(i);

        // Also index stripped locale suffix (e.g. `SDU-001-C-EN` -> `SDU-001-C`).
        for suffix in ["-EN", "-FR", "-ES", "-DE", "-IT"] {
            if let Some(prefix) = norm.strip_suffix(suffix) {
                let v = map.entry(prefix.to_string()).or_default();
                if !v.contains(&i) {
                    v.push(i);
                }
                // And stripped rarity (`SDU-001` from `SDU-001-C`).
                for rar in ["-C", "-R", "-E", "-U"] {
                    if let Some(base) = prefix.strip_suffix(rar) {
                        let bv = map.entry(base.to_string()).or_default();
                        if !bv.contains(&i) {
                            bv.push(i);
                        }
                    }
                }
            }
        }
        // Also stripped rarity directly (`BTG-001-C` -> `BTG-001`).
        for rar in ["-C", "-R", "-E", "-U"] {
            if let Some(base) = norm.strip_suffix(rar) {
                let bv = map.entry(base.to_string()).or_default();
                if !bv.contains(&i) {
                    bv.push(i);
                }
            }
        }
    }
    map
}

fn collector_sort_parts(raw: Option<&str>) -> (u32, String) {
    let Some(s) = raw else {
        return (u32::MAX, String::new());
    };
    let num = s
        .split('-')
        .nth(1)
        .and_then(|seg| seg.parse::<u32>().ok())
        .unwrap_or(u32::MAX);
    (num, s.to_string())
}

fn build_nonunique_sort_index(
    catalog: &NonUniqueCatalog,
    prints: &[NonUniquePrintMeta],
    normalized_names: &[BTreeMap<String, String>],
    families: &BTreeMap<i64, FamilyCatalogEntry>,
) -> NonUniqueSortIndex {
    let total = catalog.total_cards() as usize;
    if total == 0 {
        return NonUniqueSortIndex::default();
    }

    let print_ref = |i: usize| catalog.reference_for_index(i as u32).unwrap_or("");
    let print_set = |i: usize| prints.get(i).map(|p| p.set.as_str()).unwrap_or("");
    let print_set_code = |i: usize| {
        prints
            .get(i)
            .and_then(|p| p.set_code.as_deref().or(Some(p.set.as_str())))
            .unwrap_or("")
    };
    let print_faction = |i: usize| prints.get(i).map(|p| p.faction.as_str()).unwrap_or("");
    let print_col = |i: usize| collector_sort_parts(prints.get(i).and_then(|p| p.collector_number.as_deref()));

    let mut indices: Vec<u32> = (0..total as u32).collect();

    indices.sort_by(|&a, &b| {
        let ia = a as usize;
        let ib = b as usize;
        (
            set_release_rank(print_set(ia)),
            faction_sort_rank(print_faction(ia)),
            print_col(ia),
            print_ref(ia),
        )
            .cmp(&(
                set_release_rank(print_set(ib)),
                faction_sort_rank(print_faction(ib)),
                print_col(ib),
                print_ref(ib),
            ))
    });
    let set_date_asc = indices.clone();

    indices.sort_by(|&a, &b| {
        let ia = a as usize;
        let ib = b as usize;
        (
            Reverse(set_release_rank(print_set(ia))),
            faction_sort_rank(print_faction(ia)),
            print_col(ia),
            print_ref(ia),
        )
            .cmp(&(
                Reverse(set_release_rank(print_set(ib))),
                faction_sort_rank(print_faction(ib)),
                print_col(ib),
                print_ref(ib),
            ))
    });
    let set_date_desc = indices.clone();

    indices.sort_by(|&a, &b| {
        let ia = a as usize;
        let ib = b as usize;
        (
            print_set_code(ia),
            faction_sort_rank(print_faction(ia)),
            print_col(ia),
            set_release_rank(print_set(ia)),
            print_ref(ia),
        )
            .cmp(&(
                print_set_code(ib),
                faction_sort_rank(print_faction(ib)),
                print_col(ib),
                set_release_rank(print_set(ib)),
                print_ref(ib),
            ))
    });
    let collector_asc = indices.clone();

    indices.sort_by(|&a, &b| print_ref(a as usize).cmp(print_ref(b as usize)));
    let reference_asc = indices.clone();

    let mut locales = BTreeSet::new();
    locales.insert("en_US".to_string());
    for fam in families.values() {
        for loc in fam.name.keys() {
            locales.insert(loc.clone());
        }
    }

    let mut name_asc_by_locale = BTreeMap::new();
    for locale in locales {
        let mut loc_indices: Vec<u32> = (0..total as u32).collect();
        let names_for_locale: Vec<&str> = (0..total)
            .map(|i| {
                normalized_names
                    .get(i)
                    .and_then(|m| {
                        m.get(&locale)
                            .or_else(|| m.get("en_US"))
                            .or_else(|| m.values().next())
                    })
                    .map(String::as_str)
                    .unwrap_or_else(|| print_ref(i))
            })
            .collect();
        loc_indices.sort_by(|&a, &b| {
            let ia = a as usize;
            let ib = b as usize;
            (
                names_for_locale[ia],
                set_release_rank(print_set(ia)),
                print_ref(ia),
            )
                .cmp(&(
                    names_for_locale[ib],
                    set_release_rank(print_set(ib)),
                    print_ref(ib),
                ))
        });
        name_asc_by_locale.insert(locale, loc_indices);
    }

    NonUniqueSortIndex {
        set_date_asc,
        set_date_desc,
        collector_asc,
        reference_asc,
        name_asc_by_locale,
    }
}

fn load_named_bitmap_list(
    storage: &impl IndexStorage,
    dir_rel: &str,
    codes: &[&str],
) -> Result<BTreeMap<String, RoaringBitmap>> {
    let mut out = BTreeMap::new();
    for code in codes {
        let rel = format!("{dir_rel}/{code}.roar");
        if storage.has_file(&rel) {
            out.insert((*code).to_string(), read_roar(storage, &rel)?);
        }
    }
    Ok(out)
}

fn load_indexed_bitmaps(
    storage: &impl IndexStorage,
    dir_rel: &str,
) -> Result<BTreeMap<String, RoaringBitmap>> {
    let index_rel = format!("{dir_rel}/_index.json");
    if !storage.has_file(&index_rel) {
        return Ok(BTreeMap::new());
    }
    let names: Vec<String> = read_json(storage, &index_rel)?;
    let mut out = BTreeMap::new();
    for name in names {
        let rel = format!("{dir_rel}/{name}.roar");
        if storage.has_file(&rel) {
            out.insert(name, read_roar(storage, &rel)?);
        }
    }
    Ok(out)
}

fn load_optional_bitmap(storage: &impl IndexStorage, relative_path: &str) -> Result<RoaringBitmap> {
    if !storage.has_file(relative_path) {
        return Ok(RoaringBitmap::new());
    }
    read_roar(storage, relative_path)
}
