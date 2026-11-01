use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::routes::*;
use crate::state::AppState;

pub fn init(state: AppState) -> Router {
    std::sync::LazyLock::force(&user::DUMMY_HASH);
    Router::new()
        .route("/healthz", get(health))
        .route("/api/v1/users/register", post(user::register))
        .route("/api/v1/users/login", post(user::login))
        .route("/api/v1/users/token", post(user::token))
        .route("/api/v1/users/me", get(user::me))
        .route(
            "/api/v1/profiles/me",
            get(profile::me).put(profile::update_me),
        )
        .route("/api/v1/profiles/follow", post(profile::follow))
        .route("/api/v1/profiles/{id}", get(profile::get_profile))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(
            TraceLayer::new_for_http()
                .on_request(())
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(state)
}
