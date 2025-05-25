use std::{
    collections::HashMap,
    convert::Infallible,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
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
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_stream::StreamExt as _;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let app_state = Arc::new(AppState::default());
    let app = Router::new()
        .route("/", get(homepage))
        .route("/channel", get(new_channel))
        .route(
            "/channel/{channel_id}",
            get(sse_channel).post(webhook_channel),
        )
        .with_state(Arc::clone(&app_state));

    tokio::task::spawn(cleanup_pending_channels(app_state));

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
    pending_channels: DashMap<ChannelId, PendingChannel>,
    active_channels: DashMap<ChannelId, Sender<Event>>,
    webhook_count: AtomicU64,
}

struct PendingChannel {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    create_time: Instant,
}

type ChannelId = uuid::Uuid;

async fn homepage(State(state): State<Arc<AppState>>) -> Html<String> {
    let version = env!("CARGO_PKG_VERSION");
    let pending_channel_count = state.pending_channels.len();
    let active_channel_count = state.active_channels.len();
    let webhook_count = state.webhook_count.load(Ordering::Relaxed);
    Html(format!(
        "
        <h1>Webhook Forwarder Server {version}</h1>
        <p>There are {pending_channel_count} pending channels</p>
        <p>There are {active_channel_count} active channels</p>
        <p>There have been {webhook_count} forwarded webhooks</p>
        ",
    ))
}

async fn new_channel(State(state): State<Arc<AppState>>) -> Redirect {
    let channel_id = uuid::Uuid::new_v4();
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let channel = PendingChannel {
        tx,
        rx,
        create_time: Instant::now(),
    };
    state.pending_channels.insert(channel_id, channel);
    tracing::info!("created channel {channel_id}");
    Redirect::to(&format!("/channel/{channel_id}"))
}

async fn sse_channel(
    Path(channel_id): Path<ChannelId>,
    State(state): State<Arc<AppState>>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    let Some((_, channel)) = state.pending_channels.remove(&channel_id) else {
        return Err(StatusCode::NOT_FOUND);
    };
    let PendingChannel {
        tx,
        rx,
        create_time: _,
    } = channel;
    state.active_channels.insert(channel_id, tx);
    let stream = ReceivingChannel {
        channel_id,
        rx,
        state,
    };
    Ok(Sse::new(stream.map(Ok)).keep_alive(KeepAlive::default()))
}

struct ReceivingChannel {
    channel_id: ChannelId,
    rx: Receiver<Event>,
    state: Arc<AppState>,
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
        self.state.active_channels.remove(&self.channel_id);
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
            if let Err(err) = tx.send(event).await {
                tracing::error!("channel closed {err}");
                return StatusCode::NOT_FOUND;
            }
            state.webhook_count.fetch_add(1, Ordering::Relaxed);
            StatusCode::OK
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

async fn cleanup_pending_channels(app_state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(300));
    loop {
        interval.tick().await;
        app_state.pending_channels.retain(|channel_id, channel| {
            if channel.create_time.elapsed() > Duration::from_secs(60) {
                tracing::info!("cleanup pending channel {channel_id}");
                false
            } else {
                true
            }
        });
    }
}
