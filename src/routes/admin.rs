use actix_web::{HttpRequest, HttpResponse, web};
use futures::TryStreamExt;
use mongodb::bson::{DateTime, Document, doc};

use crate::{
    app::AppState,
    auth::{hash_password, password_is_valid, revoke_user_sessions},
    config::normalize_username,
    error::ApiError,
    models::{PasswordInput, ROLE_CLIENT, UserDoc, UserInput, UserOut},
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
        .sort(doc! { "username": 1 })
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
        return Err(ApiError::Validation("Usuario no válido".to_owned()));
    }
    if !password_is_valid(&input.password) {
        return Err(ApiError::Validation(
            "La contraseña debe tener al menos 6 caracteres".to_owned(),
        ));
    }
    let users = state.db.collection::<UserDoc>("users");
    if users
        .find_one(doc! { "username": &username })
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict("Ese usuario ya existe".to_owned()));
    }
    // Accounts made here train on their own; coaches are created from /owner.
    let user = UserDoc::new(username, hash_password(&input.password)?, ROLE_CLIENT);
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
            "La contraseña debe tener al menos 6 caracteres".to_owned(),
        ));
    }
    let users = state.db.collection::<UserDoc>("users");
    let target = users
        .find_one(doc! { "_id": id })
        .await?
        .ok_or(ApiError::NotFound)?;
    if target.is_owner() {
        return Err(ApiError::Validation(
            "La contraseña del administrador se gestiona con ADMIN_PASSWORD".to_owned(),
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
            "No puedes borrarte a ti mismo".to_owned(),
        ));
    }
    let users = state.db.collection::<UserDoc>("users");
    let target = users
        .find_one(doc! { "_id": id })
        .await?
        .ok_or(ApiError::NotFound)?;
    if target.is_owner() {
        return Err(ApiError::Validation(
            "No puedes borrar a un administrador".to_owned(),
        ));
    }
    let photos = crate::routes::measurements::user_photo_keys(&state.db, id).await?;
    users
        .delete_one(doc! { "_id": id, "role": { "$ne": crate::models::ROLE_OWNER } })
        .await?;
    crate::routes::measurements::forget_photos(state.config.s3.as_ref(), photos);
    if target.is_coach() {
        // Clients keep their data and go back to training on their own; nobody
        // could re-enable one their coach had disabled, so they are enabled.
        users
            .update_many(
                doc! { "coachId": id },
                doc! { "$unset": { "coachId": "" }, "$set": { "disabled": false, "updatedAt": DateTime::now() } },
            )
            .await?;
        state
            .db
            .collection::<Document>("payments")
            .delete_many(doc! { "coachId": id })
            .await?;
    }
    for collection in [
        "auth_sessions",
        "exercises",
        "routines",
        "sessions",
        "schedule",
        "sync_mutations",
        "measurements",
    ] {
        state
            .db
            .collection::<Document>(collection)
            .delete_many(doc! { "userId": id })
            .await?;
    }
    Ok(HttpResponse::NoContent().finish())
}
