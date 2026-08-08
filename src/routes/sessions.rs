use actix_web::{HttpRequest, HttpResponse, web};
use futures::TryStreamExt;
use mongodb::{
    Database,
    bson::{DateTime, doc, oid::ObjectId, to_bson},
};

use crate::{
    app::AppState,
    error::ApiError,
    models::{
        ExerciseDoc, RoutineDoc, SessionDoc, SessionEntryDoc, SessionEntryInput, SessionInput,
        SessionOut, WorkoutSetDoc,
    },
    routes::shared::{require_same_origin, user},
    validation::{
        MAX_REPS, MAX_SESSION_ENTRIES, MAX_SETS_PER_ENTRY, MAX_WEIGHT, clean_notes, object_id,
        round, valid_date,
    },
};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/sessions", web::get().to(list))
        .route("/sessions", web::post().to(create))
        .route("/sessions/{id}", web::get().to(get))
        .route("/sessions/{id}", web::put().to(update))
        .route("/sessions/{id}", web::delete().to(delete));
}

async fn list(
    request: HttpRequest,
    state: web::Data<AppState>,
) -> Result<web::Json<Vec<SessionOut>>, ApiError> {
    let current = user(&request, &state).await?;
    let sessions = state
        .db
        .collection::<SessionDoc>("sessions")
        .find(doc! { "userId": current.id })
        .sort(doc! { "date": -1, "createdAt": -1, "_id": -1 })
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    Ok(web::Json(
        sessions.into_iter().map(SessionOut::from).collect(),
    ))
}

pub(crate) async fn session_data(
    db: &Database,
    user_id: ObjectId,
    input: &SessionInput,
) -> Result<(Option<ObjectId>, Option<String>, Vec<SessionEntryDoc>), ApiError> {
    if !valid_date(&input.date) {
        return Err(ApiError::Validation(
            "Enter a valid session date".to_owned(),
        ));
    }
    if input.entries.is_empty() || input.entries.len() > MAX_SESSION_ENTRIES {
        return Err(ApiError::Validation(
            "Add at least one valid exercise with reps".to_owned(),
        ));
    }

    let entries = validate_entries(&input.entries)?;
    let exercise_ids = entries
        .iter()
        .map(|entry| entry.exercise_id)
        .collect::<Vec<_>>();
    let owned_exercise_count = db
        .collection::<ExerciseDoc>("exercises")
        .count_documents(doc! { "userId": user_id, "_id": { "$in": exercise_ids } })
        .await?;
    if owned_exercise_count != entries.len() as u64 {
        return Err(ApiError::Validation(
            "A session can only contain your own exercises".to_owned(),
        ));
    }

    let routine_id = match &input.routine_id {
        Some(value) if !value.is_empty() => {
            let id = object_id(value)?;
            let owned = db
                .collection::<RoutineDoc>("routines")
                .find_one(doc! { "_id": id, "userId": user_id })
                .await?
                .is_some();
            if !owned {
                return Err(ApiError::Validation(
                    "A session can only use one of your own routines".to_owned(),
                ));
            }
            Some(id)
        }
        _ => None,
    };
    Ok((routine_id, clean_notes(input.notes.clone())?, entries))
}

fn validate_entries(entries: &[SessionEntryInput]) -> Result<Vec<SessionEntryDoc>, ApiError> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(entries.len());
    for entry in entries {
        let exercise_id = object_id(&entry.exercise_id)?;
        if !seen.insert(exercise_id) {
            return Err(ApiError::Validation(
                "An exercise may appear only once in a session".to_owned(),
            ));
        }
        if entry.sets.is_empty() || entry.sets.len() > MAX_SETS_PER_ENTRY {
            return Err(ApiError::Validation(
                "Each exercise needs valid sets".to_owned(),
            ));
        }
        let sets = entry
            .sets
            .iter()
            .map(|set| {
                if !set.weight.is_finite()
                    || !set.reps.is_finite()
                    || !(0.0..=MAX_WEIGHT).contains(&set.weight)
                    || !(0.0..=MAX_REPS).contains(&set.reps)
                    || set.reps == 0.0
                {
                    return Err(ApiError::Validation("Invalid weight or reps".to_owned()));
                }
                Ok(WorkoutSetDoc {
                    weight: round(set.weight, 2),
                    reps: round(set.reps, 1),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        result.push(SessionEntryDoc { exercise_id, sets });
    }
    Ok(result)
}

async fn create(
    request: HttpRequest,
    state: web::Data<AppState>,
    input: web::Json<SessionInput>,
) -> Result<web::Json<SessionOut>, ApiError> {
    require_same_origin(&request, &state)?;
    let current = user(&request, &state).await?;
    let (routine_id, notes, entries) = session_data(&state.db, current.id, &input).await?;
    let session = SessionDoc {
        id: ObjectId::new(),
        user_id: current.id,
        date: input.date.clone(),
        routine_id,
        notes,
        entries,
        created_at: DateTime::now(),
    };
    state
        .db
        .collection::<SessionDoc>("sessions")
        .insert_one(session.clone())
        .await?;
    Ok(web::Json(SessionOut::from(session)))
}

async fn get(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
) -> Result<web::Json<SessionOut>, ApiError> {
    let current = user(&request, &state).await?;
    let session = state
        .db
        .collection::<SessionDoc>("sessions")
        .find_one(doc! { "_id": object_id(&path)?, "userId": current.id })
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(web::Json(SessionOut::from(session)))
}

async fn update(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
    input: web::Json<SessionInput>,
) -> Result<HttpResponse, ApiError> {
    require_same_origin(&request, &state)?;
    let current = user(&request, &state).await?;
    let (routine_id, notes, entries) = session_data(&state.db, current.id, &input).await?;
    let result = state.db.collection::<SessionDoc>("sessions").update_one(doc! { "_id": object_id(&path)?, "userId": current.id }, doc! { "$set": { "date": &input.date, "routineId": routine_id, "notes": notes, "entries": to_bson(&entries).map_err(|_| ApiError::Crypto)? } }).await?;
    if result.matched_count == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(HttpResponse::NoContent().finish())
}

async fn delete(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
) -> Result<HttpResponse, ApiError> {
    require_same_origin(&request, &state)?;
    let current = user(&request, &state).await?;
    let result = state
        .db
        .collection::<SessionDoc>("sessions")
        .delete_one(doc! { "_id": object_id(&path)?, "userId": current.id })
        .await?;
    if result.deleted_count == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(HttpResponse::NoContent().finish())
}
