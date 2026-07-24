use crate::catalog::DeviceIdentity;
use crate::network::local_addresses;
use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::net::{IpAddr, SocketAddr};
use thiserror::Error;

const SERVICE_TYPE: &str = "_remagic-sync._tcp.local.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredPeer {
    pub id: String,
    pub name: String,
    pub public_key: Vec<u8>,
    pub addresses: Vec<SocketAddr>,
    pub pairing_code: String,
}

pub struct DiscoveryService {
    daemon: ServiceDaemon,
    receiver: Receiver<ServiceEvent>,
    fullname: String,
    own_id: String,
    own_public_key: Vec<u8>,
    peers: BTreeMap<String, DiscoveredPeer>,
}

impl DiscoveryService {
    pub fn start(identity: &DeviceIdentity, port: u16) -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new()?;
        let addresses = local_addresses()?;
        let hostname = format!("remagic-{}.local.", identity.id);
        let mut properties = HashMap::new();
        properties.insert("schema".to_owned(), "1".to_owned());
        properties.insert("id".to_owned(), identity.id.clone());
        properties.insert("name".to_owned(), identity.name.clone());
        properties.insert("pk".to_owned(), hex::encode(&identity.public_key));
        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &identity.id,
            &hostname,
            &addresses[..],
            port,
            Some(properties),
        )?;
        let fullname = service.get_fullname().to_owned();
        daemon.register(service)?;
        let receiver = daemon.browse(SERVICE_TYPE)?;
        Ok(Self {
            daemon,
            receiver,
            fullname,
            own_id: identity.id.clone(),
            own_public_key: identity.public_key.clone(),
            peers: BTreeMap::new(),
        })
    }

    pub fn poll(&mut self) {
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    if let Some(peer) = parse_peer(&info, &self.own_public_key) {
                        if peer.id != self.own_id {
                            self.peers.insert(peer.id.clone(), peer);
                        }
                    }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    self.peers
                        .retain(|_, peer| !fullname.starts_with(&format!("{}.", peer.id)));
                }
                _ => {}
            }
        }
    }

    pub fn peers(&self) -> Vec<DiscoveredPeer> {
        self.peers.values().cloned().collect()
    }
}

impl Drop for DiscoveryService {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.shutdown();
    }
}

fn parse_peer(info: &ServiceInfo, own_key: &[u8]) -> Option<DiscoveredPeer> {
    if info.get_property_val_str("schema")? != "1" {
        return None;
    }
    let id = info.get_property_val_str("id")?.to_owned();
    let name = info.get_property_val_str("name")?.to_owned();
    let public_key = hex::decode(info.get_property_val_str("pk")?).ok()?;
    if id.len() != 16
        || name.trim().is_empty()
        || public_key.len() != 32
        || hex::encode(&blake3::hash(&public_key).as_bytes()[..8]) != id
    {
        return None;
    }
    let addresses = info
        .get_addresses()
        .iter()
        .filter(|address| usable(**address))
        .map(|address| SocketAddr::new(*address, info.get_port()))
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return None;
    }
    Some(DiscoveredPeer {
        id,
        name,
        public_key: public_key.clone(),
        addresses,
        pairing_code: pairing_code(own_key, &public_key),
    })
}

pub fn pairing_code(first: &[u8], second: &[u8]) -> String {
    let (low, high) = if first <= second {
        (first, second)
    } else {
        (second, first)
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"remagic-pair-v1");
    hasher.update(low);
    hasher.update(high);
    let bytes = hasher.finalize();
    let number = u32::from_be_bytes(bytes.as_bytes()[..4].try_into().unwrap()) % 1_000_000;
    format!("{number:06}")
}

fn usable(address: IpAddr) -> bool {
    !address.is_unspecified()
        && !address.is_loopback()
        && !address.is_multicast()
        // Every reMarkable exposes the same USB gadget address. Advertising it
        // for peer sync can make a tablet connect back to itself instead of the
        // discovered peer, so device-to-device traffic must use the LAN address.
        && address != IpAddr::V4("10.11.99.1".parse().expect("fixed USB address"))
}

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error(transparent)]
    Mdns(#[from] mdns_sd::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_code_is_order_independent() {
        assert_eq!(
            pairing_code(&[1; 32], &[2; 32]),
            pairing_code(&[2; 32], &[1; 32])
        );
        assert_eq!(pairing_code(&[1; 32], &[2; 32]).len(), 6);
    }

    #[test]
    fn peer_discovery_rejects_the_shared_usb_gadget_address() {
        assert!(!usable("10.11.99.1".parse().unwrap()));
        assert!(usable("172.16.20.41".parse().unwrap()));
    }
}
