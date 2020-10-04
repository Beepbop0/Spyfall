mod broker;
mod client;

use crate::broker::broker_actor;
use crate::client::client_actor;
use async_tungstenite;
use smol::{self, channel, net::TcpListener, stream::StreamExt};

const HOST: &str = "localhost:4212";

fn main() {
    println!("Server hosted on {}", HOST);
    smol::block_on(deploy());
}

async fn deploy() {
    let listener = TcpListener::bind(HOST).await.expect("Failed to bind");
    let mut incoming_conns = listener.incoming();
    let (broker_tx, broker_rx) = channel::unbounded();
    smol::spawn(broker_actor(broker_rx)).detach();

    println!("listening for new connections...");
    while let Some(tcp_stream) = incoming_conns.next().await {
        if let Ok(tcp_stream) = tcp_stream {
            println!(
                "Handling connection from: {}",
                tcp_stream.peer_addr().unwrap()
            );
            if let Ok(websocket) = async_tungstenite::accept_async(tcp_stream).await {
                smol::spawn(client_actor(websocket, broker_tx.clone())).detach();
            }
        }
    }
}
