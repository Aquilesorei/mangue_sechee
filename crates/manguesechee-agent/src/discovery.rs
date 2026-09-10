//! Background discovery task.
//! Advertises this agent on the LAN and stores discovered peers in IPC state.

use crate::ipc_server::SharedState;
use manguesechee_core::ipc::PeerInfo;
use manguesechee_network::Discovery;
use tracing::{info, warn};

pub async fn run(name: String, id: String, port: u16, state: SharedState) {
    match Discovery::new(&name, &id, port) {
        Err(e) => warn!("mDNS discovery unavailable: {e}"),
        Ok(disc) => {
            info!("mDNS discovery active");
            loop {
                let discovered = disc.poll();
                if !discovered.is_empty() {
                    let mut s = state.lock().unwrap();
                    for peer in discovered {
                        // Don't add ourselves if we discover our own announcement
                        if peer.id == id || peer.name == name {
                            continue;
                        }
                        info!(
                            "discovered peer: name={} id={} addr={}",
                            peer.name, peer.id, peer.address
                        );
                        let known_store = crate::session::load_known_peers().unwrap_or_default();
                        let is_paired = known_store.contains(&peer.id) || known_store.peers.iter().any(|p| p.name == peer.name);

                        if let Some(existing) = s.peers.iter_mut().find(|p| p.address == peer.address || p.name == peer.name) {
                            existing.address = peer.address.clone();
                            existing.name = peer.name.clone();
                            existing.paired = is_paired;
                        } else {
                            s.peers.push(PeerInfo {
                                name: peer.name.clone(),
                                address: peer.address.clone(),
                                paired: is_paired,
                                connected: false,
                            });
                        }
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            }
        }
    }
}
