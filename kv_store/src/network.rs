use omnipaxos::messages::Message as OPMessage;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{tcp, TcpStream},
    sync::Mutex,
};

use crate::{kv::KVCommand, server::APIResponse, NODES, PID as MY_PID};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Message {
    OmniPaxosMsg(OPMessage<KVCommand>), 
    APIRequest(KVCommand),
    APIResponse(APIResponse),
    Debug(String),
    Reconnect(u64),
}

pub struct Network {
    // Wrap sockets in an Arc<Mutex<...>> for concurrent access and updates.
    sockets: Arc<Mutex<HashMap<u64, tcp::OwnedWriteHalf>>>,
    // Wrap the API socket in a Mutex to allow mutable access when writing.
    api_socket: Arc<Mutex<Option<tcp::OwnedWriteHalf>>>,
    incoming_msg_buf: Arc<Mutex<Vec<Message>>>,
}

impl Network {
    fn get_my_api_addr() -> String {
        format!("net.default.svc.cluster.local:800{}", *MY_PID)
    }

    fn get_peer_addr(receiver_pid: u64) -> String {
        format!("net.default.svc.cluster.local:80{}{}", *MY_PID, receiver_pid)
    }

    /// Sends a serialized JSON message over the appropriate TCP connection.
    pub(crate) async fn send(&self, receiver: u64, msg: Message) {
        let mut data = serde_json::to_vec(&msg).expect("could not serialize msg");
        data.push(b'\n');
        if receiver == 0 {
            let mut api_lock = self.api_socket.lock().await;
            if let Some(writer) = api_lock.as_mut() {
                let _ = writer.write_all(&data).await;
            }
        } else {
            let mut sockets_lock = self.sockets.lock().await;
            if let Some(writer) = sockets_lock.get_mut(&receiver) {
                let _ = writer.write_all(&data).await;
            } else {
                println!("Warning: No connection to peer {}, message not sent", receiver);
            }
        }
    }

    /// Returns all messages received since last called.
    pub(crate) async fn get_received(&mut self) -> Vec<Message> {
        let mut buf = self.incoming_msg_buf.lock().await;
        let ret = buf.to_vec();
        buf.clear();
        ret
    }

    /// Constructs and initializes the Network instance.
    pub(crate) async fn new() -> Self {
        let peers: Vec<u64> = NODES
            .iter()
            .filter(|pid| **pid != *MY_PID)
            .cloned()
            .collect();
        let mut peer_addrs = HashMap::new();
        for pid in &peers {
            peer_addrs.insert(*pid, Self::get_peer_addr(*pid));
        }
        println!("My API Addr: {}", Self::get_my_api_addr());
        let err_msg = format!("Could not connect to API at {}", Self::get_my_api_addr());
        let api_stream = TcpStream::connect(Self::get_my_api_addr())
            .await
            .expect(&err_msg);
        let (api_reader, api_writer) = api_stream.into_split();
        let api_socket = Arc::new(Mutex::new(Some(api_writer)));
        let incoming_msg_buf = Arc::new(Mutex::new(vec![]));

        // Spawn a task to continuously read from the API connection.
        {
            let msg_buf = incoming_msg_buf.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(api_reader);
                let mut data = Vec::new();
                loop {
                    data.clear();
                    match reader.read_until(b'\n', &mut data).await {
                        Ok(0) => {
                            println!("API connection closed. (Reconnection not implemented here.)");
                            break;
                        }
                        Ok(_) => {
                            let msg: Message =
                                serde_json::from_slice(&data).expect("could not deserialize msg");
                            msg_buf.lock().await.push(msg);
                        }
                        Err(e) => {
                            println!("Error reading from API: {}. Retrying...", e);
                            break;
                        }
                    }
                }
            });
        }

        let sockets = Arc::new(Mutex::new(HashMap::new()));

        // For each peer, spawn a task that continuously attempts to connect and read messages.
        for peer in peers {
            let addr = peer_addrs.get(&peer).unwrap().clone();
            let sockets_clone = sockets.clone();
            let msg_buf = incoming_msg_buf.clone();
            tokio::spawn(async move {
                loop {
                    println!("Attempting connection to peer {} at {}", peer, addr);
                    match TcpStream::connect(&addr).await {
                        Ok(stream) => {
                            println!("Connected to peer {} at {}", peer, addr);
                            let (reader, writer) = stream.into_split();
                            // Update the sockets map with the new writer.
                            sockets_clone.lock().await.insert(peer, writer);
                            let mut reader = BufReader::new(reader);
                            let mut data = Vec::new();
                            loop {
                                data.clear();
                                match reader.read_until(b'\n', &mut data).await {
                                    Ok(0) => {
                                        println!("Connection closed by peer {}", peer);
                                        sockets_clone.lock().await.remove(&peer);
                                        break;
                                    }
                                    Ok(_) => {
                                        let msg: Message = serde_json::from_slice(&data)
                                            .expect("could not deserialize msg");
                                        msg_buf.lock().await.push(msg);
                                    }
                                    Err(e) => {
                                        println!(
                                            "Error reading from peer {}: {}. Retrying connection...",
                                            peer, e
                                        );
                                        sockets_clone.lock().await.remove(&peer);
                                        break;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            println!(
                                "Failed to connect to peer {} at {}: {}. Retrying...",
                                peer, addr, e
                            );
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            });
        }

        Self {
            sockets,
            api_socket,
            incoming_msg_buf,
        }
    }
}
