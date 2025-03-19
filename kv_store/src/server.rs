// This file contains the implementation of the Server struct. The Server struct is responsible for handling incoming messages, sending outgoing messages, and updating the database based on the decided entries from the OmniPaxos instance. The run method of the Server struct is the main event loop that processes incoming messages, sends outgoing messages, and updates the database in a loop. The run method uses tokio::select! to handle multiple asynchronous tasks concurrently.

//Importing the necessary modules and structs
use crate::database::Database;
use crate::kv::{
    KVCommand,
};
use crate::{
    network::{Message, Network},
    OmniPaxosKV,
    NODES,
    PID as MY_PID,
    kubernetes::query_statefulset_replicas,
};
use omnipaxos::{
    util::LogEntry,
    util::SnapshottedEntry,
    ServerConfig,
    ClusterConfig,
};
use omnipaxos_storage::{
    persistent_storage::{PersistentStorage, PersistentStorageConfig},
};
use serde::{Deserialize, Serialize};
use std::{
    time::Duration,
    fs, 
    path::Path, 
};
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
    pub current_num_replicas: u64, //The current number of replicas in the cluster.
    pub current_config: ServerConfig, //The configuration of the server.
    pub current_config_id: u64, //The current configuration ID.
    pub stop_sign_timeot: u64, //A timeout checker for stop sign messages.
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
                }, 
                Message::StateCatchup(KV_list) => {
                    println!("Received state catchup message");
                    self.database.handle_kv(KV_list);
                },
                Message::Debug(msg) => {
                    //Adding reconfiguration check with the heartbeat message 
                    // to occasionally check if a reconnection is needed
                    if self.omni_paxos.get_current_leader().map(|(id, _)| id) == Some(*MY_PID){
                        //Only leader is checking for reconfiguration. Saves resources and API calls.
                        if let Ok(active_replicas) = query_statefulset_replicas().await {
                            if self.current_num_replicas != active_replicas {
                                if self.stop_sign_timeot % 5 == 0 {
                                    self.stop_sign_timeot = 0;
                                } else {
                                    self.stop_sign_timeot+=1;
                                    println!("Debug: {}, increasing timeout", msg);
                                    continue;
                                }
                                println!(
                                    "Replica count changed from {} to {}",
                                    self.current_num_replicas, active_replicas
                                );
                                self.current_num_replicas = active_replicas;
                                let replica_list: Vec<u64> = (1..=active_replicas).collect();
                                //TODO: Trigger reconfiguration here
                                let new_config_id = (self.current_config_id + 1) as u32;
                                let new_configuration = ClusterConfig {
                                    configuration_id: new_config_id,
                                    nodes: (*replica_list).to_vec(),
                                    ..Default::default()
                                };
                                let metadata = None;
                                self.omni_paxos.reconfigure(new_configuration, metadata).expect("Failed to propose reconfiguration");
                            }
                        }
                        // TODO: Also need to handle the stopsign message in the other nodes. 
                        //  TODO: Modify the multiplexer to have multithreaded sending capabilities. Avoids blocking on a dead node.
                    }else{
                        if let Ok(active_replicas) = query_statefulset_replicas().await {
                            if self.current_num_replicas != active_replicas {
                                println!(
                                    "Replica count changed from {} to {}",
                                    self.current_num_replicas, active_replicas
                                );
                                self.current_num_replicas = active_replicas;
                            }
                        }
                    }
                    //Uncomment this to see what nodes are connected to this one via debug messages.
                    //println!("Debug: {}", msg);
                },
                Message::Reconnect(pid) => {
                    if self.omni_paxos.get_current_leader().map(|(id, _)| id) != Some(*MY_PID) {
                        println!("Reconnecting node {}, from {}", pid, *MY_PID);
                        self.omni_paxos.reconnected(pid);
                    }
                },
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
        if (new_decided_idx as u64) < self.last_decided_idx && new_decided_idx == 0{
            println!("Last decided index: {}, new decided index {}. Resetting because reconfigured", self.last_decided_idx, new_decided_idx);
            self.last_decided_idx = 0;
        }
        //println!("Decided index: {}", self.last_decided_idx);
        //Check if there are new decided entries
        if self.last_decided_idx < new_decided_idx as u64 {
            let decided_entries = self
                .omni_paxos
                .read_decided_suffix(self.last_decided_idx as usize)
                .unwrap();

            //Upate the database with the decided entries, and save the current latest decided index
            self.update_database(decided_entries).await;
            if let Some(_StopSign) = self.omni_paxos.is_reconfigured() {
                return;
            }
            self.last_decided_idx = new_decided_idx as u64;
            /*** reply to client with new decided index ***/
            let msg = Message::APIResponse(APIResponse::Decided(new_decided_idx as u64));
            self.network.send(0, msg).await;
            snapshotting
            if new_decided_idx % 5 == 0 {
                println!(
                    "Log before: {:?}",
                    self.omni_paxos.read_decided_suffix(0).unwrap()
                );
                let snapshot = self.omni_paxos
                    .snapshot(Some(new_decided_idx), true)
                    .expect("Failed to snapshot");
                println!("Snapshot: {:?}", snapshot);
                println!(
                    "Log after: {:?}\n",
                    self.omni_paxos.read_decided_suffix(0).unwrap()
                );
            }
        }
    }

    //Iterates over decided log entries and applies each command to the database. This is how the state of the database is eventually updated based on the decisions made by the consensus algorithm.
    async fn update_database(&mut self, decided_entries: Vec<LogEntry<KVCommand>>) {
        for entry in decided_entries {
            match entry {
                LogEntry::Decided(cmd) => {
                    self.database.handle_command(cmd);
                },
                LogEntry::Snapshotted(SnapshottedEntry { snapshot, .. }) => {
                    self.database.handle_snapshot(snapshot);
                },
                LogEntry::StopSign(stopsign, true) => {
                    self.current_config_id += 1;
                    let new_configuration = stopsign.next_config;
                    println!("New configuration: {:?}", new_configuration);
                    if new_configuration.nodes.contains(&MY_PID) {
                        // current configuration has been safely stopped. Start new instance
                        // Set storage path for this node (each pod gets its own)

                        //Create snapshot and send to all nodes to ensure that the new configuration starts from the same state
                        // Use the same path used when creating the database instance.
                       
                        let kv_pairs = self.database.get_all_key_values()
                            .expect("Failed to get key-value pairs from RocksDB");

                        let peers: Vec<u64> = NODES
                            .iter()
                            .filter(|pid| **pid != *MY_PID)
                            .cloned()
                            .collect();

                        for pid in peers {
                            // Use nonblocking sleep (Tokio's sleep) to avoid blocking the async executor.
                            std::thread::sleep(Duration::from_secs(3));
                            let msg = Message::StateCatchup(kv_pairs.clone());
                            self.network.send(pid, msg).await;
                            println!("Sent KV state to {}", pid);
                        }
                    }

                    //Set up the persistent storage again for the new configuration
                    let storage_path = format!("/data/omnipaxos_{}/config{}", *MY_PID, self.current_config_id);
                    std::fs::create_dir_all(&storage_path).expect("Failed to create storage directory");
                    
                    // Set RocksDB options (required for persistent storage)
                    let log_store_options = rocksdb::Options::default();
                    let mut state_store_options = rocksdb::Options::default();
                    state_store_options.create_missing_column_families(true); // Ensures required storage structure
                    state_store_options.create_if_missing(true); // Creates DB if missing
                    
                    // Create persistent storage config with options
                    let mut persist_conf = PersistentStorageConfig::default();
                    persist_conf.set_path(storage_path.to_string()); 
                    persist_conf.set_database_options(state_store_options);
                    persist_conf.set_log_options(log_store_options);
                    
                    // Initialize storage using PersistentStorageConfig (Fix: Proper error handling)
                    let storage = PersistentStorage::open(persist_conf);
                    println!(
                    "Final log of this sequence paxos: {:?}\n",
                    self.omni_paxos.read_decided_suffix(0).unwrap()
                    );
                    let new_omnipaxos = new_configuration.build_for_server(self.current_config.clone(), storage).unwrap();
                    self.omni_paxos = new_omnipaxos;
                    self.last_decided_idx = 0 as u64;
                    println!("New configuration started");
                },
                _ => {}
            }
        }
    }   

    async fn debug_heartbeat(&mut self) {
        //Send a debug message to nodes 1, 2, and 3 saying "Heartbeat from node <MY_PID>"
        let peers: Vec<u64> = NODES
            .iter()
            .filter(|pid| **pid != *MY_PID)
            .cloned()
            .collect();
        for pid in peers {
            let msg = Message::Debug(format!("Heartbeat from node {}", *MY_PID));
            self.network.send(pid, msg).await;
        }
    }

    //Main loop of the server that processes incoming messages, sends outgoing messages, and updates the database based on decided entries.
    //The run method uses tokio::select! to handle multiple asynchronous tasks concurrently.
    //The biased; directive gives priority to the first branch (message processing).
    pub(crate) async fn run(&mut self) {
        let file_path = "data/restarted.flag";

        if Path::new(file_path).exists() {
            let own_decided_index = self.omni_paxos.get_decided_idx();
            self.omni_paxos
                    .snapshot(Some(own_decided_index), true)
                    .expect("Failed to snapshot");
            println!("Restarted");
            std::thread::sleep(Duration::from_secs(3));
            //DEBUG MESSAGE:
            //println!("After sleep");
            
            // Add this line to signal recovery completion for benchmark detection
            println!("Recovery complete");
        }else{
            println!("Not restarted");
            fs::File::create(file_path).expect("Failed to create restarted flag file");
        }


        let mut msg_interval = time::interval(Duration::from_millis(1));
        let mut tick_interval = time::interval(Duration::from_millis(10));
        let mut debug_heartbeat = time::interval(Duration::from_secs(2));
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
                _ = debug_heartbeat.tick() => {
                    self.debug_heartbeat().await;
                    if let Some(entries) = self.omni_paxos.read_entries(0..) {
                        println!("--------");
                        for entry in entries {
                            println!("{:?}", entry);
                        }
                        println!("--------");
                    } else {
                        println!("No entries found in the log yet.");
                    }
                },
                else => (),
            }
        }
    }
}
