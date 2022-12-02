// use axum::extract::Json;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[derive(OpenApi, serde::Serialize)]
#[openapi(
    paths(
        super::metrics::handler,
        super::status::handler,
        super::templates::json::handler,
        super::templates::multipart::handler,
    ),
    components(schemas(
        crate::error::ServerError,
        super::templates::json::JsonPayload,
        super::templates::json::Recipient,
        super::templates::multipart::MultipartPayload,
    ))
)]
pub(crate) struct ApiDoc;

pub(super) fn service() -> SwaggerUi {
    SwaggerUi::new("/swagger").url("/openapi.json", ApiDoc::openapi())
}
