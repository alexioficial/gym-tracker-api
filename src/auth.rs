use actix_web::{
    HttpRequest,
    cookie::{Cookie, SameSite, time::Duration},
};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use mongodb::{
    Database,
    bson::{DateTime, doc, oid::ObjectId},
};
use password_hash::SaltString;
use scrypt::{Params, scrypt};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{
    config::Config,
    error::ApiError,
    models::{AuthSessionDoc, UserDoc, UserOut},
};

pub const SESSION_COOKIE: &str = "gym_session";
pub const SESSION_TTL_SECONDS: i64 = 60 * 60 * 24 * 365;

pub fn hash_password(password: &str) -> Result<String, ApiError> {
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes()).map_err(|_| ApiError::Crypto)?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|value| value.to_string())
        .map_err(|_| ApiError::Crypto)
}

/// Supports the legacy Node scrypt format once, so existing accounts continue
/// working. New and rotated passwords use Argon2id via the default Argon2 policy.
pub fn verify_password(password: &str, stored: &str) -> Result<bool, ApiError> {
    if let Some(value) = stored.strip_prefix("scrypt$") {
        let mut parts = value.split('$');
        let Some(salt_hex) = parts.next() else {
            return Ok(false);
        };
        let Some(expected_hex) = parts.next() else {
            return Ok(false);
        };
        if parts.next().is_some() {
            return Ok(false);
        }
        let salt = hex::decode(salt_hex).map_err(|_| ApiError::Crypto)?;
        let expected = hex::decode(expected_hex).map_err(|_| ApiError::Crypto)?;
        if expected.len() != 64 {
            return Ok(false);
        }
        let params = Params::new(15, 8, 1, 64).map_err(|_| ApiError::Crypto)?;
        let mut actual = vec![0_u8; expected.len()];
        scrypt(password.as_bytes(), &salt, &params, &mut actual).map_err(|_| ApiError::Crypto)?;
        return Ok(bool::from(actual.ct_eq(&expected)));
    }

    let parsed = match PasswordHash::new(stored) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

pub fn password_is_valid(password: &str) -> bool {
    (6..=256).contains(&password.chars().count())
}

pub async fn create_session(db: &Database, user_id: ObjectId) -> Result<String, ApiError> {
    let token = Uuid::new_v4().simple().to_string() + &Uuid::new_v4().simple().to_string();
    let now = DateTime::now();
    let expires_at = DateTime::from_millis(now.timestamp_millis() + SESSION_TTL_SECONDS * 1000);
    db.collection::<AuthSessionDoc>("auth_sessions")
        .insert_one(AuthSessionDoc {
            id: token.clone(),
            user_id,
            expires_at,
            created_at: now,
        })
        .await?;
    Ok(token)
}

pub async fn current_user(request: &HttpRequest, db: &Database) -> Result<UserDoc, ApiError> {
    let Some(cookie) = request.cookie(SESSION_COOKIE) else {
        return Err(ApiError::Unauthorized);
    };
    let sessions = db.collection::<AuthSessionDoc>("auth_sessions");
    let Some(session) = sessions.find_one(doc! { "_id": cookie.value() }).await? else {
        return Err(ApiError::Unauthorized);
    };
    if session.expires_at.timestamp_millis() <= DateTime::now().timestamp_millis() {
        sessions.delete_one(doc! { "_id": session.id }).await?;
        return Err(ApiError::Unauthorized);
    }
    db.collection::<UserDoc>("users")
        .find_one(doc! { "_id": session.user_id })
        .await?
        .ok_or(ApiError::Unauthorized)
}

pub async fn destroy_session(request: &HttpRequest, db: &Database) -> Result<(), ApiError> {
    if let Some(cookie) = request.cookie(SESSION_COOKIE) {
        db.collection::<AuthSessionDoc>("auth_sessions")
            .delete_one(doc! { "_id": cookie.value() })
            .await?;
    }
    Ok(())
}

pub fn session_cookie(token: String, config: &Config) -> Cookie<'static> {
    Cookie::build(SESSION_COOKIE, token)
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(config.session_cookie_secure)
        .max_age(Duration::seconds(SESSION_TTL_SECONDS))
        .finish()
}

pub fn expired_session_cookie(config: &Config) -> Cookie<'static> {
    Cookie::build(SESSION_COOKIE, "")
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(config.session_cookie_secure)
        .max_age(Duration::seconds(0))
        .finish()
}

pub async fn revoke_user_sessions(db: &Database, user_id: ObjectId) -> Result<(), ApiError> {
    db.collection::<AuthSessionDoc>("auth_sessions")
        .delete_many(doc! { "userId": user_id })
        .await?;
    Ok(())
}

pub fn public_user(user: &UserDoc) -> UserOut {
    UserOut::from(user)
}
