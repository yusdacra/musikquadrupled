use std::sync::Arc;

use base64::Engine;
use futures::{SinkExt, StreamExt};
use hyper::{client::HttpConnector, Body};
use scc::HashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    api::WsApiMessage,
    error::AppError,
    token::{MusicScopedTokens, Tokens},
    utils::{get_conf, HandleWsItem, WsError, B64},
};

type Client = hyper::Client<HttpConnector, Body>;

pub(crate) type AppState = Arc<AppStateInternal>;

#[derive(Clone)]
pub(crate) struct AppStateInternal {
    pub(crate) client: Client,
    pub(crate) tokens: Tokens,
    pub(crate) music_info: MusicInfoMap,
    pub(crate) scoped_tokens: MusicScopedTokens,
    pub(crate) tokens_path: String,
    pub(crate) public_port: u16,
    pub(crate) musikcubed_address: String,
    pub(crate) musikcubed_http_port: u16,
    pub(crate) musikcubed_metadata_port: u16,
    pub(crate) musikcubed_password: String,
    pub(crate) musikcubed_auth_header_value: http::HeaderValue,
}

impl AppStateInternal {
    pub(crate) async fn new(public_port: u16) -> Result<Self, AppError> {
        let musikcubed_http_port = get_conf("MUSIKCUBED_HTTP_PORT")?.parse()?;
        let musikcubed_metadata_port = get_conf("MUSIKCUBED_METADATA_PORT")?.parse()?;
        let musikcubed_address = get_conf("MUSIKCUBED_ADDRESS")?;
        let musikcubed_password = get_conf("MUSIKCUBED_PASSWORD")?;
        let musikcubed_auth_header_value = {
            let mut val: http::HeaderValue = format!(
                "Basic {}",
                B64.encode(format!("default:{}", musikcubed_password))
            )
            .parse()
            .expect("valid header value");
            val.set_sensitive(true);
            val
        };

        let tokens_path = get_conf("TOKENS_FILE")?;

        let music_info = MusicInfoMap::new();
        music_info
            .read(
                musikcubed_password.clone(),
                &musikcubed_address,
                musikcubed_metadata_port,
            )
            .await?;

        let this = Self {
            client: Client::new(),
            tokens: Tokens::read(&tokens_path).await?,
            scoped_tokens: MusicScopedTokens::new(get_conf("SCOPED_EXPIRY_DURATION")?.parse()?),
            musikcubed_address,
            musikcubed_http_port,
            musikcubed_metadata_port,
            musikcubed_auth_header_value,
            musikcubed_password,
            tokens_path,
            public_port,
            music_info,
        };
        Ok(this)
    }

    pub(crate) async fn verify_scoped_token(
        &self,
        token: impl AsRef<str>,
    ) -> Result<String, AppError> {
        self.scoped_tokens.verify(token).await.ok_or_else(|| {
            AppError::from("Invalid token or not authorized").status(http::StatusCode::UNAUTHORIZED)
        })
    }

    pub(crate) async fn verify_token(
        &self,
        maybe_token: Option<impl AsRef<str>>,
    ) -> Result<(), AppError> {
        if let Some(token) = maybe_token {
            if self.tokens.verify(token).await? {
                tracing::debug!("verified token");
                return Ok(());
            }
        }
        tracing::debug!("invalid token");
        Err(AppError::from("Invalid token or token not present")
            .status(http::StatusCode::UNAUTHORIZED))
    }

    pub(crate) async fn make_musikcubed_request(
        &self,
        path: impl AsRef<str>,
        mut req: http::Request<hyper::Body>,
    ) -> Result<http::Response<hyper::Body>, AppError> {
        *req.uri_mut() = format!(
            "http://{}:{}{}",
            self.musikcubed_address,
            self.musikcubed_http_port,
            path.as_ref()
        )
        .parse()?;
        req.headers_mut().insert(
            http::header::AUTHORIZATION,
            self.musikcubed_auth_header_value.clone(),
        );
        let resp = self.client.request(req).await?;
        Ok(resp)
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct MusicInfo {
    pub(crate) external_id: String,
    pub(crate) title: String,
    pub(crate) album: String,
    pub(crate) artist: String,
    pub(crate) thumbnail_id: u32,
}

#[derive(Clone)]
pub(crate) struct MusicInfoMap {
    map: HashMap<String, MusicInfo>,
}

impl MusicInfoMap {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub(crate) async fn get(&self, id: impl AsRef<str>) -> Option<MusicInfo> {
        self.map.read_async(id.as_ref(), |_, v| v.clone()).await
    }

    async fn read(
        &self,
        password: impl Into<String>,
        address: impl AsRef<str>,
        port: u16,
    ) -> Result<(), AppError> {
        use async_tungstenite::tungstenite::Message;

        let uri = format!("ws://{}:{}", address.as_ref(), port);
        let (mut ws_stream, _) = async_tungstenite::tokio::connect_async(uri)
            .await
            .map_err(WsError::from)?;

        let device_id = "musikquadrupled";

        // do the authentication
        let auth_msg = WsApiMessage::authenticate(password.into())
            .id("auth")
            .device_id(device_id);
        ws_stream
            .send(Message::Text(auth_msg.to_string()))
            .await
            .map_err(WsError::from)?;
        let auth_reply: WsApiMessage = ws_stream.next().await.handle_item()?;
        let is_authenticated = auth_reply
            .options
            .get("authenticated")
            .and_then(Value::as_bool)
            .unwrap_or_default();
        if !is_authenticated {
            return Err("not authenticated".into());
        }

        // fetch the tracks
        let fetch_tracks_msg = WsApiMessage::request("query_tracks")
            .device_id(device_id)
            .id("fetch_tracks")
            .option("limit", u32::MAX)
            .option("offset", 0);
        ws_stream
            .send(Message::Text(fetch_tracks_msg.to_string()))
            .await
            .map_err(WsError::from)?;
        let mut tracks_reply: WsApiMessage = ws_stream.next().await.handle_item()?;
        let Some(Value::Array(tracks)) = tracks_reply.options.remove("data") else {
            tracing::debug!("reply: {tracks_reply:#?}");
            return Err("must have tracks".into());
        };
        for track in tracks {
            let info: MusicInfo = serde_json::from_value(track).unwrap();
            let _ = self.map.insert_async(info.external_id.clone(), info).await;
        }

        ws_stream.close(None).await.map_err(WsError::from)?;

        Ok(())
    }
}
