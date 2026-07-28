mod admin;
mod auth;
mod exercises;
mod routines;
mod sessions;
mod shared;

use actix_web::web;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.configure(auth::configure)
        .configure(exercises::configure)
        .configure(routines::configure)
        .configure(sessions::configure)
        .configure(admin::configure);
}
