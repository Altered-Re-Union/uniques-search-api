mod handlers;
pub(crate) mod models;
#[cfg(test)]
pub(crate) mod test_support;

pub(crate) mod parse;

use axum::extract::DefaultBodyLimit;
use axum::Router;

use crate::config::CardsSettings;
use crate::http::ServerState;

pub use models::{CardV2, CardsIter, CardsResponse};

pub fn router(cards: &CardsSettings) -> Router<ServerState> {
    use axum::routing::get;

    let cards_route = Router::new().route(
        "/api/v2/cards",
        get(handlers::get_cards_v2).post(handlers::post_cards_v2),
    );
    let cards_route = if cards.max_post_payload_bytes > 0 {
        let limit = cards.max_post_payload_bytes as usize;
        cards_route.layer(DefaultBodyLimit::max(limit))
    } else {
        cards_route
    };

    Router::new()
        .merge(cards_route)
        .route("/api/v2/card/{reference}", get(handlers::get_card_v2))
}
