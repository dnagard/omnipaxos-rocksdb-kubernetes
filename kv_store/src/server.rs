// This file contains the implementation of the Server struct. The Server struct is responsible for handling incoming messages, sending outgoing messages, and updating the database based on the decided entries from the OmniPaxos instance. The run method of the Server struct is the main event loop that processes incoming messages, sends outgoing messages, and updates the database in a loop. The run method uses tokio::select! to handle multiple asynchronous tasks concurrently.

//Importing the necessary modules and structs
use crate::database::Database;
use crate::kv::KVCommand;
use crate::{
    network::{Message, Network},
    OmniPaxosKV,
};
use omnipaxos::util::LogEntry;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time;

//Defines the types of responses the server can send back to a client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum APIResponse {
    //Indicates that a new log index has been decided
    Decided(u64),
    //Represents the result of a GET operation on the kv store, where the first part is the key and the second part is the optional value (Can be None if the key does not exist)
    Get(String, Option<String>),
}

//Defines the Server struct
pub struct Server {
    pub omni_paxos: OmniPaxosKV, //OmniPaxos instance for the server
    pub network: Network, //Manages sending and receiving messages from other nodes or clients.
    pub database: Database, //Handles the actual storage and retrieval of key-value pairs.
    pub last_decided_idx: u64, //Tracks the last log entry that was "decided" (i.e., agreed upon by the consensus process) and applied to the database.
}

// Implementing the Server struct
impl Server {

    
    async fn process_incoming_msgs(&mut self) {
        let messages = self.network.get_received().await;
        for msg in messages {
            match msg {
                Message::APIRequest(kv_cmd) => match kv_cmd {
                    KVCommand::Get(key) => {
                        let value = self.database.handle_command(KVCommand::Get(key.clone()));
                        let msg = Message::APIResponse(APIResponse::Get(key, value));
                        // Send response to client (0 is the clientID)
                        self.network.send(0, msg).await;
                    }
                    cmd => {
                        self.omni_paxos.append(cmd).unwrap();
                    }
                },
                Message::OmniPaxosMsg(msg) => {
                    self.omni_paxos.handle_incoming(msg);
                }
                _ => unimplemented!(),
            }
        }
    }

    //Collects messages generated by OmniPaxos component that need to be sent to other nodes and sends them
    async fn send_outgoing_msgs(&mut self) {
        let messages = self.omni_paxos.outgoing_messages();
        for msg in messages {
            let receiver = msg.get_receiver();
            self.network
                .send(receiver, Message::OmniPaxosMsg(msg))
                .await;
        }
    }

    async fn handle_decided_entries(&mut self) {
        let new_decided_idx = self.omni_paxos.get_decided_idx();
        //Check if there are new decided entries
        if self.last_decided_idx < new_decided_idx as u64 {
            let decided_entries = self
                .omni_paxos
                .read_decided_suffix(self.last_decided_idx as usize)
                .unwrap();

            //Upate the database with the decided entries, and save the current latest decided index
            self.update_database(decided_entries);
            self.last_decided_idx = new_decided_idx as u64;
            /*** reply to client with new decided index ***/
            let msg = Message::APIResponse(APIResponse::Decided(new_decided_idx as u64));
            self.network.send(0, msg).await;
            // snapshotting
            if new_decided_idx % 5 == 0 {
                println!(
                    "Log before: {:?}",
                    self.omni_paxos.read_decided_suffix(0).unwrap()
                );
                self.omni_paxos
                    .snapshot(Some(new_decided_idx), true)
                    .expect("Failed to snapshot");
                println!(
                    "Log after: {:?}\n",
                    self.omni_paxos.read_decided_suffix(0).unwrap()
                );
            }
        }
    }

    //Iterates over decided log entries and applies each command to the database. This is how the state of the database is eventually updated based on the decisions made by the consensus algorithm.
    fn update_database(&self, decided_entries: Vec<LogEntry<KVCommand>>) {
        for entry in decided_entries {
            match entry {
                LogEntry::Decided(cmd) => {
                    self.database.handle_command(cmd);
                }
                _ => {}
            }
        }
    }

    //Main loop of the server that processes incoming messages, sends outgoing messages, and updates the database based on decided entries.
    //The run method uses tokio::select! to handle multiple asynchronous tasks concurrently.
    //The biased; directive gives priority to the first branch (message processing).
    pub(crate) async fn run(&mut self) {
        // First, try to recover state from disk or other nodes
        self.recover_state().await;
        
        let mut msg_interval = time::interval(Duration::from_millis(1));
        let mut tick_interval = time::interval(Duration::from_millis(10));
        let mut reconnect_interval = time::interval(Duration::from_secs(5));
        
        loop {
            tokio::select! {
                biased;
                _ = msg_interval.tick() => {
                    self.process_incoming_msgs().await;
                    self.send_outgoing_msgs().await;
                    self.handle_decided_entries().await;
                },
                _ = tick_interval.tick() => {
                    self.omni_paxos.tick();
                },
                _ = reconnect_interval.tick() => {
                    self.network.check_and_reconnect().await;
                },
                else => (),
            }
        }
    }

    async fn recover_state(&mut self) {
        println!("Starting recovery process...");
        
        // Give some time for network connections to establish
        time::sleep(Duration::from_secs(2)).await;
        
        // Try to catch up with the cluster by requesting the latest decided index
        // This will trigger OmniPaxos to sync the log
        for _ in 0..10 {
            self.omni_paxos.tick();
            self.send_outgoing_msgs().await;
            time::sleep(Duration::from_millis(100)).await;
            self.process_incoming_msgs().await;
        }
        
        // Check if we've recovered any entries
        let decided_idx = self.omni_paxos.get_decided_idx();
        if decided_idx > 0 {
            println!("Recovered log up to index {}", decided_idx);
            
            // Apply all decided entries to our database
            if let Ok(entries) = self.omni_paxos.read_decided_suffix(0) {
                self.update_database(entries);
                self.last_decided_idx = decided_idx as u64;
                println!("Applied {} log entries to database", decided_idx);
            }
        } else {
            println!("No entries recovered, starting with empty state");
        }
    }
}
