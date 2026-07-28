use actix_web::{HttpResponse, web};
use mongodb::Database;
use serde_json::json;

use crate::{config::Config, routes};

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub config: Config,
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/health", web::get().to(health))
        .service(web::scope("/api").configure(routes::configure));
}

async fn health() -> HttpResponse {
    HttpResponse::Ok().json(json!({ "status": "ok" }))
}
