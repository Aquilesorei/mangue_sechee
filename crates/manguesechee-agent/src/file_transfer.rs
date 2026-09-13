
use anyhow::Context;
use manguesechee_core::ipc::{FileTransferInfo, TransferHistoryEntry};
use manguesechee_core::protocol::{FileInfo, Message};
use manguesechee_network::transport::{TcpSender, TcpTransport, Transport};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use tracing::{debug, info, warn};

use crate::file_clipboard;
use crate::ipc_server::SharedState;

pub const CHUNK_SIZE: usize = 64 * 1024; // 64 KiB
 
/// Sanitize relative paths to prevent directory traversal outside staging directory
pub fn sanitize_relative_path(path_str: &str) -> PathBuf {
    let p = std::path::Path::new(path_str);
    let mut safe = PathBuf::new();
    for comp in p.components() {
        if let std::path::Component::Normal(c) = comp {
            safe.push(c);
        }
    }
    if safe.as_os_str().is_empty() {
        PathBuf::from("unnamed_file")
    } else {
        safe
    }
}

/// State tracking an in-flight file reception.
pub struct ActiveReceiver {
    pub files: Vec<FileInfo>,
    pub _staging_dir: PathBuf,
    pub disk_paths: Vec<PathBuf>,
    pub total_size: u64,
    pub bytes_received: u64,
    pub is_background: bool,
    current_file: Option<(usize, File)>,
}

impl ActiveReceiver {
    pub fn new(
        transfer_id: String,
        files: Vec<FileInfo>,
        total_size: u64,
        is_background: bool,
    ) -> anyhow::Result<Self> {
        let staging = file_clipboard::staging_dir(&transfer_id);
        std::fs::create_dir_all(&staging)?;

        let mut disk_paths = Vec::with_capacity(files.len());
        for file in &files {
            let safe_rel = if let Some(ref rel) = file.relative_path {
                sanitize_relative_path(rel)
            } else {
                sanitize_relative_path(&file.filename)
            };
            let dest = staging.join(safe_rel);
            disk_paths.push(dest);
        }

        Ok(Self {
            files,
            _staging_dir: staging,
            disk_paths,
            total_size,
            bytes_received: 0,
            is_background,
            current_file: None,
        })
    }

    pub fn write_chunk(
        &mut self,
        file_index: usize,
        offset: u64,
        data: &[u8],
    ) -> anyhow::Result<()> {
        let dest = self
            .disk_paths
            .get(file_index)
            .ok_or_else(|| anyhow::anyhow!("file index {file_index} out of bounds"))?;

        let file = match self.current_file {
            Some((idx, ref mut f)) if idx == file_index => f,
            _ => {
                if let Some((_, mut f)) = self.current_file.take() {
                    let _ = f.flush();
                }
                if let Some(parent) = dest.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let f = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(offset == 0)
                    .open(dest)
                    .with_context(|| format!("create {}", dest.display()))?;
                self.current_file = Some((file_index, f));
                &mut self.current_file.as_mut().unwrap().1
            }
        };

        file.seek(SeekFrom::Start(offset))?;
        file.write_all(data)?;
        self.bytes_received += data.len() as u64;
        Ok(())
    }

    pub fn finish(mut self) -> anyhow::Result<(Vec<PathBuf>, bool, String, u64)> {
        if let Some((_, mut f)) = self.current_file.take() {
            let _ = f.flush();
        }
        let first_name = self.files.first().map(|f| f.filename.clone()).unwrap_or_else(|| "file".into());

        let mut top_level_paths = Vec::new();
        for file in &self.files {
            let top_item = if let Some(ref rel) = file.relative_path {
                let p = std::path::Path::new(rel);
                if let Some(std::path::Component::Normal(first)) = p.components().next() {
                    self._staging_dir.join(first)
                } else {
                    self._staging_dir.join(&file.filename)
                }
            } else {
                self._staging_dir.join(sanitize_relative_path(&file.filename))
            };
            if !top_level_paths.contains(&top_item) {
                top_level_paths.push(top_item);
            }
        }

        let return_paths = if top_level_paths.is_empty() {
            self.disk_paths
        } else {
            top_level_paths
        };

        Ok((return_paths, self.is_background, first_name, self.total_size))
    }
}

/// Manages incoming transfers (both fast path on main socket and dedicated socket).
#[derive(Default)]
pub struct FileReceiver {
    active: HashMap<String, ActiveReceiver>,
}

impl FileReceiver {
    pub fn handle_offer(
        &mut self,
        transfer_id: String,
        files: Vec<FileInfo>,
        total_size: u64,
        is_background: bool,
        ipc_state: &SharedState,
    ) {
        file_clipboard::clean_old_staged_dirs();
        let fname = files.first().map(|f| f.filename.clone()).unwrap_or_else(|| "files".into());
        info!("receiving file transfer '{fname}' ({} bytes, background={is_background})", total_size);

        match ActiveReceiver::new(transfer_id.clone(), files.clone(), total_size, is_background) {
            Ok(receiver) => {
                self.active.insert(transfer_id.clone(), receiver);
                let mut s = ipc_state.lock().unwrap();
                s.active_transfers.retain(|t| t.transfer_id != transfer_id);
                s.active_transfers.push(FileTransferInfo {
                    transfer_id,
                    filename: fname.clone(),
                    bytes_transferred: 0,
                    total_bytes: total_size,
                    is_receiving: true,
                });
                let size_str = file_clipboard::format_bytes(total_size);
                let notif = if files.len() == 1 {
                    format!("📥 Réception de '{fname}' ({size_str})…")
                } else {
                    format!("📥 Réception de {} fichiers ({size_str})…", files.len())
                };
                file_clipboard::show_notification("Manguesechee", &notif);
            }
            Err(e) => warn!("failed to initialize file receiver: {e}"),
        }
    }

    pub fn handle_chunk(
        &mut self,
        transfer_id: &str,
        file_index: usize,
        offset: u64,
        data: &[u8],
        ipc_state: &SharedState,
    ) {
        if let Some(rec) = self.active.get_mut(transfer_id) {
            if let Err(e) = rec.write_chunk(file_index, offset, data) {
                warn!("error writing chunk: {e}");
            }
            let bytes = rec.bytes_received;
            let mut s = ipc_state.lock().unwrap();
            if let Some(t) = s.active_transfers.iter_mut().find(|t| t.transfer_id == transfer_id) {
                t.bytes_transferred = bytes;
            }
        }
    }

    pub fn handle_done(
        &mut self,
        transfer_id: &str,
        ipc_state: &SharedState,
    ) {
        if let Some(rec) = self.active.remove(transfer_id) {
            match rec.finish() {
                Ok((staged_paths, _is_bg, first_name, total_bytes)) => {
                    info!("file transfer complete: {} files staged", staged_paths.len());
                    if let Err(e) = file_clipboard::set_file_clipboard(&staged_paths) {
                        warn!("failed to set staged files in clipboard: {e}");
                    }

                    let count = staged_paths.len();
                    let notif = if count == 1 {
                        format!("✅ '{first_name}' prêt à être collé ! (Ctrl+V)")
                    } else {
                        format!("✅ {count} fichiers prêts à être collés ! (Ctrl+V)")
                    };
                    file_clipboard::show_notification("Manguesechee", &notif);

                    let mut s = ipc_state.lock().unwrap();
                    s.active_transfers.retain(|t| t.transfer_id != transfer_id);
                    s.transfer_history.insert(0, TransferHistoryEntry {
                        filename: first_name,
                        total_bytes,
                        completed_at: current_timestamp(),
                        is_receiving: true,
                    });
                    if s.transfer_history.len() > 15 {
                        s.transfer_history.truncate(15);
                    }
                }
                Err(e) => warn!("error finishing file transfer: {e}"),
            }
        }
    }
}

static LAST_PREMATURE_NOTIF: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Checks if any file transfer is currently being received.
/// If so, shows a desktop notification informing the user of the progress percentage.
/// Rate-limited to once every 2 seconds.
pub fn notify_premature_paste_if_transferring(ipc_state: &SharedState) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let last = LAST_PREMATURE_NOTIF.load(std::sync::atomic::Ordering::Relaxed);
    if now.saturating_sub(last) < 2 {
        return;
    }

    let s = ipc_state.lock().unwrap();
    if let Some(active) = s.active_transfers.iter().find(|t| t.is_receiving) {
        LAST_PREMATURE_NOTIF.store(now, std::sync::atomic::Ordering::Relaxed);
        let pct = if active.total_bytes > 0 {
            ((active.bytes_transferred as f64 / active.total_bytes as f64) * 100.0) as u32
        } else {
            0
        };
        let body = format!("⏳ Transfert en cours ({}%) — veuillez patienter avant de coller !", pct.min(99));
        file_clipboard::show_notification("Manguesechee", &body);
    }
}


/// Send files <= 15 MB inline over the primary connection.
pub async fn send_fast_transfer(
    transfer_id: String,
    files: Vec<FileInfo>,
    disk_paths: Vec<PathBuf>,
    total_size: u64,
    sender: &mut TcpSender,
    ipc_state: &SharedState,
) -> anyhow::Result<()> {
    let first_name = files.first().map(|f| f.filename.clone()).unwrap_or_else(|| "files".into());
    info!("sending fast-path files: '{first_name}' ({} bytes)", total_size);

    let size_str = file_clipboard::format_bytes(total_size);
    let start_msg = if files.len() == 1 {
        format!("📤 Envoi de '{first_name}' ({size_str})…")
    } else {
        format!("📤 Envoi de {} fichiers ({size_str})…", files.len())
    };
    file_clipboard::show_notification("Manguesechee", &start_msg);

    {
        let mut s = ipc_state.lock().unwrap();
        s.active_transfers.push(FileTransferInfo {
            transfer_id: transfer_id.clone(),
            filename: first_name.clone(),
            bytes_transferred: 0,
            total_bytes: total_size,
            is_receiving: false,
        });
    }

    sender.send(&Message::FileTransferOffer {
        transfer_id: transfer_id.clone(),
        files: files.clone(),
        total_size,
        is_background: false,
    }).await?;

    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut total_sent = 0u64;

    for (idx, path) in disk_paths.iter().enumerate() {
        let mut f = File::open(path)?;
        let mut offset = 0u64;
        let file_len = f.metadata()?.len();

        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let is_last = offset + (n as u64) >= file_len;
            sender.send(&Message::FileTransferChunk {
                transfer_id: transfer_id.clone(),
                file_index: idx,
                offset,
                data: buf[..n].to_vec(),
                is_last_chunk: is_last,
            }).await?;

            offset += n as u64;
            total_sent += n as u64;

            {
                let mut s = ipc_state.lock().unwrap();
                if let Some(t) = s.active_transfers.iter_mut().find(|t| t.transfer_id == transfer_id) {
                    t.bytes_transferred = total_sent;
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }

    sender.send(&Message::FileTransferDone { transfer_id: transfer_id.clone() }).await?;

    {
        let mut s = ipc_state.lock().unwrap();
        s.active_transfers.retain(|t| t.transfer_id != transfer_id);
        s.transfer_history.insert(0, TransferHistoryEntry {
            filename: first_name.clone(),
            total_bytes: total_size,
            completed_at: current_timestamp(),
            is_receiving: false,
        });
        if s.transfer_history.len() > 15 {
            s.transfer_history.truncate(15);
        }
    }

    info!("fast-path file transfer completed ({} bytes)", total_size);
    let done_msg = if files.len() == 1 {
        format!("✅ Envoi terminé : '{first_name}'")
    } else {
        format!("✅ Envoi terminé de {} fichiers", files.len())
    };
    file_clipboard::show_notification("Manguesechee", &done_msg);
    Ok(())
}

/// Send files <= 15 MB inline over an mpsc channel (used by server out_tx).
#[allow(dead_code)]
pub async fn send_fast_transfer_to_channel(
    transfer_id: String,
    files: Vec<FileInfo>,
    disk_paths: Vec<PathBuf>,
    total_size: u64,
    tx: &tokio::sync::mpsc::Sender<Message>,
    ipc_state: &SharedState,
) -> anyhow::Result<()> {
    let first_name = files.first().map(|f| f.filename.clone()).unwrap_or_else(|| "files".into());
    info!("sending fast-path files via channel: '{first_name}' ({} bytes)", total_size);

    let size_str = file_clipboard::format_bytes(total_size);
    let start_msg = if files.len() == 1 {
        format!("📤 Envoi de '{first_name}' ({size_str})…")
    } else {
        format!("📤 Envoi de {} fichiers ({size_str})…", files.len())
    };
    file_clipboard::show_notification("Manguesechee", &start_msg);

    {
        let mut s = ipc_state.lock().unwrap();
        s.active_transfers.push(FileTransferInfo {
            transfer_id: transfer_id.clone(),
            filename: first_name.clone(),
            bytes_transferred: 0,
            total_bytes: total_size,
            is_receiving: false,
        });
    }

    let _ = tx.send(Message::FileTransferOffer {
        transfer_id: transfer_id.clone(),
        files: files.clone(),
        total_size,
        is_background: false,
    }).await;

    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut total_sent = 0u64;

    for (idx, path) in disk_paths.iter().enumerate() {
        let mut f = File::open(path)?;
        let mut offset = 0u64;
        let file_len = f.metadata()?.len();

        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let is_last = offset + (n as u64) >= file_len;
            let _ = tx.send(Message::FileTransferChunk {
                transfer_id: transfer_id.clone(),
                file_index: idx,
                offset,
                data: buf[..n].to_vec(),
                is_last_chunk: is_last,
            }).await;

            offset += n as u64;
            total_sent += n as u64;

            {
                let mut s = ipc_state.lock().unwrap();
                if let Some(t) = s.active_transfers.iter_mut().find(|t| t.transfer_id == transfer_id) {
                    t.bytes_transferred = total_sent;
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }

    let _ = tx.send(Message::FileTransferDone { transfer_id: transfer_id.clone() }).await;

    {
        let mut s = ipc_state.lock().unwrap();
        s.active_transfers.retain(|t| t.transfer_id != transfer_id);
        s.transfer_history.insert(0, TransferHistoryEntry {
            filename: first_name.clone(),
            total_bytes: total_size,
            completed_at: current_timestamp(),
            is_receiving: false,
        });
        if s.transfer_history.len() > 15 {
            s.transfer_history.truncate(15);
        }
    }

    info!("fast-path channel transfer completed ({} bytes)", total_size);
    let done_msg = if files.len() == 1 {
        format!("✅ Envoi terminé : '{first_name}'")
    } else {
        format!("✅ Envoi terminé de {} fichiers", files.len())
    };
    file_clipboard::show_notification("Manguesechee", &done_msg);
    Ok(())
}

/// Spawns a dedicated background task to stream files > 15 MB over a secondary TCP connection.
pub fn spawn_background_sender(
    peer_addr: String,
    files: Vec<FileInfo>,
    disk_paths: Vec<PathBuf>,
    total_size: u64,
    ipc_state: SharedState,
) {
    let transfer_id = uuid::Uuid::new_v4().to_string();
    let first_name = files.first().map(|f| f.filename.clone()).unwrap_or_else(|| "files".into());
    info!("spawning dedicated background file sender for '{first_name}' ({} bytes) to {peer_addr}", total_size);

    let size_str = file_clipboard::format_bytes(total_size);
    let start_msg = if files.len() == 1 {
        format!("📤 Envoi de '{first_name}' ({size_str})…")
    } else {
        format!("📤 Envoi de {} fichiers ({size_str})…", files.len())
    };
    file_clipboard::show_notification("Manguesechee", &start_msg);

    let first_name_done = first_name.clone();
    let files_count = files.len();
    tokio::spawn(async move {
        {
            let mut s = ipc_state.lock().unwrap();
            s.active_transfers.push(FileTransferInfo {
                transfer_id: transfer_id.clone(),
                filename: first_name.clone(),
                bytes_transferred: 0,
                total_bytes: total_size,
                is_receiving: false,
            });
        }

        let mut stream_ok = false;
        match manguesechee_network::connect(&peer_addr).await {
            Ok(mut transport) => {
                let local_tls = ipc_state.lock().unwrap().tls_enabled;
                let _ = transport.send(&Message::StartTls { requested: local_tls }).await;
                if let Ok(Message::StartTlsAck { accept: true }) = transport.receive().await {
                    if let Ok(client_config) = manguesechee_network::tls::create_client_config() {
                        let host = peer_addr.split(':').next().unwrap_or("manguesechee.local");
                        let _ = transport.upgrade_to_tls_client(client_config, host).await;
                    }
                }
                match run_background_stream(
                    &mut transport,
                    transfer_id.clone(),
                    files,
                    disk_paths,
                    total_size,
                    &ipc_state,
                ).await {
                    Ok(()) => { stream_ok = true; }
                    Err(e) => warn!("background file transfer failed: {e:#}"),
                }
                let _ = transport.close().await;
            }
            Err(e) => warn!("failed to connect secondary data channel to {peer_addr}: {e:#}"),
        }

        let mut s = ipc_state.lock().unwrap();
        s.active_transfers.retain(|t| t.transfer_id != transfer_id);
        if stream_ok {
            let done_msg = if files_count == 1 {
                format!("✅ Envoi terminé : '{first_name_done}'")
            } else {
                format!("✅ Envoi terminé de {files_count} fichiers")
            };
            file_clipboard::show_notification("Manguesechee", &done_msg);

            s.transfer_history.insert(0, TransferHistoryEntry {
                filename: first_name,
                total_bytes: total_size,
                completed_at: current_timestamp(),
                is_receiving: false,
            });
            if s.transfer_history.len() > 15 {
                s.transfer_history.truncate(15);
            }
        }
    });
}

async fn run_background_stream(
    transport: &mut TcpTransport,
    transfer_id: String,
    files: Vec<FileInfo>,
    disk_paths: Vec<PathBuf>,
    total_size: u64,
    ipc_state: &SharedState,
) -> anyhow::Result<()> {
    // 1. Handshake as FileChannel
    transport.send(&Message::FileChannelInit { transfer_id: transfer_id.clone() }).await?;

    // 2. Offer
    transport.send(&Message::FileTransferOffer {
        transfer_id: transfer_id.clone(),
        files,
        total_size,
        is_background: true,
    }).await?;

    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut total_sent = 0u64;

    // 3. Stream chunks
    for (idx, path) in disk_paths.iter().enumerate() {
        let mut f = File::open(path)?;
        let mut offset = 0u64;
        let file_len = f.metadata()?.len();

        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let is_last = offset + (n as u64) >= file_len;
            transport.send(&Message::FileTransferChunk {
                transfer_id: transfer_id.clone(),
                file_index: idx,
                offset,
                data: buf[..n].to_vec(),
                is_last_chunk: is_last,
            }).await?;

            offset += n as u64;
            total_sent += n as u64;

            {
                let mut s = ipc_state.lock().unwrap();
                if let Some(t) = s.active_transfers.iter_mut().find(|t| t.transfer_id == transfer_id) {
                    t.bytes_transferred = total_sent;
                }
            }

            // Pace transmission to prevent bufferbloat and Wi-Fi congestion.
            // 64 KiB every 2 ms (~32 MB/s) leaves network queues free for real-time mouse/keyboard packets.
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    }

    // 4. Complete
    transport.send(&Message::FileTransferDone { transfer_id }).await?;
    info!("background stream finished ({} bytes)", total_size);
    Ok(())
}

/// Handler for an incoming dedicated secondary data socket.
pub async fn handle_incoming_file_channel(
    mut transport: TcpTransport,
    init_transfer_id: String,
    ipc_state: SharedState,
) -> anyhow::Result<()> {
    if !ipc_state.lock().unwrap().file_transfer_enabled {
        warn!("incoming file channel for transfer {init_transfer_id} rejected — file transfer disabled");
        return Ok(());
    }
    let mut receiver = FileReceiver::default();

    loop {
        match transport.receive().await {
            Ok(Message::FileTransferOffer { transfer_id, files, total_size, is_background }) => {
                if transfer_id != init_transfer_id {
                    warn!(
                        "file channel: offer transfer_id {transfer_id:?} does not match \
                         negotiated id {init_transfer_id:?} — ignoring"
                    );
                    continue;
                }
                receiver.handle_offer(transfer_id, files, total_size, is_background, &ipc_state);
            }
            Ok(Message::FileTransferChunk { transfer_id, file_index, offset, data, is_last_chunk: _ }) => {
                receiver.handle_chunk(&transfer_id, file_index, offset, &data, &ipc_state);
            }
            Ok(Message::FileTransferDone { transfer_id }) => {
                receiver.handle_done(&transfer_id, &ipc_state);
                break;
            }
            Ok(other) => {
                debug!("unexpected message on file channel: {other:?}");
            }
            Err(e) => {
                debug!("file channel ended: {e}");
                break;
            }
        }
    }

    Ok(())
}

fn current_timestamp() -> String {
    let now = std::time::SystemTime::now();
    let since = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let hours = (since / 3600) % 24;
    let minutes = (since / 60) % 60;
    let seconds = since % 60;
    format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
}
