use crate::middleware::Authentication;
use actix_web::{guard, web};

mod json;
mod multipart;

pub fn config(app: &mut web::ServiceConfig) {
    app.service(
        web::scope("/templates")
            .wrap(Authentication::from_env())
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
