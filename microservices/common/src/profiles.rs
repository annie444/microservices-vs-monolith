//! Author names for articles and comments come from profile-api on every read
//! (`GET /internal/profiles?ids=`), one batched call per request.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Author {
    pub id: Uuid,
    pub name: String,
    pub pronouns: Option<String>,
}

#[derive(Clone)]
pub struct ProfileClient {
    http: reqwest::Client,
    base_url: String,
}

impl ProfileClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
            base_url: base_url.trim_end_matches('/').to_owned(),
        }
    }

    /// Authors for `ids` (duplicates fine). Empty input makes no request. Unknown ids are
    /// simply absent from the map. Any failure is logged and surfaces as 502.
    pub async fn authors(&self, ids: &[Uuid]) -> Result<HashMap<Uuid, Author>, AppError> {
        let mut distinct: Vec<Uuid> = ids.to_vec();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.is_empty() {
            return Ok(HashMap::new());
        }
        let ids = distinct
            .iter()
            .map(Uuid::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let result = async {
            self.http
                .get(format!("{}/internal/profiles", self.base_url))
                .query(&[("ids", ids)])
                .send()
                .await?
                .error_for_status()?
                .json::<Vec<Author>>()
                .await
        }
        .await;
        match result {
            Ok(authors) => Ok(authors.into_iter().map(|a| (a.id, a)).collect()),
            Err(e) => {
                tracing::error!(error = %e, "profile-api lookup failed");
                Err(AppError::Upstream)
            }
        }
    }
}
