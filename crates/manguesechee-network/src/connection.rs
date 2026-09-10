//! Helpers for establishing inbound and outbound TCP connections.

use anyhow::Context;
use tokio::net::{TcpListener, TcpStream};

use crate::transport::TcpTransport;

pub async fn listen(port: u16) -> anyhow::Result<TcpListener> {
    let addr = format!("0.0.0.0:{port}");
    TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))
}

pub async fn connect(addr: &str) -> anyhow::Result<TcpTransport> {
    let stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("failed to connect to {addr}"))?;
    Ok(TcpTransport::new(stream))
}

pub fn wrap(stream: TcpStream) -> TcpTransport {
    TcpTransport::new(stream)
}
