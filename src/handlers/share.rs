use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
};
use http::{
    header::{CACHE_CONTROL, RANGE},
    Request, Response,
};
use hyper::Body;

use crate::{error::AppError, AppState};

use super::{Auth, AUDIO_CACHE_HEADER};

pub(crate) async fn generate_scoped_token(
    State(app): State<AppState>,
    Query(query): Query<Auth>,
    Path(music_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    app.verify_token(query.token).await?;

    // generate token
    let token = app.scoped_tokens.generate_for_id(music_id).await;
    Ok(token)
}

pub(crate) async fn get_scoped_music_info(
    State(app): State<AppState>,
    Path(token): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let music_id = app.verify_scoped_token(token).await?;
    let Some(info) = app.music_info.get(music_id).await else {
        return Err("music id not found".into());
    };
    Ok(serde_json::to_string(&info).unwrap())
}

pub(crate) async fn get_scoped_music_thumbnail(
    State(app): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response<Body>, AppError> {
    let music_id = app.verify_scoped_token(token).await?;
    let Some(info) = app.music_info.get(music_id).await else {
        return Err("music id not found".into());
    };
    app.make_musikcubed_request(
        format!("thumbnail/{}", info.thumbnail_id),
        Request::new(Body::empty()),
    )
    .await
}

pub(crate) async fn get_scoped_music_file(
    State(app): State<AppState>,
    Path(token): Path<String>,
    request: Request<Body>,
) -> Result<Response<Body>, AppError> {
    let music_id = app.verify_scoped_token(token).await?;
    let mut req = Request::new(Body::empty());
    // proxy any range headers
    if let Some(range) = request.headers().get(RANGE).cloned() {
        req.headers_mut().insert(RANGE, range);
    }
    let mut resp = app
        .make_musikcubed_request(format!("audio/external_id/{music_id}"), req)
        .await?;
    if resp.status().is_success() {
        // add cache header
        resp.headers_mut()
            .insert(CACHE_CONTROL, AUDIO_CACHE_HEADER.clone());
    }
    Ok(resp)
}
