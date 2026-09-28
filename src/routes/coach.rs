use std::collections::HashMap;

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Days;
use futures::TryStreamExt;
use mongodb::{
    Database,
    bson::{DateTime, Document, doc, oid::ObjectId},
};

use crate::{
    access::{format_date, require_writable, today},
    app::AppState,
    auth::{hash_password, password_is_valid, revoke_user_sessions},
    config::normalize_username,
    error::ApiError,
    models::{
        ClientStatusInput, ClientSummaryOut, CoachClientOut, CoachClientsOut, CoachSyncResponse,
        PasswordInput, ROLE_CLIENT, SyncMutationInput, SyncMutationResult, SyncRequest, UserDoc,
        UserInput,
    },
    routes::{
        owner::coach_out,
        shared::{coach, require_same_origin},
        sync::{apply_once, mutation_result, snapshot},
    },
    validation::{object_id, valid_username},
};

/// How far back the client list looks for "sessions this week" and inactivity.
const RECENT_DAYS: u64 = 35;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/coach/sync", web::get().to(get_sync))
        .route("/coach/sync", web::post().to(sync))
        .route("/coach/clients", web::get().to(list))
        .route("/coach/clients", web::post().to(create))
        .route("/coach/clients/{id}", web::get().to(get))
        .route(
            "/coach/clients/{id}/password",
            web::put().to(reset_password),
        )
        .route("/coach/clients/{id}/status", web::put().to(set_status));
}

/// A client of `coach`, or 404 so other users' ids are not confirmed.
pub async fn own_client(
    db: &Database,
    coach: &UserDoc,
    client_id: ObjectId,
) -> Result<UserDoc, ApiError> {
    db.collection::<UserDoc>("users")
        .find_one(doc! { "_id": client_id, "coachId": coach.id, "role": ROLE_CLIENT })
        .await?
        .ok_or(ApiError::NotFound)
}

async fn summaries(db: &Database, clients: &[UserDoc]) -> Result<Vec<ClientSummaryOut>, ApiError> {
    let ids: Vec<ObjectId> = clients.iter().map(|client| client.id).collect();
    let sessions = db.collection::<Document>("sessions");
    let mut last = HashMap::new();
    let mut groups = sessions
        .aggregate(vec![
            doc! { "$match": { "userId": { "$in": &ids } } },
            doc! { "$group": { "_id": "$userId", "last": { "$max": "$date" } } },
        ])
        .await?;
    while let Some(group) = groups.try_next().await? {
        if let (Ok(id), Ok(date)) = (group.get_object_id("_id"), group.get_str("last")) {
            last.insert(id, date.to_owned());
        }
    }
    let cutoff = format_date(today() - Days::new(RECENT_DAYS));
    let mut recent: HashMap<ObjectId, Vec<String>> = HashMap::new();
    let mut cursor = sessions
        .find(doc! { "userId": { "$in": &ids }, "date": { "$gte": cutoff } })
        .projection(doc! { "userId": 1, "date": 1 })
        .sort(doc! { "date": -1 })
        .await?;
    while let Some(session) = cursor.try_next().await? {
        if let (Ok(id), Ok(date)) = (session.get_object_id("userId"), session.get_str("date")) {
            recent.entry(id).or_default().push(date.to_owned());
        }
    }
    Ok(clients
        .iter()
        .map(|client| ClientSummaryOut {
            id: client.id.to_hex(),
            username: client.username.clone(),
            disabled: client.disabled,
            created_at: client
                .created_at
                .try_to_rfc3339_string()
                .unwrap_or_default(),
            last_session_date: last.remove(&client.id),
            recent_session_dates: recent.remove(&client.id).unwrap_or_default(),
        })
        .collect())
}

async fn list(
    request: HttpRequest,
    state: web::Data<AppState>,
) -> Result<web::Json<CoachClientsOut>, ApiError> {
    let current = coach(&request, &state).await?;
    let clients = state
        .db
        .collection::<UserDoc>("users")
        .find(doc! { "coachId": current.id, "role": ROLE_CLIENT })
        .sort(doc! { "username": 1 })
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    Ok(web::Json(CoachClientsOut {
        coach: coach_out(&state.db, &current).await?,
        clients: summaries(&state.db, &clients).await?,
    }))
}

async fn get(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
) -> Result<web::Json<CoachClientOut>, ApiError> {
    let current = coach(&request, &state).await?;
    let client = own_client(&state.db, &current, object_id(&path)?).await?;
    let summary = summaries(&state.db, std::slice::from_ref(&client))
        .await?
        .pop()
        .ok_or(ApiError::NotFound)?;
    Ok(web::Json(CoachClientOut {
        client: summary,
        snapshot: snapshot(&state.db, client.id).await?,
    }))
}

const MAX_MUTATIONS_PER_REQUEST: usize = 100;

/// What a coach may change in a client's data: their sessions and body
/// records, and new exercises for the catalogue. Routines, schedule and
/// settings stay the client's.
fn coach_may(mutation: &SyncMutationInput) -> bool {
    matches!(
        (mutation.entity.as_str(), mutation.operation.as_str()),
        ("session", "create" | "update" | "delete")
            | ("measurement", "create" | "update" | "delete")
            | ("exercise", "create")
    )
}

async fn enabled_clients(db: &Database, coach: &UserDoc) -> Result<Vec<UserDoc>, ApiError> {
    Ok(db
        .collection::<UserDoc>("users")
        .find(doc! { "coachId": coach.id, "role": ROLE_CLIENT, "disabled": { "$ne": true } })
        .sort(doc! { "username": 1 })
        .await?
        .try_collect::<Vec<_>>()
        .await?)
}

async fn roster(db: &Database, clients: &[UserDoc]) -> Result<Vec<CoachClientOut>, ApiError> {
    let summaries = summaries(db, clients).await?;
    let mut out = Vec::with_capacity(clients.len());
    for (client, summary) in clients.iter().zip(summaries) {
        out.push(CoachClientOut {
            client: summary,
            snapshot: snapshot(db, client.id).await?,
        });
    }
    Ok(out)
}

async fn sync_response(
    db: &Database,
    current: &UserDoc,
    clients: &[UserDoc],
    applied: Vec<SyncMutationResult>,
) -> Result<web::Json<CoachSyncResponse>, ApiError> {
    let disabled = db
        .collection::<UserDoc>("users")
        .find(doc! { "coachId": current.id, "role": ROLE_CLIENT, "disabled": true })
        .sort(doc! { "username": 1 })
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    Ok(web::Json(CoachSyncResponse {
        coach: coach_out(db, current).await?,
        clients: roster(db, clients).await?,
        disabled: summaries(db, &disabled).await?,
        applied,
    }))
}

/// Everything the coach needs offline: each enabled client's data.
async fn get_sync(
    request: HttpRequest,
    state: web::Data<AppState>,
) -> Result<web::Json<CoachSyncResponse>, ApiError> {
    let current = coach(&request, &state).await?;
    let clients = enabled_clients(&state.db, &current).await?;
    sync_response(&state.db, &current, &clients, vec![]).await
}

/// Applies the coach's queued changes, each tagged with its `clientId`.
async fn sync(
    request: HttpRequest,
    state: web::Data<AppState>,
    body: web::Json<SyncRequest>,
) -> Result<web::Json<CoachSyncResponse>, ApiError> {
    require_same_origin(&request, &state)?;
    let current = coach(&request, &state).await?;
    if body.mutations.len() > MAX_MUTATIONS_PER_REQUEST {
        return Err(ApiError::Validation(
            "Demasiados cambios pendientes".to_owned(),
        ));
    }
    // As in /api/sync: a coach behind on payments keeps the changes queued.
    if !body.mutations.is_empty() {
        require_writable(&state.db, &current).await?;
    }
    let clients = enabled_clients(&state.db, &current).await?;
    let mut applied = Vec::with_capacity(body.mutations.len());
    for mutation in &body.mutations {
        let client = mutation
            .client_id
            .as_deref()
            .and_then(|id| ObjectId::parse_str(id).ok())
            .filter(|id| clients.iter().any(|client| client.id == *id));
        applied.push(match client {
            None => mutation_result(
                mutation,
                Some("Este cliente ya no está activo contigo".to_owned()),
            ),
            Some(_) if !coach_may(mutation) => mutation_result(
                mutation,
                Some("Ese cambio solo lo puede hacer el cliente".to_owned()),
            ),
            Some(client_id) => {
                apply_once(
                    &state.db,
                    state.config.s3.as_ref(),
                    client_id,
                    current.id,
                    mutation,
                )
                .await?
            }
        });
    }
    sync_response(&state.db, &current, &clients, applied).await
}

/// The coach's plan caps how many enabled clients they may have.
async fn ensure_room(db: &Database, current: &UserDoc) -> Result<(), ApiError> {
    let Some(max) = current.max_clients else {
        return Ok(());
    };
    let active = db
        .collection::<UserDoc>("users")
        .count_documents(doc! { "coachId": current.id, "disabled": { "$ne": true } })
        .await?;
    if active >= max as u64 {
        return Err(ApiError::Conflict(format!(
            "Tu plan permite {max} clientes activos. Desactiva uno o habla con el administrador para ampliarlo."
        )));
    }
    Ok(())
}

async fn create(
    request: HttpRequest,
    state: web::Data<AppState>,
    input: web::Json<UserInput>,
) -> Result<web::Json<ClientSummaryOut>, ApiError> {
    require_same_origin(&request, &state)?;
    let current = coach(&request, &state).await?;
    require_writable(&state.db, &current).await?;
    let username = normalize_username(&input.username);
    if !valid_username(&username) {
        return Err(ApiError::Validation("Usuario no válido".to_owned()));
    }
    if !password_is_valid(&input.password) {
        return Err(ApiError::Validation(
            "La contraseña debe tener al menos 6 caracteres".to_owned(),
        ));
    }
    ensure_room(&state.db, &current).await?;
    let users = state.db.collection::<UserDoc>("users");
    if users
        .find_one(doc! { "username": &username })
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict("Ese usuario ya existe".to_owned()));
    }
    let mut client = UserDoc::new(username, hash_password(&input.password)?, ROLE_CLIENT);
    client.coach_id = Some(current.id);
    // A new client sees weights the way their coach does.
    client.weight_unit = current.weight_unit.clone();
    client.length_unit = current.length_unit.clone();
    users.insert_one(client.clone()).await?;
    Ok(web::Json(
        summaries(&state.db, std::slice::from_ref(&client))
            .await?
            .pop()
            .ok_or(ApiError::NotFound)?,
    ))
}

async fn reset_password(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
    input: web::Json<PasswordInput>,
) -> Result<HttpResponse, ApiError> {
    require_same_origin(&request, &state)?;
    let current = coach(&request, &state).await?;
    require_writable(&state.db, &current).await?;
    let client = own_client(&state.db, &current, object_id(&path)?).await?;
    if !password_is_valid(&input.password) {
        return Err(ApiError::Validation(
            "La contraseña debe tener al menos 6 caracteres".to_owned(),
        ));
    }
    state
        .db
        .collection::<UserDoc>("users")
        .update_one(
            doc! { "_id": client.id },
            doc! { "$set": { "passwordHash": hash_password(&input.password)?, "updatedAt": DateTime::now() } },
        )
        .await?;
    revoke_user_sessions(&state.db, client.id).await?;
    Ok(HttpResponse::NoContent().finish())
}

async fn set_status(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
    input: web::Json<ClientStatusInput>,
) -> Result<HttpResponse, ApiError> {
    require_same_origin(&request, &state)?;
    let current = coach(&request, &state).await?;
    require_writable(&state.db, &current).await?;
    let client = own_client(&state.db, &current, object_id(&path)?).await?;
    if client.disabled == input.disabled {
        return Ok(HttpResponse::NoContent().finish());
    }
    if !input.disabled {
        ensure_room(&state.db, &current).await?;
    }
    state
        .db
        .collection::<UserDoc>("users")
        .update_one(
            doc! { "_id": client.id },
            doc! { "$set": { "disabled": input.disabled, "updatedAt": DateTime::now() } },
        )
        .await?;
    if input.disabled {
        revoke_user_sessions(&state.db, client.id).await?;
    }
    Ok(HttpResponse::NoContent().finish())
}
