use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::path_key;
use crate::platform;

const CATALOG_VERSION: u32 = 1;
const MAX_CATALOG_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RECORDS: usize = 200_000;

#[derive(Serialize, Deserialize, Clone)]
struct RatingRecord {
    manual: Option<bool>,
    updated_at: u64,
}

#[derive(Serialize, Deserialize)]
struct CatalogFile {
    version: u32,
    entries: HashMap<String, RatingRecord>,
}

pub struct RatingCatalog {
    path: PathBuf,
    entries: HashMap<String, RatingRecord>,
    dirty: bool,
}

impl RatingCatalog {
    pub fn load() -> Self {
        let path = platform::config_dir().join("ratings.json");
        let entries = read_catalog(&path);
        Self {
            path,
            entries,
            dirty: false,
        }
    }

    /// `auto` is the vision engine's preferred pick. An explicit manual choice wins,
    /// including an explicit cancellation of an automatic star.
    pub fn is_favorite(&self, path: &Path, auto: bool) -> bool {
        self.entries
            .get(&path_key(path))
            .and_then(|record| record.manual)
            .unwrap_or(auto)
    }

    pub fn manual_state(&self, path: &Path) -> Option<bool> {
        self.entries
            .get(&path_key(path))
            .and_then(|record| record.manual)
    }

    pub fn toggle(&mut self, path: &Path, auto: bool) -> bool {
        let key = path_key(path);
        let next = !self.is_favorite(path, auto);
        self.entries.insert(
            key,
            RatingRecord {
                manual: Some(next),
                updated_at: unix_seconds(),
            },
        );
        self.dirty = true;
        self.save();
        next
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        self.trim();
        let catalog = CatalogFile {
            version: CATALOG_VERSION,
            entries: self.entries.clone(),
        };
        let Ok(bytes) = serde_json::to_vec(&catalog) else {
            return;
        };
        let Some(parent) = self.path.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let temporary = self
            .path
            .with_extension(format!("tmp-{}", std::process::id()));
        if fs::write(&temporary, bytes).is_err() {
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600));
        }
        if fs::rename(&temporary, &self.path).is_ok() {
            self.dirty = false;
        } else {
            let _ = fs::remove_file(&temporary);
        }
    }

    fn trim(&mut self) {
        if self.entries.len() <= MAX_RECORDS {
            return;
        }
        let mut newest: Vec<(String, u64)> = self
            .entries
            .iter()
            .map(|(key, record)| (key.clone(), record.updated_at))
            .collect();
        newest.sort_by_key(|(_, updated_at)| std::cmp::Reverse(*updated_at));
        let keep: std::collections::HashSet<String> = newest
            .into_iter()
            .take(MAX_RECORDS)
            .map(|(key, _)| key)
            .collect();
        self.entries.retain(|key, _| keep.contains(key));
    }
}

impl Drop for RatingCatalog {
    fn drop(&mut self) {
        self.save();
    }
}

fn read_catalog(path: &Path) -> HashMap<String, RatingRecord> {
    let Ok(metadata) = fs::metadata(path) else {
        return HashMap::new();
    };
    if metadata.len() > MAX_CATALOG_FILE_BYTES {
        return HashMap::new();
    }
    let Ok(bytes) = fs::read(path) else {
        return HashMap::new();
    };
    let Ok(catalog) = serde_json::from_slice::<CatalogFile>(&bytes) else {
        return HashMap::new();
    };
    if catalog.version != CATALOG_VERSION {
        return HashMap::new();
    }
    catalog.entries
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
    fn manual_star_overrides_auto_and_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ratings.json");
        let mut catalog = RatingCatalog {
            path: path.clone(),
            entries: HashMap::new(),
            dirty: false,
        };
        let photo = Path::new("/photos/a.jpg");
        assert!(!catalog.is_favorite(photo, false));
        assert!(catalog.is_favorite(photo, true));

        catalog.toggle(photo, true);
        assert!(!catalog.is_favorite(photo, true));
        assert_eq!(catalog.manual_state(photo), Some(false));
        assert!(path.exists());

        let reloaded = read_catalog(&path);
        let record = reloaded.get(&path_key(photo)).unwrap();
        assert_eq!(record.manual, Some(false));
    }
}
