use common::config::{DbConfig, ServerConfig};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub db: DbConfig,
    /// The user database, connected as `session_verifier`.
    pub auth_db: DbConfig,
    pub profile_api: ProfileApiConfig,
}

/// Where author names are looked up (`GET {url}/internal/profiles?ids=`).
#[derive(Deserialize)]
pub struct ProfileApiConfig {
    pub url: String,
}

impl AppConfig {
    /// `config.toml` in the working directory, overridden by `ARTICLE_API_<SECTION>__<KEY>`.
    pub fn load() -> anyhow::Result<Self> {
        common::config::load("ARTICLE_API")
    }
}
