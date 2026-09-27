use std::time::Instant;

use actix_web::{
    Error, HttpMessage,
    body::MessageBody,
    dev::{ServiceRequest, ServiceResponse},
    middleware::Next,
    web,
};
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, OsRng, rand_core::RngCore},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{Duration, Utc};
use futures::StreamExt;
use hmac::{Hmac, Mac};
use mongodb::bson::{DateTime, Document, doc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::app::AppState;

const COLLECTION: &str = "audit_logs";
const AAD: &[u8] = b"gym-tracker.audit.v1";
/// Matches the JSON extractor's default limit; anything larger is refused before
/// it is buffered in memory.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const BODY_PREVIEW_CHARS: usize = 4_096;
const REDACTED: &str = "[redacted]";
const SECRET_HEADERS: [&str; 4] = ["authorization", "cookie", "proxy-authorization", "set-cookie"];
type HmacSha256 = Hmac<Sha256>;

/// Encryption material is never persisted in MongoDB.  The base64 key must decode
/// to exactly 32 random bytes, which is the AES-256-GCM key size.
#[derive(Clone)]
pub struct AuditCipher {
    cipher: Aes256Gcm,
    index_key: [u8; 32],
}

impl AuditCipher {
    pub fn from_base64(value: &str) -> Result<Self, String> {
        let raw = BASE64
            .decode(value.trim())
            .map_err(|_| "AUDIT_LOG_ENCRYPTION_KEY must be base64-encoded".to_owned())?;
        let key: [u8; 32] = raw
            .try_into()
            .map_err(|_| "AUDIT_LOG_ENCRYPTION_KEY must decode to exactly 32 bytes".to_owned())?;
        let mut index_key = key;
        // Domain separation means a database index cannot be used as an AES key.
        for byte in &mut index_key {
            *byte ^= 0xA5;
        }
        Ok(Self {
            cipher: Aes256Gcm::new_from_slice(&key)
                .map_err(|_| "Invalid AUDIT_LOG_ENCRYPTION_KEY".to_owned())?,
            index_key,
        })
    }

    pub fn encrypt(&self, payload: &[u8]) -> Result<String, ()> {
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let encrypted = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                aes_gcm::aead::Payload {
                    msg: payload,
                    aad: AAD,
                },
            )
            .map_err(|_| ())?;
        let mut packed = Vec::with_capacity(nonce.len() + encrypted.len());
        packed.extend_from_slice(&nonce);
        packed.extend_from_slice(&encrypted);
        Ok(BASE64.encode(packed))
    }

    pub fn decrypt(&self, value: &str) -> Result<Vec<u8>, ()> {
        let packed = BASE64.decode(value).map_err(|_| ())?;
        if packed.len() < 12 {
            return Err(());
        }
        let (nonce, encrypted) = packed.split_at(12);
        self.cipher
            .decrypt(
                Nonce::from_slice(nonce),
                aes_gcm::aead::Payload {
                    msg: encrypted,
                    aad: AAD,
                },
            )
            .map_err(|_| ())
    }

    /// A one-way, keyed blind index allows exact filtering without storing the
    /// readable method/path/status/client values alongside the ciphertext.
    pub fn blind_index(&self, value: &str) -> String {
        let mut mac =
            <HmacSha256 as Mac>::new_from_slice(&self.index_key).expect("HMAC key is valid");
        mac.update(value.as_bytes());
        BASE64.encode(mac.finalize().into_bytes())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestAudit {
    id: String,
    received_at: String,
    method: String,
    path: String,
    query: Option<String>,
    uri: String,
    version: String,
    scheme: String,
    host: String,
    peer_ip: Option<String>,
    reported_client_ip: Option<String>,
    client_kind: String,
    headers: Vec<HeaderAudit>,
    cookies: Vec<CookieAudit>,
    body_length: usize,
    body_sha256: String,
    /// Truncated, with password fields redacted. Absent for binary bodies.
    body_preview: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HeaderAudit {
    name: String,
    value: Option<String>,
}

/// Cookie values are secrets; a short digest still lets requests from the same
/// session be correlated.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CookieAudit {
    name: String,
    fingerprint: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompleteAudit {
    request: RequestAudit,
    response: ResponseAudit,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseAudit {
    status: u16,
    duration_ms: u128,
    headers: Vec<HeaderAudit>,
}

pub async fn log_request(
    mut request: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, Error> {
    let Some(state) = request.app_data::<web::Data<AppState>>().cloned() else {
        return next.call(request).await;
    };
    let captured = capture_request(&mut request).await?;
    let started = Instant::now();
    let response = next.call(request).await?;
    let status = response.status().as_u16();
    let response_headers = capture_headers(response.headers().iter());
    let record = CompleteAudit {
        request: captured,
        response: ResponseAudit {
            status,
            duration_ms: started.elapsed().as_millis(),
            headers: response_headers,
        },
    };
    let cipher = state.config.audit_cipher.clone();
    actix_web::rt::spawn(async move {
        if let Err(error) = persist(&state, &cipher, record, status).await {
            eprintln!("audit log write failed: {error}");
        }
    });
    Ok(response)
}

async fn capture_request(request: &mut ServiceRequest) -> Result<RequestAudit, Error> {
    let declared_length = request
        .headers()
        .get(actix_web::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared_length.is_some_and(|length| length > MAX_BODY_BYTES) {
        return Err(actix_web::error::ErrorPayloadTooLarge(
            "La petición es demasiado grande",
        ));
    }
    let mut payload = request.take_payload();
    let mut raw_body = Vec::new();
    while let Some(chunk) = payload.next().await {
        let chunk = chunk?;
        if raw_body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(actix_web::error::ErrorPayloadTooLarge(
                "La petición es demasiado grande",
            ));
        }
        raw_body.extend_from_slice(&chunk);
    }
    request.set_payload(raw_body.clone().into());

    // `connection_info` holds an internal request borrow. Copy the values before
    // calling `cookies`, which needs a mutable borrow to parse/cache cookies.
    let (scheme, host, reported_client_ip) = {
        let info = request.connection_info();
        (
            info.scheme().to_owned(),
            info.host().to_owned(),
            info.realip_remote_addr().map(str::to_owned),
        )
    };
    let headers = capture_headers(request.headers().iter());
    let cookies = request
        .cookies()
        .map(|values| {
            values
                .iter()
                .map(|cookie| CookieAudit {
                    name: cookie.name().to_owned(),
                    fingerprint: hex::encode(&Sha256::digest(cookie.value().as_bytes())[..6]),
                })
                .collect()
        })
        .unwrap_or_default();
    let method = request.method().to_string();
    let path = request.path().to_owned();
    let query = request.uri().query().map(str::to_owned);
    let user_agent = request
        .headers()
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let has_origin = request.headers().contains_key("origin");
    // Server-side rendered web requests do not retain the browser's Origin or
    // user agent, so the official clients identify themselves explicitly.
    let declared_client = request
        .headers()
        .get("x-gym-client")
        .and_then(|value| value.to_str().ok());
    let client_kind = if declared_client == Some("mobile-app") {
        "mobile-app"
    } else if declared_client == Some("web") {
        "web"
    } else if user_agent.contains("Dart") || user_agent.contains("Flutter") {
        "mobile-app"
    } else if has_origin {
        "web"
    } else {
        "unknown"
    };

    Ok(RequestAudit {
        id: Uuid::new_v4().to_string(),
        received_at: Utc::now().to_rfc3339(),
        method,
        path,
        query,
        uri: request.uri().to_string(),
        version: format!("{:?}", request.version()),
        scheme,
        host,
        peer_ip: request.peer_addr().map(|address| address.ip().to_string()),
        reported_client_ip,
        client_kind: client_kind.to_owned(),
        headers,
        cookies,
        body_length: raw_body.len(),
        body_sha256: hex::encode(Sha256::digest(&raw_body)),
        body_preview: body_preview(&raw_body),
    })
}

fn body_preview(raw: &[u8]) -> Option<String> {
    let text = match serde_json::from_slice::<serde_json::Value>(raw) {
        Ok(mut value) => {
            redact_secrets(&mut value);
            value.to_string()
        }
        Err(_) => String::from_utf8(raw.to_vec()).ok()?,
    };
    Some(text.chars().take(BODY_PREVIEW_CHARS).collect())
}

fn redact_secrets(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, field) in fields.iter_mut() {
                let key = key.to_ascii_lowercase();
                if key.contains("password") || key.contains("token") || key.contains("secret") {
                    *field = serde_json::Value::String(REDACTED.to_owned());
                } else {
                    redact_secrets(field);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_secrets),
        _ => {}
    }
}

fn capture_headers<'a>(
    headers: impl Iterator<
        Item = (
            &'a actix_web::http::header::HeaderName,
            &'a actix_web::http::header::HeaderValue,
        ),
    >,
) -> Vec<HeaderAudit> {
    headers
        .map(|(name, value)| HeaderAudit {
            name: name.to_string(),
            value: if SECRET_HEADERS.contains(&name.as_str()) {
                Some(REDACTED.to_owned())
            } else {
                value.to_str().ok().map(str::to_owned)
            },
        })
        .collect()
}

async fn persist(
    state: &AppState,
    cipher: &AuditCipher,
    record: CompleteAudit,
    status: u16,
) -> Result<(), String> {
    let created_at = DateTime::now();
    let expires_at = DateTime::from_millis((Utc::now() + Duration::days(30)).timestamp_millis());
    let ciphertext = cipher
        .encrypt(&serde_json::to_vec(&record).map_err(|error| error.to_string())?)
        .map_err(|_| "encryption failed".to_owned())?;
    state
        .db
        .collection::<Document>(COLLECTION)
        .insert_one(doc! {
            "createdAt": created_at,
            "expiresAt": expires_at,
            "methodIndex": cipher.blind_index(&record.request.method),
            "pathIndex": cipher.blind_index(&record.request.path),
            "statusIndex": cipher.blind_index(&status.to_string()),
            "clientIndex": cipher.blind_index(&record.request.client_kind),
            "ciphertext": ciphertext,
            "version": 1_i32,
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub fn decrypt_document(
    cipher: &AuditCipher,
    document: &Document,
) -> Result<serde_json::Value, ()> {
    let ciphertext = document.get_str("ciphertext").map_err(|_| ())?;
    let raw = cipher.decrypt(ciphertext)?;
    serde_json::from_slice(&raw).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{REDACTED, body_preview};

    #[test]
    fn redacts_passwords_in_json_bodies() {
        let preview =
            body_preview(br#"{"username":"alex","password":"hunter22","nested":[{"newPassword":"x"}]}"#)
                .unwrap();
        assert!(!preview.contains("hunter22"));
        assert!(preview.contains("alex"));
        assert_eq!(preview.matches(REDACTED).count(), 2);
    }

    #[test]
    fn truncates_long_bodies() {
        let body = "a".repeat(10_000);
        assert_eq!(body_preview(body.as_bytes()).unwrap().len(), 4_096);
    }
}
