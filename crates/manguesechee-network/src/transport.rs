//! Length-prefixed TCP transport with split send/receive halves.
//!
//! Wire format: [u32 big-endian length][bincode-encoded Message]

use anyhow::Context;
use async_trait::async_trait;
use manguesechee_core::protocol::Message;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpStream,
    },
};

const MAX_FRAME: usize = 4 * 1024 * 1024; // 4 MiB sanity cap

// ── Shared framing helpers ────────────────────────────────────────────────────

async fn write_msg(w: &mut (impl AsyncWriteExt + Unpin), msg: &Message) -> anyhow::Result<()> {
    let bytes = bincode::serde::encode_to_vec(msg, bincode::config::standard())
        .context("encode message")?;
    let len = bytes.len() as u32;
    w.write_all(&len.to_be_bytes()).await.context("write length")?;
    w.write_all(&bytes).await.context("write payload")?;
    Ok(())
}

async fn read_msg(r: &mut (impl AsyncReadExt + Unpin)) -> anyhow::Result<Message> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await.context("read length")?;
    let len = u32::from_be_bytes(len_buf) as usize;
    anyhow::ensure!(len <= MAX_FRAME, "frame too large: {len} bytes");
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await.context("read payload")?;
    let (msg, _) = bincode::serde::decode_from_slice(&buf, bincode::config::standard())
        .context("decode message")?;
    Ok(msg)
}

// ── Full-duplex transport (used during handshake) ─────────────────────────────

#[async_trait]
pub trait Transport: Send {
    async fn send(&mut self, msg: &Message) -> anyhow::Result<()>;
    async fn receive(&mut self) -> anyhow::Result<Message>;
    async fn close(self) -> anyhow::Result<()>;
}

pub struct TcpTransport {
    stream: TcpStream,
}

impl TcpTransport {
    pub fn new(stream: TcpStream) -> Self {
        Self { stream }
    }

    /// Split into independent send and receive halves for concurrent use.
    pub fn into_split(self) -> (TcpSender, TcpReceiver) {
        let (read, write) = self.stream.into_split();
        (TcpSender { write }, TcpReceiver { read })
    }
}

#[async_trait]
impl Transport for TcpTransport {
    async fn send(&mut self, msg: &Message) -> anyhow::Result<()> {
        write_msg(&mut self.stream, msg).await
    }
    async fn receive(&mut self) -> anyhow::Result<Message> {
        read_msg(&mut self.stream).await
    }
    async fn close(mut self) -> anyhow::Result<()> {
        self.stream.shutdown().await.context("tcp shutdown")
    }
}

// ── Split halves ──────────────────────────────────────────────────────────────

pub struct TcpSender {
    write: OwnedWriteHalf,
}

impl TcpSender {
    pub async fn send(&mut self, msg: &Message) -> anyhow::Result<()> {
        write_msg(&mut self.write, msg).await
    }
}

pub struct TcpReceiver {
    read: OwnedReadHalf,
}

impl TcpReceiver {
    pub async fn receive(&mut self) -> anyhow::Result<Message> {
        read_msg(&mut self.read).await
    }
}
