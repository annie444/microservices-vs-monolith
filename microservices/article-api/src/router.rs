use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::routes::*;
use crate::state::AppState;

const ARTICLE_BODY_LIMIT: usize = 256 * 1024;

pub fn init(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/api/v1/articles/feed", get(articles::feed))
        // Article bodies may exceed the router-wide limit; the inner layer wins.
        .route(
            "/api/v1/articles/new",
            post(articles::create).layer(DefaultBodyLimit::max(ARTICLE_BODY_LIMIT)),
        )
        .route(
            "/api/v1/articles/{id}",
            get(articles::get_article)
                .put(articles::update)
                .layer(DefaultBodyLimit::max(ARTICLE_BODY_LIMIT)),
        )
        .route(
            "/api/v1/articles/{id}/favorite",
            post(articles::favorite).delete(articles::unfavorite),
        )
        // Service-to-service only (profile-api's outbox); the gateway must not route /internal.
        .route("/internal/follow-events", post(internal::follow_events))
        .route("/internal/articles/{id}", get(internal::article_ref))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(
            TraceLayer::new_for_http()
                .on_request(())
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(state)
}
