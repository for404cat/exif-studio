use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::{ImageInfo, VideoInfo};

const CACHE_VERSION: u32 = 1;
const MAX_CACHE_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_RECORDS: usize = 50_000;
const MAX_AGE_SECS: u64 = 90 * 24 * 60 * 60;

#[derive(Clone)]
pub struct CachedInfo {
    pub image: Option<ImageInfo>,
    pub video: Option<VideoInfo>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Record {
    size: u64,
    modified_ns: i64,
    used_at: u64,
    #[serde(default)]
    image: Option<ImageInfo>,
    #[serde(default)]
    video: Option<VideoInfo>,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: HashMap<String, Record>,
}

pub struct MetadataCache {
    path: PathBuf,
    records: Mutex<HashMap<String, Record>>,
    save_lock: Mutex<()>,
    dirty: AtomicBool,
}

impl MetadataCache {
    pub fn load(path: PathBuf) -> Self {
        let records = read_cache(&path);
        Self {
            path,
            records: Mutex::new(records),
            save_lock: Mutex::new(()),
            dirty: AtomicBool::new(false),
        }
    }

    pub fn lookup(&self, path: &Path, size: u64, modified_ns: i64) -> Option<CachedInfo> {
        let key = path_key(path);
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = records.get_mut(&key)?;
        if record.size != size || record.modified_ns != modified_ns {
            return None;
        }
        record.used_at = unix_seconds();
        let info = CachedInfo {
            image: record.image.clone(),
            video: record.video.clone(),
        };
        drop(records);
        self.dirty.store(true, Ordering::Relaxed);
        if info.image.is_none() && info.video.is_none() {
            None
        } else {
            Some(info)
        }
    }

    pub fn put(
        &self,
        path: &Path,
        size: u64,
        modified_ns: i64,
        image: Option<ImageInfo>,
        video: Option<VideoInfo>,
    ) {
        if image.is_none() && video.is_none() {
            return;
        }
        let key = path_key(path);
        let record = Record {
            size,
            modified_ns,
            used_at: unix_seconds(),
            image,
            video,
        };
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, record);
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn save(&self) {
        if !self.dirty.load(Ordering::Relaxed) {
            return;
        }
        let _save_guard = self
            .save_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = unix_seconds();
        let snapshot = {
            let mut records = self
                .records
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            records.retain(|_, record| now.saturating_sub(record.used_at) <= MAX_AGE_SECS);
            if records.len() > MAX_RECORDS {
                let mut newest: Vec<(String, u64)> = records
                    .iter()
                    .map(|(key, record)| (key.clone(), record.used_at))
                    .collect();
                newest.sort_by_key(|entry| std::cmp::Reverse(entry.1));
                let keep: std::collections::HashSet<String> = newest
                    .into_iter()
                    .take(MAX_RECORDS)
                    .map(|(key, _)| key)
                    .collect();
                records.retain(|key, _| keep.contains(key));
            }
            self.dirty.store(false, Ordering::Release);
            records.clone()
        };
        if write_cache(&self.path, snapshot).is_err() {
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    pub fn forget(&self, path: &Path) {
        let removed = self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&path_key(path))
            .is_some();
        if removed {
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    pub fn clear(&self) {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.dirty.store(false, Ordering::Relaxed);
        let _ = fs::remove_file(&self.path);
    }

    pub fn file_size(&self) -> u64 {
        fs::metadata(&self.path)
            .map(|value| value.len())
            .unwrap_or(0)
    }

    pub fn len(&self) -> usize {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn read_cache(path: &Path) -> HashMap<String, Record> {
    let Ok(metadata) = fs::metadata(path) else {
        return HashMap::new();
    };
    if metadata.len() > MAX_CACHE_FILE_BYTES {
        return HashMap::new();
    }
    let Ok(bytes) = fs::read(path) else {
        return HashMap::new();
    };
    let Ok(cache) = serde_json::from_slice::<CacheFile>(&bytes) else {
        return HashMap::new();
    };
    if cache.version != CACHE_VERSION {
        return HashMap::new();
    }
    cache.entries
}

fn write_cache(path: &Path, entries: HashMap<String, Record>) -> Result<(), std::io::Error> {
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::other("缓存路径没有父目录"));
    };
    fs::create_dir_all(parent)?;
    let cache = CacheFile {
        version: CACHE_VERSION,
        entries,
    };
    let bytes =
        serde_json::to_vec(&cache).map_err(|error| std::io::Error::other(error.to_string()))?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, bytes)?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

fn path_key(path: &Path) -> String {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut key = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        key.push_str(&format!("{byte:02x}"));
    }
    key
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.json");
        let cache = MetadataCache::load(path.clone());
        let photo = Path::new("/photos/a.jpg");
        cache.put(
            photo,
            1024,
            99,
            Some(ImageInfo {
                width: Some(1920),
                height: Some(1080),
                make: Some("Panasonic".to_string()),
                ..ImageInfo::default()
            }),
            None,
        );
        cache.save();
        assert!(path.exists());

        let reloaded = MetadataCache::load(path);
        let info = reloaded.lookup(photo, 1024, 99).unwrap();
        assert_eq!(info.image.unwrap().make.as_deref(), Some("Panasonic"));
        assert!(reloaded.lookup(photo, 1024, 100).is_none());
        assert!(
            reloaded
                .lookup(Path::new("/photos/b.jpg"), 1024, 99)
                .is_none()
        );
    }

    #[test]
    fn clear_removes_file_and_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.json");
        let cache = MetadataCache::load(path.clone());
        cache.put(
            Path::new("/photos/a.jpg"),
            1,
            1,
            Some(ImageInfo::default()),
            None,
        );
        cache.save();
        cache.clear();
        assert!(!path.exists());
        assert!(cache.lookup(Path::new("/photos/a.jpg"), 1, 1).is_none());
    }
}
