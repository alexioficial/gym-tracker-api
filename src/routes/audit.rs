use actix_web::{HttpRequest, web};
use chrono::{DateTime as ChronoDateTime, NaiveDate, Utc};
use futures::TryStreamExt;
use mongodb::{
    bson::{DateTime, Document, doc, oid::ObjectId},
    options::FindOptions,
};
use serde::{Deserialize, Serialize};

use crate::{app::AppState, audit::decrypt_document, error::ApiError, routes::shared::admin};

const COLLECTION: &str = "audit_logs";

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/admin/audit", web::get().to(list))
        .route("/admin/audit/{id}", web::get().to(detail));
}

#[derive(Deserialize)]
pub struct AuditQuery {
    pub method: Option<String>,
    pub path: Option<String>,
    pub status: Option<String>,
    pub client: Option<String>,
    /// Inclusive date in YYYY-MM-DD or RFC3339 form.
    pub from: Option<String>,
    /// Inclusive date in YYYY-MM-DD or RFC3339 form.
    pub to: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditListItem {
    id: String,
    created_at: String,
    request_id: String,
    method: String,
    path: String,
    status: u16,
    client_kind: String,
    reported_client_ip: Option<String>,
    duration_ms: u128,
}

async fn list(
    request: HttpRequest,
    state: web::Data<AppState>,
    query: web::Query<AuditQuery>,
) -> Result<web::Json<Vec<AuditListItem>>, ApiError> {
    admin(&request, &state).await?;
    let cipher = &state.config.audit_cipher;
    let mut filter = Document::new();
    if let Some(value) = non_empty(&query.method) {
        filter.insert("methodIndex", cipher.blind_index(value));
    }
    if let Some(value) = non_empty(&query.path) {
        filter.insert("pathIndex", cipher.blind_index(value));
    }
    if let Some(value) = non_empty(&query.status) {
        filter.insert("statusIndex", cipher.blind_index(value));
    }
    if let Some(value) = non_empty(&query.client) {
        filter.insert("clientIndex", cipher.blind_index(value));
    }
    let mut dates = Document::new();
    if let Some(value) = non_empty(&query.from) {
        dates.insert("$gte", parse_date(value, false)?);
    }
    if let Some(value) = non_empty(&query.to) {
        dates.insert("$lte", parse_date(value, true)?);
    }
    if !dates.is_empty() {
        filter.insert("createdAt", dates);
    }
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let records = state
        .db
        .collection::<Document>(COLLECTION)
        .find(filter)
        .with_options(
            FindOptions::builder()
                .sort(doc! { "createdAt": -1 })
                .limit(limit)
                .build(),
        )
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    let mut result = Vec::with_capacity(records.len());
    for record in records {
        let payload = decrypt_document(cipher, &record).map_err(|_| ApiError::Crypto)?;
        let id = record
            .get_object_id("_id")
            .map_err(|_| ApiError::Crypto)?
            .to_hex();
        let created_at = record
            .get_datetime("createdAt")
            .map_err(|_| ApiError::Crypto)?
            .try_to_rfc3339_string()
            .map_err(|_| ApiError::Crypto)?;
        result.push(AuditListItem {
            id,
            created_at,
            request_id: value_string(&payload, "/request/id")?,
            method: value_string(&payload, "/request/method")?,
            path: value_string(&payload, "/request/path")?,
            status: value_u16(&payload, "/response/status")?,
            client_kind: value_string(&payload, "/request/clientKind")?,
            reported_client_ip: payload
                .pointer("/request/reportedClientIp")
                .and_then(|value| value.as_str())
                .map(str::to_owned),
            duration_ms: payload
                .pointer("/response/durationMs")
                .and_then(|value| value.as_u64())
                .ok_or(ApiError::Crypto)? as u128,
        });
    }
    Ok(web::Json(result))
}

async fn detail(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
) -> Result<web::Json<serde_json::Value>, ApiError> {
    admin(&request, &state).await?;
    let id = ObjectId::parse_str(path.into_inner())
        .map_err(|_| ApiError::Validation("Invalid audit record id".to_owned()))?;
    let record = state
        .db
        .collection::<Document>(COLLECTION)
        .find_one(doc! { "_id": id })
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(web::Json(
        decrypt_document(&state.config.audit_cipher, &record).map_err(|_| ApiError::Crypto)?,
    ))
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.trim().is_empty())
}

fn parse_date(value: &str, end_of_day: bool) -> Result<DateTime, ApiError> {
    if let Ok(parsed) = ChronoDateTime::parse_from_rfc3339(value) {
        return Ok(DateTime::from_millis(
            parsed.with_timezone(&Utc).timestamp_millis(),
        ));
    }
    let day = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| ApiError::Validation("Dates must be YYYY-MM-DD or RFC3339".to_owned()))?;
    let time = if end_of_day {
        day.and_hms_opt(23, 59, 59).ok_or(ApiError::Crypto)?
    } else {
        day.and_hms_opt(0, 0, 0).ok_or(ApiError::Crypto)?
    };
    Ok(DateTime::from_millis(time.and_utc().timestamp_millis()))
}

fn value_string(payload: &serde_json::Value, pointer: &str) -> Result<String, ApiError> {
    payload
        .pointer(pointer)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or(ApiError::Crypto)
}

fn value_u16(payload: &serde_json::Value, pointer: &str) -> Result<u16, ApiError> {
    payload
        .pointer(pointer)
        .and_then(|value| value.as_u64())
        .and_then(|value| u16::try_from(value).ok())
        .ok_or(ApiError::Crypto)
}
