use std::{net::SocketAddr, process::ExitCode, sync::Arc};

use axum_server::tls_rustls::RustlsConfig;
use base64::Engine;
use dotenvy::Error as DotenvError;
use error::AppError;
use futures::{SinkExt, StreamExt};
use hyper::{client::HttpConnector, Body};
use scc::HashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use token::{MusicScopedTokens, Tokens};
use tracing::{info, warn};
use tracing_subscriber::prelude::*;

use crate::{
    api::WsApiMessage,
    utils::{HandleWsItem, WsError},
};

mod api;
mod error;
mod handler;
mod token;
mod utils;

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(err) = app().await {
        tracing::error!("aborting: {err}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

async fn app() -> Result<(), AppError> {
    // init tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "musikquadrupled=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // load config
    match dotenvy::dotenv() {
        Err(DotenvError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            warn!(".env file not found");
        }
        Ok(_) => info!(".env file loaded"),
        Err(err) => return Err(err.into()),
    }

    let cert_path = get_conf("TLS_CERT_PATH").ok();
    let key_path = get_conf("TLS_KEY_PATH").ok();

    let public_port: u16 = get_conf("PORT")?.parse()?;
    let internal_port: u16 = get_conf("INTERNAL_PORT")?.parse()?;

    let state = AppState::new(AppStateInternal::new(public_port).await?);
    let (public_router, internal_router) = handler::handler(state).await?;

    let internal_make_service = internal_router.into_make_service();
    let internal_task = tokio::spawn(
        axum_server::bind(SocketAddr::from(([127, 0, 0, 1], internal_port)))
            .serve(internal_make_service),
    );

    let public_addr = SocketAddr::from(([127, 0, 0, 1], public_port));
    let public_make_service = public_router.into_make_service();
    let (pub_task, scheme) = if let (Some(cert_path), Some(key_path)) = (cert_path, key_path) {
        info!("cert path is: {cert_path}");
        info!("key path is: {key_path}");
        let config = RustlsConfig::from_pem_file(cert_path, key_path).await?;
        let task =
            tokio::spawn(axum_server::bind_rustls(public_addr, config).serve(public_make_service));
        (task, "https")
    } else if get_conf("INSECURE").ok().is_some() {
        tracing::warn!("RUNNING IN INSECURE MODE (NO TLS)");
        let task = tokio::spawn(axum_server::bind(public_addr).serve(public_make_service));
        (task, "http")
    } else {
        tracing::warn!("note: either one or both of MUSIKQUAD_TLS_CERT_PATH and MUSIKQUAD_TLS_KEY_PATH has not been set");
        return Err("will not serve HTTP unless the MUSIKQUAD_INSECURE env var is set".into());
    };

    info!("listening on {scheme}://{public_addr}");

    let res = tokio::select! {
        res = internal_task => res,
        res = pub_task => res,
    };
    res.map_err(AppError::from)
        .map(|res| res.map_err(AppError::from))
        .and_then(std::convert::identity)
}

fn get_conf(key: &str) -> Result<String, AppError> {
    const ENV_NAMESPACE: &str = "MUSIKQUAD";

    let key = format!("{ENV_NAMESPACE}_{key}");
    std::env::var(&key).map_err(Into::into)
}

type Client = hyper::Client<HttpConnector, Body>;

type AppState = Arc<AppStateInternal>;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Clone)]
struct AppStateInternal {
    client: Client,
    tokens: Tokens,
    music_info: MusicInfoMap,
    scoped_tokens: MusicScopedTokens,
    tokens_path: String,
    public_port: u16,
    musikcubed_address: String,
    musikcubed_http_port: u16,
    musikcubed_metadata_port: u16,
    musikcubed_password: String,
    musikcubed_auth_header_value: http::HeaderValue,
}

impl AppStateInternal {
    async fn new(public_port: u16) -> Result<Self, AppError> {
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

    async fn verify_scoped_token(&self, token: impl AsRef<str>) -> Result<String, AppError> {
        self.scoped_tokens.verify(token).await.ok_or_else(|| {
            AppError::from("Invalid token or not authorized").status(http::StatusCode::UNAUTHORIZED)
        })
    }

    async fn verify_token(&self, maybe_token: Option<impl AsRef<str>>) -> Result<(), AppError> {
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
}

#[derive(Clone, Deserialize, Serialize)]
struct MusicInfo {
    external_id: String,
    title: String,
    album: String,
    artist: String,
    thumbnail_id: u32,
}

#[derive(Clone)]
struct MusicInfoMap {
    map: HashMap<String, MusicInfo>,
}

impl MusicInfoMap {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    async fn get(&self, id: impl AsRef<str>) -> Option<MusicInfo> {
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
