use anyhow::{Context, Result};
use clap::Parser as _;
use webhook_forwarder::{Channel, ForwardHandler};

#[derive(clap::Parser)]
struct Cli {
    #[arg(short, long)]
    channel: Option<url::Url>,

    #[arg(short, long)]
    source: Option<url::Url>,

    #[arg(short, long, default_value = "http://127.0.0.1:3000")]
    target: url::Url,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let Cli {
        channel,
        source,
        target,
    } = Cli::parse();

    tracing::info!("target url: {target}");
    let handler = ForwardHandler::new(target);
    let mut channel = if let Some(channel_url) = channel {
        Channel::from_existing_channel(channel_url, handler)
    } else {
        Channel::new(source.context("source not set")?, handler).await?
    };
    tracing::info!("webhook url: {}", channel.get_channel_url());
    tracing::info!("listening...");
    channel.start().await?;
    Ok(())
}
