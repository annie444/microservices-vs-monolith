use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, put};
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::routes::*;
use crate::state::AppState;

pub fn init(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route(
            "/api/v1/comments/{article_id}",
            get(comments::list).post(comments::create),
        )
        .route(
            "/api/v1/comments/{article_id}/{comment_id}",
            put(comments::update).delete(comments::delete),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(
            TraceLayer::new_for_http()
                .on_request(())
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(state)
}
