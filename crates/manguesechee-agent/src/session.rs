//! Pairing state and known-peers store.
//!
//! First connection flow:
//!
//!   Initiator                          Responder
//!   ─────────────────────────────────────────────
//!   PairRequest(name, id, code) ──────►
//!                                      show code to user
//!                                      user accepts
//!                              ◄────── PairAccepted(name, id, code)
//!   verify code matches ✓
//!   both sides save each other's id
//!   normal session begins
//!
//! Known peers are persisted in `~/.config/manguesechee/known_peers.json`.

use anyhow::Context;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;

// ── Known peers store ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KnownPeers {
    pub peers: Vec<KnownPeer>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownPeer {
    pub id:   String,
    pub name: String,
}

impl KnownPeers {
    pub fn contains(&self, id: &str) -> bool {
        self.peers.iter().any(|p| p.id == id)
    }

    pub fn add(&mut self, id: String, name: String) {
        if let Some(existing) = self.peers.iter_mut().find(|p| p.id == id) {
            existing.name = name; // update in case peer was renamed (e.g. hostname changed)
        } else {
            self.peers.push(KnownPeer { id, name });
        }
    }
}

pub fn known_peers_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("manguesechee")
        .join("known_peers.json")
}

pub fn load_known_peers() -> anyhow::Result<KnownPeers> {
    let path = known_peers_path();
    if !path.exists() {
        return Ok(KnownPeers::default());
    }
    let text = std::fs::read_to_string(&path)?;
    serde_json::from_str(&text).context("parse known_peers.json")
}

pub fn save_known_peers(peers: &KnownPeers) -> anyhow::Result<()> {
    let path = known_peers_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(peers)?;
    std::fs::write(&path, text).context("write known_peers.json")
}

// ── Pairing code ──────────────────────────────────────────────────────────────

/// Generate a random 6-digit verification code like "482 731".
pub fn generate_code() -> String {
    let n: u32 = rand::thread_rng().gen_range(0..1_000_000);
    format!("{:03} {:03}", n / 1000, n % 1000)
}

// ── Pairing prompt (terminal) ─────────────────────────────────────────────────

/// Block on stdin and ask the user to accept or reject a pairing request.
/// In daemon mode (no terminal attached), automatically accepts the request and logs it.
pub fn prompt_accept(peer_name: &str, code: &str) -> bool {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        info!("Pairing request received from '{peer_name}' (verification code: {code}) — auto-accepted (daemon mode)");
        return true;
    }

    println!();
    println!("┌──────────────────────────────────────────┐");
    println!("│         Manguesechee — Pair request       │");
    println!("├──────────────────────────────────────────┤");
    println!("│  Device:  {:<31}│", peer_name);
    println!("│  Code:    {:<31}│", code);
    println!("├──────────────────────────────────────────┤");
    println!("│  Accept? [y/N]                            │");
    println!("└──────────────────────────────────────────┘");
    print!("> ");
    use std::io::Write;
    let _ = std::io::stdout().flush();

    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap_or(0);
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Show the outgoing code to the user (initiator side).
pub fn show_outgoing_code(peer_name: &str, code: &str) {
    info!("Pairing with '{peer_name}' — verification code: {code}");
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        println!();
        println!("┌──────────────────────────────────────────┐");
        println!("│       Manguesechee — Pairing code         │");
        println!("├──────────────────────────────────────────┤");
        println!("│  Pairing with: {:<26}│", peer_name);
        println!("│  Code:         {:<26}│", code);
        println!("├──────────────────────────────────────────┤");
        println!("│  Confirm this code matches on the remote  │");
        println!("│  machine before proceeding.               │");
        println!("└──────────────────────────────────────────┘");
    }
}
