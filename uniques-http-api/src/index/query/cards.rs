use std::collections::BTreeMap;

use roaring::RoaringBitmap;

use index_core::bitmap::EffectLine;
use index_core::family_catalog::FamilyCatalogEntry;
use index_core::idgd_catalog::IdGdCatalogEntry;
use index_core::stat_index::StatField;

use crate::collections::CollectionStore;
use crate::formats::{FormatIndex, FormatLoadStatus};
use crate::index::loader::{FamilySpanRange, NonUniqueQueryIndex, SetBitmaps, SET_CORE, SET_COREKS};
use crate::index::UniquesIndex;

use super::error::{QueryError, QueryResult};
use crate::http::api::cards::models::{
    CardFaction, CardRarityV2, CardSetV2, CardSubTypeV2, CardTypeV2, CardV2, CardsRequest,
    CardsSort, CompareOp, CostPredicate, CostRelation, EffectCombineMode, FamilyMatchV2, SortDir,
    SortField,
};

fn union_requested_sets(bitmaps: &SetBitmaps, sets: &[String]) -> RoaringBitmap {
    let has_core = sets.iter().any(|s| s == SET_CORE);
    let has_coreks = sets.iter().any(|s| s == SET_COREKS);
    let use_combined = has_core && has_coreks;

    let mut out = RoaringBitmap::new();

    if use_combined {
        if let Some(combined) = &bitmaps.core_and_coreks {
            out = combined.clone();
        } else {
            if let Some(b) = bitmaps.by_set.get(SET_CORE) {
                out |= b.clone();
            }
            if let Some(b) = bitmaps.by_set.get(SET_COREKS) {
                out |= b.clone();
            }
        }
    }

    for code in sets {
        if use_combined && (code == SET_CORE || code == SET_COREKS) {
            continue;
        }
        if let Some(b) = bitmaps.by_set.get(code) {
            out |= b.clone();
        }
    }

    out
}

pub(crate) fn build_bitmap(
    state: &UniquesIndex,
    format_index: &FormatIndex,
    collections: &CollectionStore,
    req: &CardsRequest,
) -> QueryResult<RoaringBitmap> {
    let mut groups = Vec::new();

    // Each effect[N] searches main lines only (M1/M2/M3) with per-line bucket intersection,
    // then combines lines per matchCount (default 1 = OR). Multiple slots combine per effectMode.
    let effect_bitmaps: Vec<RoaringBitmap> = req
        .filters
        .effects
        .iter()
        .filter_map(|slot| {
            effect_slot_bitmap(
                state,
                &slot.t,
                &slot.c,
                &slot.o,
                slot.match_count,
            )
        })
        .collect();
    if let Some(bmp) = combine_effect_bitmaps(&effect_bitmaps, req.filters.effect_mode) {
        groups.push(bmp);
    }

    // support[...] searches echo/support line only (Ec), with per-line bucket intersection.
    let support = bitmap_intersect_buckets_on_line(
        state,
        EffectLine::Ec,
        &req.filters.support_t,
        &req.filters.support_c,
        &req.filters.support_o,
    );
    if let Some(bmp) = support {
        groups.push(bmp);
    }

    if !req.factions.is_empty() {
        let mut bmp = RoaringBitmap::new();
        for faction in &req.factions {
            if let Some(f) = state.factions().get(faction) {
                bmp |= f.clone();
            }
        }
        groups.push(bmp);
    }

    if !req.sets.is_empty() {
        groups.push(union_requested_sets(state.set_bitmaps(), &req.sets));
    }

    if !req.refs.is_empty() {
        let ref_slices: Vec<&str> = req.refs.iter().map(String::as_str).collect();
        groups.push(index_core::build_bitmap_from_ref_strs_lenient(
            state.catalog(),
            &ref_slices,
        ));
    }

    if !req.collector_numbers.is_empty() {
        groups.push(collector_number_bitmap(state, &req.collector_numbers));
    }

    if let Some(pred) = &req.main_cost {
        groups.push(bitmap_for_cost_predicate(state, StatField::MainCost, "mainCost", pred)?);
    }
    if let Some(pred) = &req.recall_cost {
        groups.push(bitmap_for_cost_predicate(
            state,
            StatField::RecallCost,
            "recallCost",
            pred,
        )?);
    }
    if let Some(pred) = &req.forest_power {
        groups.push(bitmap_for_cost_predicate(
            state,
            StatField::ForestPower,
            "forestPower",
            pred,
        )?);
    }
    if let Some(pred) = &req.mountain_power {
        groups.push(bitmap_for_cost_predicate(
            state,
            StatField::MountainPower,
            "mountainPower",
            pred,
        )?);
    }
    if let Some(pred) = &req.ocean_power {
        groups.push(bitmap_for_cost_predicate(
            state,
            StatField::OceanPower,
            "oceanPower",
            pred,
        )?);
    }

    if let Some(name) = &req.name {
        groups.push(state.name_search_index().bitmap_for_contains(state.catalog(), name));
    }

    if let Some(rel) = req.cost_relation {
        let bmp = match rel {
            CostRelation::Equal => &state.cost_relations().equal,
            CostRelation::MainHigher => &state.cost_relations().main_higher,
            CostRelation::RecallHigher => &state.cost_relations().recall_higher,
        };
        groups.push(bmp.clone());
    }

    if !req.sub_types.is_empty() {
        let mut combined = RoaringBitmap::new();
        for st in &req.sub_types {
            if let Some(bmp) = state.subtype_bitmaps().get(st) {
                combined |= bmp;
            }
        }
        groups.push(combined);
    }

    let mut no_effect_exclude: Option<&RoaringBitmap> = None;
    if let Some(has_no_effect) = req.has_no_effect {
        if has_no_effect {
            groups.push(state.no_effect_bitmap().clone());
        } else {
            no_effect_exclude = Some(state.no_effect_bitmap());
        }
    }

    if let Some(q) = &req.q {
        groups.push(unified_search_bitmap(state, q));
    }

    let mut format_exclude: Option<RoaringBitmap> = None;
    if let Some(id) = &req.format {
        if let Some(loaded) = format_index.get(id) {
            match &loaded.status {
                FormatLoadStatus::Ready {
                    negated: true,
                    bitmap,
                } => format_exclude = Some(bitmap.clone()),
                FormatLoadStatus::Ready {
                    negated: false,
                    bitmap,
                } => groups.push(bitmap.clone()),
                FormatLoadStatus::Failed => {}
            }
        }
    }

    if let Some(id) = &req.collection {
        if let Some(bitmaps) = collections.get(id) {
            groups.push(bitmaps.unique.clone());
        }
    }

    let mut it = groups.into_iter();
    let mut out = match it.next() {
        Some(first) => first,
        None => all_cards_bitmap(state),
    };
    for bmp in it {
        out &= bmp;
    }
    if let Some(exclude) = no_effect_exclude {
        out -= exclude;
    }
    if let Some(exclude) = format_exclude {
        out -= exclude;
    }
    Ok(out)
}

/// Exact-match, lenient like `ref[]`: unresolved values just don't set a bit. Uniques only.
fn collector_number_bitmap(state: &UniquesIndex, collector_numbers: &[String]) -> RoaringBitmap {
    let mut out = RoaringBitmap::new();
    for cn in collector_numbers {
        if let Ok(card_index) = state.resolve_card_index_by_collector_number(cn) {
            out.insert(card_index);
        }
    }
    out
}

/// `q`'s single OR'd group: name substring, exact reference, exact collector number.
fn unified_search_bitmap(state: &UniquesIndex, q: &str) -> RoaringBitmap {
    let mut out = state.name_search_index().bitmap_for_contains(state.catalog(), q);
    out |= index_core::build_bitmap_from_ref_strs_lenient(state.catalog(), &[q]);
    if let Ok(card_index) = state.resolve_card_index_by_collector_number(q) {
        out.insert(card_index);
    }
    out
}

fn all_cards_bitmap(state: &UniquesIndex) -> RoaringBitmap {
    let span = state.manifest().total_bit_span;
    if span == 0 {
        return RoaringBitmap::new();
    }
    let mut bmp = RoaringBitmap::new();
    bmp.insert_range(0..span);
    bmp
}

fn bitmap_for_cost_predicate(
    state: &UniquesIndex,
    field: StatField,
    label: &str,
    pred: &CostPredicate,
) -> QueryResult<RoaringBitmap> {
    let Some(buckets) = state.stats().get(&field) else {
        return Err(QueryError::invalid(format!("missing stats index for {label}")));
    };

    let mut out = RoaringBitmap::new();
    match pred {
        CostPredicate::Exact(v) => {
            out |= buckets[*v as usize].clone();
        }
        CostPredicate::AnyOf(values) => {
            for &v in values {
                out |= buckets[v as usize].clone();
            }
        }
        CostPredicate::RangeBounds(bounds) => {
            for v in 0u8..=15u8 {
                let keep = bounds.iter().all(|(op, value)| match op {
                    CompareOp::Gt => v > *value,
                    CompareOp::Gte => v >= *value,
                    CompareOp::Lt => v < *value,
                    CompareOp::Lte => v <= *value,
                });
                if keep {
                    out |= buckets[v as usize].clone();
                }
            }
        }
    }
    Ok(out)
}

pub(crate) fn combine_effect_bitmaps(
    bitmaps: &[RoaringBitmap],
    mode: EffectCombineMode,
) -> Option<RoaringBitmap> {
    let mut it = bitmaps.iter();
    let first = it.next()?;
    let mut out = first.clone();
    match mode {
        EffectCombineMode::And => {
            for bmp in it {
                out &= bmp;
            }
        }
        EffectCombineMode::Or => {
            for bmp in it {
                out |= bmp;
            }
        }
    }
    Some(out)
}

pub(crate) fn effect_slot_bitmap(
    state: &UniquesIndex,
    triggers: &[u32],
    conditions: &[u32],
    outputs: &[u32],
    match_count: u8,
) -> Option<RoaringBitmap> {
    if triggers.is_empty() && conditions.is_empty() && outputs.is_empty() {
        return None;
    }

    let line_matches = [EffectLine::M1, EffectLine::M2, EffectLine::M3].map(|line| {
        bitmap_intersect_buckets_on_line(state, line, triggers, conditions, outputs)
    });
    Some(combine_line_matches(&line_matches, match_count))
}

fn combine_line_matches(line_matches: &[Option<RoaringBitmap>; 3], match_count: u8) -> RoaringBitmap {
    match match_count {
        1 => line_matches
            .iter()
            .flatten()
            .fold(RoaringBitmap::new(), |acc, bmp| acc | bmp),
        2 => {
            let mut out = RoaringBitmap::new();
            for i in 0..3 {
                for j in i + 1..3 {
                    if let (Some(a), Some(b)) = (&line_matches[i], &line_matches[j]) {
                        out |= &(a & b);
                    }
                }
            }
            out
        }
        3 => match (
            &line_matches[0],
            &line_matches[1],
            &line_matches[2],
        ) {
            (Some(a), Some(b), Some(c)) => a & b & c,
            _ => RoaringBitmap::new(),
        },
        _ => RoaringBitmap::new(),
    }
}

fn bitmap_intersect_buckets_on_line(
    state: &UniquesIndex,
    line: EffectLine,
    triggers: &[u32],
    conditions: &[u32],
    outputs: &[u32],
) -> Option<RoaringBitmap> {
    let mut groups: Vec<RoaringBitmap> = Vec::new();
    if !triggers.is_empty() {
        groups.push(bitmap_line_any_ids(state, line, triggers));
    }
    if !conditions.is_empty() {
        groups.push(bitmap_line_any_ids(state, line, conditions));
    }
    if !outputs.is_empty() {
        groups.push(bitmap_line_any_ids(state, line, outputs));
    }
    let mut it = groups.into_iter();
    let mut bmp = it.next()?;
    for g in it {
        bmp &= g;
    }
    Some(bmp)
}

fn bitmap_line_any_ids(state: &UniquesIndex, line: EffectLine, ids: &[u32]) -> RoaringBitmap {
    let mut out = RoaringBitmap::new();
    for &id in ids {
        out |= bitmap_line(state, line, id);
    }
    out
}

fn bitmap_line(state: &UniquesIndex, line: EffectLine, id_gd: u32) -> RoaringBitmap {
    let resolved = state.resolve_id_gd(id_gd);
    state
        .id_gd_per_line()
        .get(&(resolved, line))
        .cloned()
        .unwrap_or_else(RoaringBitmap::new)
}

pub(crate) fn card_v2_from_index(
    state: &UniquesIndex,
    card_index: u32,
    idgd_by_id: &BTreeMap<u32, &IdGdCatalogEntry>,
    debug_bga_trigram: bool,
) -> QueryResult<CardV2> {
    let reference = state
        .decode_reference(card_index)
        .map_err(|e| QueryError::invalid(format!("failed to decode reference for card_index {card_index}: {e}")))?;

    let view = state
        .card_view(card_index)
        .ok_or_else(|| QueryError::invalid(format!("missing compact record for card_index {card_index}")))?;

    let family = state
        .catalog()
        .family_for_bit(card_index)
        .map_err(|e| QueryError::invalid(format!("family lookup for card_index {card_index}: {e}")))?;

    let faction_code = faction_from_code(view.faction_code());
    let faction_name = index_core::faction_display_name(&faction_code)
        .unwrap_or("")
        .to_string();

    Ok(CardV2 {
        reference,
        name: family.name.clone(),
        artist: family.artist.clone(),
        set: CardSetV2::from(&family.set),
        card_sub_types: family
            .card_sub_types
            .iter()
            .map(|st| CardSubTypeV2 {
                reference: st.reference.clone(),
                name: st.name.clone(),
            })
            .collect(),
        main_cost: Some(view.main_cost()),
        recall_cost: Some(view.recall_cost()),
        forest_power: Some(view.forest_power()),
        mountain_power: Some(view.mountain_power()),
        ocean_power: Some(view.ocean_power()),
        faction: CardFaction {
            code: faction_code,
            name: faction_name,
        },
        main_effect: build_main_effect_localized(idgd_by_id, &view),
        echo_effect: build_echo_effect_localized(idgd_by_id, &view),
        collector_number: build_collector_number(family, card_index),
        card_type: None,
        rarity: None,
        product: None,
        is_serialized: None,
        is_banned: None,
        is_errated: None,
        is_suspended: None,
        permanent: None,
        reserve: None,
        legal_formats: None,
        debug_bga_trigram: debug_bga_trigram.then(|| build_debug_bga_trigram(&view)),
    })
}

pub fn rarity_v2_from_code(code: &str) -> CardRarityV2 {
    match code {
        "C" | "COMMON" => CardRarityV2 {
            reference: "COMMON".to_string(),
            name: "Common".to_string(),
        },
        "R" | "RARE" => CardRarityV2 {
            reference: "RARE".to_string(),
            name: "Rare".to_string(),
        },
        "E" | "EXALTED" => CardRarityV2 {
            reference: "EXALTED".to_string(),
            name: "Exalted".to_string(),
        },
        "U" | "UNIQUE" => CardRarityV2 {
            reference: "UNIQUE".to_string(),
            name: "Unique".to_string(),
        },
        other => CardRarityV2 {
            reference: other.to_string(),
            name: other.to_string(),
        },
    }
}

pub fn unique_legal_formats(card_index: u32, formats: &FormatIndex) -> Option<Vec<String>> {
    if formats.by_id.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for (id, loaded) in &formats.by_id {
        if let FormatLoadStatus::Ready { negated, bitmap } = &loaded.status {
            let matches = bitmap.contains(card_index);
            if (*negated && !matches) || (!*negated && matches) {
                out.push(id.clone());
            }
        }
    }
    Some(out)
}

/// Populate unified-search metadata (`rarity`, `cardType`, `isBanned`, `isErrated`, `isSuspended`,
/// `legalFormats`) onto a `CardV2` built for a unique card.
pub fn enrich_unique_card_v2(
    card: &mut CardV2,
    card_index: u32,
    index: &UniquesIndex,
    nonunique: Option<&NonUniqueQueryIndex>,
    formats: &FormatIndex,
) {
    card.rarity = Some(rarity_v2_from_code("U"));
    let ct_names = nonunique
        .and_then(|nu| nu.card_types_i18n.get("CHARACTER"))
        .cloned()
        .unwrap_or_else(|| {
            let mut m = BTreeMap::new();
            m.insert("en_US".to_string(), "Character".to_string());
            m
        });
    card.card_type = Some(CardTypeV2 {
        reference: "CHARACTER".to_string(),
        name: ct_names,
    });
    card.is_banned = Some(index.banned().contains(card_index));
    card.is_errated = Some(index.errated().contains(card_index));
    card.is_suspended = Some(index.suspended().contains(card_index));
    card.legal_formats = unique_legal_formats(card_index, formats);
}

/// Assemble a full `CardV2` for a non-unique print in `NonUniqueQueryIndex`.
pub fn nonunique_card_v2(
    nu: &NonUniqueQueryIndex,
    print_index: u32,
    families: &BTreeMap<i64, FamilyCatalogEntry>,
    subtype_names: &BTreeMap<String, BTreeMap<String, String>>,
) -> Option<CardV2> {
    let reference = nu.reference_for(print_index)?.to_string();
    if let Some(meta) = nu.print_for(print_index) {
        let fam = families.get(&meta.card_family_id);
        let name = fam.map(|f| f.name.clone()).unwrap_or_default();
        let faction_name = index_core::faction_display_name(&meta.faction)
            .unwrap_or("")
            .to_string();
        let card_type_ref = if !meta.card_type.is_empty() {
            meta.card_type.clone()
        } else {
            fam.map(|f| f.card_type.clone()).unwrap_or_default()
        };
        let card_type = if card_type_ref.is_empty() {
            None
        } else {
            let ct_name = nu
                .card_types_i18n
                .get(&card_type_ref)
                .cloned()
                .unwrap_or_else(|| {
                    let mut m = BTreeMap::new();
                    m.insert("en_US".to_string(), card_type_ref.clone());
                    m
                });
            Some(CardTypeV2 {
                reference: card_type_ref,
                name: ct_name,
            })
        };
        let card_sub_types = meta
            .card_sub_types
            .iter()
            .map(|r| CardSubTypeV2 {
                reference: r.clone(),
                name: subtype_names.get(r).cloned().unwrap_or_else(|| {
                    let mut m = BTreeMap::new();
                    m.insert("en_US".to_string(), r.clone());
                    m
                }),
            })
            .collect();

        Some(CardV2 {
            reference,
            name,
            artist: meta.artist.clone(),
            set: CardSetV2 {
                reference: meta.set.clone(),
                name: meta.set_name.clone(),
                code: meta.set_code.clone(),
            },
            card_sub_types,
            main_cost: meta.main_cost,
            recall_cost: meta.recall_cost,
            forest_power: meta.forest_power,
            mountain_power: meta.mountain_power,
            ocean_power: meta.ocean_power,
            faction: CardFaction {
                code: meta.faction.clone(),
                name: faction_name,
            },
            main_effect: meta.main_effect.clone(),
            echo_effect: meta.echo_effect.clone(),
            collector_number: meta.collector_number.clone(),
            card_type,
            rarity: Some(rarity_v2_from_code(&meta.rarity)),
            product: Some(meta.product.clone()),
            is_serialized: Some(meta.is_serialized),
            is_banned: Some(meta.is_banned),
            is_errated: Some(meta.is_errated),
            is_suspended: Some(meta.is_suspended),
            permanent: meta.permanent,
            reserve: meta.reserve,
            legal_formats: None,
            debug_bga_trigram: None,
        })
    } else {
        // Fallback when legacy index lacks `prints.json`.
        let fields = nu.fields_for(print_index)?;
        let faction_code = nu
            .faction
            .iter()
            .find_map(|(k, b)| b.contains(print_index).then(|| k.clone()))
            .unwrap_or_default();
        let faction_name = index_core::faction_display_name(&faction_code)
            .unwrap_or("")
            .to_string();
        let rarity_code = index_core::nonunique::rarity_reference_from_code(fields.rarity_code)
            .unwrap_or("");
        let product_code = index_core::nonunique::product_reference_from_code(fields.product_code)
            .unwrap_or("");
        let set_ref = nu
            .set
            .iter()
            .find_map(|(k, b)| b.contains(print_index).then(|| k.clone()))
            .unwrap_or_default();
        let fam_id = nu
            .family
            .iter()
            .find_map(|(k, b)| b.contains(print_index).then(|| k.parse::<i64>().ok()).flatten());
        let fam = fam_id.and_then(|id| families.get(&id));
        let card_type_ref = nu
            .card_type
            .iter()
            .find_map(|(k, b)| b.contains(print_index).then(|| k.clone()))
            .or_else(|| fam.map(|f| f.card_type.clone()))
            .unwrap_or_default();
        let card_type = (!card_type_ref.is_empty()).then(|| CardTypeV2 {
            reference: card_type_ref.clone(),
            name: nu
                .card_types_i18n
                .get(&card_type_ref)
                .cloned()
                .unwrap_or_default(),
        });

        Some(CardV2 {
            reference,
            name: fam.map(|f| f.name.clone()).unwrap_or_default(),
            artist: String::new(),
            set: CardSetV2 {
                name: set_ref.clone(),
                code: index_core::set_code(&set_ref).map(str::to_string),
                reference: set_ref,
            },
            card_sub_types: Vec::new(),
            main_cost: Some(fields.main_cost),
            recall_cost: Some(fields.recall_cost),
            forest_power: Some(fields.forest_power),
            mountain_power: Some(fields.mountain_power),
            ocean_power: Some(fields.ocean_power),
            faction: CardFaction {
                code: faction_code,
                name: faction_name,
            },
            main_effect: BTreeMap::new(),
            echo_effect: BTreeMap::new(),
            collector_number: None,
            card_type,
            rarity: (!rarity_code.is_empty()).then(|| rarity_v2_from_code(rarity_code)),
            product: (!product_code.is_empty()).then(|| product_code.to_string()),
            is_serialized: Some(fields.is_serialized),
            is_banned: Some(nu.banned.contains(print_index)),
            is_errated: Some(nu.errated.contains(print_index)),
            is_suspended: Some(nu.suspended.contains(print_index)),
            permanent: None,
            reserve: None,
            legal_formats: None,
            debug_bga_trigram: None,
        })
    }
}

pub fn subtype_names_from_index(index: &UniquesIndex) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for fam in &index.catalog().families {
        for st in &fam.card_sub_types {
            let key = st.reference.trim().to_ascii_uppercase();
            if !key.is_empty() && !map.contains_key(&key) {
                map.insert(key, st.name.clone());
            }
        }
    }
    map
}

/// `SET-FAMILY-U-UID` (e.g. `BTG-011-U-5`) — `None` if either `set.code` or
/// `collector_family_number` is missing (older index, or the deferred `cardsdata.rs` path).
fn build_collector_number(family: &index_core::catalog::FamilyEntry, card_index: u32) -> Option<String> {
    let code = family.set.code.as_deref()?;
    let family_collector_number = family.collector_family_number.as_deref()?;
    let unique_id = card_index - family.start_bit + 1;
    Some(format!("{code}-{family_collector_number}-U-{unique_id}"))
}

fn first_match_in_range(bitmap: &RoaringBitmap, start: u32, end: u32) -> Option<u32> {
    bitmap.range(start..end).next()
}

fn family_match_from_index(
    state: &UniquesIndex,
    card_index: u32,
    family_id: &str,
    count: u64,
) -> QueryResult<FamilyMatchV2> {
    let reference = state
        .decode_reference(card_index)
        .map_err(|e| QueryError::invalid(format!("failed to decode reference for card_index {card_index}: {e}")))?;

    let family = state
        .catalog()
        .family_for_bit(card_index)
        .map_err(|e| QueryError::invalid(format!("family lookup for card_index {card_index}: {e}")))?;

    Ok(FamilyMatchV2 {
        family_id: family_id.to_string(),
        count,
        reference,
        name: family.name.clone(),
    })
}

/// `page` is 1-indexed and always applied (defaulting to page 1) — an unbounded response here is
/// exactly the "browsing a whole unique category crashes the renderer" problem Lot 3 fixes, so
/// unlike before this lot, `withFamilies` no longer returns every matching family in one response.
pub(crate) fn families_from_bitmap(
    state: &UniquesIndex,
    bitmap: &RoaringBitmap,
    page: Option<u32>,
    limit: usize,
) -> QueryResult<(Vec<FamilyMatchV2>, Vec<u32>)> {
    // Matching families are found by walking `family_span_groups()` (bounded by family count,
    // not by how many cards matched), so this pass stays cheap even for a huge bitmap. Only the
    // page we're about to return goes through `family_match_from_index` below, which does real
    // work (catalog/reference lookups).
    let mut matches = Vec::new();
    for group in state.family_span_groups() {
        let count = bitmap.range_cardinality(group.range_start..group.range_end);
        if count == 0 {
            continue;
        }
        let Some(card_index) =
            first_match_in_range(bitmap, group.range_start, group.range_end)
        else {
            continue;
        };
        matches.push((group, count, card_index));
    }

    let start = page.unwrap_or(1).saturating_sub(1) as usize * limit;
    let mut families = Vec::new();
    let mut example_indices = Vec::new();
    for (group, count, card_index) in matches.into_iter().skip(start).take(limit) {
        families.push(family_match_from_index(
            state,
            card_index,
            &group.family_id,
            count,
        )?);
        example_indices.push(card_index);
    }
    Ok((families, example_indices))
}

pub(crate) fn cards_from_indices(
    state: &UniquesIndex,
    indices: &[u32],
    debug_bga_trigram: bool,
) -> QueryResult<Vec<CardV2>> {
    let idgd_by_id: BTreeMap<u32, &IdGdCatalogEntry> = state
        .idgd_catalog()
        .entries
        .iter()
        .map(|e| (e.id_gd, e))
        .collect();

    indices
        .iter()
        .map(|&card_index| {
            card_v2_from_index(state, card_index, &idgd_by_id, debug_bga_trigram)
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn page_cards_v2(
    state: &UniquesIndex,
    bitmap: &RoaringBitmap,
    cursor: Option<u32>,
    page: Option<u32>,
    limit: usize,
    debug_bga_trigram: bool,
) -> QueryResult<(Vec<CardV2>, Option<u32>)> {
    page_cards_v2_sorted(state, bitmap, cursor, page, limit, None, debug_bga_trigram)
}

pub(crate) fn page_cards_v2_sorted(
    state: &UniquesIndex,
    bitmap: &RoaringBitmap,
    cursor: Option<u32>,
    page: Option<u32>,
    limit: usize,
    sort: Option<&CardsSort>,
    debug_bga_trigram: bool,
) -> QueryResult<(Vec<CardV2>, Option<u32>)> {
    if let Some(sort) = sort {
        let offset = page
            .map(|p| p.saturating_sub(1) as usize * limit)
            .unwrap_or_else(|| cursor.unwrap_or(0) as usize);
        let (cards, next_offset_cursor) =
            page_sorted_cards(state, bitmap, offset, limit, sort, debug_bga_trigram)?;
        let next_cursor = if page.is_none() {
            next_offset_cursor
        } else {
            None
        };
        return Ok((cards, next_cursor));
    }

    let idgd_by_id: BTreeMap<u32, &IdGdCatalogEntry> = state
        .idgd_catalog()
        .entries
        .iter()
        .map(|e| (e.id_gd, e))
        .collect();

    let indices = window_indices(bitmap, cursor, page, limit);

    let mut out = Vec::with_capacity(indices.len());
    for card_index in &indices {
        out.push(card_v2_from_index(
            state,
            *card_index,
            &idgd_by_id,
            debug_bga_trigram,
        )?);
    }

    // The "there's more, here's where to resume" cursor only makes sense for the legacy scheme —
    // page-based pagination doesn't need it, the client just requests `page + 1` directly.
    let next_cursor = if page.is_none() && out.len() == limit {
        indices.last().copied()
    } else {
        None
    };

    Ok((out, next_cursor))
}
fn collect_sorted_stat_indices(
    state: &UniquesIndex,
    bitmap: &RoaringBitmap,
    field: StatField,
    dir: SortDir,
    offset: usize,
    limit: usize,
) -> QueryResult<Vec<u32>> {
    let Some(buckets) = state.stats().get(&field) else {
        return Err(QueryError::invalid(format!(
            "missing stats index for {field:?}"
        )));
    };

    let mut remaining_skip = offset as u64;
    let mut out = Vec::with_capacity(limit);
    let bucket_order: [usize; 16] = match dir {
        SortDir::Asc => std::array::from_fn(|i| i),
        SortDir::Desc => std::array::from_fn(|i| 15 - i),
    };

    for val_idx in bucket_order {
        if out.len() >= limit {
            break;
        }
        let sub = bitmap & &buckets[val_idx];
        let count = sub.len();
        if count == 0 {
            continue;
        }
        if remaining_skip >= count {
            remaining_skip -= count;
            continue;
        }
        let need = limit - out.len();
        if remaining_skip == 0 {
            out.extend(sub.iter().take(need));
        } else if let Some(start_bit) = sub.select(remaining_skip as u32) {
            out.extend(sub.range(start_bit..).take(need));
        }
        remaining_skip = 0;
    }

    Ok(out)
}

fn family_ranges_for_sort<'a>(
    state: &'a UniquesIndex,
    field: &SortField,
    dir: SortDir,
) -> (&'a [FamilySpanRange], bool, bool) {
    let fsi = state.family_sort_index();
    match (field, dir) {
        (SortField::SetDate, SortDir::Asc) => (&fsi.set_date_asc, false, false),
        (SortField::SetDate, SortDir::Desc) => (&fsi.set_date_desc, false, false),
        (SortField::Collector, SortDir::Asc) => (&fsi.collector_asc, false, false),
        (SortField::Collector, SortDir::Desc) => (&fsi.collector_asc, true, true),
        (SortField::Reference, SortDir::Asc) => (&fsi.reference_asc, false, false),
        (SortField::Reference, SortDir::Desc) => (&fsi.reference_asc, true, true),
        (SortField::Name(locale), SortDir::Asc) => (
            fsi.name_asc_by_locale
                .get(locale)
                .or_else(|| fsi.name_asc_by_locale.get("en_US"))
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            false,
            false,
        ),
        (SortField::Name(locale), SortDir::Desc) => (
            fsi.name_asc_by_locale
                .get(locale)
                .or_else(|| fsi.name_asc_by_locale.get("en_US"))
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            true,
            false,
        ),
        _ => (&[], false, false),
    }
}

fn collect_sorted_family_indices(
    state: &UniquesIndex,
    bitmap: &RoaringBitmap,
    field: &SortField,
    dir: SortDir,
    offset: usize,
    limit: usize,
) -> Vec<u32> {
    let (ranges, reverse_ranges, reverse_within_family) =
        family_ranges_for_sort(state, field, dir);
    let mut remaining_skip = offset as u64;
    let mut out = Vec::with_capacity(limit);

    let iter: Box<dyn Iterator<Item = &FamilySpanRange>> = if reverse_ranges {
        Box::new(ranges.iter().rev())
    } else {
        Box::new(ranges.iter())
    };

    for range in iter {
        if out.len() >= limit {
            break;
        }
        let count = bitmap.range_cardinality(range.start..range.end);
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
        if reverse_within_family {
            out.extend(bitmap.range(range.start..range.end).rev().skip(skip).take(need));
        } else {
            out.extend(bitmap.range(range.start..range.end).skip(skip).take(need));
        }
    }

    out
}

fn page_sorted_cards(
    state: &UniquesIndex,
    bitmap: &RoaringBitmap,
    offset: usize,
    limit: usize,
    sort: &CardsSort,
    debug_bga_trigram: bool,
) -> QueryResult<(Vec<CardV2>, Option<u32>)> {
    let total = bitmap.len();
    if (offset as u64) >= total || limit == 0 {
        return Ok((Vec::new(), None));
    }

    let indices = match &sort.field {
        SortField::MainCost => {
            collect_sorted_stat_indices(state, bitmap, StatField::MainCost, sort.dir, offset, limit)?
        }
        SortField::RecallCost => {
            collect_sorted_stat_indices(state, bitmap, StatField::RecallCost, sort.dir, offset, limit)?
        }
        SortField::ForestPower => {
            collect_sorted_stat_indices(state, bitmap, StatField::ForestPower, sort.dir, offset, limit)?
        }
        SortField::MountainPower => {
            collect_sorted_stat_indices(state, bitmap, StatField::MountainPower, sort.dir, offset, limit)?
        }
        SortField::OceanPower => {
            collect_sorted_stat_indices(state, bitmap, StatField::OceanPower, sort.dir, offset, limit)?
        }
        SortField::SetDate | SortField::Collector | SortField::Reference | SortField::Name(_) => {
            collect_sorted_family_indices(state, bitmap, &sort.field, sort.dir, offset, limit)
        }
    };

    let cards = cards_from_indices(state, &indices, debug_bga_trigram)?;
    let next_offset = offset + indices.len();
    let next_cursor = if !indices.is_empty() && (next_offset as u64) < total {
        Some(next_offset as u32)
    } else {
        None
    };

    Ok((cards, next_cursor))
}

/// `limit` elements from `bitmap`, starting either:
/// - at `page` (1-indexed) via `select` — direct access to any page's rank, no need to have seen
///   previous pages (this is what lets a client jump straight to page 12); or
/// - strictly after `cursor` via `advance_to` — legacy resume-by-last-index, still avoiding a scan
///   from the start even though the caller must already hold that cursor; or
/// - from the very first element when neither is given.
///
/// `page` and `cursor` are mutually exclusive (rejected at parse time); `page` wins here if both
/// somehow reach this function.
fn window_indices(
    bitmap: &RoaringBitmap,
    cursor: Option<u32>,
    page: Option<u32>,
    limit: usize,
) -> Vec<u32> {
    if let Some(page) = page {
        let start_rank = page.saturating_sub(1).saturating_mul(limit as u32);
        return match bitmap.select(start_rank) {
            Some(first) => {
                let mut it = bitmap.iter();
                it.advance_to(first);
                it.take(limit).collect()
            }
            None => Vec::new(),
        };
    }

    let mut it = bitmap.iter();
    if let Some(c) = cursor {
        it.advance_to(c.saturating_add(1));
    }
    it.take(limit).collect()
}

fn faction_from_code(code: u8) -> String {
    match code {
        1 => "AX",
        2 => "BR",
        3 => "LY",
        4 => "MU",
        5 => "OR",
        6 => "YZ",
        _ => "UN",
    }
    .to_string()
}

fn build_debug_bga_trigram(view: &index_core::compact::CompactCardView<'_>) -> String {
    let mut triplets: Vec<String> = Vec::new();
    for g in 0..3 {
        if let Some(triplet) = format_tco_triplet(view.main_effect_group(g)) {
            triplets.push(triplet);
        }
    }
    if let Some(triplet) = format_tco_triplet(view.echo_effect()) {
        triplets.push(triplet);
    }
    triplets.join(";")
}

fn format_tco_triplet([t, c, o]: [u16; 3]) -> Option<String> {
    if t == 0 && c == 0 && o == 0 {
        return None;
    }
    Some(format!("{t}/{c}/{o}"))
}

fn build_main_effect_localized(
    idgd_by_id: &BTreeMap<u32, &IdGdCatalogEntry>,
    view: &index_core::compact::CompactCardView<'_>,
) -> BTreeMap<String, String> {
    let groups: [[u16; 3]; 3] = [
        view.main_effect_group(0),
        view.main_effect_group(1),
        view.main_effect_group(2),
    ];

    let mut locales: BTreeMap<String, ()> = BTreeMap::new();
    locales.insert("en_US".to_string(), ());
    for [t, c, o] in groups {
        for id in [t, c, o] {
            if id == 0 {
                continue;
            }
            if let Some(entry) = idgd_by_id.get(&(id as u32)) {
                for k in entry.translations.keys() {
                    locales.insert(k.clone(), ());
                }
            }
        }
    }

    let mut out = BTreeMap::new();
    for locale in locales.keys() {
        let mut lines: Vec<String> = Vec::new();
        for [t, c, o] in groups {
            if let Some(line) = build_effect_line_localized(idgd_by_id, [t, c, o], locale) {
                lines.push(line);
            }
        }
        if !lines.is_empty() {
            out.insert(locale.clone(), lines.join("  "));
        }
    }
    out
}

fn build_echo_effect_localized(
    idgd_by_id: &BTreeMap<u32, &IdGdCatalogEntry>,
    view: &index_core::compact::CompactCardView<'_>,
) -> BTreeMap<String, String> {
    let [t, c, o] = view.echo_effect();

    let mut locales: BTreeMap<String, ()> = BTreeMap::new();
    locales.insert("en_US".to_string(), ());
    for id in [t, c, o] {
        if id == 0 {
            continue;
        }
        if let Some(entry) = idgd_by_id.get(&(id as u32)) {
            for k in entry.translations.keys() {
                locales.insert(k.clone(), ());
            }
        }
    }

    let mut out = BTreeMap::new();
    for locale in locales.keys() {
        if let Some(line) = build_effect_line_localized(idgd_by_id, [t, c, o], locale) {
            out.insert(locale.clone(), line);
        }
    }
    out
}

fn build_effect_line_localized(
    idgd_by_id: &BTreeMap<u32, &IdGdCatalogEntry>,
    [t, c, o]: [u16; 3],
    locale: &str,
) -> Option<String> {
    if t == 0 && c == 0 && o == 0 {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for id in [t, c, o] {
        if id == 0 {
            continue;
        }
        let text = idgd_by_id
            .get(&(id as u32))
            .map(|entry| pick_translation(&entry.translations, locale))
            .unwrap_or_default();
        if !text.is_empty() {
            parts.push(text);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn pick_translation(map: &BTreeMap<String, index_core::card::LocaleText>, locale: &str) -> String {
    if let Some(t) = map.get(locale) {
        return t.text.clone();
    }
    if let Some(t) = map.get("en_US") {
        return t.text.clone();
    }
    map.values().next().map(|t| t.text.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::cards::parse::{parse_request, QueryMultiMap};
    use crate::http::api::cards::test_support::{test_state, test_state_with_sets};
    use crate::index::loader::{SET_CORE, SET_COREKS};
    use crate::http::api::cards::models::EffectCombineMode;
    use std::collections::HashMap;
    #[test]
    fn empty_name_treated_as_no_filter() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("name".to_string(), vec!["   ".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert!(req.name.is_none());
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), state.index().manifest().total_bit_span as u64);
    }

    #[test]
    fn families_from_bitmap_merges_core_coreks_span() {
        let state = test_state_with_sets();
        let index = state.index();
        let groups = index.family_span_groups();
        assert_eq!(groups.len(), 2);
        let ax01 = groups.iter().find(|g| g.family_id == "AX_01").unwrap();
        assert_eq!(ax01.range_start, 0);
        assert_eq!(ax01.range_end, 10);

        let mut bmp = RoaringBitmap::new();
        bmp.insert(1);
        bmp.insert(6);

        let (families, ensure) = families_from_bitmap(state.index().as_ref(), &bmp, None, 50).unwrap();
        assert_eq!(families.len(), 1);
        assert_eq!(families[0].family_id, "AX_01");
        assert_eq!(families[0].count, 2);
        assert_eq!(families[0].reference, "ALT_CORE_B_AX_01_U_2");
        assert_eq!(
            families[0].name.get("en_US").map(String::as_str),
            Some("Kelon Elemental")
        );

        let cards = cards_from_indices(state.index().as_ref(), &ensure, false).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].reference, families[0].reference);
    }


    #[test]
    fn cards_from_indices_matches_families_only() {
        let state = test_state_with_sets();
        let mut bmp = RoaringBitmap::new();
        for i in 0..15 {
            bmp.insert(i);
        }
        let (families, ensure) = families_from_bitmap(state.index().as_ref(), &bmp, None, 50).unwrap();
        assert_eq!(families.len(), 2);

        let cards = cards_from_indices(state.index().as_ref(), &ensure, false).unwrap();
        assert_eq!(cards.len(), families.len());
        for (card, fam) in cards.iter().zip(&families) {
            assert_eq!(card.reference, fam.reference);
        }
    }


    #[test]
    fn effect0_matches_any_main_line() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert!(bmp.contains(2));
        assert!(bmp.contains(5));
        assert_eq!(bmp.len(), 2);
    }


    #[test]
    fn effect0_intersects_across_selectors_on_same_line() {
        let state = test_state();
        // trigger 24 exists on M2 for {2,5}; output 42 exists only on Ec for {5}.
        // Since effect[0] searches M1/M2/M3 and requires same-line intersection, there are no matches.
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        params.insert("effect[0][o]".to_string(), vec!["42".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert!(bmp.is_empty());
    }


    #[test]
    fn effect_and_support_intersect() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        params.insert("support[o]".to_string(), vec!["42".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert!(!bmp.contains(2));
        assert!(bmp.contains(5));
        assert_eq!(bmp.len(), 1);
    }


    #[test]
    fn card_v2_includes_family_metadata() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        params.insert("limit".to_string(), vec!["1".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        let (page, _) = page_cards_v2(state.index().as_ref(), &bmp, None, None, 1, false).unwrap();
        let card = &page[0];
        assert_eq!(card.name.get("en_US").map(String::as_str), Some("Test Card"));
        assert_eq!(card.artist, "Test Artist");
        assert_eq!(card.set.code.as_deref(), Some("BTG"));
        assert_eq!(card.faction.code, "AX");
        assert_eq!(card.faction.name, "Axiom");
        assert_eq!(card.card_sub_types[0].reference, "ENGINEER");
    }


    #[test]
    fn paging_uses_raw_card_index_cursor() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        params.insert("limit".to_string(), vec!["1".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();

        let (page1, cur1) = page_cards_v2(state.index().as_ref(), &bmp, None, None, 1, false).unwrap();
        assert_eq!(page1.len(), 1);
        assert_eq!(page1[0].reference, "ALT_TEST_B_AX_01_U_3"); // card_index 2 => unique_id 3
        assert_eq!(cur1, Some(2));

        let (page2, cur2) = page_cards_v2(state.index().as_ref(), &bmp, cur1, None, 1, false).unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].reference, "ALT_TEST_B_AX_01_U_6"); // card_index 5 => unique_id 6
        assert_eq!(cur2, Some(5));

        let (page3, cur3) = page_cards_v2(state.index().as_ref(), &bmp, cur2, None, 1, false).unwrap();
        assert!(page3.is_empty());
        assert_eq!(cur3, None);
    }

    #[test]
    fn page_param_matches_sequential_cursor_paging() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        params.insert("limit".to_string(), vec!["1".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();

        // Same two-card result set as `paging_uses_raw_card_index_cursor`, but accessed directly
        // by page (rank/select) instead of by walking cursors sequentially.
        let (page1, cur1) = page_cards_v2(state.index().as_ref(), &bmp, None, Some(1), 1, false).unwrap();
        assert_eq!(page1.len(), 1);
        assert_eq!(page1[0].reference, "ALT_TEST_B_AX_01_U_3");
        assert_eq!(cur1, None, "page-based pagination doesn't need a cursor");

        let (page2, _) = page_cards_v2(state.index().as_ref(), &bmp, None, Some(2), 1, false).unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].reference, "ALT_TEST_B_AX_01_U_6");

        // Page 3 doesn't exist for a 2-element bitmap at limit=1 — empty, not an error.
        let (page3, _) = page_cards_v2(state.index().as_ref(), &bmp, None, Some(3), 1, false).unwrap();
        assert!(page3.is_empty());
    }

    #[test]
    fn families_from_bitmap_paginates() {
        let state = test_state_with_sets();
        let mut bmp = RoaringBitmap::new();
        for i in 0..15 {
            bmp.insert(i);
        }

        let (all, _) = families_from_bitmap(state.index().as_ref(), &bmp, None, 50).unwrap();
        assert_eq!(all.len(), 2, "sanity: two families match in this fixture");

        let (page1, _) = families_from_bitmap(state.index().as_ref(), &bmp, Some(1), 1).unwrap();
        assert_eq!(page1.len(), 1);
        assert_eq!(page1[0].family_id, all[0].family_id);

        let (page2, _) = families_from_bitmap(state.index().as_ref(), &bmp, Some(2), 1).unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].family_id, all[1].family_id);

        let (page3, _) = families_from_bitmap(state.index().as_ref(), &bmp, Some(3), 1).unwrap();
        assert!(page3.is_empty());
    }

    #[test]
    fn set_filter_uses_combined_core_coreks_bitmap() {
        let state = test_state_with_sets();
        let index = state.index();
        let bitmaps = index.set_bitmaps();

        let manual = bitmaps.by_set[SET_CORE].clone() | bitmaps.by_set[SET_COREKS].clone();
        let combined = bitmaps.core_and_coreks.as_ref().unwrap();
        assert_eq!(manual, *combined);

        let mut params: QueryMultiMap = HashMap::new();
        params.insert("set[]".to_string(), vec!["CORE".to_string(), "COREKS".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp, *combined);
        assert_eq!(bmp.len(), 10);

        let mut single: QueryMultiMap = HashMap::new();
        single.insert("set[]".to_string(), vec!["CORE".to_string()]);
        let req_single = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &single).unwrap();
        let bmp_single = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req_single).unwrap();
        assert_eq!(bmp_single.len(), 5);
        assert!(bmp_single.contains(0));
        assert!(!bmp_single.contains(5));

        let mut three: QueryMultiMap = HashMap::new();
        three.insert(
            "set[]".to_string(),
            vec!["CORE".to_string(), "COREKS".to_string(), "ALIZE".to_string()],
        );
        let req_three = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &three).unwrap();
        let bmp_three = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req_three).unwrap();
        assert_eq!(bmp_three.len(), 15);
    }


    #[test]
    fn set_only_query_without_other_filters() {
        let state = test_state_with_sets();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("set[]".to_string(), vec!["ALIZE".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), 5);
        for i in 10..15 {
            assert!(bmp.contains(i));
        }
    }


    #[test]
    fn name_contains_case_insensitive() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("name".to_string(), vec!["test card".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), 10);
    }


    #[test]
    fn name_matches_any_locale() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("name".to_string(), vec!["kelon".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), 10);
    }


    #[test]
    fn name_folds_unicode_diacritics() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("name".to_string(), vec!["elementaire".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(
            bmp.len(),
            10,
            "elementaire should match fr_FR Élémentaire de Kélon"
        );
    }


    #[test]
    fn name_no_match_returns_empty_bitmap() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("name".to_string(), vec!["zzzzz".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert!(bmp.is_empty());
    }


    #[test]
    fn name_only_query_without_other_filters() {
        let state = test_state_with_sets();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("name".to_string(), vec!["Kelon".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), 10);
        for i in 0..10 {
            assert!(bmp.contains(i));
        }
        assert!(!bmp.contains(10));
    }


    #[test]
    fn name_combined_with_set_filter() {
        let state = test_state_with_sets();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("name".to_string(), vec!["Kelon".to_string()]);
        params.insert("set[]".to_string(), vec!["CORE".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), 5);
        for i in 0..5 {
            assert!(bmp.contains(i));
        }
        assert!(!bmp.contains(5));
    }


    #[test]
    fn no_filters_returns_all_cards() {
        let state = test_state();
        let params: QueryMultiMap = HashMap::new();
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), state.index().manifest().total_bit_span as u64);
        for i in 0..state.index().manifest().total_bit_span {
            assert!(bmp.contains(i));
        }
    }


    #[test]
    fn match_count_two_requires_two_lines() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["25".to_string()]);
        params.insert("effect[0][c]".to_string(), vec!["192".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert!(bmp.contains(7));
        assert!(bmp.contains(8));
        assert_eq!(bmp.len(), 2);

        let mut params2: QueryMultiMap = HashMap::new();
        params2.insert("effect[0][t]".to_string(), vec!["25".to_string()]);
        params2.insert("effect[0][c]".to_string(), vec!["192".to_string()]);
        params2.insert("effect[0][matchCount]".to_string(), vec!["2".to_string()]);
        let req2 = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params2).unwrap();
        let bmp2 = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req2).unwrap();
        assert!(bmp2.contains(7));
        assert!(!bmp2.contains(8));
        assert_eq!(bmp2.len(), 1);
    }

    #[test]
    fn match_count_three_requires_all_main_lines() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["25".to_string()]);
        params.insert("effect[0][c]".to_string(), vec!["192".to_string()]);
        params.insert("effect[0][matchCount]".to_string(), vec!["3".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert!(bmp.is_empty());
    }

    #[test]
    fn multiple_effect_slots_combine_with_effect_mode() {
        let state = test_state();
        // slot 0: trigger 24 on M2 -> {2,5}; slot 1: output 90 on M1 -> {2}
        let mut and_params: QueryMultiMap = HashMap::new();
        and_params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        and_params.insert("effect[1][o]".to_string(), vec!["90".to_string()]);
        and_params.insert("effectMode".to_string(), vec!["and".to_string()]);
        let and_req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &and_params).unwrap();
        let and_bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &and_req).unwrap();
        assert!(and_bmp.contains(2));
        assert!(!and_bmp.contains(5));
        assert_eq!(and_bmp.len(), 1);

        let mut or_params: QueryMultiMap = HashMap::new();
        or_params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        or_params.insert("effect[1][o]".to_string(), vec!["90".to_string()]);
        or_params.insert("effectMode".to_string(), vec!["or".to_string()]);
        let or_req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &or_params).unwrap();
        let or_bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &or_req).unwrap();
        assert!(or_bmp.contains(2));
        assert!(or_bmp.contains(5));
        assert_eq!(or_bmp.len(), 2);

        // default effectMode is and
        let mut default_params: QueryMultiMap = HashMap::new();
        default_params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        default_params.insert("effect[1][o]".to_string(), vec!["90".to_string()]);
        let default_req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &default_params).unwrap();
        assert_eq!(default_req.filters.effect_mode, EffectCombineMode::And);
        let default_bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &default_req).unwrap();
        assert_eq!(default_bmp.len(), 1);
        assert!(default_bmp.contains(2));
    }


    #[test]
    fn bitmap_intersects_with_faction_and_cost() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        // base ability query matches {2,5}
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        // faction AX is {2,5}
        params.insert("faction[]".to_string(), vec!["AX".to_string()]);
        // recallCost==3 is {5}
        params.insert("recallCost".to_string(), vec!["3".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), 1);
        assert!(bmp.contains(5));
    }


    #[test]
    fn forest_power_range_filters_bitmap() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("forestPower[gte]".to_string(), vec!["2".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), 1);
        assert!(bmp.contains(5));
    }

    #[test]
    fn mountain_power_exact_filters_bitmap() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("mountainPower".to_string(), vec!["2".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), 2);
        assert!(bmp.contains(2));
        assert!(bmp.contains(5));
    }

    #[test]
    fn ocean_power_anyof_filters_bitmap() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("oceanPower[]".to_string(), vec!["0".to_string(), "1".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), 2);
        assert!(bmp.contains(2));
        assert!(bmp.contains(5));
    }

    #[test]
    fn debug_bga_trigram_includes_main_and_echo_tcos() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        params.insert("limit".to_string(), vec!["1".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        let (page, _) = page_cards_v2(state.index().as_ref(), &bmp, None, None, 1, true).unwrap();
        assert_eq!(page[0].debug_bga_trigram.as_deref(), Some("24/191/90;24/0/42"));

        let (page, _) = page_cards_v2(state.index().as_ref(), &bmp, None, None, 1, false).unwrap();
        assert!(page[0].debug_bga_trigram.is_none());
    }


    #[test]
    fn debug_bga_trigram_serializes_on_card() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("effect[0][t]".to_string(), vec!["24".to_string()]);
        params.insert("limit".to_string(), vec!["1".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        let (page, _) = page_cards_v2(state.index().as_ref(), &bmp, None, None, 1, true).unwrap();
        let json = serde_json::to_value(&page[0]).unwrap();
        assert_eq!(json["debug_bga_trigram"], "24/191/90;24/0/42");
    }

    use crate::formats::{FormatIndex, FormatLoadStatus, LoadedFormat};

    #[test]
    fn format_include_ands_with_other_filters() {
        let state = test_state();
        let mut formats = FormatIndex::empty();
        let mut allowed = RoaringBitmap::new();
        allowed.insert_range(0..5);
        formats.by_id.insert(
            "std".into(),
            LoadedFormat {
                id: "std".into(),
                status: FormatLoadStatus::Ready {
                    negated: false,
                    bitmap: allowed,
                },
            },
        );
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("format".to_string(), vec!["std".to_string()]);
        params.insert("faction[]".to_string(), vec!["AX".to_string()]);
        let req = parse_request(
            state.index().as_ref(),
            &formats,
            true,
            &state.snapshot().collections,
            &params,
        )
        .unwrap();
        let bmp = build_bitmap(
            state.index().as_ref(),
            &formats,
            &state.snapshot().collections,
            &req,
        )
        .unwrap();
        assert_eq!(bmp.len(), 1);
        assert!(bmp.contains(2));
    }

    #[test]
    fn format_exclude_subtracts_from_result() {
        let state = test_state();
        let mut formats = FormatIndex::empty();
        let mut excluded = RoaringBitmap::new();
        excluded.insert(2);
        formats.by_id.insert(
            "ban".into(),
            LoadedFormat {
                id: "ban".into(),
                status: FormatLoadStatus::Ready {
                    negated: true,
                    bitmap: excluded,
                },
            },
        );
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("format".to_string(), vec!["ban".to_string()]);
        let req = parse_request(
            state.index().as_ref(),
            &formats,
            true,
            &state.snapshot().collections,
            &params,
        )
        .unwrap();
        let bmp = build_bitmap(
            state.index().as_ref(),
            &formats,
            &state.snapshot().collections,
            &req,
        )
        .unwrap();
        assert!(!bmp.contains(2));
        assert_eq!(bmp.len(), 9);
    }

    #[test]
    fn ref_filter_matches_explicit_references() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert(
            "ref".to_string(),
            vec!["ALT_TEST_B_AX_01_U_3,ALT_TEST_B_AX_01_U_6".to_string()],
        );
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert!(bmp.contains(2));
        assert!(bmp.contains(5));
        assert_eq!(bmp.len(), 2);
    }

    #[test]
    fn ref_filter_ignores_unknown_and_malformed_references() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert(
            "ref".to_string(),
            vec!["ALT_TEST_B_AX_01_U_3,ALT_BOGUS_B_ZZ_99_U_1,not-a-reference".to_string()],
        );
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert!(bmp.contains(2));
        assert_eq!(bmp.len(), 1);
    }

    #[test]
    fn ref_filter_combines_with_format_via_and() {
        let state = test_state();
        let mut formats = FormatIndex::empty();
        let mut allowed = RoaringBitmap::new();
        allowed.insert_range(0..5); // 0,1,2,3,4 -- excludes card_index 5
        formats.by_id.insert(
            "std".into(),
            LoadedFormat {
                id: "std".into(),
                status: FormatLoadStatus::Ready {
                    negated: false,
                    bitmap: allowed,
                },
            },
        );

        let mut params: QueryMultiMap = HashMap::new();
        params.insert("format".to_string(), vec!["std".to_string()]);
        params.insert(
            "ref".to_string(),
            vec!["ALT_TEST_B_AX_01_U_3,ALT_TEST_B_AX_01_U_6".to_string()], // card_index 2, 5
        );
        let req = parse_request(
            state.index().as_ref(),
            &formats,
            true,
            &state.snapshot().collections,
            &params,
        )
        .unwrap();
        let bmp = build_bitmap(
            state.index().as_ref(),
            &formats,
            &state.snapshot().collections,
            &req,
        )
        .unwrap();
        // card_index 5 is requested via ref but excluded by the format's allowed range.
        assert!(bmp.contains(2));
        assert!(!bmp.contains(5));
        assert_eq!(bmp.len(), 1);
    }

    #[test]
    fn ref_filter_absent_does_not_restrict_results() {
        let state = test_state();
        let params: QueryMultiMap = HashMap::new();
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert!(req.refs.is_empty());
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), state.index().manifest().total_bit_span as u64);
    }

    #[test]
    fn sorted_stat_pagination_orders_buckets_and_paginates_across_pages() {
        let state = test_state();
        // stats[ForestPower] buckets in test_state: index 2 => 1, index 5 => 4.
        let mut bmp = RoaringBitmap::new();
        bmp.insert(2);
        bmp.insert(5);

        let desc = CardsSort {
            field: SortField::ForestPower,
            dir: SortDir::Desc,
        };
        let (p1, c1) = page_cards_v2_sorted(state.index().as_ref(), &bmp, None, None, 1, Some(&desc), false).unwrap();
        assert_eq!(p1.iter().map(|c| c.reference.as_str()).collect::<Vec<_>>(), vec!["ALT_TEST_B_AX_01_U_6"]);
        assert_eq!(c1, Some(1));

        let (p2, c2) = page_cards_v2_sorted(state.index().as_ref(), &bmp, c1, None, 1, Some(&desc), false).unwrap();
        assert_eq!(p2.iter().map(|c| c.reference.as_str()).collect::<Vec<_>>(), vec!["ALT_TEST_B_AX_01_U_3"]);
        assert_eq!(c2, None);

        let asc = CardsSort {
            field: SortField::ForestPower,
            dir: SortDir::Asc,
        };
        let (pa, ca) = page_cards_v2_sorted(state.index().as_ref(), &bmp, None, None, 2, Some(&asc), false).unwrap();
        assert_eq!(
            pa.iter().map(|c| c.reference.as_str()).collect::<Vec<_>>(),
            vec!["ALT_TEST_B_AX_01_U_3", "ALT_TEST_B_AX_01_U_6"]
        );
        assert_eq!(ca, None);
    }

    #[test]
    fn sorted_family_pagination_supports_set_date_name_and_reference() {
        let state = test_state_with_sets();
        let mut bmp = RoaringBitmap::new();
        bmp.insert(0);  // CORE family 01 ("Kelon Elemental")
        bmp.insert(5);  // COREKS family 01 ("Kelon Elemental")
        bmp.insert(10); // ALIZE family 02 ("Other Character")
        bmp.insert(11); // ALIZE family 02 ("Other Character")

        let set_desc = CardsSort {
            field: SortField::SetDate,
            dir: SortDir::Desc,
        };
        let (p1, cur1) = page_cards_v2_sorted(state.index().as_ref(), &bmp, None, None, 2, Some(&set_desc), false).unwrap();
        assert_eq!(p1[0].set.reference, "ALIZE");
        assert_eq!(p1[1].set.reference, "ALIZE");
        assert_eq!(cur1, Some(2));

        let (p2, cur2) = page_cards_v2_sorted(state.index().as_ref(), &bmp, cur1, None, 2, Some(&set_desc), false).unwrap();
        assert_eq!(
            p2.iter().map(|c| c.set.reference.as_str()).collect::<Vec<_>>(),
            vec!["COREKS", "CORE"]
        );
        assert_eq!(cur2, None);

        let name_desc = CardsSort {
            field: SortField::Name("en_US".to_string()),
            dir: SortDir::Desc,
        };
        let (pn, _) = page_cards_v2_sorted(state.index().as_ref(), &bmp, None, None, 4, Some(&name_desc), false).unwrap();
        assert_eq!(pn[0].name.get("en_US").map(String::as_str), Some("Other Character"));
        assert_eq!(pn[2].name.get("en_US").map(String::as_str), Some("Kelon Elemental"));

        let ref_desc = CardsSort {
            field: SortField::Reference,
            dir: SortDir::Desc,
        };
        let (pr, _) = page_cards_v2_sorted(state.index().as_ref(), &bmp, None, None, 2, Some(&ref_desc), false).unwrap();
        assert_eq!(pr[0].reference, "ALT_COREKS_B_AX_01_U_1");
        assert_eq!(pr[1].reference, "ALT_CORE_B_AX_01_U_1");
    }

    #[test]
    fn build_bitmap_supports_cost_relation_sub_types_has_no_effect_and_multi_range() {
        let state = test_state();

        // costRelation=recallHigher: in test_state, idx 5 has MainCost=2, RecallCost=3.
        let mut p_recall = HashMap::new();
        p_recall.insert("costRelation".to_string(), vec!["recallHigher".to_string()]);
        let req_recall = parse_request(
            state.index().as_ref(),
            state.formats().as_ref(),
            false,
            &state.snapshot().collections,
            &p_recall,
        )
        .unwrap();
        let bmp_recall = build_bitmap(
            state.index().as_ref(),
            state.formats().as_ref(),
            &state.snapshot().collections,
            &req_recall,
        )
        .unwrap();
        assert_eq!(bmp_recall.iter().collect::<Vec<_>>(), vec![5]);

        // subTypes[]=engineer matches catalog family 0..10; unknown subtype returns empty (not 400).
        let mut p_sub = HashMap::new();
        p_sub.insert("subTypes[]".to_string(), vec!["engineer".to_string()]);
        let req_sub = parse_request(
            state.index().as_ref(),
            state.formats().as_ref(),
            false,
            &state.snapshot().collections,
            &p_sub,
        )
        .unwrap();
        let bmp_sub = build_bitmap(
            state.index().as_ref(),
            state.formats().as_ref(),
            &state.snapshot().collections,
            &req_sub,
        )
        .unwrap();
        assert_eq!(bmp_sub.len(), 10);

        let mut p_unknown_sub = HashMap::new();
        p_unknown_sub.insert("subTypes[]".to_string(), vec!["SONG".to_string()]);
        let req_unknown_sub = parse_request(
            state.index().as_ref(),
            state.formats().as_ref(),
            false,
            &state.snapshot().collections,
            &p_unknown_sub,
        )
        .unwrap();
        let bmp_unknown_sub = build_bitmap(
            state.index().as_ref(),
            state.formats().as_ref(),
            &state.snapshot().collections,
            &req_unknown_sub,
        )
        .unwrap();
        assert!(bmp_unknown_sub.is_empty());

        // Multi-bound range on forestPower: in test_state, idx 2 => 1, idx 5 => 4.
        let mut p_range = HashMap::new();
        p_range.insert("forestPower[gte]".to_string(), vec!["2".to_string()]);
        p_range.insert("forestPower[lte]".to_string(), vec!["4".to_string()]);
        let req_range = parse_request(
            state.index().as_ref(),
            state.formats().as_ref(),
            false,
            &state.snapshot().collections,
            &p_range,
        )
        .unwrap();
        let bmp_range = build_bitmap(
            state.index().as_ref(),
            state.formats().as_ref(),
            &state.snapshot().collections,
            &req_range,
        )
        .unwrap();
        assert_eq!(bmp_range.iter().collect::<Vec<_>>(), vec![5]);

        // hasNoEffect=true / hasNoEffect=false: in test_state, idx 2, 5, 7 have effects; others do not.
        let mut p_no_fx = HashMap::new();
        p_no_fx.insert("hasNoEffect".to_string(), vec!["true".to_string()]);
        let req_no_fx = parse_request(
            state.index().as_ref(),
            state.formats().as_ref(),
            false,
            &state.snapshot().collections,
            &p_no_fx,
        )
        .unwrap();
        let bmp_no_fx = build_bitmap(
            state.index().as_ref(),
            state.formats().as_ref(),
            &state.snapshot().collections,
            &req_no_fx,
        )
        .unwrap();
        assert!(!bmp_no_fx.contains(2));
        assert!(!bmp_no_fx.contains(5));
        assert!(!bmp_no_fx.contains(7));
        assert!(bmp_no_fx.contains(0));

        let mut p_has_fx = HashMap::new();
        p_has_fx.insert("hasNoEffect".to_string(), vec!["false".to_string()]);
        let req_has_fx = parse_request(
            state.index().as_ref(),
            state.formats().as_ref(),
            false,
            &state.snapshot().collections,
            &p_has_fx,
        )
        .unwrap();
        let bmp_has_fx = build_bitmap(
            state.index().as_ref(),
            state.formats().as_ref(),
            &state.snapshot().collections,
            &req_has_fx,
        )
        .unwrap();
        assert!(bmp_has_fx.contains(2));
        assert!(bmp_has_fx.contains(5));
        assert!(bmp_has_fx.contains(7));
        assert!(!bmp_has_fx.contains(0));
    }

    #[test]
    fn collector_number_filter_matches_and_ignores_unknown() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert(
            "collectorNumber".to_string(),
            // BTG-011-U-3 -> unique_id 3 -> card_index 2 (fixture family: start_bit 0, set code BTG,
            // collector_family_number "011"); BTG-011-U-99 falls beyond max_unique_id (padding);
            // "not-a-collector-number" doesn't parse.
            vec!["BTG-011-U-3,BTG-011-U-99,not-a-collector-number".to_string()],
        );
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), 1);
        assert!(bmp.contains(2));
    }

    #[test]
    fn collector_number_field_round_trips_on_card_v2() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("ref".to_string(), vec!["ALT_TEST_B_AX_01_U_3".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        let (page, _) = page_cards_v2(state.index().as_ref(), &bmp, None, None, 1, false).unwrap();
        assert_eq!(page[0].collector_number.as_deref(), Some("BTG-011-U-3"));
    }

    #[test]
    fn q_matches_via_name_substring_only() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("q".to_string(), vec!["test card".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), 10, "matches the whole family by name substring");
    }

    #[test]
    fn q_matches_via_exact_reference_only() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("q".to_string(), vec!["ALT_TEST_B_AX_01_U_3".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), 1);
        assert!(bmp.contains(2));
    }

    #[test]
    fn q_matches_via_exact_collector_number_only() {
        let state = test_state();
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("q".to_string(), vec!["BTG-011-U-6".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), 1);
        assert!(bmp.contains(5));
    }

    #[test]
    fn q_combines_with_other_filters_via_and() {
        let state = test_state();
        // "test card" alone matches all 10 (name substring); AND'd with recallCost==3 (only
        // card_index 5 in the fixture) narrows to just that one card.
        let mut params: QueryMultiMap = HashMap::new();
        params.insert("q".to_string(), vec!["test card".to_string()]);
        params.insert("recallCost".to_string(), vec!["3".to_string()]);
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), 1);
        assert!(bmp.contains(5));
    }

    #[test]
    fn q_absent_does_not_restrict_results() {
        let state = test_state();
        let params: QueryMultiMap = HashMap::new();
        let req = parse_request(state.index().as_ref(), state.formats().as_ref(), false, &state.snapshot().collections, &params).unwrap();
        assert!(req.q.is_none());
        let bmp = build_bitmap(state.index().as_ref(), state.formats().as_ref(), &state.snapshot().collections, &req).unwrap();
        assert_eq!(bmp.len(), state.index().manifest().total_bit_span as u64);
    }
}
