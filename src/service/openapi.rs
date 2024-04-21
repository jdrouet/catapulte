use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[derive(OpenApi, serde::Serialize)]
#[openapi(
    paths(
        crate::controller::metrics::handler,
        crate::controller::status::handler,
        crate::controller::templates::json::handler,
        crate::controller::templates::multipart::handler,
    ),
    components(schemas(
        crate::error::ErrorResponse,
        crate::controller::templates::json::JsonPayload,
        crate::controller::templates::Recipient,
        crate::controller::templates::multipart::MultipartPayload,
    ))
)]
pub struct ApiDoc;

pub(crate) fn service() -> SwaggerUi {
    SwaggerUi::new("/swagger").url("/openapi.json", ApiDoc::openapi())
}
