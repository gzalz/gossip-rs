async fn client_run(dummy_pubkey: u8) {
    println!("Client started");
    let start = std::time::Instant::now();
    for i in 0 .. 1000 {
        let local_addr = UdpSocket::bind("127.0.0.1:0");
        let remote_addr: Result<SocketAddr, _> = "127.0.0.1:3000".parse();
        match (local_addr, remote_addr){
            (Ok(socket), Ok(server_addr)) => {
                let encoded = bitcode::encode(
                    &SignedMessage {
                        pubkey: [dummy_pubkey; 32],
                        signature: [1u8; 64],
                        topic: [1u8; 32],
                        payload_size: 924,
                        message: [2u8; 892],
                });
                socket.send_to(encoded.as_slice(), server_addr).unwrap();
            }
            _ => {}
        }
    }
    println!("Elapsed time: {:?}", start.elapsed());
    println!("Tx per second: {}", 1000.0 / start.elapsed().as_secs_f64());
}

