use std::collections::{BTreeMap, HashMap};

use url::form_urlencoded;

use index_core::faction_index::Faction;

use crate::collections::CollectionStore;
use crate::formats::{FormatIndex, FormatLoadStatus};
use crate::http::api::error::{bad_request, collection_not_loaded, internal_server_error, ApiResult};
use crate::collections::validate_collection_id;
use crate::index::UniquesIndex;
use super::models::{
    AbilityFilters, CardsRequest, CardsSort, CompareOp, CostPredicate, CostRelation,
    EffectCombineMode, EffectSlotFilter, IdGdSelector, SortDir, SortField,
};


pub(crate) type QueryMultiMap = HashMap<String, Vec<String>>;

pub(crate) fn parse_query_multimap(query: Option<&str>) -> ApiResult<QueryMultiMap> {
    let mut out: QueryMultiMap = HashMap::new();
    let Some(query) = query else {
        return Ok(out);
    };

    for (k, v) in form_urlencoded::parse(query.as_bytes()) {
        out.entry(k.into_owned())
            .or_insert_with(Vec::new)
            .push(v.into_owned());
    }
    Ok(out)
}

pub(crate) fn get_first<'a>(params: &'a QueryMultiMap, key: &str) -> Option<&'a str> {
    params.get(key)?.first().map(|s| s.as_str())
}

pub(crate) fn has_any(params: &QueryMultiMap, key: &str) -> bool {
    params.get(key).is_some_and(|v| v.iter().any(|s| !s.trim().is_empty()))
}

pub(crate) fn parse_collection(params: &QueryMultiMap) -> Option<String> {
    let values = params.get("collection")?;
    let last = values
        .iter()
        .rev()
        .find_map(|v| {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })?;
    Some(last)
}

pub(crate) fn resolve_collection_filter(
    collection_id: Option<String>,
    collections: &CollectionStore,
) -> ApiResult<Option<String>> {
    let Some(id) = collection_id else {
        return Ok(None);
    };

    validate_collection_id(&id).map_err(bad_request)?;

    if collections.get(&id).is_none() {
        return Err(collection_not_loaded(id));
    }

    Ok(Some(id))
}

pub(crate) fn parse_format(params: &QueryMultiMap) -> Option<String> {
    let values = params.get("format")?;
    let last = values
        .iter()
        .rev()
        .find_map(|v| {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })?;
    Some(last)
}

pub(crate) fn resolve_format_filter(
    format_id: Option<String>,
    formats_enabled: bool,
    format_index: &FormatIndex,
) -> ApiResult<Option<String>> {
    let Some(id) = format_id else {
        return Ok(None);
    };

    if !formats_enabled {
        return Err(bad_request(format!("unknown format '{id}'")));
    }

    let Some(loaded) = format_index.get(&id) else {
        return Err(bad_request(format!("unknown format '{id}'")));
    };

    match &loaded.status {
        FormatLoadStatus::Ready { .. } => Ok(Some(id)),
        FormatLoadStatus::Failed => Err(internal_server_error("format failed to load")),
    }
}

pub(crate) fn parse_request(
    state: &UniquesIndex,
    format_index: &FormatIndex,
    formats_enabled: bool,
    collections: &CollectionStore,
    params: &QueryMultiMap,
) -> ApiResult<CardsRequest> {
    let limit = match get_first(params, "limit") {
        None => 50usize,
        Some(v) => parse_usize("limit", v)?,
    };
    if !(1..=200).contains(&limit) {
        return Err(bad_request("limit must be in range 1..=200".to_string()));
    }

    let cursor = match get_first(params, "cursor") {
        None => None,
        Some(v) => Some(parse_u32("cursor", v)?),
    };
    if let Some(c) = cursor {
        let total_bit_span = state.manifest().total_bit_span;
        if c >= total_bit_span {
            return Err(bad_request(format!(
                "cursor must be < total_bit_span ({total_bit_span})"
            )));
        }
    }

    let page = match get_first(params, "page") {
        None => None,
        Some(v) => Some(parse_u32("page", v)?),
    };
    if page.is_some_and(|p| p == 0) {
        return Err(bad_request("page must be >= 1".to_string()));
    }
    if cursor.is_some() && page.is_some() {
        return Err(bad_request(
            "cursor and page are mutually exclusive: page jumps directly to a page (via rank/select), cursor resumes after the last seen card_index".to_string(),
        ));
    }

    let mut filters = AbilityFilters::default();
    filters.effects = parse_effect_slots(params)?;
    filters.effect_mode = parse_effect_mode(params)?;
    filters.support_t = parse_id_list(params, "support[t]")?;
    filters.support_c = parse_id_list(params, "support[c]")?;
    filters.support_o = parse_id_list(params, "support[o]")?;

    let factions = parse_factions(params)?;
    let sets = parse_sets(params)?;
    let refs = parse_refs(params);
    let collector_numbers = parse_collector_numbers(params);
    for code in &sets {
        if !state.set_bitmaps().by_set.contains_key(code) {
            return Err(bad_request(format!("invalid set value '{code}'")));
        }
    }
    let main_cost = parse_cost_predicate(params, "mainCost")?;
    let recall_cost = parse_cost_predicate(params, "recallCost")?;
    let forest_power = parse_cost_predicate(params, "forestPower")?;
    let mountain_power = parse_cost_predicate(params, "mountainPower")?;
    let ocean_power = parse_cost_predicate(params, "oceanPower")?;
    let name = parse_name(params);
    let cost_relation = parse_cost_relation(params)?;
    let sub_types = parse_sub_types(params);
    let has_no_effect = parse_has_no_effect(params)?;
    let has_zero_power = parse_has_zero_power(params)?;
    let artists = parse_artists(params);
    let sort = parse_sort(params)?;
    let q = parse_q(params);
    let debug_bga_trigram = params.contains_key("debug_bga_trigram");
    let with_families = params.contains_key("withFamilies");

    validate_idgd_types(state, &filters)?;

    let format = resolve_format_filter(parse_format(params), formats_enabled, format_index)?;
    let collection = resolve_collection_filter(parse_collection(params), collections)?;

    Ok(CardsRequest {
        limit,
        cursor,
        page,
        sort,
        filters,
        factions,
        sets,
        refs,
        collector_numbers,
        main_cost,
        recall_cost,
        forest_power,
        mountain_power,
        ocean_power,
        name,
        cost_relation,
        sub_types,
        has_no_effect,
        has_zero_power,
        artists,
        q,
        debug_bga_trigram,
        with_families,
        format,
        collection,
    })
}

pub(crate) fn parse_name(params: &QueryMultiMap) -> Option<String> {
    let raw = get_first(params, "name")?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_sort_dir(raw: &str, full_value: &str) -> ApiResult<SortDir> {
    match raw.to_ascii_lowercase().as_str() {
        "asc" => Ok(SortDir::Asc),
        "desc" => Ok(SortDir::Desc),
        _ => Err(bad_request(format!(
            "invalid sort value '{full_value}': expected direction ':asc' or ':desc'"
        ))),
    }
}

fn canonical_sort_locale(raw: &str) -> &'static str {
    match raw.to_ascii_lowercase().as_str() {
        "fr" | "fr_fr" => "fr_FR",
        "de" | "de_de" => "de_DE",
        "es" | "es_es" => "es_ES",
        "it" | "it_it" => "it_IT",
        _ => "en_US",
    }
}

pub(crate) fn parse_sort(params: &QueryMultiMap) -> ApiResult<Option<CardsSort>> {
    let Some(raw) = get_first(params, "sort") else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
        return Ok(None);
    }

    let parts: Vec<&str> = trimmed.split(':').collect();
    let (field_part, locale_part, dir_part) = match parts.as_slice() {
        [field, dir] => (*field, None, *dir),
        [field, locale, dir] if field.eq_ignore_ascii_case("name") => (*field, Some(*locale), *dir),
        _ => {
            return Err(bad_request(format!(
                "invalid sort value '{trimmed}': expected '<field>:<asc|desc>' or 'name:<locale>:<asc|desc>'"
            )));
        }
    };

    let dir = parse_sort_dir(dir_part, trimmed)?;
    let field = match field_part.to_ascii_lowercase().as_str() {
        "maincost" | "main_cost" | "mana" => SortField::MainCost,
        "recallcost" | "recall_cost" | "reserve" => SortField::RecallCost,
        "forestpower" | "forest_power" | "forest" => SortField::ForestPower,
        "mountainpower" | "mountain_power" | "mountain" => SortField::MountainPower,
        "oceanpower" | "ocean_power" | "ocean" => SortField::OceanPower,
        "setdate" | "set_date" => SortField::SetDate,
        "collector" => SortField::Collector,
        "reference" | "number" => SortField::Reference,
        "name" => SortField::Name(canonical_sort_locale(locale_part.unwrap_or("en_US")).to_string()),
        _ => {
            return Err(bad_request(format!(
                "unsupported sort field '{field_part}' in '{trimmed}'"
            )));
        }
    };

    Ok(Some(CardsSort { field, dir }))
}

/// Unified search box term: OR across name substring / exact ref / exact collector number. Same
/// trim/empty rule as `parse_name`.
pub(crate) fn parse_q(params: &QueryMultiMap) -> Option<String> {
    let raw = get_first(params, "q")?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn parse_collector_numbers(params: &QueryMultiMap) -> Vec<String> {
    // Spec: collectorNumber[] repeated keys
    // Convenience: collectorNumber=A,B,C (CSV)
    let mut values: Vec<String> = Vec::new();
    if let Some(vs) = params.get("collectorNumber[]") {
        for v in vs {
            for part in v.split(',') {
                let s = part.trim();
                if !s.is_empty() {
                    values.push(s.to_string());
                }
            }
        }
    }
    if let Some(vs) = params.get("collectorNumber") {
        for v in vs {
            for part in v.split(',') {
                let s = part.trim();
                if !s.is_empty() {
                    values.push(s.to_string());
                }
            }
        }
    }

    let mut out = Vec::new();
    for value in values {
        if !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

pub(crate) fn parse_id_list(params: &QueryMultiMap, key: &str) -> ApiResult<Vec<u32>> {
    let Some(values) = params.get(key) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for v in values {
        if v.trim().is_empty() {
            continue;
        }
        for part in v.split(',') {
            let s = part.trim();
            if s.is_empty() {
                continue;
            }
            out.push(parse_u32(key, s)?);
        }
    }
    Ok(out)
}

pub(crate) fn parse_effect_mode(params: &QueryMultiMap) -> ApiResult<EffectCombineMode> {
    let Some(raw) = get_first(params, "effectMode") else {
        return Ok(EffectCombineMode::And);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "and" => Ok(EffectCombineMode::And),
        "or" => Ok(EffectCombineMode::Or),
        other => Err(bad_request(format!(
            "invalid effectMode value '{other}': expected 'and' or 'or'"
        ))),
    }
}

enum EffectFieldSelector {
    IdGd(IdGdSelector),
    MatchCount,
}

/// Parses `effect[N][t|c|o|matchCount]` keys into ordered slots (sparse indices allowed).
pub(crate) fn parse_effect_slots(params: &QueryMultiMap) -> ApiResult<Vec<EffectSlotFilter>> {
    let mut by_index: BTreeMap<u32, EffectSlotFilter> = BTreeMap::new();

    for key in params.keys() {
        let Some((index, field)) = parse_effect_param_key(key)? else {
            continue;
        };
        let slot = by_index.entry(index).or_insert_with(|| EffectSlotFilter {
            index,
            ..EffectSlotFilter::default()
        });
        match field {
            EffectFieldSelector::IdGd(selector) => {
                let ids = parse_id_list(params, key)?;
                match selector {
                    IdGdSelector::T => slot.t = ids,
                    IdGdSelector::C => slot.c = ids,
                    IdGdSelector::O => slot.o = ids,
                }
            }
            EffectFieldSelector::MatchCount => {
                slot.match_count = parse_match_count(params, key)?;
            }
        }
    }

    Ok(by_index
        .into_values()
        .filter(|slot| !slot.is_empty())
        .collect())
}

fn parse_match_count(params: &QueryMultiMap, key: &str) -> ApiResult<u8> {
    let raw = get_first(params, key).unwrap_or("1").trim();
    let value = raw
        .parse::<u8>()
        .map_err(|_| bad_request(format!("invalid {key} value '{raw}'")))?;
    if !(1..=3).contains(&value) {
        return Err(bad_request(format!(
            "invalid {key} value '{raw}': expected 1, 2, or 3"
        )));
    }
    Ok(value)
}

/// Returns `None` for keys that are not `effect[N][t|c|o|matchCount]`.
fn parse_effect_param_key(key: &str) -> ApiResult<Option<(u32, EffectFieldSelector)>> {
    const PREFIX: &str = "effect[";
    if !key.starts_with(PREFIX) {
        return Ok(None);
    }
    if key == "effectMode" {
        return Ok(None);
    }

    let rest = &key[PREFIX.len()..];
    let bracket_end = rest
        .find(']')
        .ok_or_else(|| bad_request(format!("invalid effect parameter '{key}'")))?;
    let index_str = &rest[..bracket_end];
    if index_str.is_empty() {
        return Err(bad_request(format!("invalid effect parameter '{key}'")));
    }
    let index = parse_u32("effect slot index", index_str)?;

    let field_part = &rest[bracket_end + 1..];
    if !field_part.starts_with('[') || !field_part.ends_with(']') {
        return Err(bad_request(format!("invalid effect parameter '{key}'")));
    }
    let field_name = &field_part[1..field_part.len() - 1];
    let field = match field_name {
        "t" => EffectFieldSelector::IdGd(IdGdSelector::T),
        "c" => EffectFieldSelector::IdGd(IdGdSelector::C),
        "o" => EffectFieldSelector::IdGd(IdGdSelector::O),
        "matchCount" => EffectFieldSelector::MatchCount,
        _ => {
            return Err(bad_request(format!(
                "invalid effect parameter '{key}': expected [t], [c], [o], or [matchCount]"
            )));
        }
    };
    Ok(Some((index, field)))
}

fn parse_factions(params: &QueryMultiMap) -> ApiResult<Vec<Faction>> {
    // Spec: faction[] repeated keys
    // Convenience: faction=AX,BR
    let mut codes: Vec<String> = Vec::new();
    if let Some(values) = params.get("faction[]") {
        for v in values {
            for part in v.split(',') {
                let s = part.trim();
                if !s.is_empty() {
                    codes.push(s.to_string());
                }
            }
        }
    }
    if let Some(values) = params.get("faction") {
        for v in values {
            for part in v.split(',') {
                let s = part.trim();
                if !s.is_empty() {
                    codes.push(s.to_string());
                }
            }
        }
    }

    let mut out = Vec::new();
    for code in codes {
        let faction = match code.as_str() {
            "AX" => Faction::Ax,
            "BR" => Faction::Br,
            "LY" => Faction::Ly,
            "MU" => Faction::Mu,
            "OR" => Faction::Or,
            "YZ" => Faction::Yz,
            _ => return Err(bad_request(format!("invalid faction value '{code}'"))),
        };
        if !out.contains(&faction) {
            out.push(faction);
        }
    }
    Ok(out)
}

pub(crate) fn parse_refs(params: &QueryMultiMap) -> Vec<String> {
    // Spec: ref[] repeated keys
    // Convenience: ref=REF1,REF2,REF3 (CSV)
    let mut refs: Vec<String> = Vec::new();
    if let Some(values) = params.get("ref[]") {
        for v in values {
            for part in v.split(',') {
                let s = part.trim();
                if !s.is_empty() {
                    refs.push(s.to_string());
                }
            }
        }
    }
    if let Some(values) = params.get("ref") {
        for v in values {
            for part in v.split(',') {
                let s = part.trim();
                if !s.is_empty() {
                    refs.push(s.to_string());
                }
            }
        }
    }

    let mut out = Vec::new();
    for reference in refs {
        if !out.contains(&reference) {
            out.push(reference);
        }
    }
    out
}

pub(crate) fn parse_sets(params: &QueryMultiMap) -> ApiResult<Vec<String>> {
    // Spec: set[] repeated keys
    // Convenience: set=CORE,COREKS
    let mut codes: Vec<String> = Vec::new();
    if let Some(values) = params.get("set[]") {
        for v in values {
            for part in v.split(',') {
                let s = part.trim();
                if !s.is_empty() {
                    codes.push(s.to_string());
                }
            }
        }
    }
    if let Some(values) = params.get("set") {
        for v in values {
            for part in v.split(',') {
                let s = part.trim();
                if !s.is_empty() {
                    codes.push(s.to_string());
                }
            }
        }
    }

    let mut out = Vec::new();
    for code in codes {
        if !out.contains(&code) {
            out.push(code);
        }
    }
    Ok(out)
}

pub(crate) fn parse_cost_u8(field: &str, s: &str) -> ApiResult<u8> {
    let v = s
        .parse::<u8>()
        .map_err(|_| bad_request(format!("invalid {field} value '{s}'")))?;
    if v > 15 {
        return Err(bad_request(format!(
            "invalid {field} value '{s}': must be in range 0..=15"
        )));
    }
    Ok(v)
}

pub(crate) fn parse_cost_array(params: &QueryMultiMap, key: &str) -> ApiResult<Option<Vec<u8>>> {
    let Some(values) = params.get(key) else {
        return Ok(None);
    };
    let mut out: Vec<u8> = Vec::new();
    for v in values {
        if v.trim().is_empty() {
            continue;
        }
        for part in v.split(',') {
            let s = part.trim();
            if s.is_empty() {
                continue;
            }
            let n = parse_cost_u8(key, s)?;
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

pub(crate) fn parse_cost_predicate(params: &QueryMultiMap, base: &str) -> ApiResult<Option<CostPredicate>> {
    let exact_key = base;
    let array_key = format!("{base}[]");
    let gt_key = format!("{base}[gt]");
    let gte_key = format!("{base}[gte]");
    let lt_key = format!("{base}[lt]");
    let lte_key = format!("{base}[lte]");

    let has_exact = has_any(params, exact_key);
    let has_array = has_any(params, &array_key);
    let has_range = has_any(params, &gt_key)
        || has_any(params, &gte_key)
        || has_any(params, &lt_key)
        || has_any(params, &lte_key);

    let kind_count = (has_exact as u8) + (has_array as u8) + (has_range as u8);
    if kind_count > 1 {
        return Err(bad_request(format!(
            "unsupported parameter combination: do not mix {base}, {base}[], or {base}[gt|gte|lt|lte]"
        )));
    }

    if has_exact {
        let v = get_first(params, exact_key).unwrap_or_default().trim();
        if v.is_empty() {
            return Ok(None);
        }
        return Ok(Some(CostPredicate::Exact(parse_cost_u8(base, v)?)));
    }

    if has_array {
        let Some(values) = parse_cost_array(params, &array_key)? else {
            return Ok(None);
        };
        return Ok(Some(CostPredicate::AnyOf(values)));
    }

    let mut bounds = Vec::new();
    for (key, op) in [
        (&gt_key, CompareOp::Gt),
        (&gte_key, CompareOp::Gte),
        (&lt_key, CompareOp::Lt),
        (&lte_key, CompareOp::Lte),
    ] {
        if let Some(v) = get_first(params, key) {
            let v = v.trim();
            if v.is_empty() {
                continue;
            }
            bounds.push((op, parse_cost_u8(key, v)?));
        }
    }
    if !bounds.is_empty() {
        return Ok(Some(CostPredicate::RangeBounds(bounds)));
    }

    Ok(None)
}

pub(crate) fn parse_cost_relation(params: &QueryMultiMap) -> ApiResult<Option<CostRelation>> {
    let Some(raw) = get_first(params, "costRelation") else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "equal" | "eq" => Ok(Some(CostRelation::Equal)),
        "mainhigher" | "main_higher" | "main_gt" => Ok(Some(CostRelation::MainHigher)),
        "recallhigher" | "recall_higher" | "recall_gt" => Ok(Some(CostRelation::RecallHigher)),
        _ => Err(bad_request(format!(
            "invalid parameter 'costRelation': expected equal|eq, mainHigher|main_gt, or recallHigher|recall_gt, got '{trimmed}'"
        ))),
    }
}

pub(crate) fn parse_sub_types(params: &QueryMultiMap) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["subTypes[]", "subTypes", "cardSubTypes[]", "cardSubTypes"] {
        if let Some(values) = params.get(key) {
            for raw in values {
                for part in raw.split(',') {
                    let normalized = part.trim().to_ascii_uppercase();
                    if !normalized.is_empty() && !out.contains(&normalized) {
                        out.push(normalized);
                    }
                }
            }
        }
    }
    out
}

pub(crate) fn parse_has_no_effect(params: &QueryMultiMap) -> ApiResult<Option<bool>> {
    let Some(raw) = get_first(params, "hasNoEffect") else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(Some(true)),
        "false" | "0" => Ok(Some(false)),
        _ => Err(bad_request(format!(
            "invalid parameter 'hasNoEffect': expected true or false, got '{trimmed}'"
        ))),
    }
}

/// `hasZeroPower=true` (or `1`): only cards with at least one power at 0. `false` / `0` / empty:
/// no filter.
pub(crate) fn parse_has_zero_power(params: &QueryMultiMap) -> ApiResult<bool> {
    let Some(raw) = get_first(params, "hasZeroPower") else {
        return Ok(false);
    };
    let trimmed = raw.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "" | "false" | "0" => Ok(false),
        "true" | "1" => Ok(true),
        _ => Err(bad_request(format!(
            "invalid parameter 'hasZeroPower': expected true or false, got '{trimmed}'"
        ))),
    }
}

/// `artist[]` (repeatable; alias `artist`): artist names, trimmed, deduplicated
/// case-insensitively. Values are not split on commas: a name is taken whole.
pub(crate) fn parse_artists(params: &QueryMultiMap) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for key in ["artist[]", "artist"] {
        for raw in params.get(key).into_iter().flatten() {
            let name = raw.trim();
            if !name.is_empty() && !out.iter().any(|a| a.to_lowercase() == name.to_lowercase()) {
                out.push(name.to_string());
            }
        }
    }
    out
}

pub(crate) fn validate_idgd_types(state: &UniquesIndex, filters: &AbilityFilters) -> ApiResult<()> {
    let mut types: BTreeMap<u32, &str> = BTreeMap::new();
    for entry in &state.idgd_catalog().entries {
        types.insert(entry.id_gd, entry.element_type.as_str());
    }

    for slot in &filters.effects {
        for (key_suffix, selector, ids) in [
            ("[t]", IdGdSelector::T, &slot.t),
            ("[c]", IdGdSelector::C, &slot.c),
            ("[o]", IdGdSelector::O, &slot.o),
        ] {
            let key = format!("effect[{}]{key_suffix}", slot.index);
            for &id in ids.iter() {
                let resolved = state.resolve_id_gd(id);
                let Some(actual) = types.get(&resolved).copied() else {
                    return Err(bad_request(format!(
                        "{key} contains unknown idGd {id} (not present in idgd_catalog)"
                    )));
                };
                let expected = selector.expected_type();
                if actual != expected {
                    return Err(bad_request(format!(
                        "{key} contains idGd {id} of type {actual}, expected {expected}"
                    )));
                }
            }
        }
    }

    for (key, selector, ids) in [
        ("support[t]", IdGdSelector::T, &filters.support_t),
        ("support[c]", IdGdSelector::C, &filters.support_c),
        ("support[o]", IdGdSelector::O, &filters.support_o),
    ] {
        for &id in ids.iter() {
            let resolved = state.resolve_id_gd(id);
            let Some(actual) = types.get(&resolved).copied() else {
                return Err(bad_request(format!(
                    "{key} contains unknown idGd {id} (not present in idgd_catalog)"
                )));
            };
            let expected = selector.expected_type();
            if actual != expected {
                return Err(bad_request(format!(
                    "{key} contains idGd {id} of type {actual}, expected {expected}"
                )));
            }
        }
    }

    Ok(())
}
pub(crate) fn parse_u32(field: &str, s: &str) -> ApiResult<u32> {
    s.parse::<u32>()
        .map_err(|_| bad_request(format!("invalid {field} value '{s}'")))
}

pub(crate) fn parse_usize(field: &str, s: &str) -> ApiResult<usize> {
    s.parse::<usize>()
        .map_err(|_| bad_request(format!("invalid {field} value '{s}'")))
}
#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use axum::http::StatusCode;
    use index_core::faction_index::Faction;
    use roaring::RoaringBitmap;

    use super::*;
    use crate::formats::{FormatIndex, FormatLoadStatus, LoadedFormat};
    use crate::http::api::cards::models::CostPredicate;
    use crate::http::api::cards::test_support::{test_state, test_state_with_sets};
    use crate::index::loader::{SET_CORE, SET_COREKS};
    #[test]
    fn parse_with_families_flag() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("withFamilies".to_string(), vec!["".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert!(req.with_families);
    }
    #[test]
    fn type_validation_rejects_wrong_kind() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["191".to_string()]); // CONDITION but using [t]
        let err = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }
    #[test]
    fn parses_factions_from_repeated_or_csv_alias() {
        let state = test_state();

        let mut params: QueryMultiMap = HashMap::new();
        params.insert("faction[]".to_string(), vec!["AX".to_string(), "BR".to_string()]);
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert_eq!(req.factions.len(), 2);
        assert!(req.factions.contains(&Faction::Ax));
        assert!(req.factions.contains(&Faction::Br));

        let mut params2: QueryMultiMap = HashMap::new();
        params2.insert("faction".to_string(), vec!["AX,BR".to_string()]);
        params2.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        let req2 = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params2).unwrap();
        assert_eq!(req2.factions.len(), 2);
        assert!(req2.factions.contains(&Faction::Ax));
        assert!(req2.factions.contains(&Faction::Br));
    }

    #[test]
    fn invalid_faction_rejected() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("faction[]".to_string(), vec!["NOPE".to_string()]);
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        let err = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }
    #[test]
    fn parses_sets_from_repeated_or_csv_alias() {
        let state = test_state_with_sets();

        let mut params: QueryMultiMap = HashMap::new();
        params.insert("set[]".to_string(), vec!["CORE".to_string(), "COREKS".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert_eq!(req.sets.len(), 2);
        assert!(req.sets.contains(&SET_CORE.to_string()));
        assert!(req.sets.contains(&SET_COREKS.to_string()));

        let mut params2: QueryMultiMap = HashMap::new();
        params2.insert("set".to_string(), vec!["CORE,COREKS".to_string()]);
        let req2 = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params2).unwrap();
        assert_eq!(req2.sets.len(), 2);
    }

    #[test]
    fn invalid_set_rejected() {
        let state = test_state_with_sets();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("set[]".to_string(), vec!["NOPE".to_string()]);
        let err = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }
    #[test]
    fn parses_refs_from_repeated_or_csv_alias() {
        let state = test_state();

        let mut params: QueryMultiMap = HashMap::new();
        params.insert(
            "ref[]".to_string(),
            vec!["ALT_TEST_B_AX_01_U_3".to_string(), "ALT_TEST_B_AX_01_U_6".to_string()],
        );
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert_eq!(req.refs.len(), 2);
        assert!(req.refs.contains(&"ALT_TEST_B_AX_01_U_3".to_string()));
        assert!(req.refs.contains(&"ALT_TEST_B_AX_01_U_6".to_string()));

        let mut params2: QueryMultiMap = HashMap::new();
        params2.insert(
            "ref".to_string(),
            vec!["ALT_TEST_B_AX_01_U_3,ALT_TEST_B_AX_01_U_6".to_string()],
        );
        let req2 = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params2).unwrap();
        assert_eq!(req2.refs.len(), 2);
    }

    #[test]
    fn refs_dedup_and_ignore_blank_entries() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert(
            "ref".to_string(),
            vec!["ALT_TEST_B_AX_01_U_3, ,ALT_TEST_B_AX_01_U_3".to_string()],
        );
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert_eq!(req.refs, vec!["ALT_TEST_B_AX_01_U_3".to_string()]);
    }

    #[test]
    fn no_ref_param_yields_empty_refs() {
        let state = test_state();
        let params: QueryMultiMap = HashMap::new();
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert!(req.refs.is_empty());
    }

    #[test]
    fn parses_collector_numbers_from_repeated_or_csv_alias() {
        let state = test_state();

        let mut params: QueryMultiMap = HashMap::new();
        params.insert(
            "collectorNumber[]".to_string(),
            vec!["BTG-011-U-3".to_string(), "BTG-011-U-6".to_string()],
        );
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert_eq!(req.collector_numbers, vec!["BTG-011-U-3", "BTG-011-U-6"]);

        let mut params2: QueryMultiMap = HashMap::new();
        params2.insert(
            "collectorNumber".to_string(),
            vec!["BTG-011-U-3,BTG-011-U-6".to_string()],
        );
        let req2 = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params2).unwrap();
        assert_eq!(req2.collector_numbers, vec!["BTG-011-U-3", "BTG-011-U-6"]);
    }

    #[test]
    fn collector_numbers_dedup_and_ignore_blank_entries() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert(
            "collectorNumber".to_string(),
            vec!["BTG-011-U-3, ,BTG-011-U-3".to_string()],
        );
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert_eq!(req.collector_numbers, vec!["BTG-011-U-3".to_string()]);
    }

    #[test]
    fn no_collector_number_param_yields_empty() {
        let state = test_state();
        let params: QueryMultiMap = HashMap::new();
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert!(req.collector_numbers.is_empty());
    }

    #[test]
    fn q_trims_and_treats_whitespace_as_absent() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("q".to_string(), vec!["  Kelon  ".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert_eq!(req.q.as_deref(), Some("Kelon"));

        let mut blank: QueryMultiMap = HashMap::new();
        blank.insert("q".to_string(), vec!["   ".to_string()]);
        let req_blank = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &blank).unwrap();
        assert!(req_blank.q.is_none());
    }
    #[test]
    fn cost_exact_array_range_parsing_and_mixing_rejected() {
        let state = test_state();

        // exact
        let mut p1: QueryMultiMap = HashMap::new();
        p1.insert("mainCost".to_string(), vec!["2".to_string()]);
        p1.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        let r1 = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &p1).unwrap();
        assert!(matches!(r1.main_cost, Some(CostPredicate::Exact(2))));

        // array (with csv)
        let mut p2: QueryMultiMap = HashMap::new();
        p2.insert("mainCost[]".to_string(), vec!["2,3".to_string()]);
        p2.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        let r2 = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &p2).unwrap();
        assert!(matches!(r2.main_cost, Some(CostPredicate::AnyOf(_))));

        // range
        let mut p3: QueryMultiMap = HashMap::new();
        p3.insert("recallCost[lte]".to_string(), vec!["3".to_string()]);
        p3.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        let r3 = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &p3).unwrap();
        assert!(matches!(r3.recall_cost, Some(CostPredicate::RangeBounds(_))));

        // multi-bound range
        let mut p3b: QueryMultiMap = HashMap::new();
        p3b.insert("mainCost[gte]".to_string(), vec!["2".to_string()]);
        p3b.insert("mainCost[lte]".to_string(), vec!["4".to_string()]);
        let r3b = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &p3b).unwrap();
        match r3b.main_cost {
            Some(CostPredicate::RangeBounds(bounds)) => assert_eq!(bounds.len(), 2),
            other => panic!("expected RangeBounds, got {other:?}"),
        }

        // reject mixing
        let mut p4: QueryMultiMap = HashMap::new();
        p4.insert("mainCost[]".to_string(), vec!["2".to_string()]);
        p4.insert("mainCost[lte]".to_string(), vec!["3".to_string()]);
        p4.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        let err = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &p4).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn out_of_range_cost_rejected() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("mainCost".to_string(), vec!["99".to_string()]);
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        let err = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }
    #[test]
    fn cursor_and_page_together_rejected() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("cursor".to_string(), vec!["2".to_string()]);
        params.insert("page".to_string(), vec!["2".to_string()]);
        let err = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }
    #[test]
    fn page_zero_rejected() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("page".to_string(), vec!["0".to_string()]);
        let err = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }
    #[test]
    fn page_parses_when_valid() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("page".to_string(), vec!["3".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert_eq!(req.page, Some(3));
        assert_eq!(req.cursor, None);
    }
    #[test]
    fn invalid_effect_mode_rejected() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        params.insert("effectMode".to_string(), vec!["xor".to_string()]);
        let err = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn invalid_match_count_rejected() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        params.insert("effect[0][matchCount]".to_string(), vec!["4".to_string()]);
        let err = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn parse_match_count_defaults_to_one() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert_eq!(req.filters.effects[0].match_count, 1);
    }

    #[test]
    fn parse_match_count_two_and_three() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        params.insert("effect[0][matchCount]".to_string(), vec!["2".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert_eq!(req.filters.effects[0].match_count, 2);

        let mut params3: QueryMultiMap = HashMap::new();
        params3.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        params3.insert("effect[0][matchCount]".to_string(), vec!["3".to_string()]);
        let req3 = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params3).unwrap();
        assert_eq!(req3.filters.effects[0].match_count, 3);
    }

    #[test]
    fn invalid_effect_param_key_rejected() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][x]".to_string(), vec!["24".to_string()]);
        let err = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }
    #[test]
    fn debug_bga_trigram_param_parsing() {
        let state = test_state();
        let mut enabled: QueryMultiMap = HashMap::new();
        enabled.insert("debug_bga_trigram".to_string(), vec![String::new()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &enabled).unwrap();
        assert!(req.debug_bga_trigram);

        let params: QueryMultiMap = HashMap::new();
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert!(!req.debug_bga_trigram);
    }

    fn sample_format_index(id: &str, status: FormatLoadStatus) -> FormatIndex {
        FormatIndex {
            by_id: BTreeMap::from([(
                id.to_string(),
                LoadedFormat {
                    id: id.to_string(),
                    status,
                },
            )]),
            manifest_versions: BTreeMap::from([(id.to_string(), 1)]),
        }
    }

    #[test]
    fn parse_format_last_value_wins() {
        let mut params: QueryMultiMap = HashMap::new();
        params.insert(
            "format".to_string(),
            vec!["first".to_string(), "second".to_string()],
        );
        assert_eq!(parse_format(&params).as_deref(), Some("second"));
    }

    #[test]
    fn format_unknown_when_disabled_returns_400() {
        let state = test_state();
        let formats = sample_format_index("std", FormatLoadStatus::Failed);
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("format".to_string(), vec!["std".to_string()]);
        let err = parse_request(
            state.index().as_ref(),
            &formats,
            false,
            &state.snapshot().collections,
            &params,
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1["error"], "unknown format 'std'");
    }

    #[test]
    fn format_unknown_when_enabled_but_missing_returns_400() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("format".to_string(), vec!["missing".to_string()]);
        let err = parse_request(
            state.index().as_ref(),
            state.formats().as_ref(),
            true,
            &state.snapshot().collections,
            &params,
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1["error"], "unknown format 'missing'");
    }

    #[test]
    fn format_failed_returns_500() {
        let state = test_state();
        let formats = sample_format_index("broken", FormatLoadStatus::Failed);
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("format".to_string(), vec!["broken".to_string()]);
        let err = parse_request(
            state.index().as_ref(),
            &formats,
            true,
            &state.snapshot().collections,
            &params,
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.1["error"], "format failed to load");
    }

    #[test]
    fn format_ready_when_enabled_succeeds() {
        let state = test_state();
        let formats = sample_format_index(
            "std",
            FormatLoadStatus::Ready {
                negated: false,
                bitmap: RoaringBitmap::new(),
            },
        );
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("format".to_string(), vec!["std".to_string()]);
        let req = parse_request(
            state.index().as_ref(),
            &formats,
            true,
            &state.snapshot().collections,
            &params,
        )
        .unwrap();
        assert_eq!(req.format.as_deref(), Some("std"));
    }

    #[test]
    fn parse_collection_last_value_wins() {
        let mut params: QueryMultiMap = HashMap::new();
        params.insert(
            "collection".to_string(),
            vec!["first".to_string(), "second".to_string()],
        );
        assert_eq!(parse_collection(&params).as_deref(), Some("second"));
    }

    #[test]
    fn collection_not_loaded_returns_422() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("collection".to_string(), vec!["deck1".to_string()]);
        let err = parse_request(
            state.index().as_ref(),
            state.formats().as_ref(),
            false,
            &state.snapshot().collections,
            &params,
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.1["error"], "collection_not_loaded");
        assert_eq!(err.1["collection"], "deck1");
    }

    #[test]
    fn parse_sort_handles_default_valid_and_invalid_values() {
        assert_eq!(parse_sort(&HashMap::new()).unwrap(), None);

        let mut def: QueryMultiMap = HashMap::new();
        def.insert("sort".to_string(), vec!["default".to_string()]);
        assert_eq!(parse_sort(&def).unwrap(), None);

        let mut stat: QueryMultiMap = HashMap::new();
        stat.insert("sort".to_string(), vec!["mainCost:desc".to_string()]);
        assert_eq!(
            parse_sort(&stat).unwrap(),
            Some(CardsSort {
                field: SortField::MainCost,
                dir: SortDir::Desc,
            })
        );

        let mut name_fr: QueryMultiMap = HashMap::new();
        name_fr.insert("sort".to_string(), vec!["name:fr:asc".to_string()]);
        assert_eq!(
            parse_sort(&name_fr).unwrap(),
            Some(CardsSort {
                field: SortField::Name("fr_FR".to_string()),
                dir: SortDir::Asc,
            })
        );

        let mut bad_field: QueryMultiMap = HashMap::new();
        bad_field.insert("sort".to_string(), vec!["unknown:asc".to_string()]);
        assert_eq!(parse_sort(&bad_field).unwrap_err().0, StatusCode::BAD_REQUEST);

        let mut bad_dir: QueryMultiMap = HashMap::new();
        bad_dir.insert("sort".to_string(), vec!["mainCost:sideways".to_string()]);
        assert_eq!(parse_sort(&bad_dir).unwrap_err().0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn parse_cost_relation_sub_types_and_has_no_effect() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("costRelation".to_string(), vec!["mainHigher".to_string()]);
        params.insert(
            "subTypes[]".to_string(),
            vec!["engineer".to_string(), "SPIRIT,dragon".to_string()],
        );
        params.insert("hasNoEffect".to_string(), vec!["true".to_string()]);

        let req = parse_request(
            state.index().as_ref(),
            state.formats().as_ref(),
            false,
            &state.snapshot().collections,
            &params,
        )
        .unwrap();
        assert_eq!(req.cost_relation, Some(CostRelation::MainHigher));
        assert_eq!(req.sub_types, vec!["ENGINEER", "SPIRIT", "DRAGON"]);
        assert_eq!(req.has_no_effect, Some(true));

        let mut alias: QueryMultiMap = HashMap::new();
        alias.insert("costRelation".to_string(), vec!["recall_gt".to_string()]);
        alias.insert("cardSubTypes".to_string(), vec!["robot".to_string()]);
        alias.insert("hasNoEffect".to_string(), vec!["false".to_string()]);
        let req2 = parse_request(
            state.index().as_ref(),
            state.formats().as_ref(),
            false,
            &state.snapshot().collections,
            &alias,
        )
        .unwrap();
        assert_eq!(req2.cost_relation, Some(CostRelation::RecallHigher));
        assert_eq!(req2.sub_types, vec!["ROBOT"]);
        assert_eq!(req2.has_no_effect, Some(false));

        let mut bad_rel: QueryMultiMap = HashMap::new();
        bad_rel.insert("costRelation".to_string(), vec!["nope".to_string()]);
        assert_eq!(
            parse_request(
                state.index().as_ref(),
                state.formats().as_ref(),
                false,
                &state.snapshot().collections,
                &bad_rel,
            )
            .unwrap_err()
            .0,
            StatusCode::BAD_REQUEST
        );

        let mut bad_no_fx: QueryMultiMap = HashMap::new();
        bad_no_fx.insert("hasNoEffect".to_string(), vec!["maybe".to_string()]);
        assert_eq!(
            parse_request(
                state.index().as_ref(),
                state.formats().as_ref(),
                false,
                &state.snapshot().collections,
                &bad_no_fx,
            )
            .unwrap_err()
            .0,
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn parse_artists_and_has_zero_power() {
        let mut params: QueryMultiMap = HashMap::new();
        params.insert(
            "artist[]".to_string(),
            vec![
                " Edward Chee - Seok Yeong ".to_string(),
                "Last, First".to_string(),
                "edward chee - seok yeong".to_string(),
                "  ".to_string(),
            ],
        );
        params.insert("artist".to_string(), vec!["Other".to_string()]);
        // Trimmed, not split on commas, deduplicated case-insensitively, empty values dropped.
        assert_eq!(
            parse_artists(&params),
            vec!["Edward Chee - Seok Yeong", "Last, First", "Other"]
        );

        let flag = |value: &str| {
            let mut p: QueryMultiMap = HashMap::new();
            p.insert("hasZeroPower".to_string(), vec![value.to_string()]);
            parse_has_zero_power(&p)
        };
        assert!(flag("true").unwrap());
        assert!(flag("1").unwrap());
        assert!(!flag("false").unwrap());
        assert!(!flag("").unwrap());
        assert!(!parse_has_zero_power(&HashMap::new()).unwrap());
        assert_eq!(flag("maybe").unwrap_err().0, StatusCode::BAD_REQUEST);
    }
}

