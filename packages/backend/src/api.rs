use std::sync::Arc;

use app_contract_api::user_registry::msg::UserId;
use axum::{
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{header::AUTHORIZATION, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::store::{Admission, Store};

#[derive(Clone)]
pub struct ApiState {
    pub store: Store,
    pub api_key_hash: [u8; 32],
    pub hourly_quota: i64,
    pub daily_quota: i64,
}

pub fn router(state: ApiState) -> Router {
    let state = Arc::new(state);
    let protected = Router::new()
        .route("/v1/registrations", post(register))
        .route("/v1/registrations/{user_id}", get(status))
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
    if user_id.len() != 64
        || !user_id
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
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
    tracing::error!("Registration database operation failed");
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
