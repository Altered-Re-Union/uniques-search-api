use axum::extract::{Path, RawQuery, State};
use axum::Json;

use crate::http::api::cards::parse::{parse_query_multimap, parse_request, QueryMultiMap};
use crate::http::api::error::{map_query_error, ApiResult};
use crate::http::ServerState;
use crate::index::{
    build_bitmap, cards_from_indices, families_from_bitmap, page_cards_v2_sorted,
};
use super::{CardV2, CardsIter, CardsResponse};

pub async fn get_card_v2(
    State(server): State<ServerState>,
    Path(reference): Path<String>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<CardV2>> {
    let params = parse_query_multimap(query.as_deref())?;
    let debug_bga_trigram = params.contains_key("debug_bga_trigram");

    let snapshot = server.app.snapshot();
    let card = snapshot.card_by_reference(&reference, debug_bga_trigram)?;
    Ok(Json(card))
}

pub async fn get_cards_v2(
    State(server): State<ServerState>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<CardsResponse>> {
    let params = parse_query_multimap(query.as_deref())?;
    respond_cards(&server, &params)
}

/// Same parameters as `GET /api/v2/cards`, carried in a form-urlencoded body instead of the query
/// string — for callers who'd otherwise hit the query-string size ceiling with a large `ref[]`/
/// `collectorNumber[]` list. Identical response shape, identical semantics — see
/// `uniques-http-api/plans/25-post-cards-endpoint.md`.
pub async fn post_cards_v2(
    State(server): State<ServerState>,
    body: String,
) -> ApiResult<Json<CardsResponse>> {
    let params = parse_query_multimap(Some(&body))?;
    respond_cards(&server, &params)
}

fn respond_cards(server: &ServerState, params: &QueryMultiMap) -> ApiResult<Json<CardsResponse>> {
    let snapshot = server.app.snapshot();
    let index = snapshot.index.as_ref();
    let formats = snapshot.formats.as_ref();
    let formats_enabled = server
        .settings
        .formats
        .as_ref()
        .is_some_and(|f| f.is_enabled());
    let collections = &snapshot.collections;
    let req = parse_request(index, formats, formats_enabled, collections, params)?;
    let bitmap = build_bitmap(index, formats, collections, &req).map_err(map_query_error)?;
    let total = bitmap.len() as u64;

    let (cards, next_cursor, families) = if req.with_families && req.cursor.is_none() {
        let (families, example_indices) =
            families_from_bitmap(&index, &bitmap, req.page, req.limit).map_err(map_query_error)?;
        let cards = cards_from_indices(&index, &example_indices, req.debug_bga_trigram)
            .map_err(map_query_error)?;
        (cards, None, Some(families))
    } else {
        let (cards, next_cursor) = page_cards_v2_sorted(
            &index,
            &bitmap,
            req.cursor,
            req.page,
            req.limit,
            req.sort.as_ref(),
            req.debug_bga_trigram,
        )
        .map_err(map_query_error)?;
        (cards, next_cursor, None)
    };

    Ok(Json(CardsResponse {
        iter: CardsIter {
            total,
            cursor: next_cursor,
            page: req.page,
        },
        cards,
        families,
    }))
}
