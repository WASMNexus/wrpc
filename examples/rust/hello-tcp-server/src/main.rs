use core::net::SocketAddr;
use core::pin::pin;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;
use futures::stream::select_all;
use futures::{StreamExt as _, TryStreamExt as _};
use tokio::{select, signal};
use tracing::{error, info, warn};

mod bindings {
    wit_bindgen_wrpc::generate!({
        with: {
            "wrpc-examples:hello/handler": generate,
        }
    });
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Address to serve `wrpc-examples:hello/handler.hello` on
    #[arg(default_value = "[::1]:7761")]
    addr: String,
}

#[derive(Clone, Copy)]
struct Server;

impl bindings::exports::wrpc_examples::hello::handler::Handler<SocketAddr> for Server {
    async fn hello(&self, _: SocketAddr) -> anyhow::Result<String> {
        Ok("hello from Rust".to_string())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    let Args { addr } = Args::parse();

    let lis = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind TCP listener on `{addr}`"))?;
    let srv = Arc::new(wrpc_transport::Server::default());
    let accept = tokio::spawn({
        let srv = Arc::clone(&srv);
        async move {
            loop {
                if let Err(err) = srv.accept(&lis).await {
                    error!(?err, "failed to accept TCP connection")
                }
            }
        }
    });

    let invocations = bindings::serve(srv.as_ref(), Server)
        .await
        .context("failed to serve `wrpc-examples.hello/handler.hello`")?;
    // NOTE: This will conflate all invocation streams into a single stream via `futures::stream::SelectAll`,
    // to customize this, iterate over the returned `invocations` and set up custom handling per export
    let mut invocations = select_all(invocations.into_iter().map(
        |(instance, name, invocations)| {
            invocations
                .try_buffer_unordered(16) // handle up to 16 invocations concurrently
                .map(move |res| (instance, name, res))
        },
    ));
    let shutdown = signal::ctrl_c();
    let mut shutdown = pin!(shutdown);
    loop {
        select! {
            Some((instance, name, res)) = invocations.next() => {
                match res {
                    Ok(()) => {
                        info!(instance, name, "invocation successfully handled");
                    }
                    Err(err) => {
                        warn!(?err, instance, name, "failed to accept invocation");
                    }
                }
            }
            res = &mut shutdown => {
                accept.abort();
                return res.context("failed to listen for ^C")
            }
        }
    }
}