use std::{net::SocketAddr, sync::Arc};

use dotenvy::Error as DotenvError;
use error::AppError;
use hyper::{client::HttpConnector, Body};
use token::Tokens;
use tracing::{error, info, warn};
use tracing_subscriber::prelude::*;

mod error;
mod handler;
mod token;

#[tokio::main]
async fn main() {
    app().await.unwrap();
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

    // let cert_path = get_conf("TLS_CERT_PATH");
    // let key_path = get_conf("TLS_KEY_PATH");
    // info!("cert path is: {cert_path}");
    // info!("key path is: {key_path}");
    // let config = RustlsConfig::from_pem_file(cert_path, key_path)
    //     .await
    //     .unwrap();

    let public_addr: SocketAddr = get_conf("ADDRESS")?.parse()?;
    let local_addr: SocketAddr = get_conf("LOCAL_ADDRESS")?.parse()?;

    let state = AppState::new(AppStateInternal::new(public_addr.port()).await?);
    let public = handler::public(state.clone()).await?;
    let internal = handler::internal(state).await;

    info!("listening on {public_addr} for public APIs, on {local_addr} for internal API use");
    // axum_server::bind_rustls(addr, config)
    let public = axum_server::bind(public_addr)
        .serve(public.into_make_service_with_connect_info::<SocketAddr>());
    let internal = axum_server::bind(local_addr).serve(internal.into_make_service());

    tokio::try_join!(public, internal)
        .map(|_| ())
        .map_err(Into::into)
}

fn get_conf(key: &str) -> Result<String, AppError> {
    const ENV_NAMESPACE: &str = "MUSIKQUAD";

    let key = format!("{ENV_NAMESPACE}_{key}");
    match std::env::var(&key) {
        Ok(val) => return Ok(val),
        Err(err) => {
            use std::env::VarError;
            match err {
                VarError::NotPresent => {
                    error!("Config option {key} was not set but is required");
                }
                VarError::NotUnicode(_) => {
                    error!("Config option {key} was not unicode");
                }
            }
            return Err(err.into());
        }
    }
}

type Client = hyper::Client<HttpConnector, Body>;

type AppState = Arc<AppStateInternal>;

#[derive(Clone)]
struct AppStateInternal {
    client: Client,
    tokens: Tokens,
    tokens_path: String,
    public_port: u16,
    musikcubed_address: String,
    musikcubed_http_port: u16,
    musikcubed_metadata_port: u16,
    musikcubed_password: String,
}

impl AppStateInternal {
    async fn new(public_port: u16) -> Result<Self, AppError> {
        let tokens_path = get_conf("TOKENS_FILE")?;
        let this = Self {
            public_port,
            musikcubed_address: get_conf("MUSIKCUBED_ADDRESS")?,
            musikcubed_http_port: get_conf("MUSIKCUBED_HTTP_PORT")?.parse()?,
            musikcubed_metadata_port: get_conf("MUSIKCUBED_METADATA_PORT")?.parse()?,
            musikcubed_password: get_conf("MUSIKCUBED_PASSWORD")?,
            client: Client::new(),
            tokens: Tokens::read(&tokens_path).await?,
            tokens_path,
        };
        Ok(this)
    }
}
