use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::platform;

const DEFAULT_CACHE_LIMIT_MB: u64 = 1024;
const DEFAULT_API_PORT: u16 = 7420;
const MIN_CACHE_LIMIT_MB: u64 = 64;
const MAX_CACHE_LIMIT_MB: u64 = 65_536;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DuplicatesLayout {
    #[default]
    Paged,
    Waterfall,
}

impl DuplicatesLayout {
    pub fn label(self) -> &'static str {
        match self {
            Self::Paged => "分页展示",
            Self::Waterfall => "瀑布流",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhotoSort {
    #[default]
    Filename,
    TakenTime,
    Resolution,
}

impl PhotoSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Filename => "文件名",
            Self::TakenTime => "拍摄时间（新→旧）",
            Self::Resolution => "分辨率（高→低）",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DuplicateSort {
    #[default]
    Count,
    TakenTime,
    Resolution,
}

impl DuplicateSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Count => "重复数量（递减）",
            Self::TakenTime => "时间（递减）",
            Self::Resolution => "分辨率（递减）",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub cache_dir: Option<PathBuf>,
    pub cache_limit_mb: u64,
    pub backup: bool,
    pub show_raw_tags: bool,
    pub thumbnail_zoom: f32,
    pub watermark_folder: String,
    pub duplicates_layout: DuplicatesLayout,
    pub photo_sort: PhotoSort,
    pub duplicates_sort: DuplicateSort,
    pub api_enabled: bool,
    pub api_port: u16,
    pub api_token: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cache_dir: None,
            cache_limit_mb: DEFAULT_CACHE_LIMIT_MB,
            backup: true,
            show_raw_tags: false,
            thumbnail_zoom: 0.18,
            watermark_folder: "watermarked".to_string(),
            duplicates_layout: DuplicatesLayout::default(),
            photo_sort: PhotoSort::default(),
            duplicates_sort: DuplicateSort::default(),
            api_enabled: false,
            api_port: DEFAULT_API_PORT,
            api_token: String::new(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(bytes) = fs::read(&path) else {
            return Self::default();
        };
        let mut settings: Settings = serde_json::from_slice(&bytes).unwrap_or_default();
        settings.normalize();
        settings
    }

    pub fn save(&self) {
        let path = Self::path();
        let Some(parent) = path.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(json) = serde_json::to_vec_pretty(self) else {
            return;
        };
        let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
        if fs::write(&temporary, json).is_err() {
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600));
        }
        let _ = fs::rename(&temporary, &path);
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.cache_dir
            .clone()
            .unwrap_or_else(platform::default_cache_dir)
    }

    pub fn cache_limit_bytes(&self) -> u64 {
        self.cache_limit_mb.saturating_mul(1024 * 1024)
    }

    pub fn ensure_api_token(&mut self) {
        if self.api_token.is_empty() {
            self.api_token = crate::api::generate_token();
        }
    }

    pub fn normalize(&mut self) {
        self.cache_limit_mb = self
            .cache_limit_mb
            .clamp(MIN_CACHE_LIMIT_MB, MAX_CACHE_LIMIT_MB);
        self.thumbnail_zoom = self.thumbnail_zoom.clamp(0.0, 1.0);
        let folder = self.watermark_folder.trim();
        self.watermark_folder = if folder.is_empty()
            || folder == "."
            || folder == ".."
            || folder.contains(['/', '\\', ':', '\0'])
        {
            "watermarked".to_string()
        } else {
            folder.to_string()
        };
        if self.api_port < 1024 {
            self.api_port = DEFAULT_API_PORT;
        }
        if self
            .cache_dir
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            self.cache_dir = None;
        }
    }

    fn path() -> PathBuf {
        platform::config_dir().join("settings.json")
    }
}

pub fn cache_limit_range_mb() -> std::ops::RangeInclusive<u64> {
    MIN_CACHE_LIMIT_MB..=MAX_CACHE_LIMIT_MB
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_invalid_values() {
        let mut settings = Settings {
            cache_limit_mb: 1,
            api_port: 80,
            ..Settings::default()
        };
        settings.normalize();
        assert_eq!(settings.cache_limit_mb, MIN_CACHE_LIMIT_MB);
        assert_eq!(settings.api_port, DEFAULT_API_PORT);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let settings: Settings = serde_json::from_str("{\"backup\":false}").unwrap();
        assert!(!settings.backup);
        assert_eq!(settings.cache_limit_mb, DEFAULT_CACHE_LIMIT_MB);
        assert_eq!(settings.api_port, DEFAULT_API_PORT);
        assert_eq!(settings.duplicates_layout, DuplicatesLayout::Paged);
        assert_eq!(settings.photo_sort, PhotoSort::Filename);
        assert_eq!(settings.duplicates_sort, DuplicateSort::Count);
    }
}
