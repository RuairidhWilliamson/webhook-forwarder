use std::{collections::HashMap, time::Duration};

use anyhow::{Context as _, Result};
use eventsource_client::{Client as _, Event, ReconnectOptions, SSE};
use futures::TryStreamExt as _;
use reqwest::{
    header::{HeaderName, HeaderValue},
    redirect::Policy,
};

pub struct Channel<H> {
    channel: url::Url,
    handler: H,
}

impl<H> Channel<H> {
    /// Create a channel using a server
    ///
    /// `server` is the url of the remote server to use
    pub async fn new(server: url::Url, handler: H) -> Result<Self> {
        let client = reqwest::ClientBuilder::new()
            .redirect(Policy::none())
            .build()?;
        let response = client
            .head(server.clone())
            .send()
            .await?
            .error_for_status()?;
        let url = response
            .headers()
            .get("location")
            .context("no redirect location")?
            .to_str()?;
        let channel = server.join(url)?;
        Ok(Self { channel, handler })
    }

    pub fn from_existing_channel(channel: url::Url, handler: H) -> Self {
        Self { channel, handler }
    }

    pub fn get_channel_url(&self) -> &url::Url {
        &self.channel
    }
}

impl<H: MessageHandler> Channel<H> {
    pub async fn start(&mut self) -> Result<()> {
        let se_client = eventsource_client::ClientBuilder::for_url(self.channel.as_str())?
            .reconnect(
                ReconnectOptions::reconnect(true)
                    .retry_initial(false)
                    .delay(Duration::from_secs(1))
                    .backoff_factor(2)
                    .delay_max(Duration::from_secs(60))
                    .build(),
            )
            .build();
        let mut stream = se_client.stream();
        while let Some(event) = stream.try_next().await? {
            match event {
                SSE::Connected(_) => tracing::info!("connected"),
                SSE::Event(ev) => {
                    let Err(err) = Box::pin(self.handle_event(ev)).await else {
                        continue;
                    };
                    tracing::error!("Error handling event");
                    tracing::error!("{err:#}");
                }
                SSE::Comment(comment) => {
                    if comment.is_empty() {
                        tracing::debug!("comment {comment:?}");
                    } else {
                        tracing::info!("comment {comment:?}");
                    }
                }
            }
        }
        Ok(())
    }

    async fn handle_event(&self, event: Event) -> Result<()> {
        match event.event_type.as_str() {
            "ready" => {
                tracing::info!("ready");
            }
            "ping" => {}
            "message" => {
                let WebhookEvent { headers, body } = serde_json::from_str(&event.data)?;
                self.handler.handle(headers, body).await?;
            }
            _ => {
                tracing::error!("unknown event: {event:?}");
            }
        }
        Ok(())
    }
}

pub type HeaderMap = HashMap<String, Vec<u8>>;

pub trait MessageHandler: Send + Sync {
    #[expect(async_fn_in_trait)]
    async fn handle(&self, headers: HeaderMap, body: Vec<u8>) -> Result<()>;
}

pub struct ForwardHandler {
    pub client: reqwest::Client,
    pub target: url::Url,
}

impl ForwardHandler {
    pub fn new(target: url::Url) -> Self {
        Self {
            client: reqwest::Client::new(),
            target,
        }
    }
}

impl MessageHandler for ForwardHandler {
    async fn handle(&self, h: HeaderMap, body: Vec<u8>) -> Result<()> {
        tracing::info!("forwarding webhook of length {}", body.len());
        let mut headers = reqwest::header::HeaderMap::new();
        for (k, v) in h {
            headers.insert(k.parse::<HeaderName>()?, HeaderValue::from_bytes(&v)?);
        }
        let response = self
            .client
            .post(self.target.clone())
            .headers(headers)
            .body(body)
            .send()
            .await?;
        if let Err(err) = response.error_for_status() {
            tracing::error!("forwarded webhook responded with {err}");
        }
        Ok(())
    }
}

#[derive(serde::Deserialize)]
struct WebhookEvent {
    pub headers: HashMap<String, Vec<u8>>,
    pub body: Vec<u8>,
}
