use crate::network_view::*;
use crate::prelude::*;
use crossbeam::select;
use ed25519_dalek::Signature;
use ed25519_dalek::SigningKey;
use futures::{self, future::Either};
use rand::rngs::OsRng;
use smol::prelude::*;
use smol::Timer;
use std::time::Duration;
use smol_timeout::TimeoutExt;
use std::net::SocketAddr;
use std::net::UdpSocket;
use rand::thread_rng;
use rand::seq::SliceRandom;

mod network_view;
mod prelude;

// Limit datagram size to keep payloads under MTU
const MAX_DATAGRAM_SIZE: usize = 1024;

async fn forward_join(remote_addr: SocketAddr, message: &PeerMessage) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let encoded = bitcode::encode(message);
    socket.send_to(encoded.as_slice(), remote_addr).unwrap();
}

async fn send_join(
    network_view: &NetworkView,
    bootstrap_addr: SocketAddr,
    peer_id: PeerId,
    port: u16,
) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let encoded = bitcode::encode(&PeerMessage::Join(
        peer_id,
        network_view.ttl_for_network_size(),
        port,
    ));
    socket.send_to(encoded.as_slice(), bootstrap_addr).unwrap();
}

async fn send_connect(
    peer_addr: SocketAddr,
    priority: ConnectPriority,
    peer_id: PeerId,
    port: u16,
) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    println!("Sending Connect to {:?}", peer_addr);
    let encoded = bitcode::encode(&PeerMessage::Connect(peer_id, priority, port));
    socket.send_to(encoded.as_slice(), peer_addr).unwrap();
}

async fn send_shuffle(
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
        let socket = smol::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        socket.connect(remote_addr).await.unwrap();
        let sent_bytes = socket.send(encoded.as_slice()).await.unwrap();
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
                // Remove peer from active view
            }
        }
        println!("Received buffer {:?}", response);
    }
}

async fn bootstrap_local_node(bootstrap: Option<String>, local_peer_id: PeerId, local_port: u16, network_view: NetworkView){
    if let Some(bootstrap) = bootstrap {
        println!("Attempting to bootstrap from {:?}", bootstrap);
        let local_socket = UdpSocket::bind("127.0.0.1:0");
        let remote_addr: SocketAddr = bootstrap.parse().unwrap();
        if remote_addr.eq(&SocketAddr::from(([127, 0, 0, 1], local_port))) {
            println!("Cannot bootstrap from self.. Exiting.");
            return;
        }
        match (local_socket, remote_addr) {
            (Ok(socket), server_addr) => {
                send_join(
                    &network_view,
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
}

async fn server_run(local_port: &u16, bootstrap: Option<String>) {
    let mut csprng = OsRng;
    let signing_key: SigningKey = SigningKey::generate(&mut csprng);
    let local_peer_id = bs58::encode(signing_key.verifying_key().as_bytes().to_vec()).into_string();
    let network_view = NetworkView::new();
    let local_addr: SocketAddr = SocketAddr::from(([127, 0, 0, 1], *local_port));
    let socket =
        UdpSocket::bind(format!("127.0.0.1:{local_port}")).expect("Could not bind to address");
    println!("PeerId: {:?}", local_peer_id);
    println!("Server started on {:?}", socket);
    let (network_view_tx, network_view_rx) = crossbeam::channel::unbounded::<ImmutableNetworkView>(); 
  
    let shared_local_port = AsyncShared::new(local_port.clone());
    smol::spawn(async move {
        let thread_local_port = shared_local_port.clone();
        let mut latest_view: Option<ImmutableNetworkView> = None;
        loop {
            Timer::after(std::time::Duration::from_millis(10000)).await;
            let try_recv = network_view_rx.try_recv();
            if let Ok(view) = try_recv {
                latest_view = Some(view.clone());
            }
            if let Some(ref view) = latest_view {
                send_shuffle(&view, *thread_local_port).await;
            } 
        }
    }).detach();

    bootstrap_local_node(bootstrap, local_peer_id.to_owned(), *local_port, network_view.clone()).await;

    loop {
        let mut buffer = vec![0u8; MAX_DATAGRAM_SIZE];
        let (size, remote_addr) = socket.recv_from(buffer.as_mut_slice()).unwrap();
        let start = std::time::Instant::now();
        let decoded = bitcode::decode::<PeerMessage>(&buffer[..size]);
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
                                let inserted = network_view
                                    .insert_active(joining_peer_id.to_owned(), peer_connect_info.clone());
                                if let Ok(_) = inserted {
                                    network_view_tx.send(ImmutableNetworkView::from(network_view.clone())).unwrap();
                                    println!("Adding newly discovered peer {} to active view", joining_peer_id);
                                    println!("Active view: {:?}", network_view.active_view);
                                }
                                send_connect(
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
                                for remote_peer in network_view.active_view.borrow().iter() {
                                    /*println!(
                                        "Forward to Peer: {:?} Joining Peer: {:?}",
                                        remote_peer, joining_peer_id
                                    );*/
                                    if remote_peer.0.eq(&joining_peer_id) {
                                        //println!("Skipping forward to join originator");
                                        continue;
                                    }
                                    forward_join(
                                        SocketAddr::from((
                                            remote_peer.1.ip_octets,
                                            remote_peer.1.port,
                                        )),
                                        forward_message,
                                    )
                                    .await;
                                }
                                println!("Active view: {:?}", network_view.active_view);
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
                        //println!("ForwardJoin from: {:?}", remote_addr);
                        if joining_peer_id.eq(&local_peer_id) {
                            continue;
                        }
                        let peer_connect_info = PeerConnectInfo {
                            peer_id: joining_peer_id.to_owned(),
                            ip_octets: ip_octets.to_owned(),
                            port: joining_peer_port,
                        };
                        if ttl == 0 {
                            //println!("TTL expired for ForwardJoin");
                            let inserted = network_view.insert_active(
                                joining_peer_id.to_owned(),
                                PeerConnectInfo {
                                    peer_id: joining_peer_id.to_owned(),
                                    ip_octets: ip_octets.to_owned(),
                                    port: joining_peer_port,
                                },
                            );
                            if let Ok(_) = inserted {
                                network_view_tx.send(ImmutableNetworkView::from(network_view.clone())).unwrap();
                                send_connect(
                                    SocketAddr::from((ip_octets, joining_peer_port)),
                                    ConnectPriority::Normal,
                                    local_peer_id.to_owned(),
                                    local_port.to_owned(),
                                )
                                .await;
                                println!("Adding newly discovered peer {} to active view", joining_peer_id);
                                println!("Active view: {:?}", network_view.active_view.borrow());
                            }
                            continue;
                        }
                        let inserted = network_view
                            .insert_passive(joining_peer_id.to_owned(), peer_connect_info);
                        if let Ok(_) = inserted {
                            network_view_tx.send(ImmutableNetworkView::from(network_view.clone())).unwrap();
                            println!("Adding newly discovered peer {} to passive view", joining_peer_id);
                            println!("Passive view: {:?}", network_view.passive_view);
                        }
                        let forward_message = &PeerMessage::ForwardJoin(
                            ip_octets,
                            joining_peer_id.to_owned(),
                            ttl - 1,
                            joining_peer_port,
                        );
                        // TODO: Prune to randomized subset
                        for peer in network_view.active_view.borrow().iter() {
                            if peer.0.eq(&joining_peer_id) {
                                //println!("Skipping forward to join originator");
                                continue;
                            }
                            forward_join(
                                SocketAddr::from((peer.1.ip_octets, peer.1.port)),
                                forward_message,
                            )
                            .await;
                        }
                    }
                    PeerMessage::Connect(remote_peer_id, priority, remote_port) => {
                        match remote_addr {
                            SocketAddr::V4(addr) => {
                                //println!("Connect from: {:?} via ipv4", addr);
                                let peer_connect_info = PeerConnectInfo {
                                    peer_id: remote_peer_id.to_owned(),
                                    ip_octets: addr.ip().octets(),
                                    port: remote_port,
                                };
                                let inserted = network_view
                                    .insert_active(remote_peer_id.to_owned(), peer_connect_info);
                                if let Ok(_) = inserted {
                                    network_view_tx.send(ImmutableNetworkView::from(network_view.clone())).unwrap();
                                    println!("Adding newly discovered peer {} to active view", remote_peer_id);
                                    println!("Active view: {:?}", network_view.active_view);
                                }
                            }
                            SocketAddr::V6(addr) => {
                                println!("Incoming Connect request via ipv6 from {}. ipv6 is not supported at this time.", addr);
                            }
                        }
                    }
                    PeerMessage::ShuffleRequest(origin_ip, local_peer_info, ttl, origin_port) => {
                        if ttl == 0 {
                            //println!("TTL expired for ShuffleRequest.. responding with ShuffleResponse");
                            let mut shuffled_peers = ShufflePeers::new();
                            let mut active_view_peers = network_view.active_view.borrow().clone().into_values().collect::<Vec<PeerConnectInfo>>();
                            let mut rng = thread_rng();
                            active_view_peers.shuffle(&mut rng);
                            shuffled_peers.extend(active_view_peers[..local_peer_info.len()].to_vec());
                            let response_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
                            let response_addr = SocketAddr::from((origin_ip, origin_port));
                            let shuffle_response = PeerMessage::ShuffleResponse(shuffled_peers);
                            let encoded = bitcode::encode(&shuffle_response);
                            response_socket.send_to(encoded.as_slice(), response_addr).unwrap();
                            socket.send_to(b"ACK", remote_addr).unwrap();
                            continue;
                        }
                        //println!("Shuffle Payload: {:?} {:?} {:?} {:?}", origin_ip, local_peer_info, ttl, origin_port);
                        let mut shuffled_peers = ShufflePeers::new();
                        shuffled_peers.extend(local_peer_info);
                        // TODO add our own?
                        let mut rng = thread_rng();
                        let mut active_view_peers = network_view.active_view.borrow().clone().into_values().collect::<Vec<PeerConnectInfo>>();
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
                        forward_socket.send_to(encoded.as_slice(), forward_addr).unwrap();
                        socket.send_to(
                            b"ACK", 
                            remote_addr
                        ).unwrap();
                    }
                    PeerMessage::ShuffleResponse(shuffled_peers) => {
                        //println!("ShuffleResponse from: {:?} {:?}", remote_addr, shuffled_peers);
                        let mut changed = false;
                        for peer in shuffled_peers {
                            if peer.peer_id.eq(&local_peer_id) {
                                continue;
                            }
                            let inserted = network_view.insert_passive(peer.peer_id.to_owned(), peer.clone());
                            if let Ok(_) = inserted {
                                changed = true;
                                network_view_tx.send(ImmutableNetworkView::from(network_view.clone())).unwrap();
                                println!("Adding newly discovered peer {} to passive view", peer.peer_id);
                            }
                        }
                        if changed {
                            println!("Added peers to passive view from ShuffleResponse");
                            println!("Passive view: {:?}", network_view.passive_view);
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 1 {
        println!("Usage: {} <port> <bootstrap>", args[0]);
        return;
    }
    let port = &args[1].parse::<u16>().expect("Invalid port");
    println!("Local gossip port: {}", port);
    let mut bootstrap: Option<String> = None;
    if args.len() == 3 {
        bootstrap = Some(args[2].clone());
    }
    println!("Bootstrap node addr: {:?}", bootstrap);
    smol::block_on( async {
        server_run(port, bootstrap).await;
    });
}
