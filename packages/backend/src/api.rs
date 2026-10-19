use std::sync::Arc;

use app_contract_api::{service_handler::msg::CustomExecuteMsg, user_registry::msg::UserId};
use axum::{
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{header::AUTHORIZATION, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{
    action_store::{ActionOutcome, ActionResult, ActionStore, CreateAction, UpdateAction},
    registration_store::{Admission, RegistrationStore},
};

#[derive(Clone)]
pub struct ApiState {
    pub store: RegistrationStore,
    pub actions: ActionStore,
    pub api_key_hash: [u8; 32],
    pub hourly_quota: i64,
    pub daily_quota: i64,
}

pub fn router(state: ApiState) -> Router {
    let state = Arc::new(state);
    let protected = Router::new()
        .route("/v1/registrations", post(register))
        .route("/v1/registrations/{user_id}", get(status))
        .route("/v1/actions", post(create_action).get(list_actions))
        .route("/v1/actions/{id}", get(action_details))
        .route("/v1/actions/{id}/submission", post(action_submission))
        .route("/v1/actions/{id}/result", post(action_result))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/readyz", get(ready))
        .merge(protected)
        .layer(DefaultBodyLimit::max(4096))
        .with_state(state)
}

async fn authenticate(
    State(state): State<Arc<ApiState>>,
    request: Request,
    next: Next,
) -> Response {
    let candidate = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let valid = candidate.is_some_and(|key| {
        let hash: [u8; 32] = Sha256::digest(key.as_bytes()).into();
        bool::from(hash.ct_eq(&state.api_key_hash))
    });
    if !valid {
        return ApiError(StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    next.run(request).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterRequest {
    email: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateActionRequest {
    event_id: String,
    payload: CustomExecuteMsg,
}

fn is_hex_id(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|v| v.is_ascii_hexdigit() && !v.is_ascii_uppercase())
}

async fn create_action(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<CreateActionRequest>,
) -> Result<Response, ApiError> {
    if !is_hex_id(&input.event_id, 40) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_event_id"));
    }
    let CustomExecuteMsg::Email(email) = &input.payload;
    if !is_hex_id(email.from.as_str(), 64) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_user_id"));
    }
    match state
        .actions
        .create(&input.event_id, input.payload)
        .await
        .map_err(internal)?
    {
        CreateAction::Created(action) => Ok((StatusCode::CREATED, Json(action)).into_response()),
        CreateAction::Existing(action) => Ok((StatusCode::OK, Json(action)).into_response()),
        CreateAction::Conflict => Err(ApiError(StatusCode::CONFLICT, "action_conflict")),
    }
}

fn validate_action_id(id: i64) -> Result<(), ApiError> {
    if id <= 0 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_action_id"));
    }
    Ok(())
}

fn action_update_response(result: UpdateAction) -> Result<Response, ApiError> {
    match result {
        UpdateAction::Updated(action) => Ok(Json(action).into_response()),
        UpdateAction::NotFound => Err(ApiError(StatusCode::NOT_FOUND, "action_not_found")),
        UpdateAction::Conflict => Err(ApiError(StatusCode::CONFLICT, "action_conflict")),
    }
}

async fn action_submission(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    validate_action_id(id)?;
    action_update_response(state.actions.submission(id).await.map_err(internal)?)
}

async fn action_result(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<i64>,
    Json(mut input): Json<ActionResult>,
) -> Result<Response, ApiError> {
    validate_action_id(id)?;
    // Cosmos transaction hashes are 32 bytes encoded as 64 hex characters.
    if let Some(hash) = &mut input.tx_hash {
        if hash.len() != 64 || !hash.bytes().all(|v| v.is_ascii_hexdigit()) {
            return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_tx_hash"));
        }
        hash.make_ascii_uppercase();
    }
    // Accept bounded, stable error codes rather than raw error messages.
    let valid_code = input.error_code.as_ref().is_some_and(|code| {
        !code.is_empty()
            && code.len() <= 64
            && code
                .bytes()
                .all(|v| v.is_ascii_lowercase() || v.is_ascii_digit() || v == b'_')
    });
    let valid = match input.status {
        ActionOutcome::Completed => input.tx_hash.is_some() && input.error_code.is_none(),
        ActionOutcome::Failed | ActionOutcome::OutcomeUnknown => valid_code,
    };
    if !valid {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_action_result"));
    }
    action_update_response(
        state
            .actions
            .report_result(id, input)
            .await
            .map_err(internal)?,
    )
}

async fn action_details(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    validate_action_id(id)?;
    let details = state
        .actions
        .details(id)
        .await
        .map_err(internal)?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "action_not_found"))?;
    Ok(Json(details).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListActionsQuery {
    user_id: String,
    limit: Option<u32>,
    before_id: Option<i64>,
}

async fn list_actions(
    State(state): State<Arc<ApiState>>,
    Query(input): Query<ListActionsQuery>,
) -> Result<Response, ApiError> {
    if !is_hex_id(&input.user_id, 64) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_user_id"));
    }
    let limit = input.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) || input.before_id.is_some_and(|id| id <= 0) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_pagination"));
    }
    let page = state
        .actions
        .list(&input.user_id, input.before_id, limit)
        .await
        .map_err(internal)?;
    Ok(Json(page).into_response())
}

// Preserve mailbox case: changing identity normalization here would stop matching
// IDs produced by the existing email reader. Do not accept display names or lists.
pub fn validate_email(input: &str) -> Option<String> {
    let email = input.trim();
    let options = email_address::Options::default()
        .without_display_text()
        .without_domain_literal();
    (email.len() <= 254
        && email.is_ascii()
        && email_address::EmailAddress::parse_with_options(email, options).is_ok())
    .then(|| email.to_owned())
}

async fn register(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<RegisterRequest>,
) -> Result<Response, ApiError> {
    let email =
        validate_email(&input.email).ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_email"))?;
    let user_id = UserId::new_email_address(&email);
    let admission = state
        .store
        .admit(
            &email,
            user_id.as_str(),
            state.hourly_quota,
            state.daily_quota,
        )
        .await
        .map_err(internal)?;
    match admission {
        Admission::Created(row) | Admission::Resumed(row) => {
            Ok((StatusCode::ACCEPTED, Json(row)).into_response())
        }
        Admission::Existing(row) => Ok((StatusCode::OK, Json(row)).into_response()),
        Admission::QuotaExceeded => Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "registration_quota_exceeded",
        )),
    }
}

async fn status(
    State(state): State<Arc<ApiState>>,
    Path(user_id): Path<String>,
) -> Result<Response, ApiError> {
    if !is_hex_id(&user_id, 64) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_user_id"));
    }
    let row = state
        .store
        .get(&user_id)
        .await
        .map_err(internal)?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "registration_not_found"))?;
    Ok(Json(row).into_response())
}

async fn ready(State(state): State<Arc<ApiState>>) -> StatusCode {
    match sqlx::query("SELECT 1").execute(&state.store.pool).await {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

fn internal(_error: anyhow::Error) -> ApiError {
    tracing::error!("Backend database operation failed");
    ApiError(StatusCode::SERVICE_UNAVAILABLE, "backend_unavailable")
}

struct ApiError(StatusCode, &'static str);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        #[derive(Serialize)]
        struct ErrorBody {
            error: &'static str,
        }
        (self.0, Json(ErrorBody { error: self.1 })).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_identity_matches_operator_without_case_folding() {
        let email = validate_email("  Alice@example.com  ").unwrap();
        assert_eq!(email, "Alice@example.com");
        assert_eq!(
            UserId::new_email_address(&email),
            UserId::new_email_address("Alice@example.com")
        );
        assert_ne!(
            UserId::new_email_address(&email),
            UserId::new_email_address("alice@example.com")
        );
        assert!(validate_email("Alice <alice@example.com>").is_none());
        assert!(validate_email("a@example.com,b@example.com").is_none());
        assert!(validate_email("invalid").is_none());
    }
}
