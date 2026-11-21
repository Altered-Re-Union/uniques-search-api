//! `GET /api/v2/artists` — the artist names `artist[]` accepts on `/api/v2/search`,
//! `/api/v2/cards` and `/api/v2/effects/filtered`: the searchable names (people and studios) of
//! the printed credits of the loaded catalog (uniques + non-uniques). A team credit
//! ("Edward Chee - Seok Yeong") is listed as its members, never as the printed string (see
//! `index_core::artist_credits`).

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::http::ServerState;
use crate::http::state::QuerySnapshot;

pub fn router() -> Router<ServerState> {
    Router::new().route("/api/v2/artists", get(get_artists))
}

#[derive(Debug, Serialize)]
pub(crate) struct ArtistsResponse {
    artists: Vec<String>,
}

async fn get_artists(State(server): State<ServerState>) -> Json<ArtistsResponse> {
    Json(ArtistsResponse {
        artists: distinct_artists(&server.app.snapshot()),
    })
}

/// Searchable names of the distinct credits used by indexed cards, without empty values,
/// deduplicated and sorted case-insensitively (the first spelling met is kept).
fn distinct_artists(snapshot: &QuerySnapshot) -> Vec<String> {
    let unique = snapshot.index.catalog().families.iter().map(|f| f.artist.trim());
    let nonunique = snapshot
        .nonunique
        .as_deref()
        .into_iter()
        .flat_map(|nu| nu.prints.iter().map(|p| p.artist.trim()));
    let credits: BTreeSet<&str> = unique.chain(nonunique).filter(|a| !a.is_empty()).collect();

    let mut by_key: BTreeMap<String, String> = BTreeMap::new();
    for credit in credits {
        for name in snapshot.index.artist_credits.searchable_names(credit) {
            by_key.entry(name.to_lowercase()).or_insert(name);
        }
    }
    by_key.into_values().collect()
}
