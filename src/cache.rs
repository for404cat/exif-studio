use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use walkdir::WalkDir;

const CACHE_EXTENSION: &str = "cache";

struct IndexEntry {
    size: u64,
    stamp: u64,
}

#[derive(Default)]
struct CacheIndex {
    total: u64,
    entries: HashMap<PathBuf, IndexEntry>,
    order: BTreeSet<(u64, PathBuf)>,
}

pub struct CacheStore {
    root: PathBuf,
    thumbs: PathBuf,
    limit: AtomicU64,
    index: Mutex<CacheIndex>,
}

impl CacheStore {
    pub fn new(root: PathBuf, limit_bytes: u64) -> Self {
        let thumbs = root.join("thumbs");
        let _ = fs::create_dir_all(&thumbs);
        let store = Self {
            root,
            thumbs,
            limit: AtomicU64::new(limit_bytes),
            index: Mutex::new(CacheIndex::default()),
        };
        store.rebuild_index();
        store.evict();
        store
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn usage_bytes(&self) -> u64 {
        self.index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .total
    }

    pub fn set_limit_bytes(&self, limit: u64) {
        self.limit.store(limit, Ordering::Relaxed);
        self.evict();
    }

    pub fn load(&self, key: u64) -> Option<Vec<u8>> {
        let path = self.path_for(key);
        let bytes = fs::read(&path).ok()?;
        self.touch(&path, bytes.len() as u64);
        Some(bytes)
    }

    pub fn store(&self, key: u64, bytes: &[u8]) {
        let path = self.path_for(key);
        let Some(parent) = path.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let temporary =
            path.with_extension(format!("tmp-{}-{}", std::process::id(), unique_suffix()));
        if fs::write(&temporary, bytes).is_err() {
            let _ = fs::remove_file(&temporary);
            return;
        }
        if fs::rename(&temporary, &path).is_err() {
            let _ = fs::remove_file(&temporary);
            return;
        }
        self.touch(&path, bytes.len() as u64);
        self.evict();
    }

    pub fn clear_files(&self) {
        for entry in WalkDir::new(&self.thumbs)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let extension = path.extension().and_then(|value| value.to_str());
            if extension == Some(CACHE_EXTENSION)
                || extension.is_some_and(|value| value.starts_with("tmp-"))
            {
                let _ = fs::remove_file(path);
            }
        }
        let mut index = self
            .index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *index = CacheIndex::default();
    }

    fn path_for(&self, key: u64) -> PathBuf {
        let name = format!("{key:016x}.{CACHE_EXTENSION}");
        let shard = &name[..2];
        self.thumbs.join(shard).join(name)
    }

    fn touch(&self, path: &Path, size: u64) {
        let stamp = unix_millis();
        let mut index = self
            .index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(previous) = index.entries.remove(path) {
            index.order.remove(&(previous.stamp, path.to_path_buf()));
            index.total = index.total.saturating_sub(previous.size);
        }
        index.total = index.total.saturating_add(size);
        index
            .entries
            .insert(path.to_path_buf(), IndexEntry { size, stamp });
        index.order.insert((stamp, path.to_path_buf()));
    }

    fn rebuild_index(&self) {
        let mut index = CacheIndex::default();
        for entry in WalkDir::new(&self.thumbs)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some(CACHE_EXTENSION) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let size = metadata.len();
            let stamp = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|value| value.as_millis() as u64)
                .unwrap_or(0);
            index.total = index.total.saturating_add(size);
            index
                .entries
                .insert(path.to_path_buf(), IndexEntry { size, stamp });
            index.order.insert((stamp, path.to_path_buf()));
        }
        *self
            .index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = index;
    }

    fn evict(&self) {
        let limit = self.limit.load(Ordering::Relaxed);
        if limit == 0 {
            return;
        }
        let mut index = self
            .index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let attempts = index.entries.len();
        for _ in 0..attempts {
            if index.total <= limit {
                break;
            }
            let Some((stamp, path)) = index.order.iter().next().cloned() else {
                break;
            };
            index.order.remove(&(stamp, path.clone()));
            if let Some(entry) = index.entries.remove(&path) {
                index.total = index.total.saturating_sub(entry.size);
            }
            if path.starts_with(&self.thumbs) {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

pub fn file_key(path: &Path, size: u64, modified_ns: i64, variant: u32) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    fnv1a(&mut hash, b"exif-studio-cache-v1");
    fnv1a(&mut hash, path.as_os_str().as_encoded_bytes());
    fnv1a(&mut hash, &size.to_le_bytes());
    fnv1a(&mut hash, &modified_ns.to_le_bytes());
    fnv1a(&mut hash, &variant.to_le_bytes());
    hash
}

fn fnv1a(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= *byte as u64;
        *hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    *hash ^= 0xff;
    *hash = hash.wrapping_mul(0x100_0000_01b3);
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

fn unique_suffix() -> u64 {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_change_with_file_version() {
        let path = Path::new("/photos/a.jpg");
        assert_ne!(file_key(path, 10, 100, 1), file_key(path, 11, 100, 1));
        assert_ne!(file_key(path, 10, 100, 1), file_key(path, 10, 101, 1));
        assert_ne!(file_key(path, 10, 100, 1), file_key(path, 10, 100, 2));
        assert_eq!(file_key(path, 10, 100, 1), file_key(path, 10, 100, 1));
    }

    #[test]
    fn stores_and_evicts_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = CacheStore::new(dir.path().to_path_buf(), 1024);
        store.store(1, &[7u8; 600]);
        assert!(store.load(1).is_some());
        store.store(2, &[7u8; 600]);
        assert!(store.usage_bytes() <= 1024);
    }

    #[test]
    fn clearing_only_removes_owned_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = CacheStore::new(dir.path().to_path_buf(), 1024);
        store.store(3, b"cache");
        let foreign = dir.path().join("keep.txt");
        fs::write(&foreign, b"keep").unwrap();
        store.clear_files();
        assert!(store.load(3).is_none());
        assert!(foreign.exists());
    }
}
