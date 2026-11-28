use axum::extract::{RawQuery, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::http::api::cards::parse::parse_query_multimap;
use crate::http::api::error::{bad_request, ApiResult};
use crate::http::api::search::{parse_request as parse_search_request, search_bitmaps};
use crate::http::ServerState;

use super::filtered::{other_two_buckets, parse_editing};
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
        snapshot.effects.effects_body().as_ref().clone()
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

    let req = parse_search_request(&params, &snapshot, formats_enabled)?;

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

    let id_gds = snapshot.effects.reachable_id_gds(
        snapshot.index.as_ref(),
        &unique_base,
        &nonunique_base,
        part,
        region,
        &co1,
        &co2,
    );

    Ok(Json(EffectsFilteredResponse {
        editing: editing.to_string(),
        id_gds,
    }))
}
