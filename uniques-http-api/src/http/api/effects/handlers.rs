use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::Json;

use index_core::bitmap::EffectLine;

use crate::http::api::cards::parse::{parse_query_multimap, parse_request};
use crate::http::api::error::{bad_request, ApiResult};
use crate::http::{IndexSnapshot, ServerState};
use crate::index::build_bitmap;

use super::filtered::{other_two_buckets, parse_editing, union_on_line, MAIN_LINES, SUPPORT_LINES};
use super::models::{EffectsFilteredResponse, Region};

pub async fn get_effects_v2(IndexSnapshot(index): IndexSnapshot, headers: HeaderMap) -> Response {
    let gzip = index.effects_body_gzip().filter(|_| accepts_gzip(&headers));
    match gzip {
        Some(body) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CONTENT_ENCODING, "gzip"),
                (header::VARY, "accept-encoding"),
            ],
            body.as_ref().clone(),
        )
            .into_response(),
        None => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::VARY, "accept-encoding"),
            ],
            index.effects_body().as_ref().clone(),
        )
            .into_response(),
    }
}

/// `Accept-Encoding` lists `gzip` (or `*`) without `q=0`.
fn accepts_gzip(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|item| {
            let mut parts = item.split(';').map(str::trim);
            let coding = parts.next().unwrap_or_default();
            let refused = parts.any(|p| {
                p.strip_prefix("q=")
                    .and_then(|q| q.parse::<f32>().ok())
                    .is_some_and(|q| q == 0.0)
            });
            (coding.eq_ignore_ascii_case("gzip") || coding == "*") && !refused
        })
}

pub async fn get_effects_filtered(
    State(server): State<ServerState>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<EffectsFilteredResponse>> {
    let params = parse_query_multimap(query.as_deref())?;
    let snapshot = server.app.snapshot();
    let index = snapshot.index.as_ref();
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

    // Full current filter state (validates idGd types, including the edited group).
    let collections = &snapshot.collections;
    let req = parse_request(index, formats, formats_enabled, collections, &params)?;

    // Co-constraints = the edited group's other two boxes (the edited part is ignored).
    let (co1, co2) = match region {
        Region::Main(n) => match req.filters.effects.iter().find(|s| s.index == n) {
            Some(slot) => other_two_buckets(part, &slot.t, &slot.c, &slot.o),
            None => (Vec::new(), Vec::new()),
        },
        Region::Support => other_two_buckets(
            part,
            &req.filters.support_t,
            &req.filters.support_c,
            &req.filters.support_o,
        ),
    };

    // Base = the reduced search space with the edited group removed (exact, by index).
    let mut base_req = req.clone();
    match region {
        Region::Main(n) => base_req.filters.effects.retain(|s| s.index != n),
        Region::Support => {
            base_req.filters.support_t.clear();
            base_req.filters.support_c.clear();
            base_req.filters.support_o.clear();
        }
    }
    let base = build_bitmap(index, formats, collections, &base_req)
        .map_err(crate::http::api::error::map_query_error)?;

    let lines: &[EffectLine] = match region {
        Region::Main(_) => &MAIN_LINES,
        Region::Support => &SUPPORT_LINES,
    };

    // Per-line partial: Base intersected with the group's other boxes on that same line.
    let mut partials = Vec::new();
    for &line in lines {
        let mut pl = base.clone();
        if pl.is_empty() {
            break;
        }
        if !co1.is_empty() {
            pl &= union_on_line(&index, line, &co1);
            if pl.is_empty() {
                continue;
            }
        }
        if !co2.is_empty() {
            pl &= union_on_line(&index, line, &co2);
            if pl.is_empty() {
                continue;
            }
        }
        partials.push((line, pl));
    }

    // A candidate of the edited part's type is reachable iff it shares at least one card with the
    // partial on some line (same-line co-occurrence with the group's other boxes).
    let element_type = part.element_type();
    let mut id_gds: Vec<u32> = Vec::new();
    if !partials.is_empty() {
        for entry in &index.idgd_catalog().entries {
            if entry.element_type != element_type {
                continue;
            }
            let id = entry.id_gd;
            for (line, pl) in &partials {
                if let Some(bm) = index.id_gd_per_line().get(&(id, *line)) {
                    if !pl.is_disjoint(bm) {
                        id_gds.push(id);
                        break;
                    }
                }
            }
        }
    }
    id_gds.sort_unstable();

    Ok(Json(EffectsFilteredResponse {
        editing: editing.to_string(),
        id_gds,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(accept_encoding: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT_ENCODING, accept_encoding.parse().unwrap());
        h
    }

    #[test]
    fn accepts_gzip_parses_accept_encoding() {
        assert!(accepts_gzip(&headers("gzip")));
        assert!(accepts_gzip(&headers("br, gzip;q=0.8, deflate")));
        assert!(accepts_gzip(&headers("*")));
        assert!(!accepts_gzip(&headers("br, deflate")));
        assert!(!accepts_gzip(&headers("gzip;q=0")));
        assert!(!accepts_gzip(&HeaderMap::new()));
    }
}
