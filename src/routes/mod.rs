mod admin;
mod audit;
mod auth;
mod exercises;
mod routines;
mod sessions;
mod shared;
mod sync;

use actix_web::web;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.configure(auth::configure)
        .configure(exercises::configure)
        .configure(routines::configure)
        .configure(sessions::configure)
        .configure(sync::configure)
        .configure(admin::configure)
        .configure(audit::configure);
}
