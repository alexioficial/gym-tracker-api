use actix_web::{
    HttpResponse, ResponseError,
    http::{StatusCode, header},
};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Tienes que iniciar sesión")]
    Unauthorized,
    #[error("Solo para administradores")]
    Forbidden,
    #[error("No encontrado")]
    NotFound,
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Conflict(String),
    #[error("Demasiadas peticiones; espera un momento")]
    RateLimited { retry_after: u64 },
    #[error("Error interno del servidor")]
    Internal(#[from] mongodb::error::Error),
    #[error("Error interno del servidor")]
    Crypto,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Validation(_) => StatusCode::BAD_REQUEST,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal(_) | Self::Crypto => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_response(&self) -> HttpResponse {
        if self.status_code().is_server_error() {
            eprintln!("internal error: {self:?}");
        }
        let message = self.to_string();
        let mut response = HttpResponse::build(self.status_code());
        if let Self::RateLimited { retry_after } = self {
            response.insert_header((header::RETRY_AFTER, retry_after.to_string()));
        }
        response.json(ErrorBody { error: &message })
    }
}
