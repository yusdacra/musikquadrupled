use std::{borrow::Cow, collections::HashMap, fmt::Display};

use super::AppError;
use async_tungstenite::{
    tokio::TokioAdapter,
    tungstenite::{protocol::CloseFrame as TungsteniteCloseFrame, Message as TungsteniteMessage},
    WebSocketStream,
};
use axum::{
    extract::{
        ws::{CloseFrame as AxumCloseFrame, Message as AxumMessage, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    headers::UserAgent,
    response::IntoResponse,
    routing::{get, post},
    Router, TypedHeader,
};
use base64::Engine;
use futures::{SinkExt, StreamExt};
use http::{
    header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE},
    HeaderName, HeaderValue, Method, Request, Response, StatusCode,
};
use hyper::Body;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::TcpStream;
use tower_http::{
    cors::CorsLayer,
    request_id::{MakeRequestUuid, SetRequestIdLayer},
    sensitive_headers::SetSensitiveRequestHeadersLayer,
    trace::TraceLayer,
};
use tracing::{Instrument, Span};

use crate::{AppState, B64};

const AUDIO_CACHE_HEADER: HeaderValue = HeaderValue::from_static("private, max-age=604800");
const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Deserialize)]
struct Auth {
    #[serde(default)]
    token: Option<String>,
}

fn extract_password_from_basic_auth(auth: &str) -> Result<String, AppError> {
    let decoded = B64.decode(auth.trim_start_matches("Basic "))?;
    let auth = String::from_utf8(decoded)?;
    Ok(auth.trim_start_matches("default:").to_string())
}

struct QueryDisplay<'a, 'b> {
    map: HashMap<Cow<'a, str>, Cow<'b, str>>,
}

impl<'a, 'b> Display for QueryDisplay<'a, 'b> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let length = self.map.len();
        for (index, (k, v)) in self.map.iter().enumerate() {
            write!(f, "{k}={v}")?;
            if index < length - 1 {
                write!(f, "&")?;
            }
        }
        Ok(())
    }
}

fn make_span_trace<B>(req: &Request<B>) -> Span {
    let query = req.uri().query();
    let mut query_map = query
        .and_then(|v| serde_qs::from_str::<HashMap<Cow<str>, Cow<str>>>(v).ok())
        .unwrap_or_else(HashMap::new);
    query_map.remove("token");

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
        let query_display = QueryDisplay { map: query_map };
        tracing::debug_span!(
            "request",
            path = %req.uri().path(),
            query = %query_display,
            id = %request_id,
        )
    }
}

pub(super) async fn handler(state: AppState) -> Result<(Router, Router), AppError> {
    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(make_span_trace)
        .on_request(|req: &Request<Body>, _span: &Span| {
            tracing::debug!(
                "started processing request {} on {:?}",
                req.method(),
                req.version(),
            )
        });

    let internal_router = Router::new()
        .route("/token/generate", get(generate_token))
        .route("/token/revoke_all", post(revoke_all_tokens))
        .with_state(state.clone());

    let router = Router::new()
        .route("/token/generate_for_music/:id", get(generate_scoped_token))
        .route("/thumbnail/:id", get(http))
        .route("/audio/external_id/:id", get(get_music))
        .route("/audio/scoped/:id", get(get_scoped_music))
        .route("/", get(metadata_ws))
        .layer(trace_layer)
        .layer(SetSensitiveRequestHeadersLayer::new([AUTHORIZATION]))
        .layer(
            CorsLayer::new()
                .allow_origin(tower_http::cors::Any)
                .allow_headers([CONTENT_TYPE, CACHE_CONTROL, REQUEST_ID])
                .allow_methods([Method::GET]),
        )
        .layer(SetRequestIdLayer::new(REQUEST_ID.clone(), MakeRequestUuid))
        .with_state(state);

    Ok((router, internal_router))
}

async fn revoke_all_tokens(State(app): State<AppState>) -> impl IntoResponse {
    app.tokens.revoke_all().await;
    tokio::spawn(async move {
        if let Err(err) = app.tokens.write(&app.tokens_path).await {
            tracing::error!("couldn't write tokens file: {err}");
        }
    });
    StatusCode::OK
}

async fn generate_token(State(app): State<AppState>) -> Result<axum::response::Response, AppError> {
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

async fn generate_scoped_token(
    State(app): State<AppState>,
    Query(query): Query<Auth>,
    Path(music_id): Path<String>,
) -> Result<axum::response::Response, AppError> {
    let maybe_token = query.token;

    'ok: {
        if let Some(token) = maybe_token {
            if app.tokens.verify(token).await? {
                tracing::debug!("verified token");
                break 'ok;
            }
        }
        tracing::debug!("invalid token");
        return Ok((
            StatusCode::UNAUTHORIZED,
            "Invalid token or token not present",
        )
            .into_response());
    }

    // generate token
    let token = app.scoped_tokens.generate_for_id(music_id).await;
    Ok(token.into_response())
}

async fn get_scoped_music(
    State(app): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response<Body>, AppError> {
    if let Some(music_id) = app.scoped_tokens.verify(token).await {
        let mut resp = app
            .client
            .request(
                Request::builder()
                    .uri(format!(
                        "http://{}:{}/audio/external_id/{}",
                        app.musikcubed_address, app.musikcubed_http_port, music_id
                    ))
                    .header(AUTHORIZATION, app.musikcubed_auth_header_value.clone())
                    .body(Body::empty())
                    .expect("cant fail"),
            )
            .await?;
        if resp.status().is_success() {
            // add cache header
            resp.headers_mut()
                .insert(CACHE_CONTROL, AUDIO_CACHE_HEADER.clone());
        }
        Ok(resp)
    } else {
        Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body("Invalid scoped token".to_string().into())
            .expect("cant fail"))
    }
}

async fn get_music(
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

async fn http(
    State(app): State<AppState>,
    Query(query): Query<Auth>,
    mut req: Request<Body>,
) -> Result<Response<Body>, AppError> {
    let path = req.uri().path();
    let path_query = req
        .uri()
        .path_and_query()
        .map(|v| v.as_str())
        .unwrap_or(path);

    *req.uri_mut() = format!(
        "http://{}:{}{}",
        app.musikcubed_address, app.musikcubed_http_port, path_query
    )
    .parse()?;

    let maybe_token = query.token.or_else(|| {
        req.headers()
            .get(AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|auth| extract_password_from_basic_auth(auth).ok())
    });

    'ok: {
        if let Some(token) = maybe_token {
            if app.tokens.verify(token).await? {
                tracing::debug!("verified token");
                break 'ok;
            }
        }
        tracing::debug!("invalid token");
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body("Invalid token or token not present".to_string().into())
            .expect("cant fail"));
    }

    req.headers_mut()
        .insert(AUTHORIZATION, app.musikcubed_auth_header_value.clone());

    Ok(app.client.request(req).await?)
}

async fn metadata_ws(
    State(app): State<AppState>,
    TypedHeader(user_agent): TypedHeader<UserAgent>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, AppError> {
    use async_tungstenite::tokio::connect_async;

    let uri = format!(
        "ws://{}:{}",
        app.musikcubed_address, app.musikcubed_metadata_port
    );
    let (ws_stream, _) = connect_async(uri).await?;

    let upgrade = ws
        .on_failed_upgrade({
            let user_agent = user_agent.clone();
            move |error| {
                let _entered =
                    tracing::info_span!("metadata ws http", client_user_agent = %user_agent)
                        .entered();
                tracing::error!("failed to upgrade to websocket for client: {error}");
            }
        })
        .on_upgrade(move |socket| {
            let span = tracing::info_span!(
                "metadata ws",
                client_user_agent = %user_agent
            );
            handle_metadata_socket(ws_stream, socket, app).instrument(span)
        });

    Ok(upgrade)
}

#[derive(Serialize, Deserialize)]
struct WsApiMessage {
    name: String,
    r#type: String,
    id: String,
    #[serde(default)]
    device_id: Option<String>,
    options: serde_json::value::Map<String, Value>,
}

async fn handle_metadata_socket(
    mut server_socket: WebSocketStream<TokioAdapter<TcpStream>>,
    mut client_socket: WebSocket,
    app: AppState,
) {
    // get token
    let (token, og_auth_msg) = 'ok: {
        'err: {
            if let Some(Ok(AxumMessage::Text(raw))) = client_socket.recv().await {
                let Ok(parsed) = serde_json::from_str::<WsApiMessage>(&raw) else {
                    tracing::error!("invalid auth message");
                    break 'err;
                };
                let Some(token) = parsed.options.get("password").and_then(|v| v.as_str()) else {
                    tracing::error!("token was not provided");
                    break 'err;
                };
                break 'ok (token.to_string(), parsed);
            } else {
                tracing::error!("did not receive auth message from client, closing socket");
                break 'err;
            }
        }
        let _ = client_socket.close().await;
        let _ = server_socket.close(None).await;
        return;
    };
    tracing::debug!("successfully extracted token from client request");

    // validate token
    'ok: {
        let verify_res = app
            .tokens
            .verify(&token)
            .await
            .map_err(|err| err.to_string());
        'err: {
            match verify_res {
                Ok(verified) => {
                    if !verified {
                        tracing::error!("invalid token");
                        break 'err;
                    }
                    break 'ok;
                }
                Err(err) => {
                    tracing::error!("internal server error while validating token: {err}");
                    break 'err;
                }
            }
        };
        let _ = client_socket.close().await;
        let _ = server_socket.close(None).await;
        return;
    }
    tracing::debug!("successfully validated token");

    let og_auth_reply = 'ok: {
        'err: {
            // send actual auth message to the musikcubed server
            let auth_msg = WsApiMessage {
                name: "authenticate".to_string(),
                r#type: "request".to_string(),
                id: og_auth_msg.id,
                device_id: og_auth_msg.device_id,
                options: {
                    let mut map = serde_json::Map::with_capacity(1);
                    map.insert(
                        "password".to_string(),
                        app.musikcubed_password.clone().into(),
                    );
                    map
                },
            };
            let auth_msg_ser = serde_json::to_string(&auth_msg).expect("");
            if let Err(err) = server_socket
                .send(TungsteniteMessage::Text(auth_msg_ser))
                .await
            {
                tracing::error!("failed to send auth message to musikcubed: {err}");
                break 'err;
            }
            // wait for auth reply
            if let Some(Ok(TungsteniteMessage::Text(raw))) = server_socket.next().await {
                let Ok(parsed) = serde_json::from_str::<WsApiMessage>(&raw) else {
                    tracing::error!("invalid auth response message: {raw}");
                    break 'err;
                };
                let is_authenticated = parsed
                    .options
                    .get("authenticated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                match is_authenticated {
                    true => break 'ok parsed,
                    false => break 'err,
                }
            }
        }
        let _ = client_socket.close().await;
        let _ = server_socket.close(None).await;
        return;
    };

    'ok: {
        'err: {
            // send actual auth message to the musikcubed server
            let auth_reply_msg = {
                let mut auth_reply_msg = og_auth_reply;
                let maybe_env_map = auth_reply_msg
                    .options
                    .get_mut("environment")
                    .and_then(Value::as_object_mut);
                if let Some(map) = maybe_env_map {
                    map.insert("http_server_port".to_string(), Value::from(app.public_port));
                }
                auth_reply_msg
            };
            let auth_reply_msg_ser = serde_json::to_string(&auth_reply_msg).expect("");
            tracing::debug!("sending auth reply message to client: {auth_reply_msg_ser}");
            match client_socket
                .send(AxumMessage::Text(auth_reply_msg_ser))
                .await
            {
                Ok(_) => {
                    tracing::debug!("successfully sent auth reply message to client");
                    break 'ok;
                }
                Err(err) => {
                    tracing::error!("error while sending auth reply message to client: {err}");
                    break 'err;
                }
            }
        }
        let _ = client_socket.close().await;
        let _ = server_socket.close(None).await;
        return;
    }
    tracing::info!("successfully authenticated");

    let (mut in_write, mut in_read) = client_socket.split();
    let (mut out_write, mut out_read) = server_socket.split();

    let in_read_fut = async move {
        while let Some(res) = in_read.next().await {
            match res {
                Ok(msg) => {
                    tracing::trace!("got message from client: {msg:?}");
                    let res = out_write.send(axum_msg_to_tungstenite(msg)).await;
                    if let Err(err) = res {
                        tracing::error!("could not write to server socket: {err}");
                        break;
                    }
                }
                Err(err) => {
                    tracing::error!("could not read from client socket: {err}");
                    break;
                }
            }
        }
        let _ = out_write.send(TungsteniteMessage::Close(None)).await;
    };
    let in_read_task = tokio::spawn(in_read_fut.instrument(Span::current()));

    let in_write_fut = async move {
        while let Some(res) = out_read.next().await {
            match res {
                Ok(msg) => {
                    tracing::trace!("got message from server: {msg:?}");
                    let res = in_write.send(tungstenite_msg_to_axum(msg)).await;
                    if let Err(err) = res {
                        tracing::error!("could not write to client socket: {err}");
                        break;
                    }
                }
                Err(err) => {
                    tracing::error!("could not read from server socket: {err}");
                    break;
                }
            }
        }
        let _ = in_write.send(AxumMessage::Close(None)).await;
    };
    let in_write_task = tokio::spawn(in_write_fut.instrument(Span::current()));

    let _ = tokio::join!(in_read_task, in_write_task);

    tracing::debug!("ending metadata ws task");
}

#[inline(always)]
fn tungstenite_msg_to_axum(msg: TungsteniteMessage) -> AxumMessage {
    match msg {
        TungsteniteMessage::Text(data) => AxumMessage::Text(data),
        TungsteniteMessage::Binary(data) => AxumMessage::Binary(data),
        TungsteniteMessage::Ping(data) => AxumMessage::Ping(data),
        TungsteniteMessage::Pong(data) => AxumMessage::Pong(data),
        TungsteniteMessage::Close(frame) => {
            AxumMessage::Close(frame.map(tungstenite_close_frame_to_axum))
        }
        TungsteniteMessage::Frame(_) => unreachable!("we don't use raw frames"),
    }
}

#[inline(always)]
fn axum_msg_to_tungstenite(msg: AxumMessage) -> TungsteniteMessage {
    match msg {
        AxumMessage::Text(data) => TungsteniteMessage::Text(data),
        AxumMessage::Binary(data) => TungsteniteMessage::Binary(data),
        AxumMessage::Ping(data) => TungsteniteMessage::Ping(data),
        AxumMessage::Pong(data) => TungsteniteMessage::Pong(data),
        AxumMessage::Close(frame) => {
            TungsteniteMessage::Close(frame.map(axum_close_frame_to_tungstenite))
        }
    }
}

#[inline(always)]
fn tungstenite_close_frame_to_axum(frame: TungsteniteCloseFrame) -> AxumCloseFrame {
    AxumCloseFrame {
        code: frame.code.into(),
        reason: frame.reason,
    }
}

#[inline(always)]
fn axum_close_frame_to_tungstenite(frame: AxumCloseFrame) -> TungsteniteCloseFrame {
    TungsteniteCloseFrame {
        code: frame.code.into(),
        reason: frame.reason,
    }
}
