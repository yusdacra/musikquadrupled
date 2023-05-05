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

    let addr: SocketAddr = get_conf("ADDRESS")?.parse()?;

    let state = AppState::new(AppStateInternal::new(addr.port()).await?);
    let router = handler::handler(state).await?;
    let make_service = router.into_make_service_with_connect_info::<SocketAddr>();

    let (task, scheme) = if let (Some(cert_path), Some(key_path)) = (cert_path, key_path) {
        info!("cert path is: {cert_path}");
        info!("key path is: {key_path}");
        let config = RustlsConfig::from_pem_file(cert_path, key_path).await?;
        let task = tokio::spawn(axum_server::bind_rustls(addr, config).serve(make_service));
        (task, "https")
    } else if get_conf("INSECURE").ok().is_some() {
        tracing::warn!("RUNNING IN INSECURE MODE (NO TLS)");
        let task = tokio::spawn(axum_server::bind(addr).serve(make_service));
        (task, "http")
    } else {
        tracing::warn!("note: either one or both of MUSIKQUAD_TLS_CERT_PATH and MUSIKQUAD_TLS_KEY_PATH has not been set");
        return Err("will not serve HTTP unless the MUSIKQUAD_INSECURE env var is set".into());
    };

    info!("listening on {scheme}://{addr}");
    task.await??;

    Ok(())
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
            musikcubed_auth_header_value: format!(
                "Basic {}",
                B64.encode(format!("default:{}", musikcubed_password))
            )
            .parse()
            .expect("valid header value"),
            musikcubed_password,
            client: Client::new(),
            tokens: Tokens::read(&tokens_path).await?,
            scoped_tokens: MusicScopedTokens::new(get_conf("SCOPED_EXPIRY_DURATION")?.parse()?),
            tokens_path,
        };
        Ok(this)
    }
}
