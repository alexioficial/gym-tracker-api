use std::{
    net::{IpAddr, SocketAddr},
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use actix_web::{
    Error, HttpRequest, HttpResponse,
    body::{EitherBody, MessageBody},
    dev::{ServiceRequest, ServiceResponse},
    http::header::{COOKIE, HeaderMap, RETRY_AFTER},
    middleware::Next,
    web,
};
use governor::{
    DefaultDirectRateLimiter, DefaultKeyedRateLimiter, Quota, RateLimiter,
    clock::{Clock, DefaultClock},
};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::app::AppState;

const GLOBAL_REQUESTS_PER_MINUTE: u32 = 120;
const LOGIN_ATTEMPTS_PER_MINUTE: u32 = 5;
const TOTAL_REQUESTS_PER_MINUTE: u32 = 6_000;
const CLEANUP_INTERVAL: u64 = 1_024;

#[derive(Clone)]
pub struct RateLimiters {
    total: Arc<DefaultDirectRateLimiter>,
    global: Arc<DefaultKeyedRateLimiter<String>>,
    login: Arc<DefaultKeyedRateLimiter<String>>,
    checks: Arc<AtomicU64>,
}

impl RateLimiters {
    pub fn new() -> Self {
        Self {
            total: Arc::new(RateLimiter::direct(per_minute(TOTAL_REQUESTS_PER_MINUTE))),
            global: Arc::new(RateLimiter::keyed(per_minute(GLOBAL_REQUESTS_PER_MINUTE))),
            login: Arc::new(RateLimiter::keyed(per_minute(LOGIN_ATTEMPTS_PER_MINUTE))),
            checks: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn check_global(&self, key: &str) -> Result<(), u64> {
        self.clean_up_periodically();
        self.total.check().map_err(|denied| {
            denied
                .wait_time_from(DefaultClock::default().now())
                .as_secs()
                .saturating_add(1)
        })?;
        check(&self.global, key)
    }

    pub fn check_login(&self, key: &str) -> Result<(), u64> {
        check(&self.login, key)
    }

    fn clean_up_periodically(&self) {
        if self
            .checks
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(CLEANUP_INTERVAL)
        {
            self.global.retain_recent();
            self.login.retain_recent();
        }
    }
}

fn per_minute(requests: u32) -> Quota {
    Quota::per_minute(NonZeroU32::new(requests).expect("rate limit must be non-zero"))
}

fn check(limiter: &DefaultKeyedRateLimiter<String>, key: &str) -> Result<(), u64> {
    limiter.check_key(&key.to_owned()).map_err(|denied| {
        denied
            .wait_time_from(DefaultClock::default().now())
            .as_secs()
            .saturating_add(1)
    })
}

pub fn request_key(request: &HttpRequest, trust_proxy_headers: bool) -> String {
    client_key(request.headers(), request.peer_addr(), trust_proxy_headers)
}

pub fn login_key(
    request: &HttpRequest,
    trust_proxy_headers: bool,
    normalized_username: &str,
) -> String {
    format!(
        "{}:{normalized_username}",
        request_key(request, trust_proxy_headers)
    )
}

fn service_request_key(request: &ServiceRequest, trust_proxy_headers: bool) -> String {
    session_key(request.headers()).unwrap_or_else(|| {
        format!(
            "ip:{}",
            client_key(request.headers(), request.peer_addr(), trust_proxy_headers)
        )
    })
}

fn session_key(headers: &HeaderMap) -> Option<String> {
    let token = headers
        .get(COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|cookie| cookie.trim().split_once('='))
        .find_map(|(name, value)| (name == crate::auth::SESSION_COOKIE).then_some(value))?;
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!(
        "session:{}",
        hex::encode(Sha256::digest(token.as_bytes()))
    ))
}

fn client_key(
    headers: &HeaderMap,
    peer_addr: Option<SocketAddr>,
    trust_proxy_headers: bool,
) -> String {
    let address = trust_proxy_headers
        .then(|| proxy_ip(headers))
        .flatten()
        .or_else(|| peer_addr.map(|address| address.ip()));
    address
        .map(|address| address.to_string())
        .unwrap_or_else(|| "unknown-client".to_owned())
}

fn proxy_ip(headers: &HeaderMap) -> Option<IpAddr> {
    for name in ["cf-connecting-ip", "x-real-ip"] {
        if let Some(address) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_ip)
        {
            return Some(address);
        }
    }
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next_back())
        .and_then(parse_ip)
}

fn parse_ip(value: &str) -> Option<IpAddr> {
    let value = value.trim();
    value
        .parse::<IpAddr>()
        .ok()
        .or_else(|| value.parse::<SocketAddr>().ok().map(|address| address.ip()))
}

pub async fn enforce(
    request: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<EitherBody<impl MessageBody>>, Error> {
    let Some(state) = request.app_data::<web::Data<AppState>>().cloned() else {
        return Ok(next.call(request).await?.map_into_left_body());
    };
    let key = service_request_key(&request, state.config.trust_proxy_headers);
    if let Err(retry_after) = state.rate_limits.check_global(&key) {
        let response = HttpResponse::TooManyRequests()
            .insert_header((RETRY_AFTER, retry_after.to_string()))
            .json(json!({ "error": "Too many requests" }));
        return Ok(request.into_response(response).map_into_right_body());
    }
    Ok(next.call(request).await?.map_into_left_body())
}

#[cfg(test)]
mod tests {
    use actix_web::test::TestRequest;

    use super::{RateLimiters, login_key, request_key};

    #[test]
    fn uses_the_tcp_peer_when_proxy_headers_are_not_trusted() {
        let request = TestRequest::default()
            .peer_addr("10.0.0.4:1234".parse().unwrap())
            .insert_header(("cf-connecting-ip", "203.0.113.8"))
            .to_http_request();
        assert_eq!(request_key(&request, false), "10.0.0.4");
    }

    #[test]
    fn uses_cloudflare_ip_when_proxy_headers_are_trusted() {
        let request = TestRequest::default()
            .peer_addr("10.0.0.4:1234".parse().unwrap())
            .insert_header(("cf-connecting-ip", "203.0.113.8"))
            .to_http_request();
        assert_eq!(request_key(&request, true), "203.0.113.8");
    }

    #[test]
    fn blocks_the_sixth_login_attempt_for_the_same_client() {
        let limits = RateLimiters::new();
        for _ in 0..5 {
            assert!(limits.check_login("203.0.113.8").is_ok());
        }
        assert!(limits.check_login("203.0.113.8").is_err());
        assert!(limits.check_login("203.0.113.9").is_ok());
    }

    #[test]
    fn login_limits_are_scoped_by_normalized_username() {
        let request = TestRequest::default()
            .peer_addr("10.0.0.4:1234".parse().unwrap())
            .to_http_request();
        assert_ne!(
            login_key(&request, false, "alex"),
            login_key(&request, false, "sam")
        );
    }
}
