use actix_web::{HttpRequest, HttpResponse, http::header, web};
use chrono::Utc;
use mongodb::{
    Database,
    bson::{doc, oid::ObjectId},
};

use crate::{
    access::can_view_user,
    app::AppState,
    error::ApiError,
    models::{
        MeasurementDoc, MeasurementInput, MeasurementItemDoc, PhotoUploadInput, PhotoUploadOut,
    },
    routes::shared::{require_same_origin, user, writer},
    storage::{
        PHOTO_CONTENT_TYPES, S3Config, UPLOAD_URL_SECONDS, VIEW_URL_SECONDS, photo_key, photo_owner,
    },
    validation::{
        MAX_BODY_WEIGHT, MAX_LENGTH_CM, MEASUREMENT_ITEMS_MAX, MEASUREMENT_NAME_MAX,
        MEASUREMENT_PHOTOS_MAX, object_id, round, text, valid_date,
    },
};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/photos/uploads", web::post().to(upload_url))
        .route("/photos/{key:.*}", web::get().to(view));
}

pub struct MeasurementData {
    pub date: String,
    pub body_weight: Option<f64>,
    pub height: Option<f64>,
    pub body_fat: Option<f64>,
    pub items: Vec<MeasurementItemDoc>,
    pub photos: Vec<String>,
}

fn bounded(
    value: Option<f64>,
    max: f64,
    decimals: i32,
    field: &str,
) -> Result<Option<f64>, ApiError> {
    match value {
        None => Ok(None),
        Some(value) if value.is_finite() && value > 0.0 && value <= max => {
            Ok(Some(round(value, decimals)))
        }
        Some(_) => Err(ApiError::Validation(format!("Revisa el campo «{field}»"))),
    }
}

/// Validates a check-in for `user_id`. Photos must be keys the API issued to that user.
pub fn measurement_data(
    user_id: ObjectId,
    input: &MeasurementInput,
) -> Result<MeasurementData, ApiError> {
    if !valid_date(&input.date) {
        return Err(ApiError::Validation(
            "Introduce una fecha válida".to_owned(),
        ));
    }
    if input.items.len() > MEASUREMENT_ITEMS_MAX {
        return Err(ApiError::Validation(format!(
            "Como máximo {MEASUREMENT_ITEMS_MAX} medidas por registro"
        )));
    }
    let mut items = Vec::with_capacity(input.items.len());
    for item in &input.items {
        let name = text(
            &item.name,
            "nombre de la medida",
            MEASUREMENT_NAME_MAX,
            true,
        )?;
        let value = bounded(Some(item.value), MAX_LENGTH_CM, 2, &name)?.unwrap_or_default();
        items.push(MeasurementItemDoc { name, value });
    }
    if input.photos.len() > MEASUREMENT_PHOTOS_MAX {
        return Err(ApiError::Validation(format!(
            "Como máximo {MEASUREMENT_PHOTOS_MAX} fotos por registro"
        )));
    }
    let owner = user_id.to_hex();
    if input
        .photos
        .iter()
        .any(|key| photo_owner(key) != Some(owner.as_str()))
    {
        return Err(ApiError::Validation("Foto no válida".to_owned()));
    }
    let mut photos = input.photos.clone();
    photos.dedup();
    let data = MeasurementData {
        date: input.date.clone(),
        body_weight: bounded(input.body_weight, MAX_BODY_WEIGHT, 2, "peso corporal")?,
        height: bounded(input.height, MAX_LENGTH_CM, 1, "altura")?,
        body_fat: bounded(input.body_fat, 100.0, 1, "% de grasa")?,
        items,
        photos,
    };
    if data.body_weight.is_none()
        && data.height.is_none()
        && data.body_fat.is_none()
        && data.items.is_empty()
        && data.photos.is_empty()
    {
        return Err(ApiError::Validation(
            "Anota al menos una medida, el peso o una foto".to_owned(),
        ));
    }
    Ok(data)
}

/// Removes objects that no saved check-in points to any more, off the request path.
pub fn forget_photos(s3: Option<&S3Config>, keys: Vec<String>) {
    if let Some(s3) = s3.cloned() {
        if !keys.is_empty() {
            actix_web::rt::spawn(async move { s3.delete(keys).await });
        }
    }
}

pub async fn user_photo_keys(db: &Database, user_id: ObjectId) -> Result<Vec<String>, ApiError> {
    use futures::TryStreamExt;
    Ok(db
        .collection::<MeasurementDoc>("measurements")
        .find(doc! { "userId": user_id, "photos.0": { "$exists": true } })
        .await?
        .try_collect::<Vec<_>>()
        .await?
        .into_iter()
        .flat_map(|item| item.photos)
        .collect())
}

fn storage(state: &AppState) -> Result<&S3Config, ApiError> {
    state.config.s3.as_ref().ok_or_else(|| {
        ApiError::Unavailable("Las fotos no están configuradas en el servidor".to_owned())
    })
}

/// A presigned PUT the browser uses to send one photo straight to the bucket.
/// `userId` lets a coach upload for a client (stage 2b); by default it is the caller.
async fn upload_url(
    request: HttpRequest,
    state: web::Data<AppState>,
    input: web::Json<PhotoUploadInput>,
    query: web::Query<std::collections::HashMap<String, String>>,
) -> Result<web::Json<PhotoUploadOut>, ApiError> {
    require_same_origin(&request, &state)?;
    let current = writer(&request, &state).await?;
    let s3 = storage(&state)?;
    let owner = match query.get("userId") {
        Some(value) => object_id(value)?,
        None => current.id,
    };
    if !can_view_user(&state.db, &current, owner).await? {
        return Err(ApiError::Forbidden);
    }
    let Some((content_type, extension)) = PHOTO_CONTENT_TYPES
        .iter()
        .find(|(content_type, _)| *content_type == input.content_type)
    else {
        return Err(ApiError::Validation(
            "Solo se admiten fotos JPEG, PNG o WebP".to_owned(),
        ));
    };
    let key = photo_key(&owner.to_hex(), extension);
    let upload_url = s3.presign(
        "PUT",
        &key,
        UPLOAD_URL_SECONDS,
        &[("content-type", content_type)],
        Utc::now(),
    );
    Ok(web::Json(PhotoUploadOut {
        key,
        upload_url,
        content_type: (*content_type).to_owned(),
    }))
}

/// Redirects to a short-lived presigned GET, for `<img src>`.
async fn view(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
) -> Result<HttpResponse, ApiError> {
    let current = user(&request, &state).await?;
    let s3 = storage(&state)?;
    let key = path.into_inner();
    let owner = photo_owner(&key)
        .and_then(|owner| ObjectId::parse_str(owner).ok())
        .ok_or(ApiError::NotFound)?;
    if !can_view_user(&state.db, &current, owner).await? {
        return Err(ApiError::NotFound);
    }
    let url = s3.presign("GET", &key, VIEW_URL_SECONDS, &[], Utc::now());
    Ok(HttpResponse::Found()
        .insert_header((header::LOCATION, url))
        .insert_header((header::CACHE_CONTROL, "private, max-age=240"))
        .finish())
}
