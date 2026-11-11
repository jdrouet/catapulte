use axum::Extension;
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use catapulte_domain::entity::suppression::{
    Suppression, SuppressionReason, normalize_suppression_address,
};
use catapulte_domain::port::suppression_repository::ListSuppressionsParams;
use catapulte_domain::use_case::manage_suppressions::ManageSuppressionsUseCase;
use lettre::Address;
use serde::{Deserialize, Serialize};

use crate::HttpServerState;
use crate::auth::{ApiKeyIdentity, require_admin};
use crate::error::AppError;

pub const DEFAULT_SUPPRESSIONS_LIMIT: u32 = 20;
pub const MAX_SUPPRESSIONS_LIMIT: u32 = 100;
pub const MAX_SUPPRESSIONS_PER_REQUEST: usize = 100;

#[derive(Debug, Deserialize)]
pub struct ListSuppressionsQuery {
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct SuppressionDto {
    pub address: String,
    pub reason: &'static str,
    pub created_at_ms: i64,
    pub source_email_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ListSuppressionsResponse {
    pub suppressions: Vec<SuppressionDto>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Deserialize)]
pub struct AddSuppressionsRequest {
    pub suppressions: Vec<NewSuppressionDto>,
}

#[derive(Debug, Deserialize)]
pub struct NewSuppressionDto {
    pub address: String,
    #[serde(default)]
    pub reason: Option<String>,
}

fn parse_reason(raw: &str) -> Result<SuppressionReason, AppError> {
    raw.parse()
        .map_err(|_| AppError::BadRequestRaw(format!("unknown suppression reason {raw:?}")))
}

/// The stored form of an address: validated, bare and lowercased.
fn normalize_address(raw: &str) -> Result<String, AppError> {
    raw.trim()
        .parse::<Address>()
        .ok()
        .and_then(|a| normalize_suppression_address(a.as_ref()))
        .ok_or_else(|| AppError::BadRequestRaw(format!("invalid address {raw:?}")))
}

/// # Errors
///
/// Returns `AppError::Forbidden` for a non-admin key and `AppError::BadRequestRaw` for an
/// unknown reason.
#[tracing::instrument(skip_all)]
pub async fn list_suppressions<S: HttpServerState>(
    State(state): State<S>,
    Extension(identity): Extension<ApiKeyIdentity>,
    Query(query): Query<ListSuppressionsQuery>,
) -> Result<Json<ListSuppressionsResponse>, AppError> {
    require_admin(&identity)?;
    let reason = query.reason.as_deref().map(parse_reason).transpose()?;
    let limit = query
        .limit
        .unwrap_or(DEFAULT_SUPPRESSIONS_LIMIT)
        .min(MAX_SUPPRESSIONS_LIMIT);
    let offset = query.offset.unwrap_or(0);
    let suppressions = state
        .manage_suppressions()
        .list(ListSuppressionsParams {
            reason,
            address: query
                .address
                .map(|a| normalize_suppression_address(&a).unwrap_or_else(|| a.to_lowercase())),
            limit,
            offset,
        })
        .await?
        .into_iter()
        .map(|s| SuppressionDto {
            address: s.address,
            reason: s.reason.as_str(),
            created_at_ms: s.created_at_ms,
            source_email_id: s.source_email_id.map(|id| id.as_uuid().to_string()),
        })
        .collect();
    Ok(Json(ListSuppressionsResponse {
        suppressions,
        limit,
        offset,
    }))
}

/// # Errors
///
/// Returns `AppError::Forbidden` for a non-admin key and `AppError::BadRequestRaw` for an
/// invalid address, unknown reason or more than 100 entries.
#[tracing::instrument(skip_all)]
pub async fn add_suppressions<S: HttpServerState>(
    State(state): State<S>,
    Extension(identity): Extension<ApiKeyIdentity>,
    payload: Result<Json<AddSuppressionsRequest>, JsonRejection>,
) -> Result<StatusCode, AppError> {
    require_admin(&identity)?;
    let Json(request) = payload?;
    if request.suppressions.len() > MAX_SUPPRESSIONS_PER_REQUEST {
        return Err(AppError::BadRequestRaw(format!(
            "at most {MAX_SUPPRESSIONS_PER_REQUEST} suppressions per request"
        )));
    }
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX);
    let entries = request
        .suppressions
        .into_iter()
        .map(|s| {
            Ok(Suppression {
                address: normalize_address(&s.address)?,
                reason: s
                    .reason
                    .as_deref()
                    .map_or(Ok(SuppressionReason::Manual), parse_reason)?,
                created_at_ms: now_ms,
                source_email_id: None,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    state.manage_suppressions().add(entries).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// # Errors
///
/// Returns `AppError::Forbidden` for a non-admin key and `AppError::NotFound` when the address
/// is not suppressed.
#[tracing::instrument(skip_all)]
pub async fn delete_suppression<S: HttpServerState>(
    State(state): State<S>,
    Extension(identity): Extension<ApiKeyIdentity>,
    Path(address): Path<String>,
) -> Result<StatusCode, AppError> {
    require_admin(&identity)?;
    let address = normalize_address(&address)?;
    if state.manage_suppressions().remove(address).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

#[cfg(test)]
pub(crate) struct NoopManageSuppressions;

#[cfg(test)]
#[allow(async_fn_in_trait)]
impl ManageSuppressionsUseCase for NoopManageSuppressions {
    async fn list(
        &self,
        _: ListSuppressionsParams,
    ) -> Result<
        Vec<Suppression>,
        catapulte_domain::use_case::manage_suppressions::ManageSuppressionsError,
    > {
        Ok(vec![])
    }

    async fn add(
        &self,
        _: Vec<Suppression>,
    ) -> Result<(), catapulte_domain::use_case::manage_suppressions::ManageSuppressionsError> {
        Ok(())
    }

    async fn remove(
        &self,
        _: String,
    ) -> Result<bool, catapulte_domain::use_case::manage_suppressions::ManageSuppressionsError>
    {
        Ok(false)
    }
}
