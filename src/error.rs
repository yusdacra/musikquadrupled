use std::fmt::Display;

use axum::response::IntoResponse;
use http::StatusCode;

type BoxedError = Box<dyn std::error::Error>;

#[derive(Debug)]
pub(crate) struct AppError {
    internal: BoxedError,
    status: Option<StatusCode>,
}

impl AppError {
    pub(crate) fn status(mut self, code: StatusCode) -> Self {
        self.status = Some(code);
        self
    }
}

impl<E> From<E> for AppError
where
    E: Into<BoxedError>,
{
    fn from(err: E) -> Self {
        Self {
            internal: err.into(),
            status: None,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status.unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            format!("Something went wrong: {}", self.internal),
        )
            .into_response()
    }
}

impl Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.internal.fmt(f)
    }
}
