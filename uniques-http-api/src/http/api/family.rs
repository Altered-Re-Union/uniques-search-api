//! `GET /api/v2/family/{id}` — family as a first-class concept (Lot 5, first slice).
//!
//! See `cli-indexer/plans/23-family-catalog.md`. Returns the family's name/type/subtypes once
//! (from `families.json`), plus every matching unique and non-unique print.
//!
//! `uniquePrints` is paginated (`page`/`limit`, default 100): a family can have thousands of
//! unique instances, and returning them unbounded is exactly the RAM problem Lot 3 (plan 20) fixed
//! for `withFamilies` — no point reintroducing it here. `nonUniquePrints` isn't: a family's
//! non-unique print count is always small (a handful of rarities/products), not worth the
//! complexity.

use std::collections::BTreeMap;

use axum::extract::{Path, RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::http::ServerState;

use super::error::{bad_request, not_found, ApiResult};

pub fn router() -> Router<ServerState> {
    Router::new().route("/api/v2/family/{id}", get(get_family))
}

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;

fn parse_page_limit(query: Option<&str>) -> ApiResult<(u32, usize)> {
    let mut page = 1u32;
    let mut limit = DEFAULT_LIMIT;
    if let Some(q) = query {
        for (key, value) in url::form_urlencoded::parse(q.as_bytes()) {
            match key.as_ref() {
                "page" => {
                    page = value
                        .parse::<u32>()
                        .map_err(|_| bad_request("page must be an integer".to_string()))?;
                    if page == 0 {
                        return Err(bad_request("page must be >= 1".to_string()));
                    }
                }
                "limit" => {
                    limit = value
                        .parse::<usize>()
                        .map_err(|_| bad_request("limit must be an integer".to_string()))?;
                }
                _ => {}
            }
        }
    }
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(bad_request(format!("limit must be in range 1..={MAX_LIMIT}")));
    }
    Ok((page, limit))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FamilyResponse {
    family_id: i64,
    name: BTreeMap<String, String>,
    card_type: String,
    subtypes: Vec<String>,
    unique_print_total: u32,
    unique_prints: Vec<String>,
    non_unique_prints: Vec<String>,
}

async fn get_family(
    State(server): State<ServerState>,
    Path(id): Path<i64>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<FamilyResponse>> {
    let (page, limit) = parse_page_limit(query.as_deref())?;
    let snapshot = server.app.snapshot();
    let family = snapshot
        .family_page(id, page, limit)
        .ok_or_else(|| not_found(format!("family {id} not found")))?;

    Ok(Json(FamilyResponse {
        family_id: family.family_id,
        name: family.name,
        card_type: family.card_type,
        subtypes: family.subtypes,
        unique_print_total: family.unique_print_total,
        unique_prints: family.unique_prints,
        non_unique_prints: family.non_unique_prints,
    }))
}
