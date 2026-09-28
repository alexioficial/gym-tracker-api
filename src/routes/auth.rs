use actix_web::{HttpRequest, HttpResponse, web};
use mongodb::bson::{DateTime, doc};

use crate::{
    access::is_read_only,
    app::AppState,
    auth::{
        create_session, destroy_session, expired_session_cookie, hash_password, public_user,
        session_cookie, verify_dummy_password, verify_password,
    },
    config::normalize_username,
    error::ApiError,
    models::{LoginInput, UserDoc, UserOut},
    rate_limit,
    routes::shared::{require_same_origin, user},
};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/auth/login", web::post().to(login))
        .route("/auth/logout", web::post().to(logout))
        .route("/auth/me", web::get().to(me));
}

async fn login(
    request: HttpRequest,
    state: web::Data<AppState>,
    input: web::Json<LoginInput>,
) -> Result<HttpResponse, ApiError> {
    let username = normalize_username(&input.username);
    let client = rate_limit::login_key(&request, state.config.trust_proxy_headers, &username);
    state
        .rate_limits
        .check_login(&client)
        .map_err(|retry_after| ApiError::RateLimited { retry_after })?;
    require_same_origin(&request, &state)?;
    let users = state.db.collection::<UserDoc>("users");
    let account = users.find_one(doc! { "username": username }).await?;
    let Some(account) = account else {
        verify_dummy_password(&input.password);
        return Err(ApiError::Unauthorized);
    };
    if !verify_password(&input.password, &account.password_hash)? {
        return Err(ApiError::Unauthorized);
    }
    // Checked after the password so the message does not reveal the account.
    if account.disabled {
        return Err(ApiError::Disabled);
    }
    // A successful login upgrades hashes created by the former Node backend.
    if account.password_hash.starts_with("scrypt$") {
        users
            .update_one(
                doc! { "_id": account.id },
                doc! { "$set": { "passwordHash": hash_password(&input.password)?, "updatedAt": DateTime::now() } },
            )
            .await?;
    }
    let token = create_session(&state.db, account.id).await?;
    Ok(HttpResponse::Ok()
        .cookie(session_cookie(token, &state.config))
        .json(public_user(&account)))
}

async fn logout(
    request: HttpRequest,
    state: web::Data<AppState>,
) -> Result<HttpResponse, ApiError> {
    require_same_origin(&request, &state)?;
    destroy_session(&request, &state.db).await?;
    Ok(HttpResponse::NoContent()
        .cookie(expired_session_cookie(&state.config))
        .finish())
}

async fn me(
    request: HttpRequest,
    state: web::Data<AppState>,
) -> Result<web::Json<UserOut>, ApiError> {
    let current = user(&request, &state).await?;
    let mut out = public_user(&current);
    out.read_only = is_read_only(&state.db, &current).await?;
    Ok(web::Json(out))
}
