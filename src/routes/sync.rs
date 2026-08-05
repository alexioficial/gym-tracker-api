use actix_web::{HttpRequest, web};
use futures::TryStreamExt;
use mongodb::{
    Database,
    bson::{DateTime, Document, doc, oid::ObjectId, to_bson},
};
use serde::de::DeserializeOwned;

use crate::{
    app::AppState,
    error::ApiError,
    models::{
        ExerciseDoc, ExerciseInput, ExerciseOut, RoutineDoc, RoutineInput, RoutineOut, ScheduleDoc,
        ScheduleInput, SessionDoc, SessionInput, SessionOut, SyncMutationDoc, SyncMutationInput,
        SyncMutationResult, SyncRequest, SyncResponse, SyncSnapshot,
    },
    routes::{
        routines::{day_slot, owned_exercises, valid_color},
        sessions::session_data,
        shared::{require_same_origin, user},
    },
    validation::{EXERCISE_MAX, MUSCLE_GROUP_MAX, clean_notes, object_id, text},
};

const MAX_MUTATIONS_PER_REQUEST: usize = 100;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/sync", web::get().to(get_snapshot))
        .route("/sync", web::post().to(sync));
}

async fn get_snapshot(
    request: HttpRequest,
    state: web::Data<AppState>,
) -> Result<web::Json<SyncResponse>, ApiError> {
    let current = user(&request, &state).await?;
    Ok(web::Json(SyncResponse {
        snapshot: snapshot(&state.db, current.id).await?,
        applied: vec![],
    }))
}

async fn sync(
    request: HttpRequest,
    state: web::Data<AppState>,
    body: web::Json<SyncRequest>,
) -> Result<web::Json<SyncResponse>, ApiError> {
    require_same_origin(&request, &state)?;
    let current = user(&request, &state).await?;
    if body.mutations.len() > MAX_MUTATIONS_PER_REQUEST {
        return Err(ApiError::Validation("Too many pending changes".to_owned()));
    }

    let mut applied = Vec::with_capacity(body.mutations.len());
    for mutation in &body.mutations {
        applied.push(apply_once(&state.db, current.id, mutation).await?);
    }

    Ok(web::Json(SyncResponse {
        snapshot: snapshot(&state.db, current.id).await?,
        applied,
    }))
}

async fn snapshot(db: &Database, user_id: ObjectId) -> Result<SyncSnapshot, ApiError> {
    let exercises = db
        .collection::<ExerciseDoc>("exercises")
        .find(doc! { "userId": user_id })
        .sort(doc! { "name": 1 })
        .await?
        .try_collect::<Vec<_>>()
        .await?
        .into_iter()
        .map(ExerciseOut::from)
        .collect();
    let routines = db
        .collection::<RoutineDoc>("routines")
        .find(doc! { "userId": user_id })
        .sort(doc! { "order": 1, "name": 1 })
        .await?
        .try_collect::<Vec<_>>()
        .await?
        .into_iter()
        .map(RoutineOut::from)
        .collect();
    let sessions = db
        .collection::<SessionDoc>("sessions")
        .find(doc! { "userId": user_id })
        .sort(doc! { "date": -1 })
        .await?
        .try_collect::<Vec<_>>()
        .await?
        .into_iter()
        .map(SessionOut::from)
        .collect();
    let schedule = db
        .collection::<ScheduleDoc>("schedule")
        .find_one(doc! { "userId": user_id })
        .await?
        .map(|item| item.days)
        .unwrap_or_default();
    Ok(SyncSnapshot {
        exercises,
        routines,
        sessions,
        schedule,
    })
}

fn decoded<T: DeserializeOwned>(mutation: &SyncMutationInput) -> Result<T, ApiError> {
    serde_json::from_value(mutation.payload.clone())
        .map_err(|_| ApiError::Validation("Invalid offline change".to_owned()))
}

fn mutation_target(mutation: &SyncMutationInput) -> Result<ObjectId, ApiError> {
    mutation
        .entity_id
        .as_deref()
        .ok_or_else(|| ApiError::Validation("Offline change is missing an id".to_owned()))
        .and_then(object_id)
}

fn mutation_result(mutation: &SyncMutationInput) -> SyncMutationResult {
    SyncMutationResult {
        mutation_id: mutation.mutation_id.clone(),
        entity: mutation.entity.clone(),
        operation: mutation.operation.clone(),
        entity_id: mutation.entity_id.clone(),
    }
}

async fn apply_once(
    db: &Database,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<SyncMutationResult, ApiError> {
    if uuid::Uuid::parse_str(&mutation.mutation_id).is_err() {
        return Err(ApiError::Validation("Invalid offline change id".to_owned()));
    }
    let mutations = db.collection::<SyncMutationDoc>("sync_mutations");
    if let Some(previous) = mutations
        .find_one(doc! { "userId": user_id, "mutationId": &mutation.mutation_id })
        .await?
    {
        return Ok(previous.result);
    }

    apply(db, user_id, mutation).await?;
    let result = mutation_result(mutation);
    let record = SyncMutationDoc {
        id: ObjectId::new(),
        user_id,
        mutation_id: mutation.mutation_id.clone(),
        created_at: DateTime::now(),
        result: result.clone(),
    };

    if mutations.insert_one(record).await.is_err() {
        // Another retry may have completed while this request was running.
        if let Some(previous) = mutations
            .find_one(doc! { "userId": user_id, "mutationId": &mutation.mutation_id })
            .await?
        {
            return Ok(previous.result);
        }
        return Err(ApiError::Conflict(
            "Could not save offline change".to_owned(),
        ));
    }
    Ok(result)
}

async fn apply(
    db: &Database,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    match (mutation.entity.as_str(), mutation.operation.as_str()) {
        ("exercise", "create") => create_exercise(db, user_id, mutation).await,
        ("exercise", "update") => update_exercise(db, user_id, mutation).await,
        ("exercise", "delete") => delete_exercise(db, user_id, mutation).await,
        ("routine", "create") => create_routine(db, user_id, mutation).await,
        ("routine", "update") => update_routine(db, user_id, mutation).await,
        ("routine", "delete") => delete_routine(db, user_id, mutation).await,
        ("session", "create") => create_session(db, user_id, mutation).await,
        ("session", "update") => update_session(db, user_id, mutation).await,
        ("session", "delete") => delete_session(db, user_id, mutation).await,
        ("schedule", "set") => set_schedule(db, user_id, mutation).await,
        _ => Err(ApiError::Validation(
            "Unsupported offline change".to_owned(),
        )),
    }
}

async fn create_exercise(
    db: &Database,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    let id = mutation_target(mutation)?;
    if let Some(existing) = db
        .collection::<ExerciseDoc>("exercises")
        .find_one(doc! { "_id": id })
        .await?
    {
        return if existing.user_id == user_id {
            Ok(())
        } else {
            Err(ApiError::Conflict("Offline id collision".to_owned()))
        };
    }
    let input: ExerciseInput = decoded(mutation)?;
    let now = DateTime::now();
    db.collection::<ExerciseDoc>("exercises")
        .insert_one(ExerciseDoc {
            id,
            user_id,
            name: text(&input.name, "exercise name", EXERCISE_MAX, true)?,
            muscle_group: text(&input.muscle_group, "muscle group", MUSCLE_GROUP_MAX, false)?,
            notes: clean_notes(input.notes)?,
            created_at: now,
            updated_at: now,
        })
        .await?;
    Ok(())
}

async fn update_exercise(
    db: &Database,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    let id = mutation_target(mutation)?;
    let input: ExerciseInput = decoded(mutation)?;
    let result = db
        .collection::<ExerciseDoc>("exercises")
        .update_one(
            doc! { "_id": id, "userId": user_id },
            doc! { "$set": { "name": text(&input.name, "exercise name", EXERCISE_MAX, true)?, "muscleGroup": text(&input.muscle_group, "muscle group", MUSCLE_GROUP_MAX, false)?, "notes": clean_notes(input.notes)?, "updatedAt": DateTime::now() } },
        )
        .await?;
    if result.matched_count == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

async fn delete_exercise(
    db: &Database,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    let id = mutation_target(mutation)?;
    db.collection::<ExerciseDoc>("exercises")
        .delete_one(doc! { "_id": id, "userId": user_id })
        .await?;
    let routines = db.collection::<Document>("routines");
    routines
        .update_many(
            doc! { "userId": user_id, "exercises.exerciseId": id },
            doc! { "$pull": { "exercises": { "exerciseId": id } } },
        )
        .await?;
    routines
        .update_many(
            doc! { "userId": user_id, "exerciseIds": id },
            doc! { "$pull": { "exerciseIds": id } },
        )
        .await?;
    Ok(())
}

async fn create_routine(
    db: &Database,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    let id = mutation_target(mutation)?;
    if let Some(existing) = db
        .collection::<RoutineDoc>("routines")
        .find_one(doc! { "_id": id })
        .await?
    {
        return if existing.user_id == user_id {
            Ok(())
        } else {
            Err(ApiError::Conflict("Offline id collision".to_owned()))
        };
    }
    let input: RoutineInput = decoded(mutation)?;
    if !valid_color(&input.color) {
        return Err(ApiError::Validation("Invalid routine color".to_owned()));
    }
    let routines = db.collection::<RoutineDoc>("routines");
    let routine = RoutineDoc {
        id,
        user_id,
        name: text(
            &input.name,
            "routine name",
            crate::validation::ROUTINE_MAX,
            true,
        )?,
        color: input.color,
        order: routines.count_documents(doc! { "userId": user_id }).await? as i64,
        exercises: owned_exercises(db, user_id, &input.exercises).await?,
        legacy_exercise_ids: vec![],
        created_at: DateTime::now(),
        updated_at: DateTime::now(),
    };
    routines.insert_one(routine).await?;
    Ok(())
}

async fn update_routine(
    db: &Database,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    let id = mutation_target(mutation)?;
    let input: RoutineInput = decoded(mutation)?;
    if !valid_color(&input.color) {
        return Err(ApiError::Validation("Invalid routine color".to_owned()));
    }
    let exercises = owned_exercises(db, user_id, &input.exercises).await?;
    let result = db
        .collection::<RoutineDoc>("routines")
        .update_one(
            doc! { "_id": id, "userId": user_id },
            doc! { "$set": { "name": text(&input.name, "routine name", crate::validation::ROUTINE_MAX, true)?, "color": input.color, "exercises": to_bson(&exercises).map_err(|_| ApiError::Crypto)?, "updatedAt": DateTime::now() }, "$unset": { "exerciseIds": "" } },
        )
        .await?;
    if result.matched_count == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

async fn delete_routine(
    db: &Database,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    let id = mutation_target(mutation)?;
    db.collection::<RoutineDoc>("routines")
        .delete_one(doc! { "_id": id, "userId": user_id })
        .await?;
    let routine_id = id.to_hex();
    let schedules = db.collection::<ScheduleDoc>("schedule");
    if let Some(mut item) = schedules.find_one(doc! { "userId": user_id }).await? {
        for value in [
            &mut item.days.mon,
            &mut item.days.tue,
            &mut item.days.wed,
            &mut item.days.thu,
            &mut item.days.fri,
            &mut item.days.sat,
            &mut item.days.sun,
        ] {
            if value.as_deref() == Some(routine_id.as_str()) {
                *value = None;
            }
        }
        schedules
            .update_one(
                doc! { "userId": user_id },
                doc! { "$set": { "days": to_bson(&item.days).map_err(|_| ApiError::Crypto)? } },
            )
            .await?;
    }
    Ok(())
}

async fn create_session(
    db: &Database,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    let id = mutation_target(mutation)?;
    if let Some(existing) = db
        .collection::<SessionDoc>("sessions")
        .find_one(doc! { "_id": id })
        .await?
    {
        return if existing.user_id == user_id {
            Ok(())
        } else {
            Err(ApiError::Conflict("Offline id collision".to_owned()))
        };
    }
    let input: SessionInput = decoded(mutation)?;
    let (routine_id, notes, entries) = session_data(db, user_id, &input).await?;
    db.collection::<SessionDoc>("sessions")
        .insert_one(SessionDoc {
            id,
            user_id,
            date: input.date,
            routine_id,
            notes,
            entries,
            created_at: DateTime::now(),
        })
        .await?;
    Ok(())
}

async fn update_session(
    db: &Database,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    let id = mutation_target(mutation)?;
    let input: SessionInput = decoded(mutation)?;
    let (routine_id, notes, entries) = session_data(db, user_id, &input).await?;
    let result = db
        .collection::<SessionDoc>("sessions")
        .update_one(
            doc! { "_id": id, "userId": user_id },
            doc! { "$set": { "date": input.date, "routineId": routine_id, "notes": notes, "entries": to_bson(&entries).map_err(|_| ApiError::Crypto)? } },
        )
        .await?;
    if result.matched_count == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

async fn delete_session(
    db: &Database,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    db.collection::<SessionDoc>("sessions")
        .delete_one(doc! { "_id": mutation_target(mutation)?, "userId": user_id })
        .await?;
    Ok(())
}

async fn set_schedule(
    db: &Database,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    let day = mutation
        .entity_id
        .as_deref()
        .ok_or_else(|| ApiError::Validation("Offline change is missing a day".to_owned()))?;
    let input: ScheduleInput = decoded(mutation)?;
    let routine_id = match input.routine_id {
        Some(value) if !value.is_empty() => {
            let id = object_id(&value)?;
            let owned = db
                .collection::<RoutineDoc>("routines")
                .find_one(doc! { "_id": id, "userId": user_id })
                .await?
                .is_some();
            if !owned {
                return Err(ApiError::Validation(
                    "You can only schedule one of your own routines".to_owned(),
                ));
            }
            Some(id.to_hex())
        }
        _ => None,
    };
    let schedules = db.collection::<ScheduleDoc>("schedule");
    let mut days = schedules
        .find_one(doc! { "userId": user_id })
        .await?
        .map(|item| item.days)
        .unwrap_or_default();
    *day_slot(&mut days, day)? = routine_id;
    schedules
        .update_one(
            doc! { "userId": user_id },
            doc! { "$set": { "days": to_bson(&days).map_err(|_| ApiError::Crypto)? }, "$setOnInsert": { "userId": user_id } },
        )
        .upsert(true)
        .await?;
    Ok(())
}
