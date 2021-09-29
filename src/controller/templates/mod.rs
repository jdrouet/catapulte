use crate::config::Config;
use crate::middleware::authentication::Authentication;
use actix_web::{guard, web};
use std::sync::Arc;

mod json;
mod multipart;

pub fn config(config: Arc<Config>, app: &mut web::ServiceConfig) {
    app.service(
        web::scope("/templates")
            .wrap(Authentication::from(config))
            .route(
                "/{name}",
                web::route()
                    .guard(guard::Post())
                    .guard(guard::fn_guard(multipart::filter))
                    .to(multipart::handler),
            )
            .route(
                "/{name}",
                web::route()
                    .guard(guard::Post())
                    .guard(guard::fn_guard(json::filter))
                    .to(json::handler),
            ),
    );
}
