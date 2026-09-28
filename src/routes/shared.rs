use actix_web::{HttpRequest, http::header};

use crate::{
    access::require_writable, app::AppState, auth::current_user, error::ApiError, models::UserDoc,
};

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

/// The signed-in user, for requests that change their training data.
pub async fn writer(request: &HttpRequest, state: &AppState) -> Result<UserDoc, ApiError> {
    let user = user(request, state).await?;
    require_writable(&state.db, &user).await?;
    Ok(user)
}

pub async fn coach(request: &HttpRequest, state: &AppState) -> Result<UserDoc, ApiError> {
    let user = user(request, state).await?;
    if user.is_coach() {
        Ok(user)
    } else {
        Err(ApiError::Forbidden)
    }
}

/// The owner runs the service: accounts, coaches and payments.
pub async fn admin(request: &HttpRequest, state: &AppState) -> Result<UserDoc, ApiError> {
    let user = user(request, state).await?;
    if user.is_owner() {
        Ok(user)
    } else {
        Err(ApiError::Forbidden)
    }
}
