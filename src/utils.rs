use std::{borrow::Cow, collections::HashMap, error::Error, fmt::Display};

use async_tungstenite::tungstenite::{
    protocol::CloseFrame as TungsteniteCloseFrame, Error as TungsteniteError,
    Message as TungsteniteMessage,
};
use axum::{
    extract::ws::{CloseFrame as AxumCloseFrame, Message as AxumMessage},
    Error as AxumError,
};
use base64::Engine;

use crate::{error::AppError, B64};

#[derive(Debug)]
pub(crate) enum WsError {
    Closed(Cow<'static, str>),
    InvalidMessage(serde_json::Error),
    Other(Box<dyn Error + Send>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
}

impl WsError {
    pub(crate) fn is_closed(&self) -> bool {
        match self {
            WsError::Closed(_) => true,
            _ => false,
        }
    }
}

impl Display for WsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed(reason) => write!(f, "ws closed, reason: {reason}"),
            Self::InvalidMessage(err) => write!(f, "invalid JSON message, {err}"),
            Self::Other(err) => write!(f, "error: {err}"),
            Self::Ping(_) => write!(f, "was a ping"),
            Self::Pong(_) => write!(f, "was a pong"),
        }
    }
}

impl Error for WsError {}

impl From<TungsteniteError> for WsError {
    fn from(err: TungsteniteError) -> Self {
        match err {
            TungsteniteError::ConnectionClosed => WsError::Closed("was closed".into()),
            TungsteniteError::AlreadyClosed => WsError::Closed("was already closed".into()),
            err => WsError::Other(Box::new(err)),
        }
    }
}

impl From<AxumError> for WsError {
    fn from(err: AxumError) -> Self {
        use axum_tungstenite::Error as AxumError;

        let err = match err.into_inner().downcast::<AxumError>() {
            Ok(err) => *err,
            Err(err) => return WsError::Other(err),
        };

        match err {
            AxumError::ConnectionClosed => WsError::Closed("was closed".into()),
            AxumError::AlreadyClosed => WsError::Closed("was already closed".into()),
            err => WsError::Other(Box::new(err)),
        }
    }
}

pub(crate) trait HandleWsItem {
    fn handle_item<T: serde::de::DeserializeOwned>(self) -> Result<T, WsError>;
}

impl HandleWsItem for Option<Result<TungsteniteMessage, TungsteniteError>> {
    fn handle_item<T: serde::de::DeserializeOwned>(self) -> Result<T, WsError> {
        let Some(result) = self else {
            return Err(WsError::Closed("was already closed".into()));
        };

        match result? {
            TungsteniteMessage::Binary(data) => {
                serde_json::from_slice(&data).map_err(WsError::InvalidMessage)
            }
            TungsteniteMessage::Text(data) => {
                serde_json::from_str(&data).map_err(WsError::InvalidMessage)
            }
            TungsteniteMessage::Close(frame) => Err(WsError::Closed(
                frame
                    .map(|f| format!("was closed, reason {} (code {})", f.reason, f.code).into())
                    .unwrap_or_else(|| "was closed".into()),
            )),
            TungsteniteMessage::Ping(data) => Err(WsError::Ping(data)),
            TungsteniteMessage::Pong(data) => Err(WsError::Pong(data)),
            TungsteniteMessage::Frame(_) => unreachable!("we don't read raw frames"),
        }
    }
}

impl HandleWsItem for Result<TungsteniteMessage, TungsteniteError> {
    fn handle_item<T: serde::de::DeserializeOwned>(self) -> Result<T, WsError> {
        Some(self).handle_item()
    }
}

impl HandleWsItem for Option<Result<AxumMessage, AxumError>> {
    fn handle_item<T: serde::de::DeserializeOwned>(self) -> Result<T, WsError> {
        let Some(result) = self else {
            return Err(WsError::Closed("was already closed".into()));
        };

        match result? {
            AxumMessage::Binary(data) => {
                serde_json::from_slice(&data).map_err(WsError::InvalidMessage)
            }
            AxumMessage::Text(data) => serde_json::from_str(&data).map_err(WsError::InvalidMessage),
            AxumMessage::Close(frame) => Err(WsError::Closed(
                frame
                    .map(|f| format!("was closed, reason {} (code {})", f.reason, f.code).into())
                    .unwrap_or_else(|| "was closed".into()),
            )),
            AxumMessage::Ping(data) => Err(WsError::Ping(data)),
            AxumMessage::Pong(data) => Err(WsError::Pong(data)),
        }
    }
}

impl HandleWsItem for Result<AxumMessage, AxumError> {
    fn handle_item<T: serde::de::DeserializeOwned>(self) -> Result<T, WsError> {
        Some(self).handle_item()
    }
}

#[inline(always)]
pub(crate) fn tungstenite_msg_to_axum(msg: TungsteniteMessage) -> AxumMessage {
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
pub(crate) fn axum_msg_to_tungstenite(msg: AxumMessage) -> TungsteniteMessage {
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

pub(crate) struct QueryDisplay {
    map: HashMap<String, String>,
}

impl QueryDisplay {
    pub(crate) fn new(map: HashMap<String, String>) -> Self {
        Self { map }
    }
}

impl Display for QueryDisplay {
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

pub(crate) fn extract_password_from_basic_auth(auth: &str) -> Result<String, AppError> {
    let decoded = B64.decode(auth.trim_start_matches("Basic "))?;
    let auth = String::from_utf8(decoded)?;
    Ok(auth.trim_start_matches("default:").to_string())
}

pub(crate) fn remove_token_from_query(query: Option<&str>) -> HashMap<String, String> {
    let mut query_map: HashMap<String, String> = query
        .and_then(|v| serde_qs::from_str(v).ok())
        .unwrap_or_else(HashMap::new);
    query_map.remove("token");
    query_map
}
