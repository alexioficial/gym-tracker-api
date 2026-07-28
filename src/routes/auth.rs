use actix_web::{HttpRequest, HttpResponse, web};
use mongodb::bson::{DateTime, doc};

use crate::{
    app::AppState,
    auth::{
        create_session, destroy_session, expired_session_cookie, hash_password, public_user,
        session_cookie, verify_password,
    },
    config::normalize_username,
    error::ApiError,
    models::{LoginInput, UserDoc, UserOut},
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
    require_same_origin(&request, &state)?;
    let username = normalize_username(&input.username);
    let users = state.db.collection::<UserDoc>("users");
    let account = users.find_one(doc! { "username": username }).await?;
    let Some(account) = account else {
        return Err(ApiError::Unauthorized);
    };
    if !verify_password(&input.password, &account.password_hash)? {
        return Err(ApiError::Unauthorized);
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
    Ok(web::Json(public_user(&user(&request, &state).await?)))
}
