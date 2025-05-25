use std::{
    collections::HashMap,
    convert::Infallible,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::Context as _;
use axum::{
    Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        Html, Redirect, Sse,
        sse::{Event, KeepAlive},
    },
    routing::get,
};
use dashmap::DashMap;
use futures_util::Stream;
use tokio::sync::mpsc::{Receiver, Sender, error::TrySendError};
use tokio_stream::StreamExt as _;
use tracing::Level;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if cfg!(feature = "stackdriver") {
        let filter = tracing_subscriber::filter::Targets::new().with_default(Level::INFO);
        tracing_subscriber::registry()
            .with(tracing_stackdriver::layer())
            .with(filter)
            .init();
    } else {
        tracing_subscriber::fmt::init();
    }
    let app_state = Arc::new(AppState::default());
    let app = Router::new()
        .route("/", get(homepage))
        .route("/channel", get(new_channel))
        .route(
            "/channel/{channel_id}",
            get(sse_channel).post(webhook_channel),
        )
        .with_state(Arc::clone(&app_state));

    tokio::task::spawn(cleanup_closed_channels(app_state));

    let port = std::env::var("PORT").unwrap_or_else(|_| String::from("3000"));
    let port: u16 = port.parse().context("parse port")?;
    tracing::info!("Listening on port {port}");
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context("could not bind address")?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("received stop signal");
}

#[derive(Default)]
struct AppState {
    active_channels: DashMap<ChannelId, Sender<Event>>,
    webhook_count: AtomicU64,
}

type ChannelId = uuid::Uuid;

async fn homepage(State(state): State<Arc<AppState>>) -> Html<String> {
    let version = env!("CARGO_PKG_VERSION");
    let active_channel_count = state.active_channels.len();
    let webhook_count = state.webhook_count.load(Ordering::Relaxed);
    Html(format!(
        "
        <h1>Webhook Forwarder Server {version}</h1>
        <p>There are {active_channel_count} active channels</p>
        <p>There have been {webhook_count} forwarded webhooks</p>
        ",
    ))
}

async fn new_channel() -> Redirect {
    let channel_id = uuid::Uuid::new_v4();
    Redirect::to(&format!("/channel/{channel_id}"))
}

async fn sse_channel(
    Path(channel_id): Path<ChannelId>,
    State(state): State<Arc<AppState>>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    tracing::info!("channel {channel_id} started");
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    if let Some(old) = state.active_channels.insert(channel_id, tx) {
        tracing::warn!("replaced old channel");
        drop(old);
    }
    let stream = ReceivingChannel { channel_id, rx };
    Ok(Sse::new(stream.map(Ok)).keep_alive(KeepAlive::new()))
}

struct ReceivingChannel {
    channel_id: ChannelId,
    rx: Receiver<Event>,
}

impl Stream for ReceivingChannel {
    type Item = Event;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

impl Drop for ReceivingChannel {
    fn drop(&mut self) {
        tracing::info!("channel {} stopped", self.channel_id);
    }
}

async fn webhook_channel(
    headers: HeaderMap,
    Path(channel_id): Path<ChannelId>,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> StatusCode {
    let Some(tx) = state.active_channels.get(&channel_id) else {
        return StatusCode::NOT_FOUND;
    };
    let headers = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_bytes()))
        .collect();
    match Event::default().json_data(WebhookEvent {
        headers,
        body: &body,
    }) {
        Ok(event) => {
            let res = tx.try_send(event);
            match res {
                Ok(()) => {
                    state.webhook_count.fetch_add(1, Ordering::Relaxed);
                    StatusCode::OK
                }
                Err(TrySendError::Full(_)) => {
                    tracing::error!("channel full");
                    StatusCode::INTERNAL_SERVER_ERROR
                }
                Err(TrySendError::Closed(_)) => {
                    state.active_channels.remove(&channel_id);
                    tracing::error!("channel closed");
                    StatusCode::NOT_FOUND
                }
            }
        }
        Err(err) => {
            tracing::error!("event serialize json data: {err}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[derive(serde::Serialize)]
struct WebhookEvent<'a> {
    headers: HashMap<&'a str, &'a [u8]>,
    body: &'a [u8],
}

async fn cleanup_closed_channels(app_state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    // First tick completes immediately so lets skip it
    interval.tick().await;
    loop {
        interval.tick().await;
        app_state.active_channels.retain(|_, tx| !tx.is_closed());
    }
}
