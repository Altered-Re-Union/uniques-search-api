pub(crate) mod admin;
pub mod api;
pub mod extract;
pub mod state;

use axum::Router;
use tower_http::cors::CorsLayer;

pub use extract::IndexSnapshot;
pub use state::{AppState, QuerySnapshot, ServerState};

pub fn app(server: ServerState) -> Router {
    let collections = server.settings.collections.clone();
    let cards = server.settings.cards.clone();
    Router::new()
        .merge(admin::router())
        .merge(api::router(&collections, &cards))
        .layer(CorsLayer::permissive())
        .with_state(server)
}
