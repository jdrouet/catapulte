use axum::Extension;
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use catapulte_domain::port::template_repository::StoredTemplate;
use catapulte_domain::use_case::manage_templates::ManageTemplatesUseCase;
use serde::{Deserialize, Serialize};

use crate::HttpServerState;
use crate::auth::{ApiKeyIdentity, require_admin};
use crate::error::AppError;

pub const MAX_TEMPLATE_SOURCE_BYTES: usize = 512 * 1024;
pub const MAX_TEMPLATE_NAME_CHARS: usize = 128;
/// Largest JSON body the template routes read: the source plus room for the variables.
pub const MAX_TEMPLATE_REQUEST_BYTES: usize = 1024 * 1024;

type Variables = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, Deserialize)]
pub struct PutTemplateRequest {
    pub source: String,
    #[serde(default)]
    pub validation_variables: Variables,
}

#[derive(Debug, Default, Deserialize)]
pub struct PreviewRequest {
    #[serde(default)]
    pub variables: Variables,
}

#[derive(Debug, Serialize)]
pub struct TemplateSummaryDto {
    pub name: String,
    pub version: u32,
    pub created_at_ms: i64,
}

#[derive(Debug, Serialize)]
pub struct TemplateDto {
    pub name: String,
    pub version: u32,
    pub created_at_ms: i64,
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct ListTemplatesResponse {
    pub templates: Vec<TemplateSummaryDto>,
}

#[derive(Debug, Serialize)]
pub struct PreviewResponse {
    pub html: String,
    pub text: Option<String>,
}

impl From<StoredTemplate> for TemplateDto {
    fn from(t: StoredTemplate) -> Self {
        Self {
            name: t.name,
            version: t.version,
            created_at_ms: t.created_at_ms,
            source: t.source,
        }
    }
}

/// Names are 1-128 characters of `a-z`, `0-9`, `_`, `.` and `-`, and never start with a dot, so
/// they can neither be paths nor collide with hidden files.
fn validate_name(name: &str) -> Result<(), AppError> {
    let valid = (1..=MAX_TEMPLATE_NAME_CHARS).contains(&name.len())
        && !name.starts_with('.')
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(AppError::BadRequestRaw(
            "template names are 1-128 characters from a-z, 0-9, _, . and -".to_owned(),
        ))
    }
}

fn json_has_nul(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => s.contains('\0'),
        serde_json::Value::Array(items) => items.iter().any(json_has_nul),
        serde_json::Value::Object(map) => {
            map.iter().any(|(k, v)| k.contains('\0') || json_has_nul(v))
        }
        _ => false,
    }
}

fn check_variables(variables: &Variables) -> Result<(), AppError> {
    if variables
        .iter()
        .any(|(k, v)| k.contains('\0') || json_has_nul(v))
    {
        return Err(AppError::BadRequestRaw(
            "variables must not contain NUL characters".to_owned(),
        ));
    }
    Ok(())
}

/// # Errors
///
/// Returns `AppError::Forbidden` for a non-admin key.
#[tracing::instrument(skip_all)]
pub async fn list_templates<S: HttpServerState>(
    State(state): State<S>,
    Extension(identity): Extension<ApiKeyIdentity>,
) -> Result<Json<ListTemplatesResponse>, AppError> {
    require_admin(&identity)?;
    let templates = state
        .manage_templates()
        .list()
        .await?
        .into_iter()
        .map(|t| TemplateSummaryDto {
            name: t.name,
            version: t.version,
            created_at_ms: t.created_at_ms,
        })
        .collect();
    Ok(Json(ListTemplatesResponse { templates }))
}

/// # Errors
///
/// Returns `AppError::Forbidden` for a non-admin key and 404 for an unknown name.
#[tracing::instrument(skip_all)]
pub async fn get_template<S: HttpServerState>(
    State(state): State<S>,
    Extension(identity): Extension<ApiKeyIdentity>,
    Path(name): Path<String>,
) -> Result<Json<TemplateDto>, AppError> {
    require_admin(&identity)?;
    validate_name(&name)?;
    Ok(Json(state.manage_templates().get(name).await?.into()))
}

/// # Errors
///
/// Returns 403 for a non-admin key, 400 for an invalid name, source or template, and 409 when
/// the name is a file on disk.
#[tracing::instrument(skip_all)]
pub async fn put_template<S: HttpServerState>(
    State(state): State<S>,
    Extension(identity): Extension<ApiKeyIdentity>,
    Path(name): Path<String>,
    payload: Result<Json<PutTemplateRequest>, JsonRejection>,
) -> Result<Json<TemplateDto>, AppError> {
    require_admin(&identity)?;
    validate_name(&name)?;
    let Json(request) = payload?;
    if request.source.len() > MAX_TEMPLATE_SOURCE_BYTES {
        return Err(AppError::BadRequestRaw(format!(
            "template source is limited to {MAX_TEMPLATE_SOURCE_BYTES} bytes"
        )));
    }
    if request.source.contains('\0') {
        return Err(AppError::BadRequestRaw(
            "template source must not contain NUL characters".to_owned(),
        ));
    }
    check_variables(&request.validation_variables)?;
    let stored = state
        .manage_templates()
        .put(name, request.source, request.validation_variables)
        .await?;
    Ok(Json(stored.into()))
}

/// # Errors
///
/// Returns 403 for a non-admin key, 404 for an unknown name and 409 for a file on disk.
#[tracing::instrument(skip_all)]
pub async fn delete_template<S: HttpServerState>(
    State(state): State<S>,
    Extension(identity): Extension<ApiKeyIdentity>,
    Path(name): Path<String>,
) -> Result<StatusCode, AppError> {
    require_admin(&identity)?;
    validate_name(&name)?;
    state.manage_templates().delete(name).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// # Errors
///
/// Returns 403 for a non-admin key, 404 for an unknown name and 400 when rendering fails.
#[tracing::instrument(skip_all)]
pub async fn preview_template<S: HttpServerState>(
    State(state): State<S>,
    Extension(identity): Extension<ApiKeyIdentity>,
    Path(name): Path<String>,
    payload: Result<Json<PreviewRequest>, JsonRejection>,
) -> Result<Json<PreviewResponse>, AppError> {
    require_admin(&identity)?;
    validate_name(&name)?;
    let Json(request) = payload?;
    check_variables(&request.variables)?;
    let preview = state
        .manage_templates()
        .preview(name, request.variables)
        .await?;
    Ok(Json(PreviewResponse {
        html: preview.html,
        text: preview.text,
    }))
}

#[cfg(test)]
pub(crate) struct NoopManageTemplates;

#[cfg(test)]
impl ManageTemplatesUseCase for NoopManageTemplates {
    async fn list(
        &self,
    ) -> Result<
        Vec<StoredTemplate>,
        catapulte_domain::use_case::manage_templates::ManageTemplatesError,
    > {
        Ok(vec![])
    }

    async fn get(
        &self,
        name: String,
    ) -> Result<StoredTemplate, catapulte_domain::use_case::manage_templates::ManageTemplatesError>
    {
        Err(catapulte_domain::use_case::manage_templates::ManageTemplatesError::NotFound { name })
    }

    async fn put(
        &self,
        _: String,
        _: String,
        _: Variables,
    ) -> Result<StoredTemplate, catapulte_domain::use_case::manage_templates::ManageTemplatesError>
    {
        unimplemented!()
    }

    async fn delete(
        &self,
        name: String,
    ) -> Result<(), catapulte_domain::use_case::manage_templates::ManageTemplatesError> {
        Err(catapulte_domain::use_case::manage_templates::ManageTemplatesError::NotFound { name })
    }

    async fn preview(
        &self,
        name: String,
        _: Variables,
    ) -> Result<
        catapulte_domain::use_case::manage_templates::TemplatePreview,
        catapulte_domain::use_case::manage_templates::ManageTemplatesError,
    > {
        Err(catapulte_domain::use_case::manage_templates::ManageTemplatesError::NotFound { name })
    }
}

#[cfg(test)]
mod tests {
    use super::validate_name;

    #[test]
    fn names_are_restricted_to_safe_characters() {
        for ok in ["welcome", "order-v2.1", "a_b", &"a".repeat(128)] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "Upper",
            "a/b",
            "a\\b",
            "../x",
            ".hidden",
            "a b",
            &"a".repeat(129),
        ] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }
}
