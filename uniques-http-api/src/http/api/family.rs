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

use index_core::family_catalog::FamilyCatalogEntry;

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
    let catalog_entry: Option<&FamilyCatalogEntry> = snapshot.families.get(&id);

    // A merged multi-set index could in principle have more than one `FamilyEntry` row sharing a
    // `card_family_id` (e.g. the same character printed in two sets) — today's single-set-at-a-
    // time deployments never do, so the first match is taken; revisit once non-unique gets its own
    // multi-set merge story (plan 16/D16's `edition` caveat applies here too).
    let unique_family = snapshot
        .index
        .catalog()
        .families
        .iter()
        .find(|f| f.card_family_id == Some(id));

    let unique_print_total = unique_family.map(|f| f.max_unique_id).unwrap_or(0);
    let unique_prints = unique_family
        .map(|f| {
            let set = f
                .source_set
                .as_deref()
                .unwrap_or(&snapshot.index.catalog().set);
            let start = (page.saturating_sub(1) as u64) * limit as u64;
            let end = (start + limit as u64).min(f.max_unique_id as u64);
            if start >= f.max_unique_id as u64 {
                Vec::new()
            } else {
                ((start as u32 + 1)..=(end as u32))
                    .map(|uid| format!("ALT_{set}_B_{}_{}_U_{uid}", f.faction, f.family_number))
                    .collect()
            }
        })
        .unwrap_or_default();

    let non_unique_prints: Vec<String> = snapshot
        .nonunique
        .as_ref()
        .and_then(|nu| {
            nu.family
                .get(&id.to_string())
                .map(|bitmap| bitmap.iter().filter_map(|idx| nu.reference_for(idx).map(str::to_string)).collect())
        })
        .unwrap_or_default();

    if catalog_entry.is_none() && unique_family.is_none() && non_unique_prints.is_empty() {
        return Err(not_found(format!("family {id} not found")));
    }

    Ok(Json(FamilyResponse {
        family_id: id,
        name: catalog_entry.map(|e| e.name.clone()).unwrap_or_default(),
        card_type: catalog_entry
            .map(|e| e.card_type.clone())
            .unwrap_or_default(),
        subtypes: catalog_entry.map(|e| e.subtypes.clone()).unwrap_or_default(),
        unique_print_total,
        unique_prints,
        non_unique_prints,
    }))
}
