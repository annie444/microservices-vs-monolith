use axum::Json;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug)]
pub enum AppError {
    Unauthorized,
    Conflict(&'static str),
    Unprocessable(&'static str),
    TooManyRequests(u64),
    Internal(anyhow::Error),
}

#[derive(Serialize)]
struct Body {
    error: &'static str,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error) = match &self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Conflict(m) => (StatusCode::CONFLICT, *m),
            Self::Unprocessable(m) => (StatusCode::UNPROCESSABLE_ENTITY, *m),
            Self::TooManyRequests(_) => (StatusCode::TOO_MANY_REQUESTS, "too many attempts"),
            Self::Internal(e) => {
                tracing::error!(error = ?e);
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error")
            }
        };
        let mut response = (status, Json(Body { error })).into_response();
        if let Self::TooManyRequests(secs) = self {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, secs.into());
        }
        response
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        Self::Internal(err.into())
    }
}
