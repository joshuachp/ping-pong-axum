use std::{convert::identity, net::IpAddr, ops::Deref, pin::pin, str::FromStr, sync::Arc};

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use axum_extra::{headers::ContentType, TypedHeader};
use cfg_if::cfg_if;
use clap::{builder::ValueParser, Parser};
use eyre::Context;
use tokio::{net::TcpListener, signal::unix::SignalKind, sync::watch, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;
use tracing::{debug, error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

const LOG_LEVEL: &str = "receiver=info,tower_http=debug";

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

#[derive(Debug, Clone)]
struct AppStateShared {
    ping_count_tx: watch::Sender<usize>,
    ping_count_rx: watch::Receiver<usize>,
}

impl AppStateShared {
    fn new() -> Self {
        let (ping_count_tx, ping_count_rx) = watch::channel(0);
        Self {
            ping_count_tx,
            ping_count_rx,
        }
    }
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

async fn favicon_ico() -> Result<(TypedHeader<ContentType>, &'static [u8]), AppError> {
    let header = TypedHeader(ContentType::from_str("image/x-icon")?);

    Ok((header, include_bytes!("../../assets/favicon.ico")))
}

async fn events(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    let rx = state.ping_count_rx.clone();

    ws.on_upgrade(|socket| events_callback(socket, rx))
}

async fn events_callback(mut socket: WebSocket, mut rx: watch::Receiver<usize>) {
    loop {
        let count = rx.borrow_and_update().clone();

        debug!(count, "sending count");

        if let Err(err) = socket.send(Message::Text(count.to_string())).await {
            error!(error = %eyre::Report::new(err), "ws socket errror");

            return;
        }

        if let Err(err) = rx.changed().await {
            error!(error = %eyre::Report::new(err), "rx errror");

            return;
        }
    }
}

fn frontend_app() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/favicon.ico", get(favicon_ico))
        .route("/events", get(events))
}

async fn frontend(
    address: IpAddr,
    port: u16,
    state: AppState,
    cancel: CancellationToken,
) -> eyre::Result<()> {
    let listener = TcpListener::bind((address, port)).await?;

    info!("listening on http://{}", listener.local_addr()?);

    let app = frontend_app()
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            cancel.cancelled().await;
        })
        .await?;

    Ok(())
}

async fn ping(State(state): State<AppState>) {
    state.ping_count_tx.send_modify(|count| {
        *count = count.saturating_add(1);
    })
}

fn ping_srv_app() -> Router<AppState> {
    Router::new().route("/", post(ping))
}

async fn ping_srv(
    address: IpAddr,
    port: u16,
    state: AppState,
    cancel: CancellationToken,
) -> eyre::Result<()> {
    let listener = TcpListener::bind((address, port)).await?;

    info!("listening on http://{}", listener.local_addr()?);

    let app = ping_srv_app()
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            cancel.cancelled().await;
        })
        .await?;

    Ok(())
}

#[derive(Debug, Clone, Parser)]
#[clap(name = env!("CARGO_PKG_NAME"), about, version)]
struct Cli {
    /// Address to listen on
    #[arg(long,default_value = "127.0.0.1", value_parser= ValueParser::new(IpAddr::from_str) )]
    address: IpAddr,
    /// Port to listen on
    #[arg(long, short, default_value = "9000")]
    port: u16,
    /// Port to listen on
    #[arg(long, default_value = "9001")]
    ping_port: u16,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let cli = Cli::parse();

    color_eyre::install()?;

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| LOG_LEVEL.into()))
        .try_init()?;

    let cancel = CancellationToken::new();

    let mut tasks = JoinSet::new();

    let app = AppState {
        shared: Arc::new(AppStateShared::new()),
    };

    tasks.spawn(frontend(cli.address, cli.port, app.clone(), cancel.clone()));
    tasks.spawn(ping_srv(cli.address, cli.ping_port, app, cancel.clone()));

    tasks.spawn(async move {
        shutdown_signal().await;

        cancel.cancel();

        Ok(())
    });

    while let Some(join) = tasks.join_next().await {
        join.wrap_err("failed to join task").and_then(identity)?;
    }

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
