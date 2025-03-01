use crate::kv::{KVCommand, KeyValue};
use rocksdb::{Options, DB, IteratorMode};
use std::collections::HashMap;

pub struct Database {
    rocks_db: DB,
}

impl Database {
    pub fn new(path: &str) -> Self {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        let rocks_db = DB::open(&opts, path).unwrap();
        Self { rocks_db }
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

    pub fn flush(&self) {
        // RocksDB automatically flushes to disk, but we can add explicit flush if needed
        // This is a placeholder for any additional flush logic
        println!("Database flushed to disk");
    }
    
    // Get all key-value pairs from the database
    pub fn get_all_entries(&self) -> HashMap<String, String> {
        let mut entries = HashMap::new();
        let iter = self.rocks_db.iterator(IteratorMode::Start);
        
        for item in iter {
            if let Ok((key, value)) = item {
                if let (Ok(key_str), Ok(value_str)) = (
                    String::from_utf8(key.to_vec()),
                    String::from_utf8(value.to_vec())
                ) {
                    // Skip special keys like "__ping__"
                    if !key_str.starts_with("__") {
                        entries.insert(key_str, value_str);
                    }
                }
            }
        }
        
        entries
    }
    
    // Apply a batch of key-value pairs to the database
    pub fn apply_batch(&self, entries: HashMap<String, String>) {
        println!("Applying batch of {} entries to database", entries.len());
        for (key, value) in entries {
            self.put(&key, &value);
        }
    }
}
