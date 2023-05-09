use super::AppError;
use axum::{
    routing::{get, post},
    Router,
};
use http::{
    header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE},
    HeaderName, Method, Request,
};
use hyper::Body;
use tower_http::{
    cors::CorsLayer,
    request_id::{MakeRequestUuid, SetRequestIdLayer},
    sensitive_headers::SetSensitiveRequestHeadersLayer,
    trace::TraceLayer,
};
use tracing::Span;

use crate::{
    handlers,
    state::AppState,
    utils::{remove_token_from_query, QueryDisplay},
};

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

fn make_span_trace<B>(req: &Request<B>) -> Span {
    let query_map = remove_token_from_query(req.uri().query());

    let request_id = req
        .headers()
        .get(REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("no id set");

    if query_map.is_empty() {
        tracing::debug_span!(
            "request",
            path = %req.uri().path(),
            id = %request_id,
        )
    } else {
        let query_display = QueryDisplay::new(query_map);
        tracing::debug_span!(
            "request",
            path = %req.uri().path(),
            query = %query_display,
            id = %request_id,
        )
    }
}

pub(super) async fn handler(state: AppState) -> Result<(Router, Router), AppError> {
    let internal_router = Router::new()
        .route("/token/generate", get(handlers::generate_token))
        .route("/token/revoke_all", post(handlers::revoke_all_tokens))
        .with_state(state.clone());

    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(make_span_trace)
        .on_request(|req: &Request<Body>, _span: &Span| {
            tracing::debug!(
                "started processing request {} on {:?}",
                req.method(),
                req.version(),
            )
        });
    let cors_layer = CorsLayer::new()
        .allow_origin(tower_http::cors::Any)
        .allow_headers([CONTENT_TYPE, CACHE_CONTROL, REQUEST_ID])
        .allow_methods([Method::GET]);
    let sensitive_header_layer = SetSensitiveRequestHeadersLayer::new([AUTHORIZATION]);
    let request_id_layer = SetRequestIdLayer::new(REQUEST_ID.clone(), MakeRequestUuid);

    let router = Router::new()
        .route("/thumbnail/:id", get(handlers::http))
        .route("/audio/external_id/:id", get(handlers::get_music))
        .route("/share/generate/:id", get(handlers::generate_scoped_token))
        .route("/share/audio/:token", get(handlers::get_scoped_music_file))
        .route(
            "/share/thumbnail/:token",
            get(handlers::get_scoped_music_thumbnail),
        )
        .route("/share/info/:token", get(handlers::get_scoped_music_info))
        .route("/", get(handlers::metadata_ws))
        .layer(trace_layer)
        .layer(sensitive_header_layer)
        .layer(cors_layer)
        .layer(request_id_layer)
        .with_state(state);

    Ok((router, internal_router))
}
