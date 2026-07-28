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
        ExerciseDoc, RoutineDoc, RoutineExerciseDoc, RoutineExerciseInput, RoutineInput,
        RoutineOut, ScheduleDays, ScheduleDoc, ScheduleInput,
    },
    routes::shared::{require_same_origin, user},
    validation::{MAX_ROUTINE_EXERCISES, ROUTINE_COLORS, ROUTINE_MAX, object_id, text},
};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/routines", web::get().to(list))
        .route("/routines", web::post().to(create))
        .route("/routines/{id}", web::put().to(update))
        .route("/routines/{id}", web::delete().to(delete))
        .route("/schedule", web::get().to(get_schedule))
        .route("/schedule/{day}", web::put().to(set_schedule_day));
}

async fn list(
    request: HttpRequest,
    state: web::Data<AppState>,
) -> Result<web::Json<Vec<RoutineOut>>, ApiError> {
    let current = user(&request, &state).await?;
    let routines = state
        .db
        .collection::<RoutineDoc>("routines")
        .find(doc! { "userId": current.id })
        .sort(doc! { "order": 1, "name": 1 })
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    Ok(web::Json(
        routines.into_iter().map(RoutineOut::from).collect(),
    ))
}

async fn owned_exercises(
    db: &Database,
    user_id: ObjectId,
    entries: &[RoutineExerciseInput],
) -> Result<Vec<RoutineExerciseDoc>, ApiError> {
    if entries.len() > MAX_ROUTINE_EXERCISES {
        return Err(ApiError::Validation(
            "Too many exercises in a routine".to_owned(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let mut exercises = Vec::with_capacity(entries.len());
    for entry in entries {
        let exercise_id = object_id(&entry.exercise_id)?;
        if !seen.insert(exercise_id) {
            return Err(ApiError::Validation(
                "An exercise may appear only once in a routine".to_owned(),
            ));
        }
        let sets = if entry.sets.is_finite() {
            entry.sets.round().clamp(1.0, 10.0) as i32
        } else {
            3
        };
        exercises.push(RoutineExerciseDoc { exercise_id, sets });
    }
    if !exercises.is_empty() {
        let ids = exercises
            .iter()
            .map(|entry| entry.exercise_id)
            .collect::<Vec<_>>();
        let count = db
            .collection::<ExerciseDoc>("exercises")
            .count_documents(doc! { "userId": user_id, "_id": { "$in": ids } })
            .await?;
        if count != exercises.len() as u64 {
            return Err(ApiError::Validation(
                "A routine can only contain your own exercises".to_owned(),
            ));
        }
    }
    Ok(exercises)
}

fn valid_color(color: &str) -> bool {
    ROUTINE_COLORS.contains(&color)
}

async fn create(
    request: HttpRequest,
    state: web::Data<AppState>,
    input: web::Json<RoutineInput>,
) -> Result<web::Json<RoutineOut>, ApiError> {
    require_same_origin(&request, &state)?;
    let current = user(&request, &state).await?;
    if !valid_color(&input.color) {
        return Err(ApiError::Validation("Invalid routine color".to_owned()));
    }
    let routines = state.db.collection::<RoutineDoc>("routines");
    let routine = RoutineDoc {
        id: ObjectId::new(),
        user_id: current.id,
        name: text(&input.name, "routine name", ROUTINE_MAX, true)?,
        color: input.color.clone(),
        order: routines
            .count_documents(doc! { "userId": current.id })
            .await? as i64,
        exercises: owned_exercises(&state.db, current.id, &input.exercises).await?,
        legacy_exercise_ids: vec![],
        created_at: DateTime::now(),
        updated_at: DateTime::now(),
    };
    routines.insert_one(routine.clone()).await?;
    Ok(web::Json(RoutineOut::from(routine)))
}

async fn update(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
    input: web::Json<RoutineInput>,
) -> Result<HttpResponse, ApiError> {
    require_same_origin(&request, &state)?;
    let current = user(&request, &state).await?;
    if !valid_color(&input.color) {
        return Err(ApiError::Validation("Invalid routine color".to_owned()));
    }
    let exercises = owned_exercises(&state.db, current.id, &input.exercises).await?;
    let result = state
        .db
        .collection::<RoutineDoc>("routines")
        .update_one(
            doc! { "_id": object_id(&path)?, "userId": current.id },
            doc! { "$set": { "name": text(&input.name, "routine name", ROUTINE_MAX, true)?, "color": &input.color, "exercises": to_bson(&exercises).map_err(|_| ApiError::Crypto)?, "updatedAt": DateTime::now() }, "$unset": { "exerciseIds": "" } },
        )
        .await?;
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
    let id = object_id(&path)?;
    let result = state
        .db
        .collection::<RoutineDoc>("routines")
        .delete_one(doc! { "_id": id, "userId": current.id })
        .await?;
    if result.deleted_count == 0 {
        return Err(ApiError::NotFound);
    }
    let schedule = state.db.collection::<ScheduleDoc>("schedule");
    if let Some(mut item) = schedule.find_one(doc! { "userId": current.id }).await? {
        for value in [
            &mut item.days.mon,
            &mut item.days.tue,
            &mut item.days.wed,
            &mut item.days.thu,
            &mut item.days.fri,
            &mut item.days.sat,
            &mut item.days.sun,
        ] {
            if value.as_deref() == Some(path.as_str()) {
                *value = None;
            }
        }
        schedule
            .update_one(
                doc! { "userId": current.id },
                doc! { "$set": { "days": to_bson(&item.days).map_err(|_| ApiError::Crypto)? } },
            )
            .await?;
    }
    Ok(HttpResponse::NoContent().finish())
}

async fn get_schedule(
    request: HttpRequest,
    state: web::Data<AppState>,
) -> Result<web::Json<ScheduleDays>, ApiError> {
    let current = user(&request, &state).await?;
    let days = state
        .db
        .collection::<ScheduleDoc>("schedule")
        .find_one(doc! { "userId": current.id })
        .await?
        .map(|item| item.days)
        .unwrap_or_default();
    Ok(web::Json(days))
}

fn day_slot<'a>(days: &'a mut ScheduleDays, day: &str) -> Result<&'a mut Option<String>, ApiError> {
    match day {
        "mon" => Ok(&mut days.mon),
        "tue" => Ok(&mut days.tue),
        "wed" => Ok(&mut days.wed),
        "thu" => Ok(&mut days.thu),
        "fri" => Ok(&mut days.fri),
        "sat" => Ok(&mut days.sat),
        "sun" => Ok(&mut days.sun),
        _ => Err(ApiError::Validation("Invalid day".to_owned())),
    }
}

async fn set_schedule_day(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
    input: web::Json<ScheduleInput>,
) -> Result<HttpResponse, ApiError> {
    require_same_origin(&request, &state)?;
    let current = user(&request, &state).await?;
    let routine_id = match &input.routine_id {
        Some(value) if !value.is_empty() => {
            let id = object_id(value)?;
            let owned = state
                .db
                .collection::<RoutineDoc>("routines")
                .find_one(doc! { "_id": id, "userId": current.id })
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
    let schedules = state.db.collection::<ScheduleDoc>("schedule");
    let mut days = schedules
        .find_one(doc! { "userId": current.id })
        .await?
        .map(|item| item.days)
        .unwrap_or_default();
    *day_slot(&mut days, &path)? = routine_id;
    schedules.update_one(doc! { "userId": current.id }, doc! { "$set": { "days": to_bson(&days).map_err(|_| ApiError::Crypto)? }, "$setOnInsert": { "userId": current.id } }).upsert(true).await?;
    Ok(HttpResponse::NoContent().finish())
}
