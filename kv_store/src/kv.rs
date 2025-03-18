use std::collections::HashMap;
use omnipaxos::storage::{Entry, Snapshot};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KVCommand {
    Put(KeyValue),
    Delete(String),
    Get(String),
}

impl Entry for KVCommand {
    type Snapshot = KVSnapshot;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KVSnapshot {
    pub snapshotted: HashMap<String, String>,
    pub deleted_keys: Vec<String>,
}

impl Snapshot<KVCommand> for KVSnapshot {
    fn create(entries: &[KVCommand]) -> Self {
        let mut snapshotted = HashMap::new();
        let mut deleted_keys: Vec<String> = Vec::new();
        for e in entries {
            match e {
                KVCommand::Put(KeyValue { key, value }) => {
                    snapshotted.insert(key.clone(), value.clone());
                    println!("Put key: {}, value: {}", key, value);
                }
                KVCommand::Delete(key) => {
                    if snapshotted.remove(key).is_none() {
                        // key was not in the snapshot
                        println!("Not in snapshot key: {}", key);
                        deleted_keys.push(key.clone());
                    }
                    deleted_keys.push(key.clone());
                }
                KVCommand::Get(_) => (),
            }
        }
        // remove keys that were put back
        deleted_keys.retain(|k| !snapshotted.contains_key(k));
        println!("Deleted keys: {:?}", deleted_keys);
        println!("Snapshotted: {:?}", snapshotted);

        Self {
            snapshotted,
            deleted_keys,
        }
    }

    fn merge(&mut self, delta: Self) {
        println!("Merging snapshot");
        println!("Delta snapshot: {:?}", delta.snapshotted);
        println!("Delta deleted keys: {:?}", delta.deleted_keys);
        println!("Current snapshot: {:?}", self.snapshotted);
        println!("Current deleted keys: {:?}", self.deleted_keys);
        for (k, v) in delta.snapshotted {
            self.snapshotted.insert(k, v);
        }
        for k in delta.deleted_keys {
            self.snapshotted.remove(&k);
            self.deleted_keys.push(k);
        }
        self.deleted_keys.retain(|k| !self.snapshotted.contains_key(k));
    }

    fn use_snapshots() -> bool {
        true
    }
}
