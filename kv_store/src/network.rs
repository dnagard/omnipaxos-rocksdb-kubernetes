// This file defines a networking module for a distributed key-value store that uses OmniPaxos for consensus. It provides the mechanisms for sending and receiving messages between nodes and clients, as well as processing incoming messages and sending outgoing messages. The Network struct manages the connections to other nodes and clients, while the Message enum defines the types of messages that can be sent and received.

use omnipaxos::messages::Message as OPMessage;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{tcp, TcpStream},
    sync::Mutex,
};

use crate::{kv::KVCommand, server::APIResponse, NODES, PID as MY_PID};

// Represents the types of messages that can be sent and received.
// Derives Serialize and Deserialize (via serde) to allow for serialization and deserialization of the enum variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Message {
    OmniPaxosMsg(OPMessage<KVCommand>), 
    APIRequest(KVCommand), //Represents a request from a client to the server
    APIResponse(APIResponse), //Represents a response from the server to a client
}

//Defining the Network struct
pub struct Network {
    //Maps peer IDs to the write-half of their TCP connections.
    sockets: HashMap<u64, tcp::OwnedWriteHalf>, 
    //An optional write-half for teh connection to the API client (using receiver ID 0).
    api_socket: Option<tcp::OwnedWriteHalf>,
    //A shared, thread-safe (wrapped in Arc<Mutex<...>>) buffer for storing incoming messages until they are processed.
    incoming_msg_buf: Arc<Mutex<Vec<Message>>>,
}

// Implementing the Network struct
impl Network {

    //Constructs the address for the API connection based on the node's PID
    //Used to connect to the API clinet for sending or receiving messages. 
    // {} is replaced by the dereferenced PID value.
    fn get_my_api_addr() -> String {
        format!("net.default.svc.cluster.local:800{}", *MY_PID)
    }

    //Generates the address for a peer connection based on the current nodes PID and the receiver's PID.
    //Used to connect to other nodes for sending or receiving messages.
    // {}{} is replaced by the current node's PID and the receiver's PID.
    fn get_peer_addr(receiver_pid: u64) -> String {
        format!(
            "net.default.svc.cluster.local:80{}{}",
            *MY_PID, receiver_pid
        )
    }

    /// Sends a serialized JSON message over the appropriate TCP connection
    /// u64 0 is the Client.
    pub(crate) async fn send(&mut self, receiver: u64, msg: Message) {
        //If the receiver is 0, send the message over teh api_socket; 
        // otherwise retrieve the corresponding peer socket from the sockets hashmap.
        // This if statement is treated as a RHV (right-hand value) expression, setting the writer variable to the appropriate connection.
        let writer = if receiver == 0 {
            self.api_socket.as_mut()
        } else {
            self.sockets.get_mut(&receiver)
        };

        //Some(writer) = writer is a pattern to check if a writer exists. If writer is 'Some', a valid socket exists => execute the block
        if let Some(writer) = writer {
            let mut data = serde_json::to_vec(&msg).expect("could not serialize msg");
            data.push(b'\n');
            writer.write_all(&data).await.unwrap(); //Write the serialized message to the socket. Await waits for the write to complete. Unwrap panics if the write fails.
        }
    }

    /// Returns all messages received since last called.
    // Locks the shared message buffer, clones its contentens to return, and clears the buffer, releasing the lock (when it goes out of scope).
    pub(crate) async fn get_received(&mut self) -> Vec<Message> {
        let mut buf = self.incoming_msg_buf.lock().await;
        let ret = buf.to_vec();
        buf.clear();
        ret
    }

    /* 
    Sets up all the necessary connections and asynchronous tasks required for the network layer to operate. 
    Returns an initialized Network instance ready to send and receive messages. 
    -----------------------------------------------------------
    * Peer discovery: Creates a list of peer PIDs by filtering the NODES list to exclude the current node's PID.
    * Connecting to the API: Constructs the API address using get_my_api_addr() and connects to it using TCP. Splits the connection into a reader and writer, storing the writer in api_socket.
    * Shared message buffer: Creates a shared, thread-safe buffer for storing incoming messages across multiple async tasks.
    * Spawning reader tasks:
        *API reader task: A Tokio task is spawend that continously reads from the API connection using a buffered reader. Every timea  complete message (ending in a newline) is received, it is deserialied and appended to teh sahred message buffer.
        * Peer reader task: For each peer, a similar task is spawned to read incoming messages from that peer's TCP connection. These messages are alsod eserialized and appended to the shared message buffer.
    *Storing Peer Writers: The writer half of each TCP connection is stored int he sockets HashMap so that messages can be sent to those peers later.  
    */
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
        let api_socket = Some(api_writer);
        let incoming_msg_buf = Arc::new(Mutex::new(vec![]));
        let msg_buf = incoming_msg_buf.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(api_reader);
            let mut data = Vec::new();
            loop {
                data.clear();
                let bytes_read = reader.read_until(b'\n', &mut data).await;
                if bytes_read.is_err() {
                    // stream ended?
                    panic!("stream ended?")
                }
                let msg: Message =
                    serde_json::from_slice(&data).expect("could not deserialize msg");
                msg_buf.lock().await.push(msg);
            }
        });

        let mut sockets = HashMap::new();
        for peer in &peers {
            let addr = peer_addrs.get(&peer).unwrap().clone();
            println!("Connecting to {}", addr);
            let stream = TcpStream::connect(addr).await.unwrap();
            let (reader, writer) = stream.into_split();
            sockets.insert(*peer, writer);
            let msg_buf = incoming_msg_buf.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(reader);
                let mut data = Vec::new();
                loop {
                    data.clear();
                    let bytes_read = reader.read_until(b'\n', &mut data).await;
                    if bytes_read.is_err() {
                        // stream ended?
                        panic!("stream ended?")
                    }
                    let msg: Message =
                        serde_json::from_slice(&data).expect("could not deserialize msg");
                    msg_buf.lock().await.push(msg);
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
