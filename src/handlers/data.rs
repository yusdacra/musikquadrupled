use axum::extract::{Query, State};
use http::{
    header::{AUTHORIZATION, CACHE_CONTROL, RANGE},
    Request, Response,
};
use hyper::Body;

use crate::{
    error::AppError,
    utils::{extract_password_from_basic_auth, remove_token_from_query},
    AppState,
};

use super::{Auth, AUDIO_CACHE_HEADER};

pub(crate) async fn get_music(
    State(app): State<AppState>,
    Query(query): Query<Auth>,
    req: Request<Body>,
) -> Result<Response<Body>, AppError> {
    http(State(app), Query(query), req).await.map(|mut resp| {
        if resp.status().is_success() {
            // add cache header
            resp.headers_mut()
                .insert(CACHE_CONTROL, AUDIO_CACHE_HEADER.clone());
        }
        resp
    })
}

pub(crate) async fn http(
    State(app): State<AppState>,
    Query(auth): Query<Auth>,
    req: Request<Body>,
) -> Result<Response<Body>, AppError> {
    let maybe_token = auth.token.or_else(|| {
        req.headers()
            .get(AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|auth| extract_password_from_basic_auth(auth).ok())
    });
    app.verify_token(maybe_token).await?;

    // remove token from query
    let path = req.uri().path();
    let query_map = remove_token_from_query(req.uri().query());
    let has_query = !query_map.is_empty();
    let query = has_query
        .then(|| serde_qs::to_string(&query_map).unwrap())
        .unwrap_or_else(String::new);
    let query_prefix = has_query.then_some("?").unwrap_or("");

    let mut request = Request::new(Body::empty());
    if let Some(range) = req.headers().get(RANGE).cloned() {
        request.headers_mut().insert(RANGE, range);
    }

    tracing::debug!(
        "proxying request to {}:{} with headers {:?}",
        app.musikcubed_address,
        app.musikcubed_http_port,
        req.headers()
    );

    app.make_musikcubed_request(format!("{path}{query_prefix}{query}"), request)
        .await
}
