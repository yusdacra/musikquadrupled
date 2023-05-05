use std::{net::SocketAddr, process::ExitCode, sync::Arc};

use axum_server::tls_rustls::RustlsConfig;
use base64::Engine;
use dotenvy::Error as DotenvError;
use error::AppError;
use hyper::{client::HttpConnector, Body};
use token::{MusicScopedTokens, Tokens};
use tracing::{info, warn};
use tracing_subscriber::prelude::*;

mod error;
mod handler;
mod token;

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
        let musikcubed_password = get_conf("MUSIKCUBED_PASSWORD")?;
        let tokens_path = get_conf("TOKENS_FILE")?;
        let this = Self {
            public_port,
            musikcubed_address: get_conf("MUSIKCUBED_ADDRESS")?,
            musikcubed_http_port: get_conf("MUSIKCUBED_HTTP_PORT")?.parse()?,
            musikcubed_metadata_port: get_conf("MUSIKCUBED_METADATA_PORT")?.parse()?,
            musikcubed_auth_header_value: {
                let mut val: http::HeaderValue = format!(
                    "Basic {}",
                    B64.encode(format!("default:{}", musikcubed_password))
                )
                .parse()
                .expect("valid header value");
                val.set_sensitive(true);
                val
            },
            musikcubed_password,
            client: Client::new(),
            tokens: Tokens::read(&tokens_path).await?,
            scoped_tokens: MusicScopedTokens::new(get_conf("SCOPED_EXPIRY_DURATION")?.parse()?),
            tokens_path,
        };
        Ok(this)
    }
}
