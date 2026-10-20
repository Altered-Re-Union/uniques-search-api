mod filtered;
mod handlers;
mod list;
mod models;

use axum::{routing::get, Router};

use crate::http::ServerState;

pub use list::{build_effects_list, gzip_effects_body, serialize_effects_list};

pub fn router() -> Router<ServerState> {
    Router::new()
        .route("/api/v2/effects", get(handlers::get_effects_v2))
        .route(
            "/api/v2/effects/filtered",
            get(handlers::get_effects_filtered),
        )
}
