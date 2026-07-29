mod app;
mod audit;
mod auth;
mod config;
mod db;
mod error;
mod models;
mod routes;
mod validation;

use actix_cors::Cors;
use actix_web::{App, HttpServer, http::header, middleware, web};
use app::AppState;
use config::Config;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenvy::dotenv().ok();
    eprintln!("gym-tracker-api: starting");
    let config =
        Config::from_env().unwrap_or_else(|error| panic!("Invalid configuration: {error}"));
    eprintln!("gym-tracker-api: connecting to MongoDB");
    let database = db::connect(&config)
        .await
        .unwrap_or_else(|error| panic!("Database initialization failed: {error}"));
    eprintln!("gym-tracker-api: MongoDB ready");
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_owned());
    let port = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(8080);
    let state = web::Data::new(AppState {
        db: database,
        config: config.clone(),
    });

    eprintln!("gym-tracker-api: listening on http://{host}:{port}");
    HttpServer::new(move || {
        let cors = Cors::default()
            .allowed_origin(&config.frontend_origin)
            .allowed_methods(vec!["GET", "POST", "PUT", "DELETE"])
            .allowed_headers(vec![header::CONTENT_TYPE, header::ORIGIN])
            .supports_credentials()
            .max_age(3600);
        App::new()
            .app_data(state.clone())
            .wrap(cors)
            .wrap(middleware::from_fn(audit::log_request))
            .configure(app::configure)
    })
    .bind((host, port))?
    .run()
    .await
}
