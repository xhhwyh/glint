use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

#[derive(Clone, Debug, Default)]
pub struct ReadFileState {
    records: Arc<Mutex<HashMap<PathBuf, ReadFileRecord>>>,
}

#[derive(Clone, Debug)]
pub struct ReadFileRecord {
    pub content: String,
    pub modified: Option<SystemTime>,
    pub partial: bool,
}

impl ReadFileState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&self) {
        self.records.lock().expect("read state lock").clear();
    }

    pub fn get(&self, path: &Path) -> Option<ReadFileRecord> {
        self.records
            .lock()
            .expect("read state lock")
            .get(path)
            .cloned()
    }

    pub fn record(&self, path: PathBuf, content: String, partial: bool) {
        let modified = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok();
        self.records.lock().expect("read state lock").insert(
            path,
            ReadFileRecord {
                content,
                modified,
                partial,
            },
        );
    }
}
