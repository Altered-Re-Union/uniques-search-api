use std::collections::BTreeMap;

use axum::extract::{Path, RawQuery, State};
use axum::Json;

use index_core::idgd_catalog::IdGdCatalogEntry;

use crate::http::api::cards::parse::{parse_query_multimap, parse_request, QueryMultiMap};
use crate::http::api::error::{bad_request, map_query_error, not_found, ApiResult};
use crate::http::ServerState;
use crate::index::{
    build_bitmap, card_v2_from_index, cards_from_indices, enrich_unique_card_v2,
    families_from_bitmap, nonunique_card_v2, page_cards_v2_sorted, subtype_names_from_index,
    CardResolveError,
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
    let index = snapshot.index.as_ref();

    if let Some(nu) = snapshot.nonunique.as_deref() {
        if let Some(print_index) = nu.resolve_reference(&reference) {
            let subtype_names = subtype_names_from_index(index);
            if let Some(card) = nonunique_card_v2(nu, print_index, &snapshot.families, &subtype_names)
            {
                return Ok(Json(card));
            }
        }
    }

    let card_index = index.resolve_card_index(&reference).map_err(|e| match e {
        CardResolveError::BadRequest { message } => bad_request(message),
        CardResolveError::NotFound { message } => not_found(message),
    })?;

    let view = index.card_view(card_index).ok_or_else(|| {
        not_found(format!("missing compact record for card_index {card_index}"))
    })?;

    if view.faction_code() == 0 {
        return Err(not_found(format!("card not indexed at {reference}")));
    }

    let idgd_by_id: BTreeMap<u32, &IdGdCatalogEntry> = index
        .idgd_catalog()
        .entries
        .iter()
        .map(|e| (e.id_gd, e))
        .collect();

    let mut card = card_v2_from_index(index, card_index, &idgd_by_id, debug_bga_trigram)
        .map_err(map_query_error)?;
    enrich_unique_card_v2(
        &mut card,
        card_index,
        index,
        snapshot.nonunique.as_deref(),
        snapshot.formats.as_ref(),
    );

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
