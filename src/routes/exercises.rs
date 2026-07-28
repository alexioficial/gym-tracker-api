use actix_web::{HttpRequest, HttpResponse, web};
use futures::TryStreamExt;
use mongodb::bson::{DateTime, Document, doc, oid::ObjectId};

use crate::{
    app::AppState,
    error::ApiError,
    models::{ExerciseDoc, ExerciseInput, ExerciseOut},
    routes::shared::{require_same_origin, user},
    validation::{EXERCISE_MAX, MUSCLE_GROUP_MAX, clean_notes, object_id, text},
};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/exercises", web::get().to(list))
        .route("/exercises", web::post().to(create))
        .route("/exercises/{id}", web::put().to(update))
        .route("/exercises/{id}", web::delete().to(delete));
}

async fn list(
    request: HttpRequest,
    state: web::Data<AppState>,
) -> Result<web::Json<Vec<ExerciseOut>>, ApiError> {
    let current = user(&request, &state).await?;
    let exercises = state
        .db
        .collection::<ExerciseDoc>("exercises")
        .find(doc! { "userId": current.id })
        .sort(doc! { "name": 1 })
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    Ok(web::Json(
        exercises.into_iter().map(ExerciseOut::from).collect(),
    ))
}

async fn create(
    request: HttpRequest,
    state: web::Data<AppState>,
    input: web::Json<ExerciseInput>,
) -> Result<web::Json<ExerciseOut>, ApiError> {
    require_same_origin(&request, &state)?;
    let current = user(&request, &state).await?;
    let now = DateTime::now();
    let exercise = ExerciseDoc {
        id: ObjectId::new(),
        user_id: current.id,
        name: text(&input.name, "exercise name", EXERCISE_MAX, true)?,
        muscle_group: text(&input.muscle_group, "muscle group", MUSCLE_GROUP_MAX, false)?,
        notes: clean_notes(input.notes.clone())?,
        created_at: now,
        updated_at: now,
    };
    state
        .db
        .collection::<ExerciseDoc>("exercises")
        .insert_one(exercise.clone())
        .await?;
    Ok(web::Json(ExerciseOut::from(exercise)))
}

async fn update(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
    input: web::Json<ExerciseInput>,
) -> Result<HttpResponse, ApiError> {
    require_same_origin(&request, &state)?;
    let current = user(&request, &state).await?;
    let result = state
        .db
        .collection::<ExerciseDoc>("exercises")
        .update_one(
            doc! { "_id": object_id(&path)?, "userId": current.id },
            doc! { "$set": { "name": text(&input.name, "exercise name", EXERCISE_MAX, true)?, "muscleGroup": text(&input.muscle_group, "muscle group", MUSCLE_GROUP_MAX, false)?, "notes": clean_notes(input.notes.clone())?, "updatedAt": DateTime::now() } },
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
        .collection::<ExerciseDoc>("exercises")
        .delete_one(doc! { "_id": id, "userId": current.id })
        .await?;
    if result.deleted_count == 0 {
        return Err(ApiError::NotFound);
    }
    let routines = state.db.collection::<Document>("routines");
    routines
        .update_many(
            doc! { "userId": current.id, "exercises.exerciseId": id },
            doc! { "$pull": { "exercises": { "exerciseId": id } } },
        )
        .await?;
    routines
        .update_many(
            doc! { "userId": current.id, "exerciseIds": id },
            doc! { "$pull": { "exerciseIds": id } },
        )
        .await?;
    Ok(HttpResponse::NoContent().finish())
}
