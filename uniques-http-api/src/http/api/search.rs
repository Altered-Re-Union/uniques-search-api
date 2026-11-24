//! `GET` and `POST /api/v2/search` — unified unique + non-unique search.
//!
//! Combines `UniquesIndex` and `NonUniqueQueryIndex` behind one response shape (`cards[]` items
//! tagged with `"kind": "unique" | "nonunique"` and serialized as rich `CardV2` objects),
//! supporting full filtering, branch exclusion (e.g. effect/support filters are uniques-only;
//! non-Character types, `NE` faction, `COMMON`/`RARE`/`EXALTED` rarities, and non-standard
//! variations apply to non-uniques), and global sorting across both branches without sorting raw
//! card vectors per request.

use std::cmp::{Ordering, Reverse};
use std::collections::BTreeMap;

use axum::extract::{DefaultBodyLimit, RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use index_core::bitmap::EffectLine;
use index_core::idgd_catalog::IdGdCatalogEntry;
use index_core::nonunique::{canonical_keyword, NonUniquePrintMeta};
use index_core::stat_index::StatField;
use roaring::RoaringBitmap;
use serde::Serialize;

use crate::collections::CollectionStore;
use crate::config::CardsSettings;
use crate::formats::{FormatIndex, FormatLoadStatus};
use crate::http::api::cards::models::{
    AbilityFilters, CardV2, CardsRequest, CardsSort, CompareOp, CostPredicate, CostRelation,
    EffectCombineMode, EffectSlotFilter, SortDir, SortField,
};
use crate::http::api::cards::parse::{
    get_first, has_any, parse_collection, parse_collector_numbers, parse_cost_predicate,
    parse_artists, parse_cost_relation, parse_effect_mode, parse_effect_slots, parse_format,
    parse_has_no_effect, parse_has_zero_power, parse_id_list, parse_name, parse_q, parse_query_multimap, parse_refs, parse_sets, parse_sort,
    parse_sub_types, parse_u32, parse_usize, resolve_collection_filter, resolve_format_filter,
    validate_idgd_types_unified, QueryMultiMap,
};
use crate::http::api::error::{bad_request, map_query_error, ApiResult};
use crate::http::state::QuerySnapshot;
use crate::http::ServerState;
use crate::index::loader::{
    faction_sort_rank, normalize_name_for_search, set_release_rank, NonUniqueEffectLine,
};
use crate::index::{
    build_bitmap, card_v2_from_index, combine_effect_bitmaps, enrich_unique_card_v2,
    nonunique_card_v2, subtype_names_from_index, NonUniqueQueryIndex, UniquesIndex,
};

pub fn router(cards: &CardsSettings) -> Router<ServerState> {
    let route = Router::new()
        .route("/api/v2/search", get(get_search).post(post_search))
        .route("/api/v2/search/facets", get(get_facets).post(post_facets));
    if cards.max_post_payload_bytes > 0 {
        route.layer(DefaultBodyLimit::max(cards.max_post_payload_bytes as usize))
    } else {
        route
    }
}

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KindFilter {
    All,
    UniqueOnly,
    NonUniqueOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeywordMode {
    Or,
    And,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VariationKind {
    Standard,
    AltArt,
    Promo,
    Serialized,
}

#[derive(Clone)]
pub(crate) struct SearchRequest {
    pub(crate) q: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) effect_text: Option<String>,
    pub(crate) effects: Vec<EffectSlotFilter>,
    pub(crate) effect_mode: EffectCombineMode,
    pub(crate) support_t: Vec<u32>,
    pub(crate) support_c: Vec<u32>,
    pub(crate) support_o: Vec<u32>,
    factions: Vec<String>,
    sets: Vec<String>,
    rarities: Vec<String>,
    card_types: Vec<String>,
    sub_types: Vec<String>,
    keywords: Vec<String>,
    keyword_mode: KeywordMode,
    products: Vec<String>,
    variations: Vec<VariationKind>,
    serialized: Option<bool>,
    family: Option<i64>,
    banned: Option<bool>,
    errated: Option<bool>,
    suspended: Option<bool>,
    main_cost: Option<CostPredicate>,
    recall_cost: Option<CostPredicate>,
    mountain_power: Option<CostPredicate>,
    ocean_power: Option<CostPredicate>,
    forest_power: Option<CostPredicate>,
    cost_relation: Option<CostRelation>,
    has_no_effect: Option<bool>,
    has_zero_power: bool,
    artists: Vec<String>,
    refs: Vec<String>,
    collector_numbers: Vec<String>,
    format: Option<String>,
    collection: Option<String>,
    kind: KindFilter,
    sort: Option<CardsSort>,
    page: Option<u32>,
    offset: Option<usize>,
    limit: usize,
    debug_bga_trigram: bool,
}

impl SearchRequest {
    fn nonunique_branch_excluded(&self) -> bool {
        if self.kind == KindFilter::UniqueOnly {
            return true;
        }
        if !self.rarities.is_empty()
            && !self
                .rarities
                .iter()
                .any(|r| matches!(r.as_str(), "C" | "R" | "E"))
        {
            return true;
        }
        false
    }

    fn unique_branch_excluded(&self) -> bool {
        if self.kind == KindFilter::NonUniqueOnly {
            return true;
        }
        if !self.rarities.is_empty() && !self.rarities.iter().any(|r| r == "U") {
            return true;
        }
        if !self.card_types.is_empty() && !self.card_types.iter().any(|t| t == "CHARACTER") {
            return true;
        }
        if !self.factions.is_empty()
            && !self.factions.iter().any(|f| f != "NE")
        {
            return true;
        }
        if !self.products.is_empty() && !self.products.iter().any(|p| p == "B") {
            return true;
        }
        if !self.variations.is_empty()
            && !self
                .variations
                .iter()
                .any(|v| *v == VariationKind::Standard)
        {
            return true;
        }
        if self.serialized == Some(true) {
            return true;
        }
        false
    }

    fn to_cards_request(&self) -> Option<CardsRequest> {
        let factions: Vec<index_core::faction_index::Faction> = self
            .factions
            .iter()
            .filter_map(|f| match f.as_str() {
                "AX" => Some(index_core::faction_index::Faction::Ax),
                "BR" => Some(index_core::faction_index::Faction::Br),
                "LY" => Some(index_core::faction_index::Faction::Ly),
                "MU" => Some(index_core::faction_index::Faction::Mu),
                "OR" => Some(index_core::faction_index::Faction::Or),
                "YZ" => Some(index_core::faction_index::Faction::Yz),
                _ => None,
            })
            .collect();
        if !self.factions.is_empty() && factions.is_empty() {
            return None;
        }
        let sets: Vec<String> = self
            .sets
            .iter()
            .flat_map(|s| match s.to_ascii_uppercase().as_str() {
                "BTG" => vec!["CORE".to_string(), "COREKS".to_string()],
                "TBF" => vec!["ALIZE".to_string()],
                "WFM" => vec!["BISE".to_string()],
                "SKY" => vec!["CYCLONE".to_string()],
                "SDU" => vec!["DUSTER".to_string()],
                "SDUCB" => vec!["DUSTERCB".to_string()],
                "SDUOP" => vec!["DUSTEROP".to_string()],
                "SDUTOP" => vec!["DUSTERTOP".to_string()],
                "ROC" => vec!["EOLE".to_string()],
                "ROCCB" => vec!["EOLECB".to_string()],
                "ROCOP" => vec!["EOLEOP".to_string()],
                "ROCTOP" => vec!["EOLETOP".to_string()],
                "NEJ" => vec!["FUGUE".to_string()],
                other => vec![other.to_string()],
            })
            .collect();
        Some(CardsRequest {
            filters: AbilityFilters {
                effects: self.effects.clone(),
                effect_mode: self.effect_mode,
                support_t: self.support_t.clone(),
                support_c: self.support_c.clone(),
                support_o: self.support_o.clone(),
            },
            factions,
            sets,
            main_cost: self.main_cost.clone(),
            recall_cost: self.recall_cost.clone(),
            forest_power: self.forest_power.clone(),
            mountain_power: self.mountain_power.clone(),
            ocean_power: self.ocean_power.clone(),
            name: self.name.clone(),
            cost_relation: self.cost_relation,
            sub_types: self.sub_types.clone(),
            has_no_effect: self.has_no_effect,
            has_zero_power: self.has_zero_power,
            artists: self.artists.clone(),
            q: self.q.clone(),
            format: self.format.clone(),
            collection: self.collection.clone(),
            sort: self.sort.clone(),
            limit: self.limit,
            cursor: None,
            page: self.page,
            refs: self.refs.clone(),
            collector_numbers: self.collector_numbers.clone(),
            with_families: false,
            debug_bga_trigram: self.debug_bga_trigram,
        })
    }
}

#[derive(Debug, Serialize)]
struct SearchIter {
    total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<u32>,
}

#[derive(Debug, Serialize)]
struct SearchResponse {
    iter: SearchIter,
    cards: Vec<SearchCard>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind")]
enum SearchCard {
    #[serde(rename = "nonunique")]
    NonUnique {
        #[serde(flatten)]
        card: CardV2,
    },
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
    let params = parse_query_multimap(query.as_deref())?;
    respond_search(&server, &params)
}

async fn post_search(
    State(server): State<ServerState>,
    body: String,
) -> ApiResult<Json<SearchResponse>> {
    let params = parse_query_multimap(Some(&body))?;
    respond_search(&server, &params)
}

fn respond_search(server: &ServerState, params: &QueryMultiMap) -> ApiResult<Json<SearchResponse>> {
    let snapshot = server.app.snapshot();
    let formats_enabled = server
        .settings
        .formats
        .as_ref()
        .is_some_and(|f| f.is_enabled());
    let req = parse_request(
        params,
        snapshot.index.as_ref(),
        snapshot.nonunique.as_deref(),
        &snapshot.formats,
        formats_enabled,
        &snapshot.collections,
    )?;

    let unique = snapshot.index.as_ref();
    let nonunique = snapshot.nonunique.as_deref();
    let (unique_bitmap, nonunique_bitmap) = search_bitmaps(&snapshot, &req)?;

    let nu_total = nonunique_bitmap.len();
    let u_total = unique_bitmap.len();
    let total = nu_total + u_total;

    let offset = match (req.page, req.offset) {
        (Some(p), None) => (p as usize).saturating_sub(1).saturating_mul(req.limit),
        (None, Some(o)) => o,
        (None, None) => 0,
        (Some(_), Some(_)) => {
            return Err(bad_request("cannot combine 'page' and 'cursor'"));
        }
    };

    let picked = select_page(
        unique,
        nonunique,
        &unique_bitmap,
        &nonunique_bitmap,
        &req.sort,
        offset,
        req.limit,
    );

    let subtype_names = subtype_names_from_index(unique);
    let need_unique_idgd = picked.iter().any(|item| matches!(item, PickedCard::Unique(_)));
    let idgd_by_id: BTreeMap<u32, &IdGdCatalogEntry> = if need_unique_idgd {
        unique
            .idgd_catalog()
            .entries
            .iter()
            .map(|e| (e.id_gd, e))
            .collect()
    } else {
        BTreeMap::new()
    };

    let mut cards = Vec::with_capacity(picked.len());
    for item in picked {
        match item {
            PickedCard::NonUnique(print_index) => {
                if let Some(nu) = nonunique {
                    if let Some(card) = nonunique_card_v2(
                        nu,
                        print_index,
                        &snapshot.families,
                        &subtype_names,
                        snapshot.formats.as_ref(),
                        snapshot.index.id_gd_aliases(),
                    ) {
                        cards.push(SearchCard::NonUnique { card });
                    }
                }
            }
            PickedCard::Unique(card_index) => {
                let mut card =
                    card_v2_from_index(unique, card_index, &idgd_by_id, req.debug_bga_trigram)
                        .map_err(map_query_error)?;
                enrich_unique_card_v2(
                    &mut card,
                    card_index,
                    unique,
                    nonunique,
                    &snapshot.formats,
                );
                cards.push(SearchCard::Unique { card });
            }
        }
    }

    let next_cursor = if req.page.is_none()
        && !cards.is_empty()
        && (offset as u64) + (cards.len() as u64) < total
    {
        Some((offset as u64) + (cards.len() as u64))
    } else {
        None
    };

    Ok(Json(SearchResponse {
        iter: SearchIter {
            total,
            cursor: next_cursor,
            page: req.page,
        },
        cards,
    }))
}

/// Matching cards of both branches for `req`: `(unique card indices, non-unique print indices)`.
/// A branch excluded by `req` (see `unique_branch_excluded` / `nonunique_branch_excluded`) is empty.
pub(crate) fn search_bitmaps(
    snapshot: &QuerySnapshot,
    req: &SearchRequest,
) -> ApiResult<(RoaringBitmap, RoaringBitmap)> {
    let nonunique_bitmap = if req.nonunique_branch_excluded() {
        RoaringBitmap::new()
    } else {
        match snapshot.nonunique.as_deref() {
            Some(nu) => build_nonunique_bitmap(
                nu,
                snapshot.index.as_ref(),
                &snapshot.formats,
                &snapshot.collections,
                req,
            )?,
            None => RoaringBitmap::new(),
        }
    };

    let unique_bitmap = if req.unique_branch_excluded() {
        RoaringBitmap::new()
    } else {
        build_unique_search_bitmap(
            snapshot.index.as_ref(),
            &snapshot.formats,
            &snapshot.collections,
            req,
        )?
    };

    Ok((unique_bitmap, nonunique_bitmap))
}

/// `GET` / `POST /api/v2/search/facets`: for the same filters as `/api/v2/search`, how many cards
/// each value of a facet would keep. Lets a UI only offer the options that still lead somewhere.
#[derive(Debug, Serialize)]
struct FacetsResponse {
    /// Subtype reference -> number of matching cards (uniques + non-uniques) carrying it. Counted
    /// with every filter **except** `subTypes[]` itself, so that picking one subtype doesn't hide
    /// the others (`subTypes[]` values are OR'd). Subtypes with no matching card are omitted.
    #[serde(rename = "subTypes")]
    sub_types: BTreeMap<String, u64>,
}

async fn get_facets(
    State(server): State<ServerState>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<FacetsResponse>> {
    let params = parse_query_multimap(query.as_deref())?;
    respond_facets(&server, &params)
}

async fn post_facets(
    State(server): State<ServerState>,
    body: String,
) -> ApiResult<Json<FacetsResponse>> {
    let params = parse_query_multimap(Some(&body))?;
    respond_facets(&server, &params)
}

fn respond_facets(server: &ServerState, params: &QueryMultiMap) -> ApiResult<Json<FacetsResponse>> {
    let snapshot = server.app.snapshot();
    let formats_enabled = server
        .settings
        .formats
        .as_ref()
        .is_some_and(|f| f.is_enabled());
    let mut req = parse_request(
        params,
        snapshot.index.as_ref(),
        snapshot.nonunique.as_deref(),
        &snapshot.formats,
        formats_enabled,
        &snapshot.collections,
    )?;
    req.sub_types.clear();

    let (unique_bitmap, nonunique_bitmap) = search_bitmaps(&snapshot, &req)?;
    let nonunique_subtypes = snapshot.nonunique.as_deref().map(|nu| &nu.subtype);
    Ok(Json(FacetsResponse {
        sub_types: count_by_value(
            [
                (&unique_bitmap, Some(snapshot.index.subtype_bitmaps())),
                (&nonunique_bitmap, nonunique_subtypes),
            ],
        ),
    }))
}

/// For each value, the number of cards of `matches` that carry it, summed over both branches
/// (each branch pairs its matching cards with its own per-value bitmaps). Zero counts are omitted.
fn count_by_value<'a>(
    branches: [(&RoaringBitmap, Option<&'a BTreeMap<String, RoaringBitmap>>); 2],
) -> BTreeMap<String, u64> {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for (matches, by_value) in branches {
        if matches.is_empty() {
            continue;
        }
        for (value, bitmap) in by_value.into_iter().flatten() {
            let n = matches.intersection_len(bitmap);
            if n > 0 {
                *counts.entry(value.clone()).or_default() += n;
            }
        }
    }
    counts
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickedCard {
    NonUnique(u32),
    Unique(u32),
}

fn select_page(
    unique: &UniquesIndex,
    nonunique: Option<&NonUniqueQueryIndex>,
    u_bitmap: &RoaringBitmap,
    nu_bitmap: &RoaringBitmap,
    sort: &Option<CardsSort>,
    offset: usize,
    limit: usize,
) -> Vec<PickedCard> {
    if limit == 0 || (u_bitmap.is_empty() && nu_bitmap.is_empty()) {
        return Vec::new();
    }
    let Some(sort) = sort else {
        let (nu_indices, u_indices) = window_across(nu_bitmap, u_bitmap, offset as u64, limit);
        let mut out = Vec::with_capacity(nu_indices.len() + u_indices.len());
        out.extend(nu_indices.into_iter().map(PickedCard::NonUnique));
        out.extend(u_indices.into_iter().map(PickedCard::Unique));
        return out;
    };

    match sort.field {
        SortField::MainCost => page_by_stat(
            unique,
            nonunique,
            u_bitmap,
            nu_bitmap,
            StatField::MainCost,
            "main_cost",
            sort.dir,
            offset,
            limit,
        ),
        SortField::RecallCost => page_by_stat(
            unique,
            nonunique,
            u_bitmap,
            nu_bitmap,
            StatField::RecallCost,
            "recall_cost",
            sort.dir,
            offset,
            limit,
        ),
        SortField::ForestPower => page_by_stat(
            unique,
            nonunique,
            u_bitmap,
            nu_bitmap,
            StatField::ForestPower,
            "forest_power",
            sort.dir,
            offset,
            limit,
        ),
        SortField::MountainPower => page_by_stat(
            unique,
            nonunique,
            u_bitmap,
            nu_bitmap,
            StatField::MountainPower,
            "mountain_power",
            sort.dir,
            offset,
            limit,
        ),
        SortField::OceanPower => page_by_stat(
            unique,
            nonunique,
            u_bitmap,
            nu_bitmap,
            StatField::OceanPower,
            "ocean_power",
            sort.dir,
            offset,
            limit,
        ),
        SortField::SetDate | SortField::Collector | SortField::Reference | SortField::Name(_) => {
            page_by_family_sort(unique, nonunique, u_bitmap, nu_bitmap, sort, offset, limit)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn page_by_stat(
    unique: &UniquesIndex,
    nonunique: Option<&NonUniqueQueryIndex>,
    u_bitmap: &RoaringBitmap,
    nu_bitmap: &RoaringBitmap,
    u_stat: StatField,
    nu_stat: &str,
    dir: SortDir,
    offset: usize,
    limit: usize,
) -> Vec<PickedCard> {
    let mut out = Vec::with_capacity(limit);
    let mut remaining_skip = offset as u64;
    let u_buckets = unique.stats().get(&u_stat);
    let nu_buckets = nonunique.and_then(|nu| nu.stats.get(nu_stat));

    let push_slice = |out: &mut Vec<PickedCard>,
                      remaining_skip: &mut u64,
                      sub: &RoaringBitmap,
                      wrap: fn(u32) -> PickedCard| {
        if out.len() >= limit || sub.is_empty() {
            return;
        }
        let count = sub.len();
        if *remaining_skip >= count {
            *remaining_skip -= count;
            return;
        }
        let need = limit - out.len();
        if *remaining_skip == 0 {
            out.extend(sub.iter().take(need).map(wrap));
        } else if let Some(start_bit) = sub.select(*remaining_skip as u32) {
            out.extend(sub.range(start_bit..).take(need).map(wrap));
        }
        *remaining_skip = 0;
    };

    let bucket_iter: Vec<u8> = match dir {
        SortDir::Asc => (0u8..16).collect(),
        SortDir::Desc => (0u8..16).rev().collect(),
    };

    for value in bucket_iter {
        if out.len() >= limit {
            break;
        }
        if !nu_bitmap.is_empty() {
            if let Some(nu_sub) = nu_buckets.and_then(|b| b.get(&value)) {
                let matched = nu_bitmap & nu_sub;
                push_slice(&mut out, &mut remaining_skip, &matched, PickedCard::NonUnique);
            }
        }
        if !u_bitmap.is_empty() && out.len() < limit {
            if let Some(u_sub) = u_buckets.and_then(|b| b.get(value as usize)) {
                let matched = u_bitmap & u_sub;
                push_slice(&mut out, &mut remaining_skip, &matched, PickedCard::Unique);
            }
        }
    }

    // NULLS LAST: Heroes, Spells, Landmarks, Tokens without this stat always come after 0..=15.
    if out.len() < limit && !nu_bitmap.is_empty() {
        if let Some(missing) = nonunique.and_then(|nu| nu.stat_missing.get(nu_stat)) {
            let matched = nu_bitmap & missing;
            push_slice(&mut out, &mut remaining_skip, &matched, PickedCard::NonUnique);
        }
    }

    out
}

fn page_by_family_sort(
    unique: &UniquesIndex,
    nonunique: Option<&NonUniqueQueryIndex>,
    u_bitmap: &RoaringBitmap,
    nu_bitmap: &RoaringBitmap,
    sort: &CardsSort,
    offset: usize,
    limit: usize,
) -> Vec<PickedCard> {
    let fsi = unique.family_sort_index();

    let nu_ordered: Vec<u32> = if let (Some(nu), false) = (nonunique, nu_bitmap.is_empty()) {
        let base: Vec<u32> = match (&sort.field, sort.dir) {
            (SortField::SetDate, SortDir::Asc) => nu
                .sort_index
                .set_date_asc
                .iter()
                .copied()
                .filter(|&i| nu_bitmap.contains(i))
                .collect(),
            (SortField::SetDate, SortDir::Desc) => nu
                .sort_index
                .set_date_desc
                .iter()
                .copied()
                .filter(|&i| nu_bitmap.contains(i))
                .collect(),
            (SortField::Collector, dir) => {
                let mut v: Vec<u32> = nu
                    .sort_index
                    .collector_asc
                    .iter()
                    .copied()
                    .filter(|&i| nu_bitmap.contains(i))
                    .collect();
                if dir == SortDir::Desc {
                    v.reverse();
                }
                v
            }
            (SortField::Reference, dir) => {
                let mut v: Vec<u32> = nu
                    .sort_index
                    .reference_asc
                    .iter()
                    .copied()
                    .filter(|&i| nu_bitmap.contains(i))
                    .collect();
                if dir == SortDir::Desc {
                    v.reverse();
                }
                v
            }
            (SortField::Name(locale), dir) => {
                let src = nu
                    .sort_index
                    .name_asc_by_locale
                    .get(locale.as_str())
                    .or_else(|| nu.sort_index.name_asc_by_locale.get("en_US"))
                    .unwrap_or(&nu.sort_index.reference_asc);
                let mut v: Vec<u32> = src.iter().copied().filter(|&i| nu_bitmap.contains(i)).collect();
                if dir == SortDir::Desc {
                    v.reverse();
                }
                v
            }
            _ => Vec::new(),
        };
        base
    } else {
        Vec::new()
    };

    // Fast path: nonunique-only (the primary "Toutes les cartes" browsing case!).
    if u_bitmap.is_empty() {
        return nu_ordered
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(PickedCard::NonUnique)
            .collect();
    }

    let (u_ranges, reverse_ranges, reverse_within): (&[_], bool, bool) = match (&sort.field, sort.dir) {
        (SortField::SetDate, SortDir::Asc) => (&fsi.set_date_asc, false, false),
        (SortField::SetDate, SortDir::Desc) => (&fsi.set_date_desc, false, false),
        (SortField::Collector, SortDir::Asc) => (&fsi.collector_asc, false, false),
        (SortField::Collector, SortDir::Desc) => (&fsi.collector_asc, true, true),
        (SortField::Reference, SortDir::Asc) => (&fsi.reference_asc, false, false),
        (SortField::Reference, SortDir::Desc) => (&fsi.reference_asc, true, true),
        (SortField::Name(locale), SortDir::Asc) => (
            fsi.name_asc_by_locale
                .get(locale.as_str())
                .or_else(|| fsi.name_asc_by_locale.get("en_US"))
                .unwrap_or(&fsi.reference_asc),
            false,
            false,
        ),
        (SortField::Name(locale), SortDir::Desc) => (
            fsi.name_asc_by_locale
                .get(locale.as_str())
                .or_else(|| fsi.name_asc_by_locale.get("en_US"))
                .unwrap_or(&fsi.reference_asc),
            true,
            true,
        ),
        _ => return Vec::new(),
    };

    // Fast path: unique-only.
    if nu_ordered.is_empty() {
        let mut out = Vec::with_capacity(limit);
        let mut remaining_skip = offset as u64;
        let iter: Box<dyn Iterator<Item = &_>> = if reverse_ranges {
            Box::new(u_ranges.iter().rev())
        } else {
            Box::new(u_ranges.iter())
        };
        for range in iter {
            if out.len() >= limit {
                break;
            }
            let count = u_bitmap.range_cardinality(range.start..range.end);
            if count == 0 {
                continue;
            }
            if remaining_skip >= count {
                remaining_skip -= count;
                continue;
            }
            let need = limit - out.len();
            let skip = remaining_skip as usize;
            remaining_skip = 0;
            if reverse_within {
                out.extend(
                    u_bitmap
                        .range(range.start..range.end)
                        .rev()
                        .skip(skip)
                        .take(need)
                        .map(PickedCard::Unique),
                );
            } else {
                out.extend(
                    u_bitmap
                        .range(range.start..range.end)
                        .skip(skip)
                        .take(need)
                        .map(PickedCard::Unique),
                );
            }
        }
        return out;
    }

    // Two-way merge when BOTH nonunique and unique results are non-empty under family sort.
    let nu_ref = nonunique.expect("nu_ordered non-empty implies nonunique is Some");
    let mut nu_pos = 0usize;
    let mut u_range_pos = 0usize;
    let u_len = u_ranges.len();

    let mut current_u: Option<(crate::index::loader::FamilySpanRange, u64, usize)> = None;
    let mut remaining_skip = offset as u64;
    let mut out = Vec::with_capacity(limit);

    let catalog = unique.catalog();

    while out.len() < limit {
        // Advance `current_u` to next non-empty family span if needed.
        if current_u.is_none() {
            while u_range_pos < u_len {
                let r = if reverse_ranges {
                    u_ranges[u_len - 1 - u_range_pos]
                } else {
                    u_ranges[u_range_pos]
                };
                u_range_pos += 1;
                let cnt = u_bitmap.range_cardinality(r.start..r.end);
                if cnt > 0 {
                    current_u = Some((r, cnt, 0));
                    break;
                }
            }
        }

        let Some(&nu_idx) = nu_ordered.get(nu_pos) else {
            // Drain remaining unique ranges.
            if let Some((r, cnt, consumed)) = current_u.take() {
                let avail = cnt - (consumed as u64);
                if remaining_skip >= avail {
                    remaining_skip -= avail;
                } else {
                    let skip = consumed + (remaining_skip as usize);
                    remaining_skip = 0;
                    let need = limit - out.len();
                    if reverse_within {
                        out.extend(
                            u_bitmap
                                .range(r.start..r.end)
                                .rev()
                                .skip(skip)
                                .take(need)
                                .map(PickedCard::Unique),
                        );
                    } else {
                        out.extend(
                            u_bitmap
                                .range(r.start..r.end)
                                .skip(skip)
                                .take(need)
                                .map(PickedCard::Unique),
                        );
                    }
                }
                continue;
            }
            break;
        };

        let Some((u_range, u_cnt, u_consumed)) = current_u else {
            // Drain remaining non-uniques.
            let skip = (remaining_skip as usize).min(nu_ordered.len() - nu_pos);
            let need = limit - out.len();
            out.extend(
                nu_ordered[nu_pos + skip..]
                    .iter()
                    .take(need)
                    .copied()
                    .map(PickedCard::NonUnique),
            );
            break;
        };

        let ord = match catalog.family_for_bit(u_range.start) {
            Ok(fam) => compare_nu_vs_family(nu_ref, nu_idx, catalog, fam, &sort.field, sort.dir),
            Err(_) => Ordering::Less,
        };

        if ord != Ordering::Greater {
            nu_pos += 1;
            if remaining_skip > 0 {
                remaining_skip -= 1;
            } else {
                out.push(PickedCard::NonUnique(nu_idx));
            }
        } else {
            let avail = u_cnt - (u_consumed as u64);
            if remaining_skip >= avail {
                remaining_skip -= avail;
                current_u = None;
            } else {
                let skip = u_consumed + (remaining_skip as usize);
                remaining_skip = 0;
                let need = limit - out.len();
                let taken: Vec<u32> = if reverse_within {
                    u_bitmap
                        .range(u_range.start..u_range.end)
                        .rev()
                        .skip(skip)
                        .take(need)
                        .collect()
                } else {
                    u_bitmap
                        .range(u_range.start..u_range.end)
                        .skip(skip)
                        .take(need)
                        .collect()
                };
                let new_consumed = skip + taken.len();
                out.extend(taken.into_iter().map(PickedCard::Unique));
                if (new_consumed as u64) >= u_cnt {
                    current_u = None;
                } else {
                    current_u = Some((u_range, u_cnt, new_consumed));
                }
            }
        }
    }

    out
}

fn compare_nu_vs_family(
    nu: &NonUniqueQueryIndex,
    nu_idx: u32,
    catalog: &index_core::catalog::Catalog,
    fam: &index_core::catalog::FamilyEntry,
    field: &SortField,
    dir: SortDir,
) -> Ordering {
    let nu_ref = nu.catalog.reference_for_index(nu_idx).unwrap_or("");
    let nu_meta: Option<&NonUniquePrintMeta> = nu.print_for(nu_idx);
    let nu_set = nu_meta.map(|m| m.set.as_str()).unwrap_or("");
    let nu_faction = nu_meta.map(|m| m.faction.as_str()).unwrap_or("");
    let u_set = fam.source_set.as_deref().unwrap_or(&catalog.set);

    let nu_num = nu_meta
        .and_then(|m| m.collector_number.as_deref())
        .and_then(|s| s.split('-').nth(1))
        .and_then(|seg| seg.parse::<u32>().ok())
        .unwrap_or(u32::MAX);
    let u_col_str = fam
        .collector_family_number
        .as_deref()
        .unwrap_or(&fam.family_number);
    let u_num = u_col_str.parse::<u32>().unwrap_or(u32::MAX);

    match field {
        SortField::SetDate => {
            if dir == SortDir::Desc {
                (
                    Reverse(set_release_rank(nu_set)),
                    faction_sort_rank(nu_faction),
                    nu_num,
                    nu_ref,
                )
                    .cmp(&(
                        Reverse(set_release_rank(u_set)),
                        faction_sort_rank(&fam.faction),
                        u_num,
                        fam.first_reference.as_str(),
                    ))
            } else {
                (
                    set_release_rank(nu_set),
                    faction_sort_rank(nu_faction),
                    nu_num,
                    nu_ref,
                )
                    .cmp(&(
                        set_release_rank(u_set),
                        faction_sort_rank(&fam.faction),
                        u_num,
                        fam.first_reference.as_str(),
                    ))
            }
        }
        SortField::Collector => {
            let nu_code = nu_meta
                .and_then(|m| m.set_code.as_deref())
                .unwrap_or(nu_set);
            let u_code = fam.set.code.as_deref().unwrap_or(u_set);
            let ord = (
                nu_code,
                faction_sort_rank(nu_faction),
                nu_num,
                set_release_rank(nu_set),
                nu_ref,
            )
                .cmp(&(
                    u_code,
                    faction_sort_rank(&fam.faction),
                    u_num,
                    set_release_rank(u_set),
                    fam.first_reference.as_str(),
                ));
            if dir == SortDir::Desc {
                ord.reverse()
            } else {
                ord
            }
        }
        SortField::Reference => {
            let ord = nu_ref.cmp(&fam.first_reference);
            if dir == SortDir::Desc {
                ord.reverse()
            } else {
                ord
            }
        }
        SortField::Name(locale) => {
            let nu_name = nu
                .normalized_names
                .get(nu_idx as usize)
                .and_then(|m| {
                    m.get(locale.as_str())
                        .or_else(|| m.get("en_US"))
                        .or_else(|| m.values().next())
                })
                .map(String::as_str)
                .unwrap_or(nu_ref);
            let u_raw = fam
                .name
                .get(locale.as_str())
                .or_else(|| fam.name.get("en_US"))
                .or_else(|| fam.name.values().next())
                .map(String::as_str)
                .unwrap_or("");
            let u_name = normalize_name_for_search(u_raw);
            let ord = (nu_name, set_release_rank(nu_set), nu_ref).cmp(&(
                u_name.as_str(),
                set_release_rank(u_set),
                fam.first_reference.as_str(),
            ));
            if dir == SortDir::Desc {
                ord.reverse()
            } else {
                ord
            }
        }
        _ => Ordering::Equal,
    }
}

/// Rank-based window spanning `first` (`0..first.len()`) followed by `second`
/// (`first.len()..total`), pulling `[offset .. offset + limit)` into `(picked_first, picked_second)`
/// via `select` + bounded range iteration.
fn window_across(
    first: &RoaringBitmap,
    second: &RoaringBitmap,
    offset: u64,
    limit: usize,
) -> (Vec<u32>, Vec<u32>) {
    if limit == 0 {
        return (Vec::new(), Vec::new());
    }
    let first_len = first.len();
    let mut picked_first = Vec::new();
    let mut picked_second = Vec::new();

    if offset < first_len {
        if let Some(start) = first.select(offset as u32) {
            picked_first.extend(first.range(start..).take(limit));
        }
    }

    let remaining = limit - picked_first.len();
    if remaining > 0 {
        let second_offset = offset.saturating_sub(first_len);
        if let Some(start) = u32::try_from(second_offset)
            .ok()
            .and_then(|rank| second.select(rank))
        {
            picked_second.extend(second.range(start..).take(remaining));
        }
    }

    (picked_first, picked_second)
}

pub(crate) fn parse_request(
    params: &QueryMultiMap,
    index: &UniquesIndex,
    nonunique: Option<&NonUniqueQueryIndex>,
    format_index: &FormatIndex,
    formats_enabled: bool,
    collections: &CollectionStore,
) -> ApiResult<SearchRequest> {
    let q = parse_q(params);
    let name = parse_name(params);
    let effect_text = parse_nonempty_param(params, &["effectText", "effect_text"]);
    let effects = parse_effect_slots(params)?;
    let effect_mode = parse_effect_mode(params)?;
    let support_t = parse_id_list(params, "support[t]")?;
    let support_c = parse_id_list(params, "support[c]")?;
    let support_o = parse_id_list(params, "support[o]")?;
    if !effects.is_empty() || !support_t.is_empty() || !support_c.is_empty() || !support_o.is_empty()
    {
        let ability_filters = AbilityFilters {
            effects: effects.clone(),
            effect_mode,
            support_t: support_t.clone(),
            support_c: support_c.clone(),
            support_o: support_o.clone(),
        };
        validate_idgd_types_unified(index, nonunique, &ability_filters)?;
    }

    let factions = parse_factions_str(params)?;
    let sets = parse_all_sets(params)?;
    let rarities = parse_rarities(params)?;
    let card_types = parse_card_types(params);
    let sub_types = parse_sub_types(params);
    let keywords = parse_keywords(params);
    let keyword_mode = parse_keyword_mode(params)?;
    let products = parse_products(params)?;
    let variations = parse_variations(params)?;
    let serialized = parse_opt_bool_any(params, &["serialized", "isSerialized"])?;
    let family = match get_first(params, "family") {
        Some(raw) if !raw.trim().is_empty() => Some(
            raw.trim()
                .parse::<i64>()
                .map_err(|_| bad_request(format!("invalid 'family': '{raw}'")))?,
        ),
        _ => None,
    };
    let banned = parse_opt_bool_any(params, &["banned", "isBanned"])?;
    let errated = parse_opt_bool_any(params, &["errated", "isErrated"])?;
    let suspended = parse_opt_bool_any(params, &["suspended", "isSuspended"])?;

    let main_cost = parse_cost_predicate(params, "mainCost")?;
    let recall_cost = parse_cost_predicate(params, "recallCost")?;
    let mountain_power = parse_cost_predicate(params, "mountainPower")?;
    let ocean_power = parse_cost_predicate(params, "oceanPower")?;
    let forest_power = parse_cost_predicate(params, "forestPower")?;
    let cost_relation = parse_cost_relation(params)?;
    let has_no_effect = parse_has_no_effect(params)?;
    let has_zero_power = parse_has_zero_power(params)?;
    let artists = parse_artists(params);
    let refs = parse_refs(params);
    let collector_numbers = parse_collector_numbers(params);

    let format = resolve_format_filter(parse_format(params), formats_enabled, format_index)?;
    let collection = resolve_collection_filter(parse_collection(params), collections)?;
    let kind = parse_kind(params)?;
    let sort = parse_sort(params)?;

    let limit = match get_first(params, "limit").or_else(|| get_first(params, "itemsPerPage")) {
        Some(s) => {
            let n = parse_usize("limit", s)?;
            if n == 0 || n > MAX_LIMIT {
                return Err(bad_request(format!(
                    "'limit' must be between 1 and {MAX_LIMIT}, got {n}"
                )));
            }
            n
        }
        None => DEFAULT_LIMIT,
    };

    let page = match get_first(params, "page") {
        Some(s) if !s.trim().is_empty() => {
            let n = parse_u32("page", s.trim())?;
            if n == 0 {
                return Err(bad_request("'page' must be >= 1"));
            }
            Some(n)
        }
        _ => None,
    };
    let offset = match get_first(params, "cursor") {
        Some(s) if !s.trim().is_empty() => Some(parse_usize("cursor", s.trim())?),
        _ => None,
    };
    if page.is_some() && offset.is_some() {
        return Err(bad_request("cannot combine 'page' and 'cursor'"));
    }

    let debug_bga_trigram = has_any(params, "debug_bga_trigram");

    Ok(SearchRequest {
        q,
        name,
        effect_text,
        effects,
        effect_mode,
        support_t,
        support_c,
        support_o,
        factions,
        sets,
        rarities,
        card_types,
        sub_types,
        keywords,
        keyword_mode,
        products,
        variations,
        serialized,
        family,
        banned,
        errated,
        suspended,
        main_cost,
        recall_cost,
        mountain_power,
        ocean_power,
        forest_power,
        cost_relation,
        has_no_effect,
        has_zero_power,
        artists,
        refs,
        collector_numbers,
        format,
        collection,
        kind,
        sort,
        page,
        offset,
        limit,
        debug_bga_trigram,
    })
}

fn parse_nonempty_param(params: &QueryMultiMap, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(raw) = get_first(params, k) {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn parse_opt_bool_any(params: &QueryMultiMap, keys: &[&str]) -> ApiResult<Option<bool>> {
    for k in keys {
        if let Some(raw) = get_first(params, k) {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            return match trimmed.to_ascii_lowercase().as_str() {
                "true" | "1" => Ok(Some(true)),
                "false" | "0" => Ok(Some(false)),
                _ => Err(bad_request(format!(
                    "invalid '{k}': expected true/false, got '{raw}'"
                ))),
            };
        }
    }
    Ok(None)
}

fn collect_multi_values(params: &QueryMultiMap, keys: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for key in keys {
        if let Some(values) = params.get(*key) {
            for raw in values {
                for part in raw.split(',') {
                    let s = part.trim();
                    if !s.is_empty() {
                        out.push(s.to_string());
                    }
                }
            }
        }
    }
    out
}

fn parse_factions_str(params: &QueryMultiMap) -> ApiResult<Vec<String>> {
    let mut out = Vec::new();
    for v in collect_multi_values(params, &["faction[]", "faction", "factions[]", "factions"]) {
        let upper = v.to_ascii_uppercase();
        if !matches!(upper.as_str(), "AX" | "BR" | "LY" | "MU" | "OR" | "YZ" | "NE") {
            return Err(bad_request(format!("unknown faction '{v}'")));
        }
        if !out.contains(&upper) {
            out.push(upper);
        }
    }
    Ok(out)
}

fn parse_all_sets(params: &QueryMultiMap) -> ApiResult<Vec<String>> {
    let mut out = parse_sets(params)?;
    for s in collect_multi_values(params, &["cardSet[]", "cardSet", "edition[]", "edition"]) {
        let upper = s.trim().to_ascii_uppercase();
        if !upper.is_empty() && !out.contains(&upper) {
            out.push(upper);
        }
    }
    Ok(out)
}

fn parse_rarities(params: &QueryMultiMap) -> ApiResult<Vec<String>> {
    let mut out = Vec::new();
    for v in collect_multi_values(params, &["rarity[]", "rarity"]) {
        let code = match v.to_ascii_uppercase().as_str() {
            "C" | "COMMON" => "C",
            "R" | "RARE" => "R",
            "E" | "EXALTED" => "E",
            "U" | "UNIQUE" => "U",
            _ => return Err(bad_request(format!("unknown rarity '{v}'"))),
        }
        .to_string();
        if !out.contains(&code) {
            out.push(code);
        }
    }
    Ok(out)
}

fn parse_card_types(params: &QueryMultiMap) -> Vec<String> {
    let mut out = Vec::new();
    for v in collect_multi_values(
        params,
        &["cardType[]", "cardType", "type[]", "type", "cardTypes[]"],
    ) {
        let upper = match v.to_ascii_uppercase().as_str() {
            "PERMANENT" => "LANDMARK_PERMANENT".to_string(),
            other => other.to_string(),
        };
        if !out.contains(&upper) {
            out.push(upper);
        }
    }
    out
}

fn parse_keywords(params: &QueryMultiMap) -> Vec<String> {
    let mut out = Vec::new();
    for v in collect_multi_values(params, &["keywords[]", "keywords", "keyword[]", "keyword"]) {
        let upper = v.to_ascii_uppercase();
        let mapped = match upper.as_str() {
            "SUPPORT" => Some("ECHO".to_string()),
            _ => canonical_keyword(&upper),
        };
        if let Some(canon) = mapped {
            if !out.contains(&canon) {
                out.push(canon);
            }
        }
    }
    out
}

fn parse_keyword_mode(params: &QueryMultiMap) -> ApiResult<KeywordMode> {
    match get_first(params, "keywordMode") {
        Some(raw) if !raw.trim().is_empty() => match raw.trim().to_ascii_lowercase().as_str() {
            "or" => Ok(KeywordMode::Or),
            "and" => Ok(KeywordMode::And),
            _ => Err(bad_request(format!(
                "invalid 'keywordMode': expected or|and, got '{raw}'"
            ))),
        },
        _ => Ok(KeywordMode::Or),
    }
}

fn parse_products(params: &QueryMultiMap) -> ApiResult<Vec<String>> {
    let mut out = Vec::new();
    for v in collect_multi_values(params, &["product[]", "product"]) {
        let upper = v.to_ascii_uppercase();
        if !matches!(upper.as_str(), "B" | "A" | "P") {
            return Err(bad_request(format!("unknown product '{v}'")));
        }
        if !out.contains(&upper) {
            out.push(upper);
        }
    }
    Ok(out)
}

fn parse_variations(params: &QueryMultiMap) -> ApiResult<Vec<VariationKind>> {
    let mut out = Vec::new();
    for v in collect_multi_values(params, &["variation[]", "variation"]) {
        let kind = match v.to_ascii_lowercase().as_str() {
            "standard" | "base" => VariationKind::Standard,
            "altart" | "alt_art" | "alt-art" | "alt" => VariationKind::AltArt,
            "promo" => VariationKind::Promo,
            "serialized" => VariationKind::Serialized,
            _ => return Err(bad_request(format!("unknown variation '{v}'"))),
        };
        if !out.contains(&kind) {
            out.push(kind);
        }
    }
    Ok(out)
}

fn parse_kind(params: &QueryMultiMap) -> ApiResult<KindFilter> {
    match get_first(params, "kind") {
        Some(raw) if !raw.trim().is_empty() => match raw.trim().to_ascii_lowercase().as_str() {
            "all" | "both" => Ok(KindFilter::All),
            "unique" | "uniques" => Ok(KindFilter::UniqueOnly),
            "nonunique" | "non-unique" | "non_unique" | "nonuniques" => {
                Ok(KindFilter::NonUniqueOnly)
            }
            _ => Err(bad_request(format!(
                "invalid 'kind': expected all|unique|nonunique, got '{raw}'"
            ))),
        },
        _ => Ok(KindFilter::All),
    }
}

fn build_nonunique_bitmap(
    nu: &NonUniqueQueryIndex,
    unique: &UniquesIndex,
    format_index: &FormatIndex,
    collections: &CollectionStore,
    req: &SearchRequest,
) -> ApiResult<RoaringBitmap> {
    let total = nu.total();
    if total == 0 {
        return Ok(RoaringBitmap::new());
    }
    let mut acc: Option<RoaringBitmap> = None;

    // Structured effect[N][t|c|o] + matchCount + effectMode across main lines (1..=max_main_lines).
    let effect_bitmaps: Vec<RoaringBitmap> = req
        .effects
        .iter()
        .filter_map(|slot| {
            nonunique_effect_slot_bitmap(
                nu,
                unique,
                &slot.t,
                &slot.c,
                &slot.o,
                slot.match_count,
            )
        })
        .collect();
    if let Some(bmp) = combine_effect_bitmaps(&effect_bitmaps, req.effect_mode) {
        intersect(&mut acc, bmp);
    }

    // Structured support[t|c|o] on echo/support line.
    if let Some(bmp) = nonunique_intersect_buckets_on_line(
        nu,
        unique,
        NonUniqueEffectLine::Echo,
        &req.support_t,
        &req.support_c,
        &req.support_o,
    ) {
        intersect(&mut acc, bmp);
    }

    let mut format_exclude: Option<RoaringBitmap> = None;
    if let Some(id) = &req.format {
        if let Some(loaded) = format_index.get(id) {
            match &loaded.status {
                FormatLoadStatus::Ready {
                    negated: true,
                    nonunique_bitmap,
                    ..
                } => format_exclude = Some(nonunique_bitmap.clone()),
                FormatLoadStatus::Ready {
                    negated: false,
                    nonunique_bitmap,
                    ..
                } => intersect(&mut acc, nonunique_bitmap.clone()),
                FormatLoadStatus::Failed => {}
            }
        }
    }

    if !req.factions.is_empty() {
        let mut combined = RoaringBitmap::new();
        for f in &req.factions {
            if let Some(bmp) = nu.faction.get(f) {
                combined |= bmp;
            }
        }
        intersect(&mut acc, combined);
    }

    if !req.rarities.is_empty() {
        let mut combined = RoaringBitmap::new();
        for r in &req.rarities {
            if r != "U" {
                if let Some(bmp) = nu.rarity.get(r) {
                    combined |= bmp;
                }
            }
        }
        intersect(&mut acc, combined);
    }

    if !req.sets.is_empty() {
        let mut combined = RoaringBitmap::new();
        for s in &req.sets {
            if let Some(bmp) = nu.set.get(s.as_str()) {
                combined |= bmp;
            } else {
                // Also support matching display set code (`BTG` -> `CORE`, etc.).
                let upper = s.to_ascii_uppercase();
                for (idx, meta) in nu.prints.iter().enumerate() {
                    if meta.set.eq_ignore_ascii_case(&upper)
                        || meta
                            .set_code
                            .as_deref()
                            .is_some_and(|c| c.eq_ignore_ascii_case(&upper))
                    {
                        combined.insert(idx as u32);
                    }
                }
            }
        }
        intersect(&mut acc, combined);
    }

    if !req.card_types.is_empty() {
        let mut combined = RoaringBitmap::new();
        for t in &req.card_types {
            if let Some(bmp) = nu.card_type.get(t) {
                combined |= bmp;
            }
        }
        intersect(&mut acc, combined);
    }

    if !req.sub_types.is_empty() {
        let mut combined = RoaringBitmap::new();
        for st in &req.sub_types {
            if let Some(bmp) = nu.subtype.get(st) {
                combined |= bmp;
            }
        }
        intersect(&mut acc, combined);
    }

    if !req.keywords.is_empty() {
        let combined = match req.keyword_mode {
            KeywordMode::Or => {
                let mut u = RoaringBitmap::new();
                for kw in &req.keywords {
                    if let Some(bmp) = nu.keywords.get(kw) {
                        u |= bmp;
                    }
                }
                u
            }
            KeywordMode::And => {
                let mut first: Option<RoaringBitmap> = None;
                for kw in &req.keywords {
                    let bmp = nu.keywords.get(kw).cloned().unwrap_or_default();
                    intersect(&mut first, bmp);
                }
                first.unwrap_or_default()
            }
        };
        intersect(&mut acc, combined);
    }

    if !req.products.is_empty() {
        let mut combined = RoaringBitmap::new();
        for p in &req.products {
            if let Some(bmp) = nu.product.get(p) {
                combined |= bmp;
            }
        }
        intersect(&mut acc, combined);
    }

    if !req.variations.is_empty() {
        // Variation = product letter (B/A/P), except the prints listed in
        // `index_core::variation_overrides` (e.g. Kuraokami, Sofia: `P` but base cards).
        let mut combined = RoaringBitmap::new();
        for v in &req.variations {
            match v {
                VariationKind::Standard => combined |= nu.variation_standard(),
                VariationKind::AltArt => combined |= nu.variation_alt_art(),
                VariationKind::Promo => combined |= nu.variation_promo(),
                VariationKind::Serialized => {
                    if let Some(ser_bmp) = &nu.serialized {
                        combined |= ser_bmp;
                    }
                }
            }
        }
        intersect(&mut acc, combined);
    }

    if let Some(serialized) = req.serialized {
        let empty = RoaringBitmap::new();
        let ser_ref = nu.serialized.as_ref().unwrap_or(&empty);
        intersect(&mut acc, bool_bitmap(serialized, ser_ref, total));
    }

    if let Some(family_id) = req.family {
        intersect(
            &mut acc,
            nu.family
                .get(&family_id.to_string())
                .cloned()
                .unwrap_or_default(),
        );
    }

    if let Some(banned) = req.banned {
        intersect(&mut acc, bool_bitmap(banned, &nu.banned, total));
    }
    if let Some(errated) = req.errated {
        intersect(&mut acc, bool_bitmap(errated, &nu.errated, total));
    }
    if let Some(suspended) = req.suspended {
        intersect(&mut acc, bool_bitmap(suspended, &nu.suspended, total));
    }

    if let Some(rel) = req.cost_relation {
        let bmp = match rel {
            CostRelation::Equal => &nu.cost_relations.equal,
            CostRelation::MainHigher => &nu.cost_relations.main_higher,
            CostRelation::RecallHigher => &nu.cost_relations.recall_higher,
        };
        intersect(&mut acc, bmp.clone());
    }

    if let Some(has_no_effect) = req.has_no_effect {
        intersect(&mut acc, bool_bitmap(has_no_effect, &nu.has_no_effect, total));
    }

    if req.has_zero_power {
        intersect(&mut acc, nu.bitmap_for_zero_power());
    }

    if !req.artists.is_empty() {
        intersect(&mut acc, nu.bitmap_for_artists(&req.artists));
    }

    if let Some(id) = &req.collection {
        if let Some(bitmaps) = collections.get(id) {
            intersect(&mut acc, bitmaps.nonunique.clone());
        }
    }

    if !req.refs.is_empty() {
        let mut combined = RoaringBitmap::new();
        for r in &req.refs {
            if let Some(idx) = nu.resolve_reference(r) {
                combined.insert(idx);
            }
        }
        intersect(&mut acc, combined);
    }

    if !req.collector_numbers.is_empty() {
        let mut combined = RoaringBitmap::new();
        for cn in &req.collector_numbers {
            let key = cn.trim().to_ascii_uppercase();
            if let Some(list) = nu.collector_lookup.get(&key) {
                for &idx in list {
                    combined.insert(idx);
                }
            }
        }
        intersect(&mut acc, combined);
    }

    if let Some(q) = &req.q {
        if !q.trim().is_empty() {
            intersect(&mut acc, nu.bitmap_for_free_text(q));
        }
    }

    if let Some(name) = &req.name {
        if !name.trim().is_empty() {
            intersect(&mut acc, nu.bitmap_for_name_or_ident(name));
        }
    }

    if let Some(effect_text) = &req.effect_text {
        intersect(&mut acc, nu.bitmap_for_effect_contains(effect_text));
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
            intersect(&mut acc, bitmap);
        }
    }

    let mut out = acc.unwrap_or_else(|| {
        let mut all = RoaringBitmap::new();
        all.insert_range(0..total);
        all
    });
    if let Some(exclude) = format_exclude {
        out -= exclude;
    }
    Ok(out)
}

fn nonunique_effect_slot_bitmap(
    nu: &NonUniqueQueryIndex,
    unique: &UniquesIndex,
    triggers: &[u32],
    conditions: &[u32],
    outputs: &[u32],
    match_count: u8,
) -> Option<RoaringBitmap> {
    if triggers.is_empty() && conditions.is_empty() && outputs.is_empty() {
        return None;
    }
    let mut line_matches: Vec<RoaringBitmap> = Vec::new();
    for line in 1..=nu.max_main_lines {
        if let Some(bmp) = nonunique_intersect_buckets_on_line(
            nu,
            unique,
            NonUniqueEffectLine::Main(line),
            triggers,
            conditions,
            outputs,
        ) {
            if !bmp.is_empty() {
                line_matches.push(bmp);
            }
        }
    }
    if match_count <= 1 {
        let mut out = RoaringBitmap::new();
        for bmp in line_matches {
            out |= bmp;
        }
        return Some(out);
    }
    let mut counts: BTreeMap<u32, u8> = BTreeMap::new();
    for bmp in &line_matches {
        for idx in bmp.iter() {
            *counts.entry(idx).or_insert(0) += 1;
        }
    }
    let out: RoaringBitmap = counts
        .into_iter()
        .filter_map(|(idx, cnt)| (cnt >= match_count).then_some(idx))
        .collect();
    Some(out)
}

pub(crate) fn nonunique_union_on_line(
    nu: &NonUniqueQueryIndex,
    unique: &UniquesIndex,
    line: NonUniqueEffectLine,
    ids: &[u32],
) -> RoaringBitmap {
    let mut out = RoaringBitmap::new();
    for &id in ids {
        for eq_id in unique.equivalent_id_gds(id) {
            if let Some(bmp) = nu.id_gd_per_line.get(&(eq_id, line)) {
                out |= bmp;
            }
        }
    }
    out
}

fn nonunique_intersect_buckets_on_line(
    nu: &NonUniqueQueryIndex,
    unique: &UniquesIndex,
    line: NonUniqueEffectLine,
    triggers: &[u32],
    conditions: &[u32],
    outputs: &[u32],
) -> Option<RoaringBitmap> {
    let mut groups: Vec<RoaringBitmap> = Vec::new();
    if !triggers.is_empty() {
        groups.push(nonunique_union_on_line(nu, unique, line, triggers));
    }
    if !conditions.is_empty() {
        groups.push(nonunique_union_on_line(nu, unique, line, conditions));
    }
    if !outputs.is_empty() {
        groups.push(nonunique_union_on_line(nu, unique, line, outputs));
    }
    let mut it = groups.into_iter();
    let mut bmp = it.next()?;
    for g in it {
        bmp &= g;
    }
    Some(bmp)
}

fn build_unique_search_bitmap(
    unique: &UniquesIndex,
    format_index: &FormatIndex,
    collections: &CollectionStore,
    req: &SearchRequest,
) -> ApiResult<RoaringBitmap> {
    let Some(mut cards_req) = req.to_cards_request() else {
        return Ok(RoaringBitmap::new());
    };
    cards_req.sort = None;
    let mut bitmap =
        build_bitmap(unique, format_index, collections, &cards_req).map_err(map_query_error)?;
    let total_bit_span = unique.catalog().total_bit_span;
    if let Some(effect_text) = &req.effect_text {
        bitmap &= unique.bitmap_for_effect_text(effect_text);
    }
    if let Some(banned) = req.banned {
        bitmap &= bool_bitmap(banned, unique.banned(), total_bit_span);
    }
    if let Some(errated) = req.errated {
        bitmap &= bool_bitmap(errated, unique.errated(), total_bit_span);
    }
    if let Some(suspended) = req.suspended {
        bitmap &= bool_bitmap(suspended, unique.suspended(), total_bit_span);
    }
    if let Some(family_id) = req.family {
        let mut fam_bmp = RoaringBitmap::new();
        for family in &unique.catalog().families {
            if family.card_family_id == Some(family_id) {
                let end = family.start_bit.saturating_add(family.max_unique_id);
                fam_bmp.insert_range(family.start_bit..end);
            }
        }
        bitmap &= fam_bmp;
    }
    if !req.keywords.is_empty() {
        bitmap &= unique_keyword_bitmap(unique, &req.keywords, req.keyword_mode);
    }
    Ok(bitmap)
}

fn unique_keyword_bitmap(
    unique: &UniquesIndex,
    keywords: &[String],
    mode: KeywordMode,
) -> RoaringBitmap {
    let single_kw_bitmap = |canon: &str| -> RoaringBitmap {
        if canon == "ECHO" {
            let mut echo = RoaringBitmap::new();
            for entry in &unique.idgd_catalog().entries {
                if entry.ec.is_some() {
                    if let Some(b) = unique.id_gd_per_line().get(&(entry.id_gd, EffectLine::Ec)) {
                        echo |= b;
                    }
                }
            }
            return echo;
        }
        let mut matching_ids = Vec::new();
        for entry in &unique.idgd_catalog().entries {
            let Some(en) = entry.translations.get("en_US").map(|t| t.text.as_str()) else {
                continue;
            };
            if text_has_canonical_keyword(en, canon) {
                matching_ids.push(entry.id_gd);
            }
        }
        let mut out = RoaringBitmap::new();
        for id_gd in matching_ids {
            if let Some(bmp) = unique.id_gd_whole().get(&id_gd) {
                out |= bmp;
            }
        }
        out
    };

    match mode {
        KeywordMode::Or => {
            let mut u = RoaringBitmap::new();
            for kw in keywords {
                u |= single_kw_bitmap(kw);
            }
            u
        }
        KeywordMode::And => {
            let mut acc: Option<RoaringBitmap> = None;
            for kw in keywords {
                intersect(&mut acc, single_kw_bitmap(kw));
            }
            acc.unwrap_or_default()
        }
    }
}

fn intersect(acc: &mut Option<RoaringBitmap>, bitmap: RoaringBitmap) {
    *acc = Some(match acc.take() {
        Some(current) => current & bitmap,
        None => bitmap,
    });
}

fn text_has_canonical_keyword(text: &str, expected_canon: &str) -> bool {
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            break;
        };
        let norm = after[..close].trim().replace(' ', "_");
        if !norm.is_empty()
            && norm
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            if let Some(canon) = canonical_keyword(&norm) {
                if canon == expected_canon {
                    return true;
                }
            }
        }
        rest = &after[close + 1..];
    }
    false
}

fn bool_bitmap(value: bool, flag_bitmap: &RoaringBitmap, total: u32) -> RoaringBitmap {
    if value {
        flag_bitmap.clone()
    } else {
        let mut all = RoaringBitmap::new();
        all.insert_range(0..total);
        all - flag_bitmap
    }
}

fn apply_cost_predicate(
    predicate: &CostPredicate,
    get_bucket: impl Fn(u8) -> RoaringBitmap,
) -> RoaringBitmap {
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
        let first = bitmap(0..30);
        let second = bitmap(100..200);
        let (a, b) = window_across(&first, &second, 0, 10);
        assert_eq!(a, (0..10).collect::<Vec<_>>());
        assert!(b.is_empty());
    }

    #[test]
    fn straddles_the_boundary() {
        let first = bitmap(0..30);
        let second = bitmap(1000..1100);
        let (a, b) = window_across(&first, &second, 20, 20);
        assert_eq!(a, (20..30).collect::<Vec<_>>());
        assert_eq!(b, (1000..1010).collect::<Vec<_>>());
    }

    #[test]
    fn entirely_within_second() {
        let first = bitmap(0..30);
        let second = bitmap(1000..1100);
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

    #[test]
    fn bracket_keyword_matching_avoids_prose_false_positives() {
        assert!(!super::text_has_canonical_keyword(
            "{D} Boost target.",
            "BOOSTED"
        ));
        assert!(super::text_has_canonical_keyword(
            "[BOOSTED].",
            "BOOSTED"
        ));
        assert!(super::text_has_canonical_keyword(
            "[AFTER_YOU].",
            "AFTER_YOU"
        ));
        assert!(super::text_has_canonical_keyword(
            "[AFTER YOU].",
            "AFTER_YOU"
        ));
        assert!(super::text_has_canonical_keyword(
            "[TOUGH 1].",
            "TOUGH"
        ));
    }
}
