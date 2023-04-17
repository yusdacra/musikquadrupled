use axum::{routing::get, Router};
use axum_server::tls_rustls::RustlsConfig;
use dotenvy::Error as DotenvError;

#[tokio::main]
async fn main() {
    if let Err(DotenvError::Io(err)) = dotenvy::dotenv() {}

    let app = Router::new().route("/", get(|| async { "Hello world" }));
    let config = RustlsConfig::from_pem_file(cert, key).await.unwrap();
}
