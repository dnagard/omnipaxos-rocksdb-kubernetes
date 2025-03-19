use crate::kv::KVCommand;
use crate::server::Server;
use omnipaxos::*;
use omnipaxos_storage::persistent_storage::{PersistentStorage, PersistentStorageConfig};
use rocksdb;
use std::env;
use tokio;

#[macro_use]
extern crate lazy_static;

mod database;
mod kv;
mod network;
mod server;
mod kubernetes;

lazy_static! {
    pub static ref NODES: Vec<u64> = if let Ok(var) = env::var("NODES") {
        serde_json::from_str::<Vec<u64>>(&var).expect("wrong config format")
    } else {
        vec![]
    };
    pub static ref PID: u64 = if let Ok(var) = env::var("PID") {
        let x = var.parse().expect("PIDs must be u64");
        if x == 0 {
            panic!("PIDs cannot be 0")
        } else {
            x
        }
    } else {
        panic!("missing PID")
    };
}

type OmniPaxosKV = OmniPaxos<KVCommand, PersistentStorage<KVCommand>>;

#[tokio::main]
async fn main() {
    let server_config = ServerConfig {
        pid: *PID,
        election_tick_timeout: 5,
        ..Default::default()
    };

    let mut replica_list: Vec<u64> = vec![];
    let mut num_replicas: u64 = 3;
    match kubernetes::query_statefulset_replicas().await {
        Ok(replicas) => {
            if replicas > 3 {
                num_replicas = replicas;
            }

        }
        Err(e) => eprintln!("Error querying replicas: {}", e),
    }
    replica_list = (1..=num_replicas).collect();

    let cluster_config = ClusterConfig {
        configuration_id: 1,
        nodes: (*replica_list).to_vec(),
        ..Default::default()
    };
    let op_config = OmniPaxosConfig {
        server_config: server_config.clone(),
        cluster_config,
    };
    
    // Set storage path for this node (each pod gets its own)
    let storage_path = format!("/data/omnipaxos_{}/config_1", *PID);
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

    // Recover OmniPaxos state from storage
    let omni_paxos = op_config
        .build(storage)
        .expect("Failed to build OmniPaxos");
    
    // Ensure database storage also uses /data/
    let mut server = Server {
        omni_paxos,
        network: network::Network::new().await,
        database: database::Database::new(format!("/data/db_{}", *PID).as_str()),
        last_decided_idx: 0,
        current_num_replicas: num_replicas,
        current_config: server_config.clone(),
        current_config_id: 1,
        stop_sign_timeot: 0,
    };
    server.run().await;
}