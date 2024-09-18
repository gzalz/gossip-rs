use crate::prelude::*;
use bitcode::{Decode, Encode};
use std::collections::HashMap;

pub type PeerId = String;
pub type IpOctets = [u8; 4];
pub type TTL = u8;
pub type Port = u16;
pub type Alive = bool;
pub type Respond = bool;
pub type ShufflePeers = Vec<PeerConnectInfo>;
pub type Peers = HashMap<PeerId, PeerConnectInfo>;

#[derive(Encode, Decode, Debug, Clone, PartialEq, Hash)]
pub struct PeerConnectInfo {
    pub peer_id: PeerId,
    pub ip_octets: IpOctets,
    pub port: Port,
}

#[derive(Debug, Clone)]
pub struct NetworkView {
    assumed_network_size: Mutable<usize>,
    pub active_view: Mutable<Peers>,
    pub passive_view: Mutable<Peers>,
}

#[derive(Debug, Clone)]
pub struct ImmutableNetworkView {
    pub assumed_network_size: usize,
    pub active_view: Peers,
    pub passive_view: Peers,
    pub ttl: TTL,
}

impl ImmutableNetworkView {
    pub fn default() -> Self {
        Self {
            assumed_network_size: 4,
            active_view: HashMap::new(),
            passive_view: HashMap::new(),
            ttl: 3,
        }
    }
}

impl From<NetworkView> for ImmutableNetworkView {
    fn from(network_view: NetworkView) -> Self {
        Self {
            assumed_network_size: *network_view.assumed_network_size.borrow(),
            active_view: network_view.active_view.borrow().clone(),
            passive_view: network_view.passive_view.borrow().clone(),
            ttl: network_view.ttl_for_network_size(),
        }
    }
}

#[derive(Encode, Decode, Debug, Clone)]
pub enum ConnectPriority {
    High,
    Normal,
}

#[derive(Encode, Decode, Debug, Clone)]
pub enum ConnectionEvent {
    NeighborUp(IpOctets, Port),
    NeighborDown(IpOctets, Port),
}

#[derive(Encode, Decode, Debug, Clone)]
pub enum PeerMessage {
    Join(PeerId, TTL, Port),
    ForwardJoin(IpOctets, PeerId, TTL, Port),
    ShuffleRequest(IpOctets, ShufflePeers, TTL, Port),
    ShuffleResponse(ShufflePeers),
    Connect(PeerId, ConnectPriority, Port),
    Disconnect(PeerId, Alive, Respond),
}

const PEER_VIEW_SIZE_CONSTANT: usize = 2;

impl NetworkView {
    pub fn new() -> Self {
        Self {
            active_view: Mutable::new(HashMap::new()),
            passive_view: Mutable::new(HashMap::new()),
            assumed_network_size: Mutable::new(4),
        }
    }

    pub fn remove_active(&self, peer_id: &PeerId) -> Option<PeerConnectInfo> {
        self.active_view.borrow_mut().remove(peer_id)
    }

    fn active_peer_capacity(&self) -> usize {
        (*self.assumed_network_size.borrow() as f64).log2() as usize
    }

    fn passive_peer_capacity(&self) -> usize {
        ((*self.assumed_network_size.borrow()) * PEER_VIEW_SIZE_CONSTANT) as usize
    }

    pub fn ttl_for_network_size(&self) -> TTL {
        let calculated = (*self.assumed_network_size.borrow() as f64).log2() as TTL;
        if calculated < 3 {
            3 as TTL
        } else {
            calculated
        }
    }

    pub fn insert_active(&self, peer_id: PeerId, peer_socket: PeerConnectInfo) -> Result<(), ()> {
        if self.passive_view.borrow().contains_key(&peer_id)
            || self.active_view.borrow().contains_key(&peer_id)
        {
            return Err(());
        }
        self.active_view.borrow_mut().insert(peer_id, peer_socket);
        Ok(())
    }

    pub fn insert_passive(&self, peer_id: PeerId, peer_socket: PeerConnectInfo) -> Result<(), ()> {
        if self.active_view.borrow().contains_key(&peer_id)
            || self.passive_view.borrow().contains_key(&peer_id)
        {
            return Err(());
        }
        self.passive_view.borrow_mut().insert(peer_id, peer_socket);
        if self.active_view.borrow().len() + self.passive_view.borrow().len()
            > *self.assumed_network_size.borrow()
        {
            //let mut network_size = self.assumed_network_size.borrow_mut();
            //*network_size *= 2;
            //println!("Assumed network size increased to: {}", *network_size);
        }
        Ok(())
    }
}
