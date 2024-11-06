use std::{collections::HashMap, net::IpAddr, ops::Deref, pin::pin, str::FromStr, sync::Arc};

use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Router,
};
use axum_extra::{headers::ContentType, TypedHeader};
use cfg_if::cfg_if;
use clap::{builder::ValueParser, Parser};
use reqwest::Url;
use tokio::{net::TcpListener, signal::unix::SignalKind};
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use uuid::Uuid;

const LOG_LEVEL: &str = "sender=info,tower_http=debug";

#[derive(Debug, Clone)]
struct AppState {
    shared: Arc<AppStateShared>,
}

impl Deref for AppState {
    type Target = AppStateShared;

    fn deref(&self) -> &Self::Target {
        &self.shared
    }
}

#[derive(Debug)]
struct AppStateShared {
    receiver: Url,
}

#[derive(Debug)]
enum AppError {
    Internal(eyre::Report),
}

impl<E> From<E> for AppError
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn from(value: E) -> Self {
        AppError::Internal(eyre::Report::new(value))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        match self {
            AppError::Internal(err) => {
                error!(error = %err, "insternal server error");

                (StatusCode::INTERNAL_SERVER_ERROR, "something whent wrong").into_response()
            }
        }
    }
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../templates/index.html"))
}

async fn send_ping(State(state): State<AppState>) -> Result<StatusCode, AppError> {
    let client = reqwest::Client::new();

    let mut body = HashMap::with_capacity(1);
    body.insert("id", Uuid::new_v4());

    client
        .post(state.receiver.clone())
        .json(&body)
        .send()
        .await?
        .error_for_status()?;

    Ok(StatusCode::NO_CONTENT)
}

async fn favicon_ico() -> Result<(TypedHeader<ContentType>, &'static [u8]), AppError> {
    let header = TypedHeader(ContentType::from_str("image/x-icon")?);

    Ok((header, include_bytes!("../../assets/favicon.ico")))
}

fn app() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/favicon.ico", get(favicon_ico))
        .route("/send-ping", post(send_ping))
}

#[derive(Debug, Clone, Parser)]
#[clap(name = env!("CARGO_PKG_NAME"), about, version)]
struct Cli {
    /// Address to listen on
    #[arg(default_value = "127.0.0.1", value_parser= ValueParser::new(IpAddr::from_str) )]
    address: IpAddr,
    /// Port to listen on
    #[arg(default_value = "9000")]
    port: u16,
    /// Url of the receiver internal port
    #[arg(default_value = "http://receiver:9000")]
    receiver: Url,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let cli = Cli::parse();

    color_eyre::install()?;

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| LOG_LEVEL.into()))
        .try_init()?;

    let listener = TcpListener::bind((cli.address, cli.port)).await?;

    info!("listening on http://{}", listener.local_addr()?);

    let app = app()
        .layer(TraceLayer::new_for_http())
        .with_state(AppState {
            shared: Arc::new(AppStateShared {
                receiver: cli.receiver,
            }),
        });

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    async fn sigint() {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                info!("SIGINT received");
            }
            Err(err) => {
                error!(error = %eyre::Report::new(err), "couldn't wait from signal");
            }
        }
    }

    cfg_if! {
        if #[cfg(target_family = "unix")] {
            let mut sigterm = match tokio::signal::unix::signal(SignalKind::terminate()) {
                Ok(term) => term,
                Err(err) => {
                    error!(error = %eyre::Report::new(err), "couldn't wait from SIGTERM");

                    // Wait only SIGINT
                    sigint().await;

                    return;
                },
            };

            let sigterm = pin!(sigterm.recv());
            let sigint = pin!(sigint());

            if let futures::future::Either::Left(_) = futures::future::select(sigterm, sigint).await {
                info!("SIGTERM receved");
            }
        } else {
           sigint().await;
        }
    }
}
