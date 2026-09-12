use serde::{Deserialize, Serialize};
use crate::events::InputEvent;

/// Screen edge that a cursor crosses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edge { Left, Right, Top, Bottom }

impl Edge {
    pub fn opposite(&self) -> Edge {
        match self {
            Edge::Left   => Edge::Right,
            Edge::Right  => Edge::Left,
            Edge::Top    => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "left" => Edge::Left,
            "above" | "top" => Edge::Top,
            "below" | "bottom" => Edge::Bottom,
            _ => Edge::Right,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Edge::Left => "left",
            Edge::Right => "right",
            Edge::Top => "above",
            Edge::Bottom => "below",
        }
    }

    pub fn delta(&self) -> (i32, i32) {
        match self {
            Edge::Right => (1, 0),
            Edge::Left => (-1, 0),
            Edge::Top => (0, 1),
            Edge::Bottom => (0, -1),
        }
    }

    pub fn from_delta(dx: i32, dy: i32) -> Option<Edge> {
        if dx > 0 {
            Some(Edge::Right)
        } else if dx < 0 {
            Some(Edge::Left)
        } else if dy > 0 {
            Some(Edge::Top)
        } else if dy < 0 {
            Some(Edge::Bottom)
        } else {
            None
        }
    }
}

pub fn opposite_position(pos: &str) -> &'static str {
    match pos.trim().to_lowercase().as_str() {
        "left" => "right",
        "right" => "left",
        "above" | "top" => "below",
        "below" | "bottom" => "above",
        _ => "left",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    // ── Phase 1 ──────────────────────────────────────────────────────────────
    Ping,
    Pong,
    Identity { name: String, id: String },

    // ── TLS Negotiation ───────────────────────────────────────────────────────
    /// Sent immediately on connection to negotiate TLS upgrade.
    StartTls { requested: bool },
    /// Responder answers whether it accepts upgrading to TLS.
    StartTlsAck { accept: bool },

    // ── Phase 2/3 ─────────────────────────────────────────────────────────────
    InputEvent(InputEvent),

    // ── Phase 4 ──────────────────────────────────────────────────────────────
    EdgeCrossed   {
        edge: Edge,
        #[serde(default)]
        ratio: Option<f32>,
    },
    ReturnControl {
        edge: Edge,
        #[serde(default)]
        ratio: Option<f32>,
    },

    // ── Phase 5 — pairing ─────────────────────────────────────────────────────
    /// Initiator sends its identity + a 6-digit verification code.
    PairRequest  { name: String, id: String, code: String },
    /// Responder echoes the code back if the user accepted.
    PairAccepted { name: String, id: String, code: String },
    /// Responder rejected the pairing attempt.
    PairRejected { reason: String },

    // ── Phase 7 — clipboard ───────────────────────────────────────────────────
    /// Sent whenever the active controller's clipboard text changes.
    ClipboardSync { text: String },

    // ── Screen Topology Synchronization ──────────────────────────────────────
    /// Synchronizes monitor layout across machines.
    /// `position` specifies where the receiver screen is located relative to the sender screen.
    /// Receiver automatically sets the sender to the complementary opposite position.
    TopologySync { position: String },

    // ── Phase 8 — File Transfer ───────────────────────────────────────────────
    /// Informs the peer whether local file transfers are enabled or disabled.
    FileTransferStatus { enabled: bool },

    /// Connection handshake for dedicated secondary data channel
    FileChannelInit { transfer_id: String },

    /// Start a file transfer (both fast path on main channel or secondary channel)
    FileTransferOffer {
        transfer_id: String,
        files: Vec<FileInfo>,
        total_size: u64,
        is_background: bool,
    },

    /// A chunk of file data
    FileTransferChunk {
        transfer_id: String,
        file_index: usize,
        offset: u64,
        data: Vec<u8>,
        is_last_chunk: bool,
    },

    /// Completed transfer of all files in this transfer_id
    FileTransferDone {
        transfer_id: String,
    },

    Goodbye,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    pub filename: String,
    pub size: u64,
    pub relative_path: Option<String>,
}
