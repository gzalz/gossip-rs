use crate::network_view::*;
use crate::prelude::*;
use crate::server::Server;

mod network_view;
mod prelude;
mod server;

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
        let server: &'static Server = Box::leak(Box::new(Server::new()));
        let _ = server.run(port, bootstrap).await;
    });
}
