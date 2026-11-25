use std::collections::BTreeSet;

use axum::extract::{RawQuery, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use index_core::bitmap::EffectLine;

use crate::http::api::cards::parse::parse_query_multimap;
use crate::http::api::error::{bad_request, ApiResult};
use crate::http::api::search::{
    nonunique_union_on_line, parse_request as parse_search_request, search_bitmaps,
};
use crate::http::ServerState;
use crate::index::loader::NonUniqueEffectLine;

use super::filtered::{other_two_buckets, parse_editing, union_on_line, MAIN_LINES, SUPPORT_LINES};
use super::models::{EffectsFilteredResponse, Region};

pub async fn get_effects_v2(
    State(server): State<ServerState>,
    RawQuery(query): RawQuery,
) -> Response {
    let snapshot = server.app.snapshot();
    let unique_only = query.as_deref().is_some_and(|q| {
        q.split('&').any(|pair| {
            let mut kv = pair.splitn(2, '=');
            kv.next() == Some("kind")
                && kv.next().is_some_and(|v| v.eq_ignore_ascii_case("unique"))
        })
    });
    let body = if unique_only {
        snapshot.index.effects_body().as_ref().clone()
    } else {
        snapshot.effects_body.as_ref().clone()
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

pub async fn get_effects_filtered(
    State(server): State<ServerState>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<EffectsFilteredResponse>> {
    let params = parse_query_multimap(query.as_deref())?;
    let snapshot = server.app.snapshot();
    let index = snapshot.index.as_ref();
    let nonunique = snapshot.nonunique.as_deref();
    let formats = snapshot.formats.as_ref();
    let formats_enabled = server
        .settings
        .formats
        .as_ref()
        .is_some_and(|f| f.is_enabled());

    let editing = params
        .get("editing")
        .and_then(|v| v.first())
        .map(|s| s.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            bad_request(
                "missing required query parameter 'editing' (format '<part>:<slot>', \
                 e.g. trigger:0 or output:support)"
                    .to_string(),
            )
        })?;

    let (part, region) = parse_editing(editing)?;

    let collections = &snapshot.collections;
    let req = parse_search_request(
        &params,
        index,
        nonunique,
        formats,
        formats_enabled,
        collections,
    )?;

    // Co-constraints = the edited group's other two boxes (the edited part is ignored).
    let (co1, co2) = match region {
        Region::Main(n) => match req.effects.iter().find(|s| s.index == n) {
            Some(slot) => other_two_buckets(part, &slot.t, &slot.c, &slot.o),
            None => (Vec::new(), Vec::new()),
        },
        Region::Support => other_two_buckets(part, &req.support_t, &req.support_c, &req.support_o),
    };

    // Base = the reduced search space (both unique + non-unique) with the edited group removed.
    let mut base_req = req.clone();
    match region {
        Region::Main(n) => base_req.effects.retain(|s| s.index != n),
        Region::Support => {
            base_req.support_t.clear();
            base_req.support_c.clear();
            base_req.support_o.clear();
        }
    }
    let (unique_base, nonunique_base) = search_bitmaps(&snapshot, &base_req)?;

    let element_type = part.element_type();
    let mut reachable: BTreeSet<u32> = BTreeSet::new();

    // --- Unique branch ---
    if !unique_base.is_empty() {
        let lines: &[EffectLine] = match region {
            Region::Main(_) => &MAIN_LINES,
            Region::Support => &SUPPORT_LINES,
        };
        let mut partials = Vec::new();
        for &line in lines {
            let mut pl = unique_base.clone();
            if !co1.is_empty() {
                pl &= union_on_line(index, line, &co1);
                if pl.is_empty() {
                    continue;
                }
            }
            if !co2.is_empty() {
                pl &= union_on_line(index, line, &co2);
                if pl.is_empty() {
                    continue;
                }
            }
            partials.push((line, pl));
        }
        if !partials.is_empty() {
            for entry in &index.idgd_catalog().entries {
                if entry.element_type != element_type {
                    continue;
                }
                let id = entry.id_gd;
                for (line, pl) in &partials {
                    if let Some(bm) = index.id_gd_per_line().get(&(id, *line)) {
                        if !pl.is_disjoint(bm) {
                            reachable.insert(id);
                            break;
                        }
                    }
                }
            }
        }
    }

    // --- Non-unique branch ---
    if let (Some(nu), false) = (nonunique, nonunique_base.is_empty()) {
        let nu_lines: Vec<NonUniqueEffectLine> = match region {
            Region::Main(_) => (1..=nu.max_main_lines)
                .map(NonUniqueEffectLine::Main)
                .collect(),
            Region::Support => vec![NonUniqueEffectLine::Echo],
        };
        let mut partials = Vec::new();
        for line in nu_lines {
            let mut pl = nonunique_base.clone();
            if !co1.is_empty() {
                pl &= nonunique_union_on_line(nu, index, line, &co1);
                if pl.is_empty() {
                    continue;
                }
            }
            if !co2.is_empty() {
                pl &= nonunique_union_on_line(nu, index, line, &co2);
                if pl.is_empty() {
                    continue;
                }
            }
            partials.push((line, pl));
        }
        if !partials.is_empty() {
            for entry in &nu.idgd_catalog {
                if entry.element_type != element_type {
                    continue;
                }
                let raw_id = entry.id_gd;
                let canon_id = index.resolve_id_gd(raw_id);
                if reachable.contains(&canon_id) {
                    continue;
                }
                for (line, pl) in &partials {
                    if let Some(bm) = nu.id_gd_per_line.get(&(raw_id, *line)) {
                        if !pl.is_disjoint(bm) {
                            reachable.insert(canon_id);
                            break;
                        }
                    }
                }
            }
        }
    }

    Ok(Json(EffectsFilteredResponse {
        editing: editing.to_string(),
        id_gds: reachable.into_iter().collect(),
    }))
}
