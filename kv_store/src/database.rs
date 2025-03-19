use crate::kv::{KVCommand, KeyValue, KVSnapshot};
use rocksdb::{Options, DB, IteratorMode};
use std::{
    error::Error,
    collections::HashMap,
};

pub struct Database {
    rocks_db: DB,
}

impl Database {
    
    pub fn get_all_key_values(&self) -> Result<Vec<(String, String)>, Box<dyn Error>> {
        let mut kv_list = Vec::new();
        // Create an iterator starting at the beginning of the RocksDB instance.
        let iter = self.rocks_db.iterator(IteratorMode::Start);
        
        for result in iter {
            match result {
                Ok((key, value)) => {
                    // Convert key and value from Vec<u8> to String.
                    let key_str = String::from_utf8(key.to_vec())?;
                    let value_str = String::from_utf8(value.to_vec())?;
                    kv_list.push((key_str, value_str));
                },
                Err(e) => {
                    // Optionally handle or log the error.
                    eprintln!("Error reading key-value pair: {:?}", e);
                }
            }
        }
        Ok(kv_list)
    }

    pub fn handle_kv(&self, kv_list: Vec<(String, String)>) {
        // Build a HashMap from the new state for quick lookup.
        let new_state: HashMap<String, String> = kv_list.into_iter().collect();

        // Update/insert all key-value pairs from the new state.
        for (key, value) in new_state.iter() {
            self.put(key, value);
        }

        // Iterate over all current keys in the destination database.
        let current_keys: Vec<String> = self
            .rocks_db
            .iterator(IteratorMode::Start)
            .filter_map(|result| {
                match result {
                    Ok((key, _)) => Some(String::from_utf8(key.to_vec()).ok()).flatten(),
                    Err(_) => None, // Handle errors as needed
                }
            })
            .collect();

        // Delete any key that is in the destination but not in the new state.
        for key in current_keys {
            if !new_state.contains_key(&key) {
                self.delete(&key);
            }
        }
    }

    pub fn new(path: &str) -> Self {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        let rocks_db = DB::open(&opts, path).unwrap();
        Self { rocks_db }
    }

    pub fn handle_snapshot(&self, snapshot: KVSnapshot) {
        //DEBUG MESSAGE:
        //println!("Handling snapshot {:?}", snapshot);
        for (key, value) in snapshot.snapshotted {
            self.put(&key, &value);
        }
        for key in snapshot.deleted_keys {
            self.delete(&key);
        }
    }

    pub fn handle_command(&self, command: KVCommand) -> Option<String> {
        match command {
            KVCommand::Put(KeyValue { key, value }) => {
                self.put(&key, &value);
                None
            }
            KVCommand::Delete(key) => {
                self.delete(&key);
                None
            }
            KVCommand::Get(key) => self.get(key.as_str()),
        }
    }

    fn get(&self, key: &str) -> Option<String> {
        match self.rocks_db.get(key.as_bytes()) {
            Ok(Some(value)) => {
                let value = String::from_utf8(value).unwrap();
                Some(value)
            }
            Ok(None) => None,
            Err(e) => panic!("failed to get value: {}", e),
        }
    }

    fn put(&self, key: &str, value: &str) {
        match self.rocks_db.put(key.as_bytes(), value.as_bytes()) {
            Ok(_) => {}
            Err(e) => panic!("failed to put value: {}", e),
        }
    }

    fn delete(&self, key: &str) {
        match self.rocks_db.delete(key.as_bytes()) {
            Ok(_) => {}
            Err(e) => panic!("failed to delete value: {}", e),
        }
    }
}
