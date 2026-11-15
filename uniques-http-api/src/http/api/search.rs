//! `GET /api/v2/search` — merged unique + non-unique search (Lot 2, first slice, extended).
//!
//! Additive endpoint, deliberately separate from `GET /api/v2/cards`: see
//! `uniques-http-api/plans/19-merged-search.md` for why.
//!
//! Filters: `faction`, `rarity`, `product`, `serialized`, `edition` (set), `type`, `subtype`,
//! `family` (`CardFamilyId`, see plan 23), `banned`/`errated`/`suspended`, and the 5 cost/power
//! predicates (`mainCost`, `recallCost`, `mountainPower`, `oceanPower`, `forestPower` — exact /
//! `[]` any-of / `[gt|gte|lt|lte]` range, reusing `CostPredicate`/`parse_cost_predicate` from
//! `/api/v2/cards` rather than re-implementing the same parsing).
//!
//! Effects, interim (Lot 4 is still the real fix, see D6): **uniques** get `effect[N][t|c|o]` +
//! `matchCount` + `effectMode`, identical semantics to `/api/v2/cards` (reuses
//! `effect_slot_bitmap`/`combine_effect_bitmaps`/`parse_effect_slots`/`validate_idgd_types`
//! directly) — id-based, so it only means anything on the unique side; presence of any `effect[N]`
//! zeroes the non-unique branch. **Non-uniques** get `effectText=<substring>` instead — case/
//! accent-insensitive substring search over effect text (same mechanism `name` already uses on
//! `/api/v2/cards`), since non-unique effects aren't decomposed into shared ids yet; presence of
//! `effectText` zeroes the unique branch. Neither works on the other side — this is intentionally
//! two different mechanisms for two different data maturity levels, not one filter.
//!
//! Branch-exclusion rule (from the original ask): `rarity`/`product`/`serialized` only exist on
//! the non-unique side, and `type` other than `CHARACTER` can't match a unique either (a unique is
//! always a Character) — any of these zero the unique branch entirely rather than silently
//! ignoring the filter. `edition`/`subtype`/`family`/`banned`/`errated`/`suspended`/cost-power
//! apply symmetrically to both branches, since both card kinds carry those facets.
//!
//! `format` stays unique-branch only (formats are defined over the unique bit-span, no non-unique
//! representation exists). `collection` now applies to both branches: a named collection resolves
//! to *two* bitmaps (see `collections::CollectionBitmaps`) since `POST /api/v2/collection/{id}` can
//! mix unique- and non-unique-shaped references in the same body — each branch here intersects
//! with its own half. `resolve_format_filter`/`resolve_collection_filter` from `/api/v2/cards` are
//! reused unchanged for validation (same error messages); only the bitmap lookup differs per
//! branch.
//!
//! `page` (Lot 3 continuation, see `uniques-http-api/plans/21-search-page-pagination.md`) is a rank
//! over the *virtual concatenation* of the two bitmaps (non-unique first, then unique) — a page can
//! straddle the boundary between them. `window_across` splits the requested rank range across both
//! bitmaps via `select`/`advance_to`, same mechanism `/api/v2/cards` already uses on one bitmap.

use std::collections::BTreeMap;

use axum::extract::{RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use roaring::RoaringBitmap;
use serde::Serialize;

use index_core::compact::faction_reference_from_code;
use index_core::faction_index::Faction;
use index_core::idgd_catalog::IdGdCatalogEntry;
use index_core::nonunique::{product_reference_from_code, rarity_reference_from_code};
use index_core::stat_index::StatField;

use crate::collections::CollectionStore;
use crate::formats::{FormatIndex, FormatLoadStatus};
use crate::http::ServerState;
use crate::index::{NonUniqueQueryIndex, UniquesIndex};

use super::cards::models::{
    AbilityFilters, CardV2, CompareOp, CostPredicate, EffectCombineMode, EffectSlotFilter,
};
use super::cards::parse::{
    get_first, parse_collection, parse_cost_predicate, parse_effect_mode, parse_effect_slots,
    parse_format, parse_query_multimap, resolve_collection_filter, resolve_format_filter,
    validate_idgd_types, QueryMultiMap,
};
use super::error::{bad_request, ApiResult};

pub fn router() -> Router<ServerState> {
    Router::new().route("/api/v2/search", get(get_search))
}

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

#[derive(Debug, Default)]
struct SearchRequest {
    faction: Option<String>,
    rarity: Option<String>,
    product: Option<String>,
    serialized: Option<bool>,
    edition: Option<String>,
    card_type: Option<String>,
    subtype: Option<String>,
    family: Option<i64>,
    banned: Option<bool>,
    errated: Option<bool>,
    suspended: Option<bool>,
    main_cost: Option<CostPredicate>,
    recall_cost: Option<CostPredicate>,
    mountain_power: Option<CostPredicate>,
    ocean_power: Option<CostPredicate>,
    forest_power: Option<CostPredicate>,
    /// Unique-branch only — see the module doc comment's `format`/`collection` note. Neither
    /// concept has a non-unique bitmap representation yet.
    format: Option<String>,
    collection: Option<String>,
    /// Unique-branch only (interim, see the module doc comment's effects note): id-based
    /// `effect[N][t|c|o]`/`matchCount`/`effectMode`, identical semantics to `/api/v2/cards`.
    effects: Vec<EffectSlotFilter>,
    effect_mode: EffectCombineMode,
    /// Non-unique-branch only (interim, see the module doc comment): case/accent-insensitive
    /// substring search over effect text, same mechanism `name` uses on `/api/v2/cards`.
    effect_text: Option<String>,
    limit: usize,
    /// 1-indexed direct page access over the virtual non-unique-then-unique concatenation.
    page: Option<u32>,
}

impl SearchRequest {
    /// `rarity`/`product`/`serialized` only exist on the non-unique side, and `type` other than
    /// `CHARACTER` can't match a unique either — a unique print is always rarity "Unique", has no
    /// product/serialization concept the same way, and is always a Character. Requesting any of
    /// these zeroes the unique branch entirely rather than silently ignoring the filter.
    fn unique_branch_excluded(&self) -> bool {
        self.rarity.is_some()
            || self.product.is_some()
            || self.serialized.is_some()
            || self
                .card_type
                .as_deref()
                .is_some_and(|t| t != "CHARACTER")
            || self.effect_text.is_some()
    }

    /// `effect[N]` is id-based — those ids only mean anything on the unique side (D6: non-unique
    /// effects aren't decomposed into shared ids yet). Symmetric to `unique_branch_excluded`.
    fn nonunique_branch_excluded(&self) -> bool {
        !self.effects.is_empty()
    }
}

fn parse_bool_param(params: &QueryMultiMap, key: &str) -> ApiResult<Option<bool>> {
    let Some(raw) = get_first(params, key) else {
        return Ok(None);
    };
    match raw {
        "true" | "1" => Ok(Some(true)),
        "false" | "0" => Ok(Some(false)),
        other => Err(bad_request(format!(
            "invalid {key} value '{other}': expected true/false"
        ))),
    }
}

fn parse_request(
    query: Option<&str>,
    unique: &UniquesIndex,
    format_index: &FormatIndex,
    formats_enabled: bool,
    collections: &CollectionStore,
) -> ApiResult<SearchRequest> {
    let params = parse_query_multimap(query)?;
    let get = |key: &str| get_first(&params, key).map(str::to_string);

    let effects = parse_effect_slots(&params)?;
    let effect_mode = parse_effect_mode(&params)?;
    validate_idgd_types(
        unique,
        &AbilityFilters {
            effects: effects.clone(),
            effect_mode,
            support_t: Vec::new(),
            support_c: Vec::new(),
            support_o: Vec::new(),
        },
    )?;

    let mut req = SearchRequest {
        limit: DEFAULT_LIMIT,
        faction: get("faction"),
        rarity: get("rarity"),
        product: get("product"),
        edition: get("edition"),
        card_type: get("type"),
        subtype: get("subtype"),
        effect_text: get("effectText"),
        effects,
        effect_mode,
        serialized: parse_bool_param(&params, "serialized")?,
        banned: parse_bool_param(&params, "banned")?,
        errated: parse_bool_param(&params, "errated")?,
        suspended: parse_bool_param(&params, "suspended")?,
        main_cost: parse_cost_predicate(&params, "mainCost")?,
        recall_cost: parse_cost_predicate(&params, "recallCost")?,
        mountain_power: parse_cost_predicate(&params, "mountainPower")?,
        ocean_power: parse_cost_predicate(&params, "oceanPower")?,
        forest_power: parse_cost_predicate(&params, "forestPower")?,
        format: resolve_format_filter(parse_format(&params), formats_enabled, format_index)?,
        collection: resolve_collection_filter(parse_collection(&params), collections)?,
        ..Default::default()
    };

    if let Some(v) = get_first(&params, "limit") {
        req.limit = v
            .parse::<usize>()
            .map_err(|_| bad_request("limit must be an integer".to_string()))?;
    }
    if !(1..=MAX_LIMIT).contains(&req.limit) {
        return Err(bad_request(format!("limit must be in range 1..={MAX_LIMIT}")));
    }

    if let Some(v) = get_first(&params, "family") {
        req.family = Some(
            v.parse::<i64>()
                .map_err(|_| bad_request("family must be an integer".to_string()))?,
        );
    }

    if let Some(v) = get_first(&params, "page") {
        let page = v
            .parse::<u32>()
            .map_err(|_| bad_request("page must be an integer".to_string()))?;
        if page == 0 {
            return Err(bad_request("page must be >= 1".to_string()));
        }
        req.page = Some(page);
    }

    Ok(req)
}

fn parse_faction(reference: &str) -> Option<Faction> {
    Faction::ALL.into_iter().find(|f| f.reference() == reference)
}

#[derive(Debug, Serialize)]
struct SearchIter {
    total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<u32>,
}

#[derive(Debug, Serialize)]
struct SearchResponse {
    iter: SearchIter,
    cards: Vec<SearchCard>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum SearchCard {
    #[serde(rename = "nonunique")]
    NonUnique {
        reference: String,
        faction: Option<String>,
        rarity: Option<String>,
        product: Option<String>,
        serialized: bool,
        main_cost: u8,
        recall_cost: u8,
        mountain_power: u8,
        ocean_power: u8,
        forest_power: u8,
    },
    /// Full `CardV2` (same shape `/api/v2/cards` returns) flattened alongside the `kind` tag —
    /// name/artist/set/subtypes/effects, not just the minimal fields the non-unique side has.
    #[serde(rename = "unique")]
    Unique {
        #[serde(flatten)]
        card: CardV2,
    },
}

async fn get_search(
    State(server): State<ServerState>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<SearchResponse>> {
    let snapshot = server.app.snapshot();
    let formats_enabled = server
        .settings
        .formats
        .as_ref()
        .is_some_and(|f| f.is_enabled());
    let req = parse_request(
        query.as_deref(),
        snapshot.index.as_ref(),
        &snapshot.formats,
        formats_enabled,
        &snapshot.collections,
    )?;
    let unique = snapshot.index.as_ref();
    let nonunique = snapshot.nonunique.as_deref();

    let nonunique_bitmap = if req.nonunique_branch_excluded() {
        RoaringBitmap::new()
    } else {
        match nonunique {
            Some(nu) => build_nonunique_bitmap(nu, &snapshot.collections, &req)?,
            None => RoaringBitmap::new(),
        }
    };

    let unique_bitmap = if req.unique_branch_excluded() {
        RoaringBitmap::new()
    } else {
        build_unique_bitmap(unique, &snapshot.formats, &snapshot.collections, &req)?
    };

    let total = nonunique_bitmap.len() + unique_bitmap.len();
    let start_rank = req
        .page
        .map(|p| u64::from(p.saturating_sub(1)) * req.limit as u64)
        .unwrap_or(0);
    let (nonunique_indices, unique_indices) =
        window_across(&nonunique_bitmap, &unique_bitmap, start_rank, req.limit);

    let mut cards = Vec::with_capacity(nonunique_indices.len() + unique_indices.len());

    if let Some(nu) = nonunique {
        for print_index in nonunique_indices {
            let (Some(reference), Some(fields)) =
                (nu.reference_for(print_index), nu.fields_for(print_index))
            else {
                continue;
            };
            cards.push(SearchCard::NonUnique {
                reference: reference.to_string(),
                faction: faction_reference_from_code(fields.faction_code).map(str::to_string),
                rarity: rarity_reference_from_code(fields.rarity_code).map(str::to_string),
                product: product_reference_from_code(fields.product_code).map(str::to_string),
                serialized: fields.is_serialized,
                main_cost: fields.main_cost,
                recall_cost: fields.recall_cost,
                mountain_power: fields.mountain_power,
                ocean_power: fields.ocean_power,
                forest_power: fields.forest_power,
            });
        }
    }

    if !unique_indices.is_empty() {
        // Same idgd_by_id map `/api/v2/cards`'s handler builds once per request — full `CardV2`
        // parity (name/artist/subtypes/effects, not just reference/faction/stats) reuses
        // `card_v2_from_index` directly rather than re-deriving a thinner shape by hand.
        let idgd_by_id: BTreeMap<u32, &IdGdCatalogEntry> = unique
            .idgd_catalog()
            .entries
            .iter()
            .map(|e| (e.id_gd, e))
            .collect();
        for card_index in unique_indices {
            if let Ok(card) = crate::index::card_v2_from_index(unique, card_index, &idgd_by_id, false) {
                cards.push(SearchCard::Unique { card });
            }
        }
    }

    Ok(Json(SearchResponse {
        iter: SearchIter {
            total,
            page: req.page,
        },
        cards,
    }))
}

/// Splits the rank window `[start_rank, start_rank + limit)` of the *virtual concatenation* of
/// `first` (non-unique) then `second` (unique) across the two real bitmaps, each accessed directly
/// by rank (`select` + `advance_to`) — no scan from the start of either, and no need to have
/// fetched previous pages. Returns `(indices_from_first, indices_from_second)`, together holding at
/// most `limit` entries.
fn window_across(
    first: &RoaringBitmap,
    second: &RoaringBitmap,
    start_rank: u64,
    limit: usize,
) -> (Vec<u32>, Vec<u32>) {
    let first_total = first.len();

    let mut first_out = Vec::new();
    if start_rank < first_total {
        // Guarded by `start_rank < first_total`, and a bitmap's cardinality never exceeds what a
        // u32 rank can address, so this narrowing is exact.
        if let Some(first_val) = first.select(start_rank as u32) {
            let mut it = first.iter();
            it.advance_to(first_val);
            first_out = it.take(limit).collect();
        }
    }

    let remaining = limit - first_out.len();
    let mut second_out = Vec::new();
    if remaining > 0 {
        let second_start_rank = start_rank.saturating_sub(first_total);
        // Unlike `first_total`, `start_rank` comes straight from `page * limit` and isn't bounded
        // by anything real — reject rather than silently wrap if it's absurdly large.
        if let Ok(second_start_rank) = u32::try_from(second_start_rank) {
            if let Some(second_val) = second.select(second_start_rank) {
                let mut it = second.iter();
                it.advance_to(second_val);
                second_out = it.take(remaining).collect();
            }
        }
    }

    (first_out, second_out)
}

fn build_nonunique_bitmap(
    nu: &NonUniqueQueryIndex,
    collections: &CollectionStore,
    req: &SearchRequest,
) -> ApiResult<RoaringBitmap> {
    let mut acc: Option<RoaringBitmap> = None;
    let mut intersect = |bitmap: RoaringBitmap| {
        acc = Some(match acc.take() {
            Some(current) => current & bitmap,
            None => bitmap,
        });
    };

    if let Some(faction) = &req.faction {
        let bitmap = nu.faction.get(faction).cloned().unwrap_or_default();
        intersect(bitmap);
    }
    if let Some(rarity) = &req.rarity {
        let bitmap = nu.rarity.get(rarity).cloned().unwrap_or_default();
        intersect(bitmap);
    }
    if let Some(product) = &req.product {
        let bitmap = nu.product.get(product).cloned().unwrap_or_default();
        intersect(bitmap);
    }
    if let Some(edition) = &req.edition {
        let bitmap = nu.set.get(edition).cloned().unwrap_or_default();
        intersect(bitmap);
    }
    if let Some(card_type) = &req.card_type {
        let bitmap = nu.card_type.get(card_type).cloned().unwrap_or_default();
        intersect(bitmap);
    }
    if let Some(subtype) = &req.subtype {
        let bitmap = nu.subtype.get(subtype).cloned().unwrap_or_default();
        intersect(bitmap);
    }
    if let Some(family) = req.family {
        let bitmap = nu.family.get(&family.to_string()).cloned().unwrap_or_default();
        intersect(bitmap);
    }
    if let Some(query) = &req.effect_text {
        intersect(nu.bitmap_for_effect_contains(query));
    }
    if let Some(serialized) = req.serialized {
        intersect(bool_bitmap(serialized, &nu.serialized.clone().unwrap_or_default(), nu.total()));
    }
    if let Some(banned) = req.banned {
        intersect(bool_bitmap(banned, &nu.banned, nu.total()));
    }
    if let Some(errated) = req.errated {
        intersect(bool_bitmap(errated, &nu.errated, nu.total()));
    }
    if let Some(suspended) = req.suspended {
        intersect(bool_bitmap(suspended, &nu.suspended, nu.total()));
    }
    if let Some(id) = &req.collection {
        if let Some(bitmaps) = collections.get(id) {
            intersect(bitmaps.nonunique.clone());
        }
    }

    for (predicate, field) in [
        (&req.main_cost, "main_cost"),
        (&req.recall_cost, "recall_cost"),
        (&req.mountain_power, "mountain_power"),
        (&req.ocean_power, "ocean_power"),
        (&req.forest_power, "forest_power"),
    ] {
        if let Some(predicate) = predicate {
            let buckets = nu.stats.get(field);
            let bitmap = apply_cost_predicate(predicate, |value| {
                buckets
                    .and_then(|b| b.get(&value))
                    .cloned()
                    .unwrap_or_default()
            });
            intersect(bitmap);
        }
    }

    Ok(acc.unwrap_or_else(|| {
        let mut all = RoaringBitmap::new();
        all.insert_range(0..nu.total());
        all
    }))
}

fn build_unique_bitmap(
    unique: &UniquesIndex,
    format_index: &FormatIndex,
    collections: &CollectionStore,
    req: &SearchRequest,
) -> ApiResult<RoaringBitmap> {
    let mut acc: Option<RoaringBitmap> = None;

    // Interim id-based effect search (uniques only, see the module doc comment): identical
    // semantics to `/api/v2/cards` — each `effect[N]` searches main lines (M1/M2/M3) with
    // per-line T/C/O intersection and `matchCount` (how many distinct lines must match), multiple
    // slots combine per `effectMode`. `parse_request` already validated the ids' types.
    let effect_bitmaps: Vec<RoaringBitmap> = req
        .effects
        .iter()
        .filter_map(|slot| {
            crate::index::effect_slot_bitmap(unique, &slot.t, &slot.c, &slot.o, slot.match_count)
        })
        .collect();
    if let Some(bitmap) = crate::index::combine_effect_bitmaps(&effect_bitmaps, req.effect_mode) {
        intersect(&mut acc, bitmap);
    }

    if let Some(reference) = &req.faction {
        let faction = parse_faction(reference)
            .ok_or_else(|| bad_request(format!("unknown faction '{reference}'")))?;
        intersect(&mut acc, unique.factions.get(&faction).cloned().unwrap_or_default());
    }
    if let Some(edition) = &req.edition {
        intersect(
            &mut acc,
            unique
                .set_bitmaps()
                .by_set
                .get(edition)
                .cloned()
                .unwrap_or_default(),
        );
    }
    if let Some(subtype) = &req.subtype {
        intersect(
            &mut acc,
            unique
                .subtype_bitmaps()
                .get(subtype)
                .cloned()
                .unwrap_or_default(),
        );
    }
    if let Some(family_id) = req.family {
        // No precomputed bitmap for this (unlike subtype/set) — family count is small, so building
        // one on the fly by scanning `catalog.families` for a match is cheap. In principle more
        // than one row could share a `card_family_id` (same character across multiple merged sets,
        // see plan 23's caveat); all matches are unioned.
        let mut bitmap = RoaringBitmap::new();
        for family in &unique.catalog.families {
            if family.card_family_id == Some(family_id) {
                let end = family.start_bit.saturating_add(family.max_unique_id);
                bitmap.insert_range(family.start_bit..end);
            }
        }
        intersect(&mut acc, bitmap);
    }
    let total_bit_span = unique.catalog.total_bit_span;
    if let Some(banned) = req.banned {
        intersect(&mut acc, bool_bitmap(banned, unique.banned(), total_bit_span));
    }
    if let Some(errated) = req.errated {
        intersect(&mut acc, bool_bitmap(errated, unique.errated(), total_bit_span));
    }
    if let Some(suspended) = req.suspended {
        intersect(&mut acc, bool_bitmap(suspended, unique.suspended(), total_bit_span));
    }

    for (predicate, field) in [
        (&req.main_cost, StatField::MainCost),
        (&req.recall_cost, StatField::RecallCost),
        (&req.mountain_power, StatField::MountainPower),
        (&req.ocean_power, StatField::OceanPower),
        (&req.forest_power, StatField::ForestPower),
    ] {
        if let Some(predicate) = predicate {
            let buckets = unique.stats().get(&field);
            let bitmap = apply_cost_predicate(predicate, |value| {
                buckets
                    .and_then(|b| b.get(value as usize))
                    .cloned()
                    .unwrap_or_default()
            });
            intersect(&mut acc, bitmap);
        }
    }

    // Unique-branch only — see the module doc comment. `resolve_format_filter`/
    // `resolve_collection_filter` (called in `parse_request`) already validated these exist and,
    // for format, are `Ready` — safe to look up again here without re-checking status.
    if let Some(id) = &req.format {
        if let Some(loaded) = format_index.get(id) {
            if let FormatLoadStatus::Ready { negated, bitmap } = &loaded.status {
                if *negated {
                    acc = Some(match acc.take() {
                        Some(current) => current - bitmap.clone(),
                        None => {
                            let mut all = RoaringBitmap::new();
                            all.insert_range(0..total_bit_span);
                            all - bitmap.clone()
                        }
                    });
                } else {
                    intersect(&mut acc, bitmap.clone());
                }
            }
        }
    }
    if let Some(id) = &req.collection {
        if let Some(bitmaps) = collections.get(id) {
            intersect(&mut acc, bitmaps.unique.clone());
        }
    }

    Ok(acc.unwrap_or_else(|| {
        let mut all = RoaringBitmap::new();
        all.insert_range(0..total_bit_span);
        all
    }))
}

/// AND `bitmap` into `acc` (first call just sets it) — a free function rather than a closure
/// capturing `acc`, since `build_unique_bitmap` also needs to manipulate `acc` directly in one
/// place (format's negated case, a subtraction, not an AND) and a closure borrowing `acc` for its
/// whole lifetime would conflict with that.
fn intersect(acc: &mut Option<RoaringBitmap>, bitmap: RoaringBitmap) {
    *acc = Some(match acc.take() {
        Some(current) => current & bitmap,
        None => bitmap,
    });
}

/// `value` present in a boolean-flag bitmap (`true`) or its complement (`false`, needs the total
/// cardinality to build "everything except this bitmap").
fn bool_bitmap(value: bool, flag_bitmap: &RoaringBitmap, total: u32) -> RoaringBitmap {
    if value {
        flag_bitmap.clone()
    } else {
        let mut all = RoaringBitmap::new();
        all.insert_range(0..total);
        all - flag_bitmap
    }
}

/// Applies a `CostPredicate` against a 0..=15 value space via `get_bucket(value)` — the bitmap of
/// cards whose stat equals `value`. Generic over the lookup so both the unique side
/// (`[RoaringBitmap; 16]` indexed by value) and the non-unique side (`BTreeMap<u8, RoaringBitmap>`)
/// share this logic.
fn apply_cost_predicate(predicate: &CostPredicate, get_bucket: impl Fn(u8) -> RoaringBitmap) -> RoaringBitmap {
    match predicate {
        CostPredicate::Exact(value) => get_bucket(*value),
        CostPredicate::AnyOf(values) => {
            let mut acc = RoaringBitmap::new();
            for value in values {
                acc |= get_bucket(*value);
            }
            acc
        }
        CostPredicate::RangeBounds(bounds) => {
            let mut acc = RoaringBitmap::new();
            for candidate in 0u8..16 {
                let matches = bounds.iter().all(|(op, value)| match op {
                    CompareOp::Gt => candidate > *value,
                    CompareOp::Gte => candidate >= *value,
                    CompareOp::Lt => candidate < *value,
                    CompareOp::Lte => candidate <= *value,
                });
                if matches {
                    acc |= get_bucket(candidate);
                }
            }
            acc
        }
    }
}

#[cfg(test)]
mod tests {
    use super::window_across;
    use roaring::RoaringBitmap;

    fn bitmap(values: impl IntoIterator<Item = u32>) -> RoaringBitmap {
        values.into_iter().collect()
    }

    #[test]
    fn entirely_within_first() {
        let first = bitmap(0..30); // 30 non-unique matches
        let second = bitmap(100..200); // 100 unique matches
        let (a, b) = window_across(&first, &second, 0, 10);
        assert_eq!(a, (0..10).collect::<Vec<_>>());
        assert!(b.is_empty());
    }

    #[test]
    fn straddles_the_boundary() {
        let first = bitmap(0..30); // ranks 0..29
        let second = bitmap(1000..1100); // ranks (rebased) 0..99, values 1000..1099
        // Page covering ranks 20..39: 10 left in `first` (20..29), then 10 from the start of
        // `second` (rank 0..9 there, i.e. values 1000..1009).
        let (a, b) = window_across(&first, &second, 20, 20);
        assert_eq!(a, (20..30).collect::<Vec<_>>());
        assert_eq!(b, (1000..1010).collect::<Vec<_>>());
    }

    #[test]
    fn entirely_within_second() {
        let first = bitmap(0..30);
        let second = bitmap(1000..1100);
        // Rank 30 is exactly where `first` ends, so this page is 100% `second`, from its start.
        let (a, b) = window_across(&first, &second, 30, 10);
        assert!(a.is_empty());
        assert_eq!(b, (1000..1010).collect::<Vec<_>>());
    }

    #[test]
    fn beyond_both_returns_empty() {
        let first = bitmap(0..30);
        let second = bitmap(1000..1100);
        let (a, b) = window_across(&first, &second, 1000, 10);
        assert!(a.is_empty());
        assert!(b.is_empty());
    }

    #[test]
    fn page_one_matches_no_page_default() {
        let first = bitmap(0..30);
        let second = bitmap(1000..1100);
        let default = window_across(&first, &second, 0, 10);
        let page_one = window_across(&first, &second, (1u32.saturating_sub(1) as u64) * 10, 10);
        assert_eq!(default, page_one);
    }
}
