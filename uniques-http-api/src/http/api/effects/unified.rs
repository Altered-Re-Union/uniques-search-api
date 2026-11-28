//! Deep in-memory unified effect index combining unique `idgd_catalog` with non-unique
//! `NonUniqueQueryIndex` abilities behind one canonical `idGd` seam.
//!
//! Responsibilities owned once at bind time:
//! - Canonical `idGd -> element_type` lookup (eliminates per-request `BTreeMap` rebuilds).
//! - Pre-canonicalized non-unique per-line bitmaps (unions alias `idGd`s onto their canonical ID
//!   once at snapshot assembly, eliminating O(N) `equivalent_ids` scans per query).
//! - Pre-serialized unified `/api/v2/effects` response body.
//! - Filter validation, non-unique slot/support matching, and dual-branch reachability for
//!   `/api/v2/effects/filtered`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::body::Bytes;
use index_core::bitmap::EffectLine;
use index_core::idgd_collapse::IdGdAliasMap;
use roaring::RoaringBitmap;

use crate::http::api::cards::models::{AbilityFilters, IdGdSelector};
use crate::http::api::error::{bad_request, ApiResult};
use crate::index::loader::NonUniqueEffectLine;
use crate::index::{NonUniqueQueryIndex, UniquesIndex};

use super::filtered::{union_on_line, MAIN_LINES, SUPPORT_LINES};
use super::list::{build_unified_effects_list, serialize_effects_list};
use super::models::{Part, Region};

pub struct UnifiedEffectIndex {
    /// Canonical `idGd` -> `"TRIGGER" | "CONDITION" | "OUTPUT"`.
    id_to_type: BTreeMap<u32, String>,
    /// Non-unique per-line bitmaps keyed by *canonical* `idGd` (aliases merged at bind time).
    nu_per_line: BTreeMap<(u32, NonUniqueEffectLine), RoaringBitmap>,
    /// Distinct canonical `idGd`s present in the non-unique branch, grouped by element_type.
    nu_ids_by_type: BTreeMap<String, Vec<u32>>,
    nu_max_main_lines: u8,
    /// Pre-serialized JSON body for `GET /api/v2/effects`.
    effects_body: Arc<Bytes>,
}

impl UnifiedEffectIndex {
    pub fn build(unique: &UniquesIndex, nonunique: Option<&NonUniqueQueryIndex>) -> Self {
        let mut id_to_type: BTreeMap<u32, String> = BTreeMap::new();
        for entry in &unique.idgd_catalog().entries {
            id_to_type.insert(entry.id_gd, entry.element_type.clone());
        }

        let mut nu_per_line: BTreeMap<(u32, NonUniqueEffectLine), RoaringBitmap> = BTreeMap::new();
        let mut nu_ids_by_type_set: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
        let mut nu_max_main_lines: u8 = 0;

        if let Some(nu) = nonunique {
            nu_max_main_lines = nu.max_main_lines;
            for entry in &nu.idgd_catalog {
                let canon_id = unique.resolve_id_gd(entry.id_gd);
                id_to_type
                    .entry(canon_id)
                    .or_insert_with(|| entry.element_type.clone());
                nu_ids_by_type_set
                    .entry(entry.element_type.clone())
                    .or_default()
                    .insert(canon_id);
            }
            for (&(raw_id, line), bmp) in &nu.id_gd_per_line {
                let canon_id = unique.resolve_id_gd(raw_id);
                nu_per_line
                    .entry((canon_id, line))
                    .and_modify(|existing| *existing |= bmp)
                    .or_insert_with(|| bmp.clone());
            }
        }

        let nu_ids_by_type = nu_ids_by_type_set
            .into_iter()
            .map(|(k, s)| (k, s.into_iter().collect()))
            .collect();

        let effects_body = match nonunique.filter(|nu| !nu.idgd_catalog.is_empty()) {
            Some(nu) => {
                // Derive card_types per raw non-unique idGd directly from the loaded bitmaps so
                // prebuilt indexes without persisted `card_types` still report exact card types.
                let mut nu_card_types: BTreeMap<u32, Vec<String>> = BTreeMap::new();
                if !nu.card_type.is_empty() {
                    for (&raw_id, id_bmp) in &nu.id_gd_whole {
                        let types: Vec<String> = nu
                            .card_type
                            .iter()
                            .filter_map(|(ct, ct_bmp)| {
                                (!id_bmp.is_disjoint(ct_bmp)).then(|| ct.clone())
                            })
                            .collect();
                        if !types.is_empty() {
                            nu_card_types.insert(raw_id, types);
                        }
                    }
                }
                let list = build_unified_effects_list(
                    unique.idgd_catalog(),
                    Some(&nu.idgd_catalog),
                    Some(&nu_card_types),
                );
                match serialize_effects_list(&list) {
                    Ok(bytes) => Arc::new(bytes),
                    Err(e) => {
                        eprintln!(
                            "failed to serialize unified effects list: {e:#}; falling back to uniques"
                        );
                        Arc::clone(unique.effects_body())
                    }
                }
            }
            None => Arc::clone(unique.effects_body()),
        };

        Self {
            id_to_type,
            nu_per_line,
            nu_ids_by_type,
            nu_max_main_lines,
            effects_body,
        }
    }

    pub fn effects_body(&self) -> &Arc<Bytes> {
        &self.effects_body
    }

    /// Validate that every `idGd` in `filters` exists in the merged catalog and matches its slot
    /// (`TRIGGER` / `CONDITION` / `OUTPUT`).
    pub(crate) fn validate_filters(
        &self,
        aliases: &IdGdAliasMap,
        filters: &AbilityFilters,
    ) -> ApiResult<()> {
        for slot in &filters.effects {
            for (key_suffix, selector, ids) in [
                ("[t]", IdGdSelector::T, &slot.t),
                ("[c]", IdGdSelector::C, &slot.c),
                ("[o]", IdGdSelector::O, &slot.o),
            ] {
                let key = format!("effect[{}]{key_suffix}", slot.index);
                self.validate_bucket(&key, selector, ids, aliases)?;
            }
        }
        for (key, selector, ids) in [
            ("support[t]", IdGdSelector::T, &filters.support_t),
            ("support[c]", IdGdSelector::C, &filters.support_c),
            ("support[o]", IdGdSelector::O, &filters.support_o),
        ] {
            self.validate_bucket(key, selector, ids, aliases)?;
        }
        Ok(())
    }

    fn validate_bucket(
        &self,
        key: &str,
        selector: IdGdSelector,
        ids: &[u32],
        aliases: &IdGdAliasMap,
    ) -> ApiResult<()> {
        let expected = selector.expected_type();
        for &id in ids {
            let resolved = aliases.resolve(id);
            let Some(actual) = self.id_to_type.get(&resolved).map(String::as_str) else {
                return Err(bad_request(format!(
                    "{key} contains unknown idGd {id} (not present in idgd_catalog)"
                )));
            };
            if actual != expected {
                return Err(bad_request(format!(
                    "{key} contains idGd {id} of type {actual}, expected {expected}"
                )));
            }
        }
        Ok(())
    }

    /// Match one main-effect slot (`t`, `c`, `o`, `match_count`) across all non-unique main lines.
    pub fn match_nonunique_slot(
        &self,
        aliases: &IdGdAliasMap,
        triggers: &[u32],
        conditions: &[u32],
        outputs: &[u32],
        match_count: u8,
    ) -> Option<RoaringBitmap> {
        if triggers.is_empty() && conditions.is_empty() && outputs.is_empty() {
            return None;
        }
        let mut line_matches: Vec<RoaringBitmap> = Vec::new();
        for line in 1..=self.nu_max_main_lines {
            if let Some(bmp) = self.nu_intersect_buckets_on_line(
                aliases,
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

    /// Match the echo/support slot across non-unique prints.
    pub fn match_nonunique_support(
        &self,
        aliases: &IdGdAliasMap,
        triggers: &[u32],
        conditions: &[u32],
        outputs: &[u32],
    ) -> Option<RoaringBitmap> {
        self.nu_intersect_buckets_on_line(
            aliases,
            NonUniqueEffectLine::Echo,
            triggers,
            conditions,
            outputs,
        )
    }

    /// Compute canonical `idGd`s of `part` reachable in `region` given the already-filtered base
    /// bitmaps and the edited slot's other two co-constraint buckets (`co1`, `co2`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reachable_id_gds(
        &self,
        unique: &UniquesIndex,
        unique_base: &RoaringBitmap,
        nonunique_base: &RoaringBitmap,
        part: Part,
        region: Region,
        co1: &[u32],
        co2: &[u32],
    ) -> Vec<u32> {
        let element_type = part.element_type();
        let aliases = unique.id_gd_aliases();
        let mut reachable: BTreeSet<u32> = BTreeSet::new();

        // Unique branch
        if !unique_base.is_empty() {
            let lines: &[EffectLine] = match region {
                Region::Main(_) => &MAIN_LINES,
                Region::Support => &SUPPORT_LINES,
            };
            let partials: Vec<(EffectLine, RoaringBitmap)> = lines
                .iter()
                .filter_map(|&line| {
                    let mut pl = unique_base.clone();
                    if !co1.is_empty() {
                        pl &= union_on_line(unique, line, co1);
                        if pl.is_empty() {
                            return None;
                        }
                    }
                    if !co2.is_empty() {
                        pl &= union_on_line(unique, line, co2);
                        if pl.is_empty() {
                            return None;
                        }
                    }
                    Some((line, pl))
                })
                .collect();

            if !partials.is_empty() {
                for entry in &unique.idgd_catalog().entries {
                    if entry.element_type != element_type {
                        continue;
                    }
                    let id = entry.id_gd;
                    for (line, pl) in &partials {
                        if let Some(bm) = unique.id_gd_per_line().get(&(id, *line)) {
                            if !pl.is_disjoint(bm) {
                                reachable.insert(id);
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Non-unique branch (already canonicalized by idGd at bind time!)
        if !nonunique_base.is_empty() {
            let nu_lines: Vec<NonUniqueEffectLine> = match region {
                Region::Main(_) => (1..=self.nu_max_main_lines)
                    .map(NonUniqueEffectLine::Main)
                    .collect(),
                Region::Support => vec![NonUniqueEffectLine::Echo],
            };
            let partials: Vec<(NonUniqueEffectLine, RoaringBitmap)> = nu_lines
                .into_iter()
                .filter_map(|line| {
                    let mut pl = nonunique_base.clone();
                    if !co1.is_empty() {
                        pl &= self.nu_union_on_line(aliases, line, co1);
                        if pl.is_empty() {
                            return None;
                        }
                    }
                    if !co2.is_empty() {
                        pl &= self.nu_union_on_line(aliases, line, co2);
                        if pl.is_empty() {
                            return None;
                        }
                    }
                    Some((line, pl))
                })
                .collect();

            if !partials.is_empty() {
                if let Some(candidate_ids) = self.nu_ids_by_type.get(element_type) {
                    for &canon_id in candidate_ids {
                        if reachable.contains(&canon_id) {
                            continue;
                        }
                        for (line, pl) in &partials {
                            if let Some(bm) = self.nu_per_line.get(&(canon_id, *line)) {
                                if !pl.is_disjoint(bm) {
                                    reachable.insert(canon_id);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }

        reachable.into_iter().collect()
    }

    fn nu_union_on_line(
        &self,
        aliases: &IdGdAliasMap,
        line: NonUniqueEffectLine,
        ids: &[u32],
    ) -> RoaringBitmap {
        let mut out = RoaringBitmap::new();
        for &id in ids {
            let canon = aliases.resolve(id);
            if let Some(bmp) = self.nu_per_line.get(&(canon, line)) {
                out |= bmp;
            }
        }
        out
    }

    fn nu_intersect_buckets_on_line(
        &self,
        aliases: &IdGdAliasMap,
        line: NonUniqueEffectLine,
        triggers: &[u32],
        conditions: &[u32],
        outputs: &[u32],
    ) -> Option<RoaringBitmap> {
        let mut groups: Vec<RoaringBitmap> = Vec::new();
        if !triggers.is_empty() {
            groups.push(self.nu_union_on_line(aliases, line, triggers));
        }
        if !conditions.is_empty() {
            groups.push(self.nu_union_on_line(aliases, line, conditions));
        }
        if !outputs.is_empty() {
            groups.push(self.nu_union_on_line(aliases, line, outputs));
        }
        let mut it = groups.into_iter();
        let mut bmp = it.next()?;
        for g in it {
            bmp &= g;
        }
        Some(bmp)
    }
}
