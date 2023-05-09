use async_tungstenite::{
    tokio::TokioAdapter, tungstenite::Message as TungsteniteMessage, WebSocketStream,
};
use axum::{
    extract::{
        ws::{Message as AxumMessage, WebSocket},
        State, WebSocketUpgrade,
    },
    headers::UserAgent,
    response::IntoResponse,
    TypedHeader,
};
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tracing::{Instrument, Span};

use crate::{
    api::WsApiMessage,
    error::AppError,
    utils::{axum_msg_to_tungstenite, tungstenite_msg_to_axum, WsError},
    AppState,
};

pub(crate) async fn metadata_ws(
    State(app): State<AppState>,
    TypedHeader(user_agent): TypedHeader<UserAgent>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, AppError> {
    use async_tungstenite::tokio::connect_async;

    let uri = format!(
        "ws://{}:{}",
        app.musikcubed_address, app.musikcubed_metadata_port
    );
    tracing::debug!("proxying websocket request to {uri}");
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

pub(crate) async fn handle_metadata_socket(
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
            let auth_msg = WsApiMessage::authenticate(app.musikcubed_password.clone())
                .id(og_auth_msg.id)
                .device_id(og_auth_msg.device_id.unwrap_or_default());
            let auth_msg_ser = serde_json::to_string(&auth_msg).expect("");
            if let Err(err) = server_socket
                .send(TungsteniteMessage::Text(auth_msg_ser))
                .await
            {
                tracing::error!("failed to send auth message to musikcubed: {err}");
                break 'err;
            }
            // wait for auth reply
            match server_socket.next().await {
                Some(Ok(TungsteniteMessage::Text(raw))) => {
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
                        false => {
                            tracing::error!("unauthorized");
                            break 'err;
                        }
                    }
                }
                Some(Ok(TungsteniteMessage::Close(frame))) => {
                    let reason = frame
                        .map(|v| format!("{} (code {})", v.reason, v.code))
                        .unwrap_or_else(|| "no reason given".to_string());
                    tracing::error!("socket was closed by musikcubed: {}", reason);
                }
                _ => tracing::error!("unexpected message from musikcubed"),
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
            let res = res.map_err(WsError::from);
            match res {
                Ok(msg) => {
                    tracing::trace!("got message from client: {msg:?}");
                    let res = out_write
                        .send(axum_msg_to_tungstenite(msg))
                        .await
                        .map_err(WsError::from);
                    if let Err(err) = res {
                        match err {
                            WsError::Closed(reason) => {
                                tracing::error!("server socket was closed: {reason}");
                                break;
                            }
                            err => {
                                tracing::error!("could not write to server socket: {err}");
                            }
                        }
                    }
                }
                Err(err) => match err {
                    WsError::Closed(reason) => {
                        tracing::error!("client socket was closed, {reason}");
                        break;
                    }
                    err => {
                        tracing::error!("could not read from client socket: {err}");
                    }
                },
            }
        }
        let _ = out_write.send(TungsteniteMessage::Close(None)).await;
    };
    let in_read_task = tokio::spawn(in_read_fut.instrument(Span::current()));

    let in_write_fut = async move {
        while let Some(res) = out_read.next().await {
            let res = res.map_err(WsError::from);
            match res {
                Ok(msg) => {
                    tracing::trace!("got message from server: {msg:?}");
                    let res = in_write
                        .send(tungstenite_msg_to_axum(msg))
                        .await
                        .map_err(WsError::from);
                    if let Err(err) = res {
                        match err {
                            WsError::Closed(reason) => {
                                tracing::error!("client socket was closed, {reason}");
                                break;
                            }
                            err => {
                                tracing::error!("could not write to server socket: {err}");
                            }
                        }
                        break;
                    }
                }
                Err(err) => match err {
                    WsError::Closed(reason) => {
                        tracing::error!("server socket was closed, {reason}");
                        break;
                    }
                    err => {
                        tracing::error!("could not read from server socket: {err}");
                    }
                },
            }
        }
        let _ = in_write.send(AxumMessage::Close(None)).await;
    };
    let in_write_task = tokio::spawn(in_write_fut.instrument(Span::current()));

    let _ = tokio::join!(in_read_task, in_write_task);

    tracing::debug!("ending metadata ws task");
}
