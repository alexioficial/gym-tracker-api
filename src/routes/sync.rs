use actix_web::{HttpRequest, web};
use futures::TryStreamExt;
use mongodb::{
    Database,
    bson::{DateTime, Document, doc, oid::ObjectId, to_bson},
};
use serde::de::DeserializeOwned;

use crate::{
    access::require_writable,
    app::AppState,
    error::ApiError,
    models::{
        ExerciseDoc, ExerciseInput, ExerciseOut, MeasurementDoc, MeasurementInput, MeasurementOut,
        RoutineDoc, RoutineInput, RoutineOut, ScheduleDoc, ScheduleInput, SessionDoc, SessionInput,
        SessionOut, SettingsInput, SettingsOut, SyncMutationDoc, SyncMutationInput,
        SyncMutationResult, SyncMutationStatus, SyncRequest, SyncResponse, SyncSnapshot, UserDoc,
    },
    routes::{
        measurements::{forget_photos, measurement_data},
        routines::{day_slot, owned_exercises, valid_color},
        sessions::session_data,
        shared::{require_same_origin, user},
    },
    storage::S3Config,
    validation::{
        EXERCISE_MAX, LENGTH_UNITS, MUSCLE_GROUP_MAX, WEIGHT_UNITS, clean_notes, object_id, text,
    },
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
        return Err(ApiError::Validation(
            "Demasiados cambios pendientes".to_owned(),
        ));
    }
    // Refusing the whole request keeps the changes queued on the device, so they
    // upload once the payment is renewed instead of being dropped as rejected.
    if !body.mutations.is_empty() {
        require_writable(&state.db, &current).await?;
    }

    let mut applied = Vec::with_capacity(body.mutations.len());
    for mutation in &body.mutations {
        applied.push(apply_once(&state.db, state.config.s3.as_ref(), current.id, mutation).await?);
    }

    Ok(web::Json(SyncResponse {
        snapshot: snapshot(&state.db, current.id).await?,
        applied,
    }))
}

pub async fn snapshot(db: &Database, user_id: ObjectId) -> Result<SyncSnapshot, ApiError> {
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
        .sort(doc! { "date": -1, "createdAt": -1, "_id": -1 })
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
    let settings = db
        .collection::<UserDoc>("users")
        .find_one(doc! { "_id": user_id })
        .await?
        .map(|user| SettingsOut {
            weight_unit: user.weight_unit,
            length_unit: user.length_unit,
        })
        .unwrap_or_else(|| SettingsOut {
            weight_unit: crate::models::default_weight_unit(),
            length_unit: crate::models::default_length_unit(),
        });
    let measurements = db
        .collection::<MeasurementDoc>("measurements")
        .find(doc! { "userId": user_id })
        .sort(doc! { "date": -1, "createdAt": -1 })
        .await?
        .try_collect::<Vec<_>>()
        .await?
        .into_iter()
        .map(MeasurementOut::from)
        .collect();
    Ok(SyncSnapshot {
        exercises,
        routines,
        sessions,
        schedule,
        settings,
        measurements,
    })
}

fn decoded<T: DeserializeOwned>(mutation: &SyncMutationInput) -> Result<T, ApiError> {
    serde_json::from_value(mutation.payload.clone())
        .map_err(|_| ApiError::Validation("Cambio sin conexión no válido".to_owned()))
}

fn mutation_target(mutation: &SyncMutationInput) -> Result<ObjectId, ApiError> {
    mutation
        .entity_id
        .as_deref()
        .ok_or_else(|| ApiError::Validation("Al cambio sin conexión le falta el id".to_owned()))
        .and_then(object_id)
}

fn mutation_result(mutation: &SyncMutationInput, error: Option<String>) -> SyncMutationResult {
    SyncMutationResult {
        mutation_id: mutation.mutation_id.clone(),
        entity: mutation.entity.clone(),
        operation: mutation.operation.clone(),
        entity_id: mutation.entity_id.clone(),
        status: if error.is_some() {
            SyncMutationStatus::Rejected
        } else {
            SyncMutationStatus::Applied
        },
        error,
    }
}

/// Client errors are final: retrying the same change can never succeed, so it is
/// reported as rejected instead of failing the whole batch and blocking the queue.
fn rejection(error: ApiError) -> Result<String, ApiError> {
    match error {
        ApiError::NotFound => Ok("Este elemento ya no existe".to_owned()),
        ApiError::Validation(message) | ApiError::Conflict(message) => Ok(message),
        ApiError::Forbidden => Ok("Este cambio no está permitido".to_owned()),
        other => Err(other),
    }
}

async fn apply_once(
    db: &Database,
    s3: Option<&S3Config>,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<SyncMutationResult, ApiError> {
    if uuid::Uuid::parse_str(&mutation.mutation_id).is_err() {
        return Ok(mutation_result(
            mutation,
            Some("Id de cambio sin conexión no válido".to_owned()),
        ));
    }
    let mutations = db.collection::<SyncMutationDoc>("sync_mutations");
    if let Some(previous) = mutations
        .find_one(doc! { "userId": user_id, "mutationId": &mutation.mutation_id })
        .await?
    {
        return Ok(previous.result);
    }

    let error = match apply(db, s3, user_id, mutation).await {
        Ok(()) => None,
        Err(error) => Some(rejection(error)?),
    };
    let result = mutation_result(mutation, error);
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
            "No se pudo guardar el cambio sin conexión".to_owned(),
        ));
    }
    Ok(result)
}

async fn apply(
    db: &Database,
    s3: Option<&S3Config>,
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
        ("settings", "set") => set_settings(db, user_id, mutation).await,
        ("measurement", "create") => create_measurement(db, user_id, mutation).await,
        ("measurement", "update") => update_measurement(db, s3, user_id, mutation).await,
        ("measurement", "delete") => delete_measurement(db, s3, user_id, mutation).await,
        _ => Err(ApiError::Validation(
            "Cambio sin conexión no admitido".to_owned(),
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
            Err(ApiError::Conflict(
                "Conflicto de id sin conexión".to_owned(),
            ))
        };
    }
    let input: ExerciseInput = decoded(mutation)?;
    let now = DateTime::now();
    db.collection::<ExerciseDoc>("exercises")
        .insert_one(ExerciseDoc {
            id,
            user_id,
            name: text(&input.name, "nombre del ejercicio", EXERCISE_MAX, true)?,
            muscle_group: text(
                &input.muscle_group,
                "grupo muscular",
                MUSCLE_GROUP_MAX,
                false,
            )?,
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
            doc! { "$set": { "name": text(&input.name, "nombre del ejercicio", EXERCISE_MAX, true)?, "muscleGroup": text(&input.muscle_group, "grupo muscular", MUSCLE_GROUP_MAX, false)?, "notes": clean_notes(input.notes)?, "updatedAt": DateTime::now() } },
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
            Err(ApiError::Conflict(
                "Conflicto de id sin conexión".to_owned(),
            ))
        };
    }
    let input: RoutineInput = decoded(mutation)?;
    if !valid_color(&input.color) {
        return Err(ApiError::Validation("Color de rutina no válido".to_owned()));
    }
    let routines = db.collection::<RoutineDoc>("routines");
    let routine = RoutineDoc {
        id,
        user_id,
        name: text(
            &input.name,
            "nombre de la rutina",
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
        return Err(ApiError::Validation("Color de rutina no válido".to_owned()));
    }
    let exercises = owned_exercises(db, user_id, &input.exercises).await?;
    let result = db
        .collection::<RoutineDoc>("routines")
        .update_one(
            doc! { "_id": id, "userId": user_id },
            doc! { "$set": { "name": text(&input.name, "nombre de la rutina", crate::validation::ROUTINE_MAX, true)?, "color": input.color, "exercises": to_bson(&exercises).map_err(|_| ApiError::Crypto)?, "updatedAt": DateTime::now() }, "$unset": { "exerciseIds": "" } },
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
            Err(ApiError::Conflict(
                "Conflicto de id sin conexión".to_owned(),
            ))
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
        .ok_or_else(|| ApiError::Validation("Al cambio sin conexión le falta el día".to_owned()))?;
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
                    "Solo puedes programar tus propias rutinas".to_owned(),
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

async fn set_settings(
    db: &Database,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    let input: SettingsInput = decoded(mutation)?;
    let change = match mutation.entity_id.as_deref() {
        // Older clients sent the weight unit without naming the setting.
        Some("weightUnit") | None => {
            let unit = input.weight_unit.unwrap_or_default();
            if !WEIGHT_UNITS.contains(&unit.as_str()) {
                return Err(ApiError::Validation("Unidad de peso no válida".to_owned()));
            }
            doc! { "weightUnit": unit }
        }
        Some("lengthUnit") => {
            let unit = input.length_unit.unwrap_or_default();
            if !LENGTH_UNITS.contains(&unit.as_str()) {
                return Err(ApiError::Validation(
                    "Unidad de longitud no válida".to_owned(),
                ));
            }
            doc! { "lengthUnit": unit }
        }
        Some(_) => return Err(ApiError::Validation("Ajuste no válido".to_owned())),
    };
    let mut set = change;
    set.insert("updatedAt", DateTime::now());
    db.collection::<UserDoc>("users")
        .update_one(doc! { "_id": user_id }, doc! { "$set": set })
        .await?;
    Ok(())
}

async fn create_measurement(
    db: &Database,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    let id = mutation_target(mutation)?;
    let measurements = db.collection::<MeasurementDoc>("measurements");
    if let Some(existing) = measurements.find_one(doc! { "_id": id }).await? {
        return if existing.user_id == user_id {
            Ok(())
        } else {
            Err(ApiError::Conflict(
                "Conflicto de id sin conexión".to_owned(),
            ))
        };
    }
    let input: MeasurementInput = decoded(mutation)?;
    let data = measurement_data(user_id, &input)?;
    let now = DateTime::now();
    measurements
        .insert_one(MeasurementDoc {
            id,
            user_id,
            date: data.date,
            body_weight: data.body_weight,
            height: data.height,
            body_fat: data.body_fat,
            items: data.items,
            photos: data.photos,
            logged_by: None,
            created_at: now,
            updated_at: now,
        })
        .await?;
    Ok(())
}

async fn update_measurement(
    db: &Database,
    s3: Option<&S3Config>,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    let id = mutation_target(mutation)?;
    let input: MeasurementInput = decoded(mutation)?;
    let data = measurement_data(user_id, &input)?;
    let measurements = db.collection::<MeasurementDoc>("measurements");
    let previous = measurements
        .find_one_and_update(
            doc! { "_id": id, "userId": user_id },
            doc! { "$set": {
                "date": &data.date,
                "bodyWeight": data.body_weight,
                "height": data.height,
                "bodyFat": data.body_fat,
                "items": to_bson(&data.items).map_err(|_| ApiError::Crypto)?,
                "photos": &data.photos,
                "updatedAt": DateTime::now(),
            } },
        )
        .await?
        .ok_or(ApiError::NotFound)?;
    let removed = previous
        .photos
        .into_iter()
        .filter(|key| !data.photos.contains(key))
        .collect();
    forget_photos(s3, removed);
    Ok(())
}

async fn delete_measurement(
    db: &Database,
    s3: Option<&S3Config>,
    user_id: ObjectId,
    mutation: &SyncMutationInput,
) -> Result<(), ApiError> {
    if let Some(previous) = db
        .collection::<MeasurementDoc>("measurements")
        .find_one_and_delete(doc! { "_id": mutation_target(mutation)?, "userId": user_id })
        .await?
    {
        forget_photos(s3, previous.photos);
    }
    Ok(())
}
