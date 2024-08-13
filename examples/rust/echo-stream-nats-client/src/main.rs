use core::time::Duration;

use anyhow::Context as _;
use async_stream::stream;
use clap::Parser;
use futures::StreamExt as _;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use url::Url;

mod bindings {
    wit_bindgen_wrpc::generate!({
        with: {
            "wrpc-examples:echo-stream/handler": generate
        }
    });
}

use bindings::wrpc_examples::echo_stream::handler::{echo, Req};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// NATS.io URL to connect to
    #[arg(short, long, default_value = "nats://127.0.0.1:4222")]
    nats: Url,

    /// Prefixes to invoke `wrpc-examples:echo-stream/handler.echo` on
    #[arg(default_value = "rust")]
    prefixes: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with(tracing_subscriber::fmt::layer().compact().without_time())
        .init();

    let Args { nats, prefixes } = Args::parse();

    let nats = connect(nats)
        .await
        .context("failed to connect to NATS.io")?;
    for prefix in prefixes {
        let input_stream = Box::pin(stream! {
            for i in 1..=10 {
                yield vec![i];
                sleep(Duration::from_secs(1)).await;
            }
        });
        let wrpc = wrpc_transport_nats::Client::new(nats.clone(), prefix.clone(), None);
        let (mut output_stream, res) = echo(
            &wrpc,
            None,
            Req {
                input: input_stream,
            },
        )
        .await
        .context("failed to invoke `wrpc-examples.hello/handler.hello`")?;
        let task = tokio::spawn(async move {
            match res {
                Some(fut) => Some(fut.await),
                None => None,
            }
        });
        while let Some(item) = output_stream.next().await {
            eprintln!("got {item:?}");
        }
        if let Some(res) = task.await? {
            res?;
        }
    }
    Ok(())
}

/// Connect to NATS.io server and ensure that the connection is fully established before
/// returning the resulting [`async_nats::Client`]
async fn connect(url: Url) -> anyhow::Result<async_nats::Client> {
    let (conn_tx, mut conn_rx) = mpsc::channel(1);
    let client = async_nats::connect_with_options(
        String::from(url),
        async_nats::ConnectOptions::new()
            .retry_on_initial_connect()
            .event_callback(move |event| {
                let conn_tx = conn_tx.clone();
                async move {
                    if let async_nats::Event::Connected = event {
                        conn_tx
                            .send(())
                            .await
                            .expect("failed to send NATS.io server connection notification");
                    }
                }
            }),
    )
    .await
    .context("failed to connect to NATS.io server")?;
    conn_rx
        .recv()
        .await
        .context("failed to await NATS.io server connection to be established")?;
    Ok(client)
}
