use common::config::{DbConfig, ServerConfig};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub db: DbConfig,
    /// The user database, connected as `session_verifier`.
    pub auth_db: DbConfig,
    pub profile_api: ServiceConfig,
    pub article_api: ServiceConfig,
}

#[derive(Deserialize)]
pub struct ServiceConfig {
    pub url: String,
}

impl AppConfig {
    /// `config.toml` in the working directory, overridden by `COMMENT_API_<SECTION>__<KEY>`.
    pub fn load() -> anyhow::Result<Self> {
        common::config::load("COMMENT_API")
    }
}
