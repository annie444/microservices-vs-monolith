//! Article existence and ownership for comment-api, from article-api's
//! `GET /internal/articles/{id}`.

use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArticleRef {
    pub id: Uuid,
    pub creator_id: Uuid,
}

#[derive(Clone)]
pub struct ArticleClient {
    http: reqwest::Client,
    base_url: String,
}

impl ArticleClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
            base_url: base_url.trim_end_matches('/').to_owned(),
        }
    }

    /// `None` when article-api says 404. Any other failure is logged and surfaces as 502.
    pub async fn get(&self, id: Uuid) -> Result<Option<ArticleRef>, AppError> {
        let result = async {
            let response = self
                .http
                .get(format!("{}/internal/articles/{id}", self.base_url))
                .send()
                .await?;
            if response.status() == StatusCode::NOT_FOUND {
                return Ok(None);
            }
            response
                .error_for_status()?
                .json::<ArticleRef>()
                .await
                .map(Some)
        }
        .await;
        match result {
            Ok(article) => Ok(article),
            Err(e) => {
                tracing::error!(error = %e, "article-api lookup failed");
                Err(AppError::Upstream)
            }
        }
    }
}
