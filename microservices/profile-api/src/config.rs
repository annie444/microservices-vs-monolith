use common::config::{DbConfig, ServerConfig};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub db: DbConfig,
    /// The user database, connected as `session_verifier`.
    pub auth_db: DbConfig,
    pub article_api: ArticleApiConfig,
}

/// Where follow/unfollow events are delivered (`POST {url}/internal/follow-events`).
#[derive(Deserialize)]
pub struct ArticleApiConfig {
    pub url: String,
}

impl AppConfig {
    /// `config.toml` in the working directory, overridden by `PROFILE_API_<SECTION>__<KEY>`.
    pub fn load() -> anyhow::Result<Self> {
        common::config::load("PROFILE_API")
    }
}
