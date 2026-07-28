use actix_web::{HttpRequest, HttpResponse, web};
use futures::TryStreamExt;
use mongodb::bson::{DateTime, Document, doc, oid::ObjectId};

use crate::{
    app::AppState,
    auth::{hash_password, password_is_valid, revoke_user_sessions},
    config::normalize_username,
    error::ApiError,
    models::{PasswordInput, UserDoc, UserInput, UserOut},
    routes::shared::{admin, require_same_origin},
    validation::{object_id, valid_username},
};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/admin/users", web::get().to(list))
        .route("/admin/users", web::post().to(create))
        .route("/admin/users/{id}/password", web::put().to(reset_password))
        .route("/admin/users/{id}", web::delete().to(delete));
}

async fn list(
    request: HttpRequest,
    state: web::Data<AppState>,
) -> Result<web::Json<Vec<UserOut>>, ApiError> {
    admin(&request, &state).await?;
    let users = state
        .db
        .collection::<UserDoc>("users")
        .find(doc! {})
        .sort(doc! { "isAdmin": -1, "username": 1 })
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    Ok(web::Json(users.iter().map(UserOut::from).collect()))
}

async fn create(
    request: HttpRequest,
    state: web::Data<AppState>,
    input: web::Json<UserInput>,
) -> Result<web::Json<UserOut>, ApiError> {
    require_same_origin(&request, &state)?;
    admin(&request, &state).await?;
    let username = normalize_username(&input.username);
    if !valid_username(&username) {
        return Err(ApiError::Validation("Invalid username".to_owned()));
    }
    if !password_is_valid(&input.password) {
        return Err(ApiError::Validation(
            "Password must be at least 6 characters".to_owned(),
        ));
    }
    let users = state.db.collection::<UserDoc>("users");
    if users
        .find_one(doc! { "username": &username })
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict(
            "That username is already taken".to_owned(),
        ));
    }
    let now = DateTime::now();
    let user = UserDoc {
        id: ObjectId::new(),
        username,
        password_hash: hash_password(&input.password)?,
        is_admin: false,
        created_at: now,
        updated_at: now,
    };
    users.insert_one(user.clone()).await?;
    Ok(web::Json(UserOut::from(&user)))
}

async fn reset_password(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
    input: web::Json<PasswordInput>,
) -> Result<HttpResponse, ApiError> {
    require_same_origin(&request, &state)?;
    admin(&request, &state).await?;
    let id = object_id(&path)?;
    if !password_is_valid(&input.password) {
        return Err(ApiError::Validation(
            "Password must be at least 6 characters".to_owned(),
        ));
    }
    let users = state.db.collection::<UserDoc>("users");
    let target = users
        .find_one(doc! { "_id": id })
        .await?
        .ok_or(ApiError::NotFound)?;
    if target.is_admin {
        return Err(ApiError::Validation(
            "The admin password is managed by ADMIN_PASSWORD".to_owned(),
        ));
    }
    users.update_one(doc! { "_id": id }, doc! { "$set": { "passwordHash": hash_password(&input.password)?, "updatedAt": DateTime::now() } }).await?;
    revoke_user_sessions(&state.db, id).await?;
    Ok(HttpResponse::NoContent().finish())
}

async fn delete(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
) -> Result<HttpResponse, ApiError> {
    require_same_origin(&request, &state)?;
    let current = admin(&request, &state).await?;
    let id = object_id(&path)?;
    if id == current.id {
        return Err(ApiError::Validation(
            "You cannot delete yourself".to_owned(),
        ));
    }
    let users = state.db.collection::<UserDoc>("users");
    let target = users
        .find_one(doc! { "_id": id })
        .await?
        .ok_or(ApiError::NotFound)?;
    if target.is_admin {
        return Err(ApiError::Validation(
            "You cannot delete an admin".to_owned(),
        ));
    }
    users
        .delete_one(doc! { "_id": id, "isAdmin": { "$ne": true } })
        .await?;
    for collection in [
        "auth_sessions",
        "exercises",
        "routines",
        "sessions",
        "schedule",
    ] {
        state
            .db
            .collection::<Document>(collection)
            .delete_many(doc! { "userId": id })
            .await?;
    }
    Ok(HttpResponse::NoContent().finish())
}
