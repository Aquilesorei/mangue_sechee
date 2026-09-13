//! Background discovery task.
//! Advertises this agent on the LAN and stores discovered peers in IPC state.

use crate::ipc_server::SharedState;
use manguesechee_core::ipc::PeerInfo;
use manguesechee_network::Discovery;
use tracing::{info, warn};

pub async fn run(
    name: String,
    id: String,
    display_name: String,
    port: u16,
    state: SharedState,
    broadcast_tx: tokio::sync::broadcast::Sender<manguesechee_core::protocol::Message>,
) {
    match Discovery::new(&name, &id, &display_name, port) {
        Err(e) => warn!("mDNS discovery unavailable: {e}"),
        Ok(disc) => {
            info!("mDNS discovery active (advertising display name '{display_name}')");
            loop {
                let discovered = disc.poll();
                if !discovered.is_empty() {
                    let mut s = state.lock().unwrap();
                    let current_local_disp = s.local_display_name.clone();

                    for peer in discovered {
                        // Don't add ourselves if we discover our own announcement
                        if peer.id == id || peer.name == name {
                            continue;
                        }
                        info!(
                            "discovered peer: name={} display={} id={} addr={}",
                            peer.name, peer.display_name, peer.id, peer.address
                        );

                        // ── Autonomous Name Conflict Resolution ───────────────────
                        if !peer.display_name.trim().is_empty()
                            && !manguesechee_core::names::is_raw_uuid(&peer.display_name)
                            && peer.display_name.eq_ignore_ascii_case(&current_local_disp)
                        {
                            // Deterministic tie-breaker: compare internal IDs.
                            // If my id < peer.id, this device yields and renegotiates a free name.
                            if id < peer.id {
                                let mut taken: Vec<String> = s.peers.iter()
                                    .map(|p| p.effective_display_name())
                                    .collect();
                                taken.push(peer.display_name.clone());
                                let new_name = manguesechee_core::names::generate_free_name(&taken);
                                info!(
                                    "mDNS: Autonomous name negotiation! Display name collision on '{}' with peer '{}' (id={}). Yielding and renegotiated to '{}'",
                                    current_local_disp, peer.name, peer.id, new_name
                                );

                                s.local_display_name = new_name.clone();

                                if let Ok(mut cfg) = manguesechee_core::config::load() {
                                    cfg.device.display_name = new_name.clone();
                                    let _ = manguesechee_core::config::save(&cfg);
                                }

                                let _ = disc.update_display_name(&name, &id, &new_name, port);
                                let _ = broadcast_tx.send(manguesechee_core::protocol::Message::IdentityUpdate {
                                    name: name.clone(),
                                    display_name: new_name,
                                });
                            } else {
                                info!(
                                    "mDNS: Autonomous name negotiation: collision on '{}' with peer '{}' (id={}). This device holds priority; peer will yield.",
                                    current_local_disp, peer.name, peer.id
                                );
                            }
                        }

                        let known_store = crate::session::load_known_peers().unwrap_or_default();
                        let is_paired = known_store.contains(&peer.id) || known_store.peers.iter().any(|p| p.name == peer.name);

                        if let Some(existing) = s.peers.iter_mut().find(|p| p.address == peer.address || p.name == peer.name) {
                            existing.address = peer.address.clone();
                            existing.name = peer.name.clone();
                            if !manguesechee_core::names::is_raw_uuid(&peer.display_name) {
                                existing.display_name = peer.display_name.clone();
                            }
                            existing.paired = is_paired;
                        } else {
                            let pos = manguesechee_core::config::load().ok().and_then(|cfg| {
                                cfg.peers.iter().find(|p| {
                                    p.address.as_deref().unwrap_or("").contains(&peer.address)
                                        || p.address.as_deref().is_some_and(|a| !a.is_empty() && peer.address.contains(a))
                                        || p.id == peer.id
                                        || (!p.position.is_empty() && cfg.peers.len() == 1)
                                }).map(|p| p.position.clone())
                            }).unwrap_or_else(|| "right".to_string());

                            s.peers.push(PeerInfo::new(
                                peer.name.clone(),
                                peer.address.clone(),
                                is_paired,
                                false,
                                pos,
                            ).with_display_name(peer.display_name.clone()));
                        }
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            }
        }
    }
}
