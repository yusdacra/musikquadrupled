use axum::{extract::State, response::IntoResponse};
use http::StatusCode;

use crate::{error::AppError, AppState};

pub(crate) async fn revoke_all_tokens(State(app): State<AppState>) -> impl IntoResponse {
    app.tokens.revoke_all().await;
    tokio::spawn(async move {
        if let Err(err) = app.tokens.write(&app.tokens_path).await {
            tracing::error!("couldn't write tokens file: {err}");
        }
    });
    StatusCode::OK
}

pub(crate) async fn generate_token(
    State(app): State<AppState>,
) -> Result<axum::response::Response, AppError> {
    // generate token
    let token = app.tokens.generate().await?;
    // start task to write tokens
    tokio::spawn(async move {
        if let Err(err) = app.tokens.write(&app.tokens_path).await {
            tracing::error!("couldn't write tokens file: {err}");
        }
    });
    Ok(token.into_response())
}
