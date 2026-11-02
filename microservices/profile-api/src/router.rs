use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::routes::*;
use crate::state::AppState;

pub fn init(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route(
            "/api/v1/profiles/me",
            get(profile::me).put(profile::update_me),
        )
        .route("/api/v1/profiles/{id}", get(profile::get_profile))
        .route(
            "/api/v1/profiles/{id}/follow",
            post(profile::follow).delete(profile::unfollow),
        )
        // Service-to-service only; the gateway must not route /internal.
        .route(
            "/internal/profiles",
            post(profile::create_internal).get(profile::authors_internal),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(
            TraceLayer::new_for_http()
                .on_request(())
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(state)
}
