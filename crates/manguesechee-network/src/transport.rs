//! Length-prefixed TCP transport with optional TLS encryption and split send/receive halves.
//!
//! Wire format: [u32 big-endian length][bincode-encoded Message]

use anyhow::Context;
use async_trait::async_trait;
use manguesechee_core::protocol::Message;
use std::sync::Arc;
use tokio::{
    io::{split, AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpStream,
    },
};

const MAX_FRAME: usize = 64 * 1024 * 1024; // 64 MiB — large enough for FileTransferOffer with thousands of entries


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


#[async_trait]
pub trait Transport: Send {
    async fn send(&mut self, msg: &Message) -> anyhow::Result<()>;
    async fn receive(&mut self) -> anyhow::Result<Message>;
    async fn close(self) -> anyhow::Result<()>;
}

pub enum ConnectionStream {
    Plain(TcpStream),
    TlsClient(tokio_rustls::client::TlsStream<TcpStream>),
    TlsServer(tokio_rustls::server::TlsStream<TcpStream>),
}

pub struct TcpTransport {
    stream: Option<ConnectionStream>,
}

impl TcpTransport {
    pub fn new(stream: TcpStream) -> Self {
        let _ = stream.set_nodelay(true);
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = stream.as_raw_fd();
            unsafe {
                let opt: libc::c_int = 1;
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_KEEPALIVE,
                    &opt as *const _ as *const libc::c_void,
                    std::mem::size_of_val(&opt) as libc::socklen_t,
                );
                let idle: libc::c_int = 2;
                libc::setsockopt(
                    fd,
                    libc::IPPROTO_TCP,
                    libc::TCP_KEEPIDLE,
                    &idle as *const _ as *const libc::c_void,
                    std::mem::size_of_val(&idle) as libc::socklen_t,
                );
                let intvl: libc::c_int = 1;
                libc::setsockopt(
                    fd,
                    libc::IPPROTO_TCP,
                    libc::TCP_KEEPINTVL,
                    &intvl as *const _ as *const libc::c_void,
                    std::mem::size_of_val(&intvl) as libc::socklen_t,
                );
                let cnt: libc::c_int = 2;
                libc::setsockopt(
                    fd,
                    libc::IPPROTO_TCP,
                    libc::TCP_KEEPCNT,
                    &cnt as *const _ as *const libc::c_void,
                    std::mem::size_of_val(&cnt) as libc::socklen_t,
                );
                #[cfg(target_os = "linux")]
                {
                    let timeout: libc::c_uint = 3000; // 3 seconds timeout for unacknowledged TCP data
                    libc::setsockopt(
                        fd,
                        libc::IPPROTO_TCP,
                        libc::TCP_USER_TIMEOUT,
                        &timeout as *const _ as *const libc::c_void,
                        std::mem::size_of_val(&timeout) as libc::socklen_t,
                    );
                }
            }
        }
        Self { stream: Some(ConnectionStream::Plain(stream)) }
    }

    pub fn is_tls(&self) -> bool {
        matches!(
            self.stream,
            Some(ConnectionStream::TlsClient(_)) | Some(ConnectionStream::TlsServer(_))
        )
    }

    /// Upgrade connection to TLS client
    pub async fn upgrade_to_tls_client(
        &mut self,
        config: Arc<rustls::ClientConfig>,
        domain: &str,
    ) -> anyhow::Result<()> {
        let s = self.stream.take().context("stream already taken")?;
        match s {
            ConnectionStream::Plain(tcp) => {
                let connector = tokio_rustls::TlsConnector::from(config);
                let server_name = rustls::pki_types::ServerName::try_from(domain.to_string())
                    .unwrap_or_else(|_| rustls::pki_types::ServerName::try_from("manguesechee.local").unwrap());
                let tls = connector.connect(server_name, tcp).await
                    .context("TLS client handshake failed")?;
                self.stream = Some(ConnectionStream::TlsClient(tls));
                Ok(())
            }
            other => {
                self.stream = Some(other);
                anyhow::bail!("cannot upgrade: stream is already TLS");
            }
        }
    }

    /// Upgrade connection to TLS server
    pub async fn upgrade_to_tls_server(
        &mut self,
        config: Arc<rustls::ServerConfig>,
    ) -> anyhow::Result<()> {
        let s = self.stream.take().context("stream already taken")?;
        match s {
            ConnectionStream::Plain(tcp) => {
                let acceptor = tokio_rustls::TlsAcceptor::from(config);
                let tls = acceptor.accept(tcp).await
                    .context("TLS server handshake failed")?;
                self.stream = Some(ConnectionStream::TlsServer(tls));
                Ok(())
            }
            other => {
                self.stream = Some(other);
                anyhow::bail!("cannot upgrade: stream is already TLS");
            }
        }
    }

    /// Split into independent send and receive halves for concurrent use.
    pub fn into_split(mut self) -> (TcpSender, TcpReceiver) {
        let s = self.stream.take().expect("stream must be present");
        match s {
            ConnectionStream::Plain(tcp) => {
                let (r, w) = tcp.into_split();
                (
                    TcpSender { write: StreamWriter::Plain(w) },
                    TcpReceiver { read: StreamReader::Plain(r) },
                )
            }
            ConnectionStream::TlsClient(tls) => {
                let (r, w) = split(tls);
                (
                    TcpSender { write: StreamWriter::TlsClient(w) },
                    TcpReceiver { read: StreamReader::TlsClient(r) },
                )
            }
            ConnectionStream::TlsServer(tls) => {
                let (r, w) = split(tls);
                (
                    TcpSender { write: StreamWriter::TlsServer(w) },
                    TcpReceiver { read: StreamReader::TlsServer(r) },
                )
            }
        }
    }
}

#[async_trait]
impl Transport for TcpTransport {
    async fn send(&mut self, msg: &Message) -> anyhow::Result<()> {
        let s = self.stream.as_mut().context("stream closed")?;
        match s {
            ConnectionStream::Plain(tcp) => write_msg(tcp, msg).await,
            ConnectionStream::TlsClient(tls) => write_msg(tls, msg).await,
            ConnectionStream::TlsServer(tls) => write_msg(tls, msg).await,
        }
    }

    async fn receive(&mut self) -> anyhow::Result<Message> {
        let s = self.stream.as_mut().context("stream closed")?;
        match s {
            ConnectionStream::Plain(tcp) => read_msg(tcp).await,
            ConnectionStream::TlsClient(tls) => read_msg(tls).await,
            ConnectionStream::TlsServer(tls) => read_msg(tls).await,
        }
    }

    async fn close(mut self) -> anyhow::Result<()> {
        if let Some(mut s) = self.stream.take() {
            match &mut s {
                ConnectionStream::Plain(tcp) => tcp.shutdown().await.context("tcp shutdown")?,
                ConnectionStream::TlsClient(tls) => tls.shutdown().await.context("tls shutdown")?,
                ConnectionStream::TlsServer(tls) => tls.shutdown().await.context("tls shutdown")?,
            }
        }
        Ok(())
    }
}


pub enum StreamWriter {
    Plain(OwnedWriteHalf),
    TlsClient(WriteHalf<tokio_rustls::client::TlsStream<TcpStream>>),
    TlsServer(WriteHalf<tokio_rustls::server::TlsStream<TcpStream>>),
}

pub enum StreamReader {
    Plain(OwnedReadHalf),
    TlsClient(ReadHalf<tokio_rustls::client::TlsStream<TcpStream>>),
    TlsServer(ReadHalf<tokio_rustls::server::TlsStream<TcpStream>>),
}

pub struct TcpSender {
    write: StreamWriter,
}

impl TcpSender {
    pub async fn send(&mut self, msg: &Message) -> anyhow::Result<()> {
        match &mut self.write {
            StreamWriter::Plain(w) => write_msg(w, msg).await,
            StreamWriter::TlsClient(w) => write_msg(w, msg).await,
            StreamWriter::TlsServer(w) => write_msg(w, msg).await,
        }
    }
}

pub struct TcpReceiver {
    read: StreamReader,
}

impl TcpReceiver {
    pub async fn receive(&mut self) -> anyhow::Result<Message> {
        match &mut self.read {
            StreamReader::Plain(r) => read_msg(r).await,
            StreamReader::TlsClient(r) => read_msg(r).await,
            StreamReader::TlsServer(r) => read_msg(r).await,
        }
    }
}
