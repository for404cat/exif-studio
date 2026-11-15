use std::collections::BTreeMap;

use crate::model::{MediaEntry, MediaKind};

#[derive(Clone, Default)]
pub struct Stats {
    pub images: usize,
    pub videos: usize,
    pub image_bytes: u64,
    pub video_bytes: u64,
    pub errors: usize,
    pub formats: BTreeMap<String, usize>,
    pub makes: BTreeMap<String, usize>,
    pub models: BTreeMap<String, usize>,
    pub lenses: BTreeMap<String, usize>,
    pub isos: BTreeMap<u32, usize>,
    pub apertures: BTreeMap<u32, usize>,
    pub focal_bins: Vec<(String, usize)>,
    pub years: BTreeMap<i32, usize>,
    pub months: [usize; 12],
    pub gps_count: usize,
    pub gps_points: Vec<(f64, f64)>,
    pub gps_clusters: Vec<(String, usize)>,
    pub image_resolutions: BTreeMap<String, usize>,
    pub video_codecs: BTreeMap<String, usize>,
    pub video_resolutions: BTreeMap<String, usize>,
    pub video_makes: BTreeMap<String, usize>,
    pub video_years: BTreeMap<i32, usize>,
    pub video_seconds: f64,
}

const FOCAL_BINS: [(&str, f64, f64); 8] = [
    ("< 16mm", 0.0, 16.0),
    ("16–23mm", 16.0, 24.0),
    ("24–34mm", 24.0, 35.0),
    ("35–49mm", 35.0, 50.0),
    ("50–84mm", 50.0, 85.0),
    ("85–134mm", 85.0, 135.0),
    ("135–199mm", 135.0, 200.0),
    ("≥ 200mm", 200.0, f64::INFINITY),
];

impl Stats {
    pub fn compute(entries: &[MediaEntry]) -> Stats {
        let mut stats = Stats {
            focal_bins: FOCAL_BINS
                .iter()
                .map(|(label, _, _)| (label.to_string(), 0))
                .collect(),
            ..Default::default()
        };
        let mut gps_all: Vec<(f64, f64)> = Vec::new();
        let mut cluster_counts: BTreeMap<String, usize> = BTreeMap::new();

        for entry in entries {
            if entry.read_error.is_some() {
                stats.errors += 1;
            }
            *stats
                .formats
                .entry(normalize_format(&entry.ext))
                .or_insert(0) += 1;

            match entry.kind {
                MediaKind::Image => {
                    stats.images += 1;
                    stats.image_bytes += entry.size;
                    let Some(info) = &entry.image else { continue };
                    if let Some(make) = clean(&info.make) {
                        *stats.makes.entry(make).or_insert(0) += 1;
                    }
                    if let Some(camera) = info.camera_label() {
                        *stats.models.entry(camera).or_insert(0) += 1;
                    }
                    if let Some(lens) = clean(&info.lens) {
                        *stats.lenses.entry(lens).or_insert(0) += 1;
                    }
                    if let Some(iso) = info.iso {
                        *stats.isos.entry(iso).or_insert(0) += 1;
                    }
                    if let Some(f) = info.f_number.filter(|f| *f > 0.0) {
                        let key = (f * 10.0).round() as u32;
                        *stats.apertures.entry(key).or_insert(0) += 1;
                    }
                    if let Some(focal) = info.focal_length
                        && let Some(index) = FOCAL_BINS
                            .iter()
                            .position(|(_, min, max)| focal >= *min && focal < *max)
                    {
                        stats.focal_bins[index].1 += 1;
                    }
                    if let Some(at) = info.taken_at {
                        use chrono::Datelike;
                        *stats.years.entry(at.year()).or_insert(0) += 1;
                        let month = at.month0() as usize;
                        stats.months[month] += 1;
                    }
                    if let Some(resolution) = resolution_label(info.width, info.height) {
                        *stats.image_resolutions.entry(resolution).or_insert(0) += 1;
                    }
                    if let Some((lat, lon)) = info.gps {
                        stats.gps_count += 1;
                        gps_all.push((lat, lon));
                        *cluster_counts
                            .entry(format!("{lat:.2}, {lon:.2}"))
                            .or_insert(0) += 1;
                    }
                }
                MediaKind::Video => {
                    stats.videos += 1;
                    stats.video_bytes += entry.size;
                    let Some(info) = &entry.video else { continue };
                    if let Some(codec) = clean(&info.codec) {
                        *stats.video_codecs.entry(codec).or_insert(0) += 1;
                    }
                    if let Some(resolution) = resolution_label(info.width, info.height) {
                        *stats.video_resolutions.entry(resolution).or_insert(0) += 1;
                    }
                    if let Some(make) = clean(&info.make) {
                        *stats.video_makes.entry(make).or_insert(0) += 1;
                    }
                    if let Some(at) = info.created_at {
                        use chrono::Datelike;
                        *stats.video_years.entry(at.year()).or_insert(0) += 1;
                    }
                    if let Some(duration) = info.duration_secs {
                        stats.video_seconds += duration;
                    }
                    if let Some((lat, lon)) = info.gps {
                        stats.gps_count += 1;
                        gps_all.push((lat, lon));
                        *cluster_counts
                            .entry(format!("{lat:.2}, {lon:.2}"))
                            .or_insert(0) += 1;
                    }
                }
            }
        }

        if gps_all.len() > 600 {
            let step = (gps_all.len() as f64 / 600.0).ceil() as usize;
            stats.gps_points = gps_all.into_iter().step_by(step.max(1)).collect();
        } else {
            stats.gps_points = gps_all;
        }

        let mut clusters: Vec<(String, usize)> = cluster_counts.into_iter().collect();
        clusters.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        clusters.truncate(12);
        stats.gps_clusters = clusters;

        stats
    }
}

fn clean(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

fn normalize_format(ext: &str) -> String {
    match ext {
        "jpg" | "jpeg" => "JPEG",
        "tif" | "tiff" => "TIFF",
        "heic" | "heif" | "hif" => "HEIF",
        "mov" => "MOV",
        "mp4" => "MP4",
        "m4v" => "M4V",
        "3gp" => "3GP",
        "3g2" => "3G2",
        other => other,
    }
    .to_uppercase()
}

fn resolution_label(width: Option<u32>, height: Option<u32>) -> Option<String> {
    match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some(format!("{w}×{h}")),
        _ => None,
    }
}

pub fn sorted_counts(map: &BTreeMap<String, usize>, limit: usize) -> Vec<(String, usize)> {
    let mut values: Vec<(String, usize)> = map.iter().map(|(k, v)| (k.clone(), *v)).collect();
    values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    values.truncate(limit);
    values
}

pub fn sorted_numeric_counts(map: &BTreeMap<u32, usize>, limit: usize) -> Vec<(u32, usize)> {
    let mut values: Vec<(u32, usize)> = map.iter().map(|(k, v)| (*k, *v)).collect();
    values.sort_by_key(|(k, _)| *k);
    if values.len() > limit {
        let step = (values.len() as f64 / limit as f64).ceil() as usize;
        values = values.into_iter().step_by(step.max(1)).collect();
    }
    values
}
