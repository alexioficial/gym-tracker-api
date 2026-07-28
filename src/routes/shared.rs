use actix_web::{HttpRequest, http::header};

use crate::{app::AppState, auth::current_user, error::ApiError, models::UserDoc};

pub fn require_same_origin(request: &HttpRequest, state: &AppState) -> Result<(), ApiError> {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    if origin == Some(state.config.frontend_origin.as_str()) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

pub async fn user(request: &HttpRequest, state: &AppState) -> Result<UserDoc, ApiError> {
    current_user(request, &state.db).await
}

pub async fn admin(request: &HttpRequest, state: &AppState) -> Result<UserDoc, ApiError> {
    let user = user(request, state).await?;
    if user.is_admin {
        Ok(user)
    } else {
        Err(ApiError::Forbidden)
    }
}
