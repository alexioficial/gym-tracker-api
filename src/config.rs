use std::env;

use crate::audit::AuditCipher;

#[derive(Clone)]
pub struct Config {
    pub mongodb_uri: String,
    pub mongodb_db: String,
    pub admin_username: String,
    pub admin_password: String,
    pub frontend_origin: String,
    pub session_cookie_secure: bool,
    pub audit_cipher: AuditCipher,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let is_production = matches!(env::var("RUST_ENV").as_deref(), Ok("production"))
            || matches!(env::var("NODE_ENV").as_deref(), Ok("production"));
        let mongodb_uri = required("MONGODB_URI", is_production)?;
        let admin_password = required("ADMIN_PASSWORD", is_production)?;
        let audit_key = required("AUDIT_LOG_ENCRYPTION_KEY", is_production)?;
        let frontend_origin = env::var("FRONTEND_ORIGIN")
            .unwrap_or_else(|_| "http://localhost:5173".to_owned())
            .trim_end_matches('/')
            .to_owned();

        if is_production && frontend_origin.starts_with("http://") {
            return Err("FRONTEND_ORIGIN must use HTTPS in production".to_owned());
        }

        Ok(Self {
            mongodb_uri,
            mongodb_db: env::var("MONGODB_DB").unwrap_or_else(|_| "gym_tracker".to_owned()),
            admin_username: normalize_username(
                &env::var("ADMIN_USERNAME").unwrap_or_else(|_| "alexioficial".to_owned()),
            ),
            admin_password,
            frontend_origin,
            // Never permit a production deployment to weaken a session cookie.
            session_cookie_secure: is_production
                || env::var("SESSION_COOKIE_SECURE")
                    .map(|value| value.eq_ignore_ascii_case("true"))
                    .unwrap_or(false),
            audit_cipher: AuditCipher::from_base64(&audit_key)?,
        })
    }
}

fn required(key: &str, is_production: bool) -> Result<String, String> {
    match env::var(key) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ if is_production => Err(format!(
            "Missing required production environment variable: {key}"
        )),
        _ if key == "ADMIN_PASSWORD" => Ok("1029384756".to_owned()),
        _ => Err(format!("Missing environment variable: {key}")),
    }
}

pub fn normalize_username(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}
