//! mDNS/DNS-SD peer discovery.
//!
//! Each agent advertises itself as `_manguesechee._tcp.local` and browses
//! for other agents on the same LAN. No IP addresses needed.

use anyhow::Context;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use std::net::IpAddr;
use tracing::{info, warn};

const SERVICE_TYPE: &str = "_manguesechee._tcp.local.";

#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub name:         String,
    pub display_name: String,
    pub id:           String,
    pub address:      String,   // host:port
    pub port:         u16,
    pub ip:           IpAddr,
}

/// Advertise this agent on the LAN and browse for peers.
///
/// Returns a channel receiver that yields [`DiscoveredPeer`] events as they
/// arrive. The daemon runs in the background until dropped.
pub struct Discovery {
    mdns:         ServiceDaemon,
    pub receiver: mdns_sd::Receiver<ServiceEvent>,
}

impl Discovery {
    pub fn new(
        instance_name: &str,  // e.g. device name
        peer_id:       &str,
        display_name:  &str,
        port:          u16,
    ) -> anyhow::Result<Self> {
        let mdns = ServiceDaemon::new().context("start mDNS daemon")?;
        Self::register_service(&mdns, instance_name, peer_id, display_name, port)?;
        let receiver = Self::browse_services(&mdns)?;

        Ok(Self { mdns, receiver })
    }

    fn register_service(
        mdns: &ServiceDaemon,
        instance_name: &str,
        peer_id: &str,
        display_name: &str,
        port: u16,
    ) -> anyhow::Result<()> {
        let mut props = HashMap::new();
        props.insert("id".to_string(), peer_id.to_string());
        props.insert("display_name".to_string(), display_name.to_string());

        let service = ServiceInfo::new(
            SERVICE_TYPE,
            instance_name,
            &format!("{instance_name}.local."),
            (),       // let the library resolve our IP
            port,
            Some(props),
        ).context("create ServiceInfo")?;

        mdns.register(service).context("mDNS register")?;
        info!("mDNS: advertising as '{instance_name}' ({display_name}) on port {port}");
        Ok(())
    }

    fn browse_services(mdns: &ServiceDaemon) -> anyhow::Result<mdns_sd::Receiver<ServiceEvent>> {
        let receiver = mdns.browse(SERVICE_TYPE).context("mDNS browse")?;
        info!("mDNS: browsing for peers on '{SERVICE_TYPE}'");
        Ok(receiver)
    }

    /// Dynamically update the advertised display name on the LAN.
    pub fn update_display_name(
        &self,
        instance_name: &str,
        peer_id: &str,
        new_display_name: &str,
        port: u16,
    ) -> anyhow::Result<()> {
        let mut props = HashMap::new();
        props.insert("id".to_string(), peer_id.to_string());
        props.insert("display_name".to_string(), new_display_name.to_string());

        let service = ServiceInfo::new(
            SERVICE_TYPE,
            instance_name,
            &format!("{instance_name}.local."),
            (),
            port,
            Some(props),
        ).context("create ServiceInfo for display_name update")?;

        self.mdns.register(service).context("update mDNS display_name")?;
        info!("mDNS: re-advertised as '{instance_name}' with negotiated display name '{new_display_name}'");
        Ok(())
    }

    /// Process pending mDNS events and return any newly discovered peers.
    /// Non-blocking — returns immediately with whatever is available.
    pub fn poll(&self) -> Vec<DiscoveredPeer> {
        let mut peers = Vec::new();
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    let name = info.get_fullname().to_string();
                    let id = info.get_properties()
                        .get("id")
                        .map(|p| p.val_str().to_string())
                        .unwrap_or_default();
                    let display_name = info.get_properties()
                        .get("display_name")
                        .map(|p| p.val_str().to_string())
                        .filter(|d| !d.trim().is_empty() && !manguesechee_core::names::is_raw_uuid(d))
                        .unwrap_or_else(|| manguesechee_core::names::name_from_id(&id));
                    let port = info.get_port();

                    for addr in info.get_addresses() {
                        info!("mDNS: discovered peer '{display_name}' ({name}) at {addr}:{port}");
                        peers.push(DiscoveredPeer {
                            name:         info.get_hostname().trim_end_matches('.').to_string(),
                            display_name: display_name.clone(),
                            id:           id.clone(),
                            address:      format!("{addr}:{port}"),
                            port,
                            ip:           *addr,
                        });
                    }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    info!("mDNS: peer left: {fullname}");
                }
                _ => {}
            }
        }
        peers
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        if let Err(e) = self.mdns.shutdown() {
            warn!("mDNS shutdown error: {e}");
        }
    }
}
