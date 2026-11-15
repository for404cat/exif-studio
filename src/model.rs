use std::fmt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Local, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MediaKind {
    Image,
    Video,
}

impl MediaKind {
    pub fn label(self) -> &'static str {
        match self {
            MediaKind::Image => "图片",
            MediaKind::Video => "视频",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageInfo {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub taken_at: Option<NaiveDateTime>,
    pub iso: Option<u32>,
    pub f_number: Option<f64>,
    pub exposure_time: Option<f64>,
    pub focal_length: Option<f64>,
    pub focal_length_35mm: Option<u32>,
    pub orientation: u16,
    pub gps: Option<(f64, f64)>,
    pub has_exif: bool,
}

impl Default for ImageInfo {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            make: None,
            model: None,
            lens: None,
            taken_at: None,
            iso: None,
            f_number: None,
            exposure_time: None,
            focal_length: None,
            focal_length_35mm: None,
            orientation: 1,
            gps: None,
            has_exif: false,
        }
    }
}

impl ImageInfo {
    pub fn camera_label(&self) -> Option<String> {
        match (&self.make, &self.model) {
            (Some(make), Some(model)) => {
                if model.to_lowercase().contains(&make.to_lowercase()) {
                    Some(model.clone())
                } else {
                    Some(format!("{make} {model}"))
                }
            }
            (Some(make), None) => Some(make.clone()),
            (None, Some(model)) => Some(model.clone()),
            (None, None) => None,
        }
    }

    pub fn exposure_label(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(t) = self.exposure_time {
            parts.push(format_exposure(t));
        }
        if let Some(f) = self.f_number {
            parts.push(format!("f/{}", trim_float(f, 1)));
        }
        if let Some(iso) = self.iso {
            parts.push(format!("ISO {iso}"));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("  "))
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoInfo {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<f64>,
    pub fps: Option<f64>,
    pub codec: Option<String>,
    pub container: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub created_at: Option<NaiveDateTime>,
    pub gps: Option<(f64, f64)>,
    pub tags: Vec<(String, String)>,
}

impl VideoInfo {
    pub fn resolution_label(&self) -> Option<String> {
        match (self.width, self.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some(format!("{w}×{h}")),
            _ => None,
        }
    }

    pub fn duration_label(&self) -> Option<String> {
        self.duration_secs.map(format_duration)
    }

    pub fn camera_label(&self) -> Option<String> {
        match (&self.make, &self.model) {
            (Some(make), Some(model)) => {
                if model.to_lowercase().contains(&make.to_lowercase()) {
                    Some(model.clone())
                } else {
                    Some(format!("{make} {model}"))
                }
            }
            (Some(make), None) => Some(make.clone()),
            (None, Some(model)) => Some(model.clone()),
            (None, None) => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MediaEntry {
    pub path: PathBuf,
    pub kind: MediaKind,
    pub name: String,
    pub ext: String,
    pub size: u64,
    pub modified: Option<DateTime<Local>>,
    pub image: Option<ImageInfo>,
    pub video: Option<VideoInfo>,
    pub read_error: Option<String>,
}

impl MediaEntry {
    pub fn format_label(&self) -> String {
        self.ext.to_uppercase()
    }

    pub fn size_label(&self) -> String {
        format_bytes(self.size)
    }

    pub fn modified_ns(&self) -> i64 {
        self.modified
            .and_then(|value| value.timestamp_nanos_opt())
            .unwrap_or(0)
    }

    pub fn resolution_label(&self) -> Option<String> {
        match self.kind {
            MediaKind::Image => {
                let info = self.image.as_ref()?;
                match (info.width, info.height) {
                    (Some(w), Some(h)) => Some(format!("{w}×{h}")),
                    _ => None,
                }
            }
            MediaKind::Video => self.video.as_ref()?.resolution_label(),
        }
    }

    pub fn camera_label(&self) -> Option<String> {
        match self.kind {
            MediaKind::Image => self.image.as_ref()?.camera_label(),
            MediaKind::Video => self.video.as_ref()?.camera_label(),
        }
    }

    pub fn taken_at(&self) -> Option<NaiveDateTime> {
        match self.kind {
            MediaKind::Image => self.image.as_ref()?.taken_at,
            MediaKind::Video => self.video.as_ref()?.created_at,
        }
    }

    pub fn list_subtitle(&self) -> String {
        let mut parts = Vec::new();
        parts.push(self.format_label());
        if let Some(res) = self.resolution_label() {
            parts.push(res);
        }
        if self.kind == MediaKind::Video
            && let Some(v) = &self.video
            && let Some(d) = v.duration_label()
        {
            parts.push(d);
        }
        parts.push(self.size_label());
        parts.join("  ·  ")
    }
}

impl fmt::Display for MediaKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

pub fn image_extensions() -> &'static [&'static str] {
    &[
        "jpg", "jpeg", "png", "tif", "tiff", "webp", "heic", "heif", "hif", "avif", "jxl",
    ]
}

pub fn video_extensions() -> &'static [&'static str] {
    &["mp4", "m4v", "mov", "3gp", "3g2"]
}

pub fn is_video_extension(ext: &str) -> bool {
    video_extensions().contains(&ext)
}

/// Extensions which little_exif can write. JPEG-family files are all mapped to JPEG.
pub fn is_writable_extension(ext: &str) -> bool {
    matches!(
        ext,
        "jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp" | "heic" | "heif" | "hif" | "avif" | "jxl"
    )
}

pub const CAMERA_BRANDS: [&str; 17] = [
    "SONY",
    "Canon",
    "Nikon",
    "Lumix",
    "Fujifilm",
    "Leica",
    "HASSELBLAD",
    "SIGMA",
    "PENTAX",
    "RICOH",
    "Minolta",
    "Phase One",
    "小蚁",
    "永诺",
    "DJI",
    "Insta360",
    "其他",
];

pub fn known_camera_brand(make: &str) -> Option<&'static str> {
    let make = make.trim().to_lowercase();
    if make.is_empty() {
        return None;
    }
    let brand = match make.as_str() {
        "sony" => "SONY",
        "canon" => "Canon",
        "nikon" | "niokn" => "Nikon",
        "lumix" | "panasonic" => "Lumix",
        "fujifilm" | "fuji" => "Fujifilm",
        "leica" => "Leica",
        "hasselblad" => "HASSELBLAD",
        "sigma" => "SIGMA",
        "pentax" => "PENTAX",
        "ricoh" => "RICOH",
        "minolta" | "konica minolta" => "Minolta",
        "phase one" | "phaseone" => "Phase One",
        "小蚁" | "xiaoyi" | "yi" => "小蚁",
        "永诺" | "yongnuo" => "永诺",
        "dji" => "DJI",
        "insta360" | "insta 360" => "Insta360",
        _ => return None,
    };
    Some(brand)
}

pub fn kind_for_extension(ext: &str) -> Option<MediaKind> {
    if image_extensions().contains(&ext) {
        Some(MediaKind::Image)
    } else if video_extensions().contains(&ext) {
        Some(MediaKind::Video)
    } else {
        None
    }
}

/// Stable, filesystem-safe cache key for a path without exposing it in JSON keys.
pub fn path_key(path: &Path) -> String {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut key = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(key, "{byte:02x}");
    }
    key
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "—".to_string();
    }
    let total = seconds.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn format_exposure(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "—".to_string();
    }
    if seconds >= 1.0 {
        format!("{}s", trim_float(seconds, 1))
    } else {
        let denom = (1.0 / seconds).round() as u64;
        format!("1/{denom}s")
    }
}

pub fn trim_float(value: f64, decimals: usize) -> String {
    let mut text = format!("{value:.decimals$}");
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    text
}

pub fn format_datetime(value: &NaiveDateTime) -> String {
    value.format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn format_friendly_datetime(value: &NaiveDateTime) -> String {
    const WEEKDAYS: [&str; 7] = [
        "星期一",
        "星期二",
        "星期三",
        "星期四",
        "星期五",
        "星期六",
        "星期日",
    ];
    let weekday = WEEKDAYS[value.weekday().num_days_from_monday() as usize];
    format!(
        "{}.{:02}.{:02}（{}）{:02}:{:02}",
        value.year(),
        value.month(),
        value.day(),
        weekday,
        value.hour(),
        value.minute()
    )
}

pub fn format_friendly_local(value: &DateTime<Local>) -> String {
    format_friendly_datetime(&value.naive_local())
}

pub fn format_fps(fps: f64) -> String {
    if !fps.is_finite() || fps <= 0.0 {
        "—".to_string()
    } else {
        format!("{} fps", trim_float(fps, 2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn formats_friendly_datetime_with_weekday() {
        let value = NaiveDate::from_ymd_opt(2026, 7, 22)
            .unwrap()
            .and_hms_opt(23, 33, 0)
            .unwrap();
        assert_eq!(
            format_friendly_datetime(&value),
            "2026.07.22（星期三）23:33"
        );
    }
}
