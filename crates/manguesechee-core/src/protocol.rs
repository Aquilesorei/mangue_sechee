use serde::{Deserialize, Serialize};
use crate::events::InputEvent;

/// Screen edge that a cursor crosses.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    // ── Phase 1 ──────────────────────────────────────────────────────────────
    Ping,
    Pong,
    Identity { name: String, id: String },

    // ── Phase 2/3 ─────────────────────────────────────────────────────────────
    InputEvent(InputEvent),

    // ── Phase 4 ──────────────────────────────────────────────────────────────
    EdgeCrossed   { edge: Edge },
    ReturnControl { edge: Edge },

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

    Goodbye,
}
