use axum::extract::{Json, Path, State};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub pronouns: Option<String>,
    pub bio: Option<String>,
    pub followers_count: i64,
}

pub async fn get_profile(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Profile>, AppError> {
    state
        .get_user_profile(id)
        .await
        .map(Json)
        .map_err(|e| e.into())
}
