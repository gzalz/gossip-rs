use bitcode::{Decode, Encode};
use crate::network_view::*;
use crate::prelude::*;
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use smol::prelude::*;
use smol::Timer;
use std::time::Duration;
use smol_timeout::TimeoutExt;
use std::net::SocketAddr;
use std::net::UdpSocket;
use std::sync::atomic::AtomicBool;
use rand::thread_rng;
use rand::seq::SliceRandom;

// Limit datagram size to keep payloads under MTU
const MAX_DATAGRAM_SIZE: usize = 1024;

#[derive(Clone, Debug)]
enum NetworkViewEvent {
    UnreachablePeer(String),
    AddActivePeer(PeerConnectInfo),
    RemoveActivePeer,
    AddPassivePeer(PeerConnectInfo),
}

#[derive(Encode, Decode, Debug, Clone)]
struct NetworkStatsRequest;

#[derive(Debug)]
pub struct Server {
    is_running: AtomicBool,
}

impl Server {
    pub fn new() -> Server {
        Self {
            is_running: AtomicBool::new(false),
        }
    }

async fn forward_join(&self, remote_addr: SocketAddr, message: &PeerMessage) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();// TODO handle failure case
    let encoded = bitcode::encode(message);
    socket.send_to(encoded.as_slice(), remote_addr).unwrap(); // TODO handle failure case
}

async fn send_join(
    &self,
    bootstrap_addr: SocketAddr,
    peer_id: PeerId,
    port: u16,
) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap(); // TODO handle failure case
    let encoded = bitcode::encode(&PeerMessage::Join(
        peer_id,
        4, // TODO Get the assumed network size
        port,
    ));
    socket.send_to(encoded.as_slice(), bootstrap_addr).unwrap(); // TODO handle failure case
}

async fn send_connect(
    &self,
    peer_addr: SocketAddr,
    priority: ConnectPriority,
    peer_id: PeerId,
    port: u16,
) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap(); // TODO handle failure case
    println!("Sending Connect to {:?}", peer_addr);
    let encoded = bitcode::encode(&PeerMessage::Connect(peer_id, priority, port));
    socket.send_to(encoded.as_slice(), peer_addr).unwrap(); // TODO
}

async fn send_shuffle(
    &self,
    view: &ImmutableNetworkView,
    local_port: u16,
) {
    let mut sampled_peers = ShufflePeers::new();
    for peer in view.active_view.iter() {
        sampled_peers.push(peer.1.clone());
    }
    for peer in view.passive_view.iter() {
        sampled_peers.push(peer.1.clone());
    }
    let mut rng = OsRng;
    sampled_peers.shuffle(&mut rng);
    let sampled_peers = sampled_peers[..1].to_vec();
    let ttl_draw = (rand::random::<f32>()*(view.ttl as f32)) as u8;
    let encoded = bitcode::encode(
        &PeerMessage::ShuffleRequest(
            [127, 0, 0, 1], // TODO
            sampled_peers.clone(),
            ttl_draw,
            local_port.to_owned(),
            )
        );
    for peer in sampled_peers {
        let remote_addr = SocketAddr::from((peer.ip_octets, peer.port));
        let socket = smol::net::UdpSocket::bind("127.0.0.1:0").await.unwrap(); // TODO
        socket.connect(remote_addr).await.unwrap(); // TODO
        let sent_bytes = socket.send(encoded.as_slice()).await.unwrap(); // TODO
        if sent_bytes == 0 {
            println!("Failed to send ShuffleRequest to {:?}", remote_addr);
            return;
        }
        let mut buffer = [0u8; 3];
        let response = socket.recv_from(&mut buffer).timeout(Duration::from_millis(150)).await;
        match response {
            Some(Ok(_)) => {
                println!("Received buffer {:?}", buffer);
            }
            _ => {
                println!("Failed to receive ShuffleResponse from {:?}, removing from active view", remote_addr);
                // TODO Demote peer to passive view. Promote a peer from passive view to active view.
            }
        }
        println!("Received buffer {:?}", response);
    }
}

async fn bootstrap_local_node(
    &self,
    bootstrap: Option<String>, 
    local_peer_id: PeerId, 
    local_port: u16,) -> Result<(), ()> {
    if let Some(bootstrap) = bootstrap {
        println!("Attempting to bootstrap from {:?}", bootstrap);
        let local_socket = UdpSocket::bind("127.0.0.1:0");
        let remote_addr = bootstrap.parse();
        if let Err(e) = remote_addr {
            println!("Invalid bootstrap address: {:?}", e);
            return Err(());
        }
        let remote_addr: SocketAddr = remote_addr.unwrap();
        if remote_addr.eq(&SocketAddr::from(([127, 0, 0, 1], local_port))) {
            println!("Cannot bootstrap from self.. Exiting.");
            return Err(());
        }
        match (local_socket, remote_addr) {
            (Ok(socket), server_addr) => {
                self.send_join(
                    remote_addr,
                    local_peer_id.to_owned(),
                    local_port.to_owned(),
                )
                .await;
                println!("Join sent to {:?}", remote_addr);
            }
            _ => {}
        }
    }
    Ok(())
}

pub async fn run(&'static self, local_port: &u16, bootstrap: Option<String>) -> Result<(),()> {
    // TODO lots of clones in this function should be tackled
    let mut csprng = OsRng;
    let signing_key: SigningKey = SigningKey::generate(&mut csprng);
    let local_peer_id = bs58::encode(signing_key.verifying_key().as_bytes().to_vec()).into_string();
    let local_addr: SocketAddr = SocketAddr::from(([127, 0, 0, 1], *local_port));
    let socket =
        UdpSocket::bind(format!("0.0.0.0:{local_port}")).expect("Could not bind to address");
    println!("PeerId: {:?}", local_peer_id);
    println!("Server started on {:?}", socket);
    let (network_view_tx, network_view_rx) = crossbeam::channel::unbounded::<ImmutableNetworkView>();
    let (active_view_tx, active_view_rx) = crossbeam::channel::unbounded::<ImmutableNetworkView>();
    let (network_event_tx, network_event_rx) = crossbeam::channel::unbounded::<NetworkViewEvent>();

    // PeriodicShuffleController
    let shared_local_port = AsyncShared::new(local_port.clone());

    smol::spawn(async move {
        let thread_local_port = AsyncShared::clone(&shared_local_port);
        let mut latest_view: Option<ImmutableNetworkView> = None;
        println!("Started periodic shuffle controller.");
        loop {
            Timer::after(std::time::Duration::from_millis(10000)).await;
            println!("Shuffle interval elapsed");
            let try_recv = network_view_rx.try_recv();
            if let Ok(view) = try_recv {
                latest_view = Some(view.clone());
            }
            if let Some(ref view) = latest_view {
                self.send_shuffle(&view, *thread_local_port).await;
            } 
        }
    }).detach();

    // NetworkViewController
    smol::spawn(async move {
        let network_view = NetworkView::new();
        let mut latest_event: Option<NetworkViewEvent> = None;
        println!("Started network view event controller");
        loop {
            Timer::after(std::time::Duration::from_millis(10000)).await;
            let try_recv = network_event_rx.try_recv();
            if let Ok(event) = try_recv {
                latest_event = Some(event.clone());
            }
            if let Some(ref event) = latest_event {
                println!("Processing network event...");
                match event {
                    NetworkViewEvent::UnreachablePeer(peer_id) => {
                        println!("NetworkViewEvent: Unreachable peer: {}", peer_id);
                    }
                    NetworkViewEvent::AddActivePeer(peer_connect_info) => {
                        println!("NetworkViewEvent: Add active peer");
                        let inserted = network_view.active_view.borrow_mut().insert(peer_connect_info.peer_id.to_owned(), peer_connect_info.clone());
                        if let Some(_) = inserted {
                            network_view_tx.send(ImmutableNetworkView::from(network_view.clone())).unwrap(); // TODO
                            active_view_tx.send(ImmutableNetworkView::from(network_view.clone())).unwrap(); // TODO
                            println!("Adding newly discovered peer {} to active view", peer_connect_info.peer_id);
                            println!("Active view: {:?}", network_view.active_view);
                        }
                    }
                    NetworkViewEvent::RemoveActivePeer => {
                        println!("NetworkViewEvent: Remove active peer");
                    }
                    NetworkViewEvent::AddPassivePeer(peer_connect_info) => {
                        println!("NetworkViewEvent: Add passive peer");
                        let inserted = network_view.insert_passive(peer_connect_info.peer_id.to_owned(), peer_connect_info.clone());
                        if let Ok(_) = inserted {
                            network_view_tx.send(ImmutableNetworkView::from(network_view.clone())).unwrap(); // TODO
                            println!("Adding newly discovered peer {} to passive view", peer_connect_info.peer_id);
                            println!("Passive view: {:?}", network_view.passive_view);
                        }
                    }
                }
            } 
        }
    }).detach();


    let bootstrapped = self.bootstrap_local_node(bootstrap, local_peer_id.to_owned(), *local_port).await;
    if let Err(_) = bootstrapped {
        return Err(());
    }
    self.is_running.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut last_network_view = ImmutableNetworkView::default();
    // PeerEventHandler
    loop {
        let mut buffer = vec![0u8; MAX_DATAGRAM_SIZE];
        let (size, remote_addr) = socket.recv_from(buffer.as_mut_slice()).unwrap(); // TODO
        let start = std::time::Instant::now();
        let decoded = bitcode::decode::<PeerMessage>(&buffer[..size]);
        let updated_network_view = active_view_rx.try_recv(); 
        if let Ok(view) = updated_network_view {
            last_network_view = view;
        }
        match decoded {
            Ok(decoded) => {
                match decoded {
                    PeerMessage::Join(joining_peer_id, ttl, joining_peer_port) => {
                        match remote_addr {
                            SocketAddr::V4(addr) => {
                                println!("Join from: {:?}", addr);
                                let peer_connect_info = PeerConnectInfo {
                                    peer_id: joining_peer_id.to_owned(),
                                    ip_octets: addr.ip().octets(),
                                    port: joining_peer_port,
                                };
                                network_event_tx.send(NetworkViewEvent::AddActivePeer(peer_connect_info)).unwrap();
                                self.send_connect(
                                    SocketAddr::from((addr.ip().octets(), joining_peer_port)),
                                    ConnectPriority::Normal,
                                    local_peer_id.to_owned(),
                                    local_port.to_owned(),
                                )
                                .await;
                                let forward_message = &PeerMessage::ForwardJoin(
                                    addr.ip().octets(),
                                    joining_peer_id.to_owned(),
                                    ttl - 1,
                                    joining_peer_port,
                                );
                                // TODO prune to randomized subset
                                for remote_peer in last_network_view.active_view.iter() {
                                    println!(
                                        "Forward to Peer: {:?} Joining Peer: {:?}",
                                        remote_peer, joining_peer_id
                                    );
                                    if remote_peer.0.eq(&joining_peer_id) {
                                        continue;
                                    }
                                    self.forward_join(
                                        SocketAddr::from((
                                            remote_peer.1.ip_octets,
                                            remote_peer.1.port,
                                        )),
                                        forward_message,
                                    )
                                    .await;
                                }
                            }
                            SocketAddr::V6(addr) => {
                                println!("Peer connected from: {:?}", addr);
                            }
                        }
                    }
                    PeerMessage::ForwardJoin(
                        ip_octets,
                        joining_peer_id,
                        ttl,
                        joining_peer_port,
                    ) => {
                        println!("ForwardJoin from: {:?}", remote_addr);
                        if joining_peer_id.eq(&local_peer_id) {
                            continue;
                        }
                        let peer_connect_info = PeerConnectInfo {
                            peer_id: joining_peer_id.to_owned(),
                            ip_octets: ip_octets.to_owned(),
                            port: joining_peer_port,
                        };
                        if ttl == 0 {
                            println!("TTL expired for ForwardJoin");
                            network_event_tx.send(NetworkViewEvent::AddActivePeer(peer_connect_info)).unwrap();
                            continue;
                        }
                        network_event_tx.send(NetworkViewEvent::AddPassivePeer(peer_connect_info)).unwrap();
                        let forward_message = &PeerMessage::ForwardJoin(
                            ip_octets,
                            joining_peer_id.to_owned(),
                            ttl - 1,
                            joining_peer_port,
                        );
                        // TODO: Prune to randomized subset
                        for peer in last_network_view.active_view.iter() {
                            if peer.0.eq(&joining_peer_id) {
                                continue;
                            }
                            self.forward_join(
                                SocketAddr::from((peer.1.ip_octets, peer.1.port)),
                                forward_message,
                            )
                            .await;
                        }
                    }
                    PeerMessage::Connect(remote_peer_id, priority, remote_port) => {
                        match remote_addr {
                            SocketAddr::V4(addr) => {
                                println!("Connect from: {:?} via ipv4", addr);
                                let peer_connect_info = PeerConnectInfo {
                                    peer_id: remote_peer_id.to_owned(),
                                    ip_octets: addr.ip().octets(),
                                    port: remote_port,
                                };
                                network_event_tx.send(NetworkViewEvent::AddActivePeer(peer_connect_info)).unwrap();
                            }
                            SocketAddr::V6(addr) => {
                                println!("Incoming Connect request via ipv6 from {}. ipv6 is not supported at this time.", addr);
                            }
                        }
                    }
                    PeerMessage::ShuffleRequest(origin_ip, local_peer_info, ttl, origin_port) => {
                        if ttl == 0 {
                            println!("TTL expired for ShuffleRequest.. responding with ShuffleResponse");
                            let mut shuffled_peers = ShufflePeers::new();
                            let mut active_view_peers = last_network_view.active_view.clone().into_values().collect::<Vec<PeerConnectInfo>>();
                            let mut rng = thread_rng();
                            active_view_peers.shuffle(&mut rng);
                            shuffled_peers.extend(active_view_peers[..local_peer_info.len()].to_vec());
                            let response_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
                            let response_addr = SocketAddr::from((origin_ip, origin_port));
                            let shuffle_response = PeerMessage::ShuffleResponse(shuffled_peers);
                            let encoded = bitcode::encode(&shuffle_response);
                            response_socket.send_to(encoded.as_slice(), response_addr).unwrap(); // TODO
                            socket.send_to(b"ACK", remote_addr).unwrap();
                            continue;
                        }
                        println!("Shuffle Payload: {:?} {:?} {:?} {:?}", origin_ip, local_peer_info, ttl, origin_port);
                        let mut shuffled_peers = ShufflePeers::new();
                        shuffled_peers.extend(local_peer_info);
                        // TODO add our own?
                        let mut rng = thread_rng();
                        let mut active_view_peers = last_network_view.active_view.clone().into_values().collect::<Vec<PeerConnectInfo>>();
                        active_view_peers.shuffle(&mut rng);
                        let remote_peer_info = active_view_peers[0].clone();
                        let forwarded_shuffle = PeerMessage::ShuffleRequest(
                            origin_ip,
                            shuffled_peers,
                            ttl - 1,
                            origin_port,
                        );
                        let forward_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
                        let forward_addr = SocketAddr::from((remote_peer_info.ip_octets, remote_peer_info.port));
                        let encoded = bitcode::encode(&forwarded_shuffle);
                        forward_socket.send_to(encoded.as_slice(), forward_addr).unwrap(); // TODO
                        socket.send_to(
                            b"ACK", 
                            remote_addr
                        ).unwrap();
                    }
                    PeerMessage::ShuffleResponse(shuffled_peers) => {
                        println!("ShuffleResponse from: {:?} {:?}", remote_addr, shuffled_peers);
                        let changed = false;
                        for peer in shuffled_peers {
                            if peer.peer_id.eq(&local_peer_id) {
                                continue;
                            }
                            network_event_tx.send(NetworkViewEvent::AddPassivePeer(peer)).unwrap();
                        }
                    }
                    PeerMessage::Disconnect(peer_id, alive, respond) => {
                        println!("Disconnect: {} {} {}", peer_id, alive, respond);

                    }
                }
            }
            Err(e) => {
                println!("Error: {:?}", e);
            }
        }
    }
}

fn is_running(&self) -> bool {
    self.is_running.load(std::sync::atomic::Ordering::SeqCst)
}

fn shutdown(&self) {
    self.is_running.store(false, std::sync::atomic::Ordering::SeqCst);
}

}

#[cfg(test)]
mod lifecycle {
    use super::*;
    
    use std::time::Duration;
    use std::assert;

    #[test]
    fn test_server_lifecycle_happy_path() {
        let server: &'static Server = Box::leak(Box::new(Server::new()));

        smol::block_on(async {
            smol::spawn(server.run(&7000, None)).detach();
            Timer::after(Duration::from_secs(5)).await;
            assert!(server.is_running());
            server.shutdown();
            assert!(!server.is_running());
        });
    }
}

