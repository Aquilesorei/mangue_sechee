//! Background discovery task.
//! Advertises this agent on the LAN and logs peers as they appear.

use manguesechee_network::Discovery;
use tracing::{info, warn};

pub async fn run(name: String, id: String, port: u16) {
    match Discovery::new(&name, &id, port) {
        Err(e) => warn!("mDNS discovery unavailable: {e}"),
        Ok(disc) => {
            info!("mDNS discovery active");
            loop {
                for peer in disc.poll() {
                    info!(
                        "discovered peer: name={} id={} addr={}",
                        peer.name, peer.id, peer.address
                    );
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            }
        }
    }
}
