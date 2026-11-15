use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use image::{DynamicImage, ImageReader, Limits};
use serde::{Deserialize, Serialize};

use crate::exif_io::read_embedded_thumbnail;
use crate::model::{MediaEntry, MediaKind, path_key};

const CACHE_VERSION: u32 = 1;
const MAX_CACHE_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RECORDS: usize = 100_000;
const MIN_WORKERS: usize = 4;
const MAX_WORKERS: usize = 16;
const HASH_DISTANCE: u32 = 6;
const MAX_COLOR_DISTANCE: u32 = 58;
const MAX_ASPECT_DIFF: f32 = 0.12;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VisualFeatures {
    pub hash: u64,
    pub width: u32,
    pub height: u32,
    pub aspect: f32,
    pub avg_rgb: [u8; 3],
    pub sharpness: f32,
    pub brightness: f32,
    pub contrast: f32,
    pub quality: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fingerprint {
    pub size: u64,
    pub modified_ns: i64,
    pub features: VisualFeatures,
}

impl VisualFeatures {
    fn pixels(&self) -> u64 {
        self.width as u64 * self.height as u64
    }

    /// Relative preference inside a duplicate group. Resolution still dominates,
    /// but a visibly sharper, better-exposed frame now beats a soft one of the
    /// same scene instead of resolution being the only tie-breaker.
    fn preference_score(&self) -> f64 {
        let pixels = self.pixels();
        let resolution = if pixels > 0 {
            (((pixels as f64).log10() - 5.0) / 3.0).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let sharpness = (self.sharpness as f64 / 0.12).clamp(0.0, 1.0);
        let contrast = (self.contrast as f64 / 0.28).clamp(0.0, 1.0);
        let exposure = (1.0 - ((self.brightness as f64 - 0.5).abs() * 2.0)).clamp(0.0, 1.0);
        resolution * 0.45 + sharpness * 0.33 + contrast * 0.12 + exposure * 0.10
    }
}

#[derive(Clone, Debug)]
pub struct DuplicateGroup {
    pub paths: Vec<PathBuf>,
    pub preferred: PathBuf,
    pub min_distance: u32,
}

impl DuplicateGroup {
    pub fn similarity_percent(&self) -> u8 {
        (((64 - self.min_distance) * 100) / 64).min(100) as u8
    }
}

#[derive(Default)]
pub struct VisionReport {
    pub groups: Vec<DuplicateGroup>,
    pub preferred: Arc<HashSet<PathBuf>>,
    pub analyzed: usize,
    pub failed: usize,
    pub total: usize,
}

pub enum VisionMsg {
    Progress { done: usize, total: usize },
    Finished(VisionReport),
    Failed(String),
}

pub struct VisionHandle {
    pub rx: Receiver<VisionMsg>,
    pub cancel: Arc<AtomicBool>,
}

impl Drop for VisionHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct VisionEntry {
    path: PathBuf,
    size: u64,
    modified_ns: i64,
    orientation: u16,
    width: u32,
    height: u32,
}

fn supported_entry(entry: &MediaEntry) -> Option<VisionEntry> {
    if entry.kind != MediaKind::Image || !is_visual_extension(&entry.ext) {
        return None;
    }
    let image = entry.image.as_ref();
    Some(VisionEntry {
        path: entry.path.clone(),
        size: entry.size,
        modified_ns: entry.modified_ns(),
        orientation: image.map(|value| value.orientation).unwrap_or(1),
        width: image.and_then(|value| value.width).unwrap_or(0),
        height: image.and_then(|value| value.height).unwrap_or(0),
    })
}

pub fn worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4)
        .clamp(MIN_WORKERS, MAX_WORKERS)
}

pub fn is_visual_extension(ext: &str) -> bool {
    matches!(
        ext,
        "jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp" | "bmp" | "gif"
    )
}

fn start_analysis(entries: Vec<VisionEntry>, cache: Arc<VisionCache>) -> VisionHandle {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let thread_cancel = cancel.clone();

    std::thread::Builder::new()
        .name("exif-vision".to_string())
        .spawn(move || {
            run_analysis(entries, cache, tx, thread_cancel);
        })
        .expect("无法启动视觉分析线程");

    VisionHandle { rx, cancel }
}

pub fn start_entries(entries: &[MediaEntry], cache: Arc<VisionCache>) -> Option<VisionHandle> {
    let entries: Vec<VisionEntry> = entries.iter().filter_map(supported_entry).collect();
    if entries.is_empty() {
        return None;
    }
    Some(start_analysis(entries, cache))
}

/// Build a provisional report from cached fingerprints only. This costs a few
/// map lookups and lets auto-preferred stars appear immediately without
/// decoding images; a full scan runs later when the user opens 查重.
pub fn cached_report(entries: &[MediaEntry], cache: &VisionCache) -> Option<VisionReport> {
    let total = entries
        .iter()
        .filter(|entry| supported_entry(entry).is_some())
        .count();
    let mut items = Vec::new();
    for entry in entries {
        let Some(entry) = supported_entry(entry) else {
            continue;
        };
        if let Some(fingerprint) = cache.lookup(&entry.path, entry.size, entry.modified_ns) {
            items.push((entry.path, fingerprint));
        }
    }
    if items.is_empty() {
        return None;
    }
    Some(build_report(items, 0, total))
}

fn run_analysis(
    entries: Vec<VisionEntry>,
    cache: Arc<VisionCache>,
    tx: Sender<VisionMsg>,
    cancel: Arc<AtomicBool>,
) {
    let total = entries.len();
    let mut cached = Vec::new();
    let mut missing = Vec::new();
    for entry in entries {
        if let Some(fingerprint) = cache.lookup(&entry.path, entry.size, entry.modified_ns) {
            cached.push((entry.path, fingerprint));
        } else {
            missing.push(entry);
        }
    }
    let mut done = cached.len();
    let _ = tx.send(VisionMsg::Progress { done, total });

    if missing.is_empty() {
        let report = build_report(cached, 0, total);
        cache.save();
        let _ = tx.send(VisionMsg::Finished(report));
        return;
    }

    let missing = Arc::new(missing);
    let next = Arc::new(AtomicUsize::new(0));
    let (result_tx, result_rx) = mpsc::channel::<(PathBuf, Result<Fingerprint, String>)>();
    let workers = worker_count().min(missing.len());
    let mut spawned = 0usize;
    for index in 0..workers {
        let missing = missing.clone();
        let next = next.clone();
        let cancel = cancel.clone();
        let result_tx = result_tx.clone();
        let spawn = std::thread::Builder::new()
            .name(format!("exif-vision-{index}"))
            .spawn(move || {
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let position = next.fetch_add(1, Ordering::Relaxed);
                    let Some(entry) = missing.get(position) else {
                        return;
                    };
                    let result = fingerprint_entry(entry);
                    if result_tx.send((entry.path.clone(), result)).is_err() {
                        return;
                    }
                }
            });
        if spawn.is_err() {
            break;
        }
        spawned += 1;
    }
    drop(result_tx);

    if spawned == 0 {
        let _ = tx.send(VisionMsg::Failed("无法启动视觉分析工作线程".to_string()));
        return;
    }

    let mut failed = 0usize;
    let mut last_save = std::time::Instant::now();
    while done < total {
        if cancel.load(Ordering::Relaxed) {
            cache.save();
            return;
        }
        match result_rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok((path, Ok(fingerprint))) => {
                cache.put(&path, fingerprint.clone());
                cached.push((path, fingerprint));
                done += 1;
                if done % 8 == 0 || done == total {
                    let _ = tx.send(VisionMsg::Progress { done, total });
                }
                if last_save.elapsed() >= std::time::Duration::from_secs(15) {
                    cache.save();
                    last_save = std::time::Instant::now();
                }
            }
            Ok((_, Err(_))) => {
                failed += 1;
                done += 1;
                if done % 8 == 0 || done == total {
                    let _ = tx.send(VisionMsg::Progress { done, total });
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    if done < total {
        failed += total - done;
    }
    cache.save();
    let report = build_report(cached, failed, total);
    let _ = tx.send(VisionMsg::Finished(report));
}

fn fingerprint_entry(entry: &VisionEntry) -> Result<Fingerprint, String> {
    let decoded = decode_source(&entry.path)?;
    let oriented = apply_orientation(decoded, entry.orientation);
    let (mut width, mut height) = (entry.width, entry.height);
    if width == 0 || height == 0 {
        width = oriented.width();
        height = oriented.height();
    } else if (5..=8).contains(&entry.orientation) {
        std::mem::swap(&mut width, &mut height);
    }
    let features = fingerprint_image(&oriented, width, height);
    Ok(Fingerprint {
        size: entry.size,
        modified_ns: entry.modified_ns,
        features,
    })
}

fn decode_source(path: &Path) -> Result<DynamicImage, String> {
    if is_jpeg(path)
        && let Some(bytes) = read_embedded_thumbnail(path)
        && let Ok(image) = image::load_from_memory(&bytes)
    {
        return Ok(image);
    }
    let mut reader = ImageReader::open(path)
        .map_err(|error| format!("无法打开图片：{error}"))?
        .with_guessed_format()
        .map_err(|error| format!("无法识别图片格式：{error}"))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|error| format!("无法解码图片：{error}"))
}

fn fingerprint_image(image: &DynamicImage, width: u32, height: u32) -> VisualFeatures {
    let hash_source = image.resize_exact(9, 8, image::imageops::FilterType::Triangle);
    let hash_luma = hash_source.to_luma8();
    let mut hash = 0u64;
    for y in 0..8u32 {
        for x in 0..8u32 {
            let left = hash_luma.get_pixel(x, y)[0];
            let right = hash_luma.get_pixel(x + 1, y)[0];
            if left > right {
                hash |= 1u64 << (y * 8 + x);
            }
        }
    }

    let sample = image
        .resize_exact(64, 64, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mut sum = 0f64;
    let mut sum_sq = 0f64;
    let mut clipped = 0usize;
    for pixel in sample.pixels() {
        let value = pixel[0] as f64;
        sum += value;
        sum_sq += value * value;
        if value <= 8.0 || value >= 247.0 {
            clipped += 1;
        }
    }
    let count = (sample.width() * sample.height()).max(1) as f64;
    let mean = sum / count;
    let variance = (sum_sq / count - mean * mean).max(0.0);
    let brightness = (mean / 255.0) as f32;
    let contrast = ((variance.sqrt()) / 255.0) as f32;

    let mut laplacian = 0f64;
    let mut lap_count = 0usize;
    for y in 1..63u32 {
        for x in 1..63u32 {
            let center = sample.get_pixel(x, y)[0] as f64;
            let value = 4.0 * center
                - sample.get_pixel(x - 1, y)[0] as f64
                - sample.get_pixel(x + 1, y)[0] as f64
                - sample.get_pixel(x, y - 1)[0] as f64
                - sample.get_pixel(x, y + 1)[0] as f64;
            laplacian += value * value;
            lap_count += 1;
        }
    }
    let sharpness = ((laplacian / lap_count.max(1) as f64).sqrt() / 255.0) as f32;

    let color_source = image.resize_exact(8, 8, image::imageops::FilterType::Triangle);
    let color = color_source.to_rgb8();
    let mut rgb = [0u64; 3];
    for pixel in color.pixels() {
        rgb[0] += pixel[0] as u64;
        rgb[1] += pixel[1] as u64;
        rgb[2] += pixel[2] as u64;
    }
    let color_count = (color.width() * color.height()).max(1) as u64;
    let avg_rgb = [
        (rgb[0] / color_count) as u8,
        (rgb[1] / color_count) as u8,
        (rgb[2] / color_count) as u8,
    ];

    let exposure = (1.0 - ((brightness - 0.5).abs() * 2.0)).clamp(0.0, 1.0);
    let clip_ratio = clipped as f32 / count as f32;
    let pixels = width as f64 * height as f64;
    let resolution = if pixels > 0.0 {
        ((pixels.log10() - 5.0) / 3.0).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let quality =
        sharpness * 1.8 + contrast * 0.9 + exposure * 0.5 + resolution * 0.25 - clip_ratio * 0.8;
    let aspect = if height > 0 {
        width as f32 / height as f32
    } else {
        1.0
    };

    VisualFeatures {
        hash,
        width,
        height,
        aspect,
        avg_rgb,
        sharpness,
        brightness,
        contrast,
        quality,
    }
}

fn apply_orientation(image: DynamicImage, orientation: u16) -> DynamicImage {
    match orientation {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.rotate90().fliph(),
        6 => image.rotate90(),
        7 => image.rotate270().fliph(),
        8 => image.rotate270(),
        _ => image,
    }
}

fn is_jpeg(path: &Path) -> bool {
    path.extension()
        .map(|value| {
            let value = value.to_string_lossy().to_lowercase();
            value == "jpg" || value == "jpeg"
        })
        .unwrap_or(false)
}

fn build_report(
    mut items: Vec<(PathBuf, Fingerprint)>,
    failed: usize,
    total: usize,
) -> VisionReport {
    items.sort_by(|left, right| {
        left.0
            .file_name()
            .map(|value| value.to_string_lossy().to_lowercase())
            .cmp(
                &right
                    .0
                    .file_name()
                    .map(|value| value.to_string_lossy().to_lowercase()),
            )
            .then_with(|| left.0.cmp(&right.0))
    });
    let analyzed = items.len();
    let groups = duplicate_groups(&items);
    let preferred = Arc::new(groups.iter().map(|group| group.preferred.clone()).collect());
    VisionReport {
        groups,
        preferred,
        analyzed,
        failed,
        total,
    }
}

fn duplicate_groups(items: &[(PathBuf, Fingerprint)]) -> Vec<DuplicateGroup> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|left, right| {
        let a = &items[*left].1.features;
        let b = &items[*right].1.features;
        b.pixels()
            .cmp(&a.pixels())
            .then_with(|| b.quality.total_cmp(&a.quality))
            .then_with(|| items[*left].0.cmp(&items[*right].0))
    });

    // Leader clustering with 4 x 16-bit Hamming buckets. Any pair within six
    // bit flips must agree within one bit in at least one 16-bit block
    // (pigeonhole), so probing 17 keys per block finds every candidate while
    // avoiding an O(n^2) scan on large libraries.
    let mut clusters: Vec<(usize, Vec<usize>, u32)> = Vec::new();
    let mut buckets: [HashMap<u16, Vec<usize>>; 4] = Default::default();
    let mut candidates: Vec<usize> = Vec::new();
    for index in order {
        let features = &items[index].1.features;
        candidates.clear();
        for (block, map) in buckets.iter().enumerate() {
            let key = hash_block(features.hash, block);
            for distance in 0..=1 {
                for bit in 0..16 {
                    let probe = if distance == 0 {
                        key
                    } else {
                        key ^ (1u16 << bit)
                    };
                    if let Some(list) = map.get(&probe) {
                        candidates.extend_from_slice(list);
                    }
                }
            }
        }
        candidates.sort_unstable();
        candidates.dedup();

        let mut placed: Option<(usize, u32)> = None;
        for cluster_index in candidates.iter().copied() {
            let leader = clusters[cluster_index].0;
            if let Some(distance) = pair_distance(&items[leader].1.features, features)
                && placed.is_none_or(|(_, best)| distance < best)
            {
                placed = Some((cluster_index, distance));
            }
        }
        match placed {
            Some((cluster_index, distance)) => {
                clusters[cluster_index].1.push(index);
                clusters[cluster_index].2 = clusters[cluster_index].2.min(distance);
            }
            None => {
                let cluster_index = clusters.len();
                clusters.push((index, vec![index], HASH_DISTANCE));
                for (block, map) in buckets.iter_mut().enumerate() {
                    map.entry(hash_block(features.hash, block))
                        .or_default()
                        .push(cluster_index);
                }
            }
        }
    }

    let mut groups = Vec::new();
    for (_, mut indices, min_distance) in clusters {
        if indices.len() < 2 {
            continue;
        }
        indices.sort_by(|left, right| {
            let a = &items[*left].1.features;
            let b = &items[*right].1.features;
            b.preference_score()
                .total_cmp(&a.preference_score())
                .then_with(|| b.pixels().cmp(&a.pixels()))
                .then_with(|| b.quality.total_cmp(&a.quality))
                .then_with(|| items[*left].0.cmp(&items[*right].0))
        });
        let paths = indices
            .into_iter()
            .map(|index| items[index].0.clone())
            .collect::<Vec<_>>();
        let preferred = paths[0].clone();
        groups.push(DuplicateGroup {
            paths,
            preferred,
            min_distance,
        });
    }
    groups.sort_by(|left, right| {
        right
            .paths
            .len()
            .cmp(&left.paths.len())
            .then_with(|| left.min_distance.cmp(&right.min_distance))
            .then_with(|| left.preferred.cmp(&right.preferred))
    });
    groups
}

fn hash_block(hash: u64, block: usize) -> u16 {
    ((hash >> (block * 16)) & 0xFFFF) as u16
}

fn pair_distance(left: &VisualFeatures, right: &VisualFeatures) -> Option<u32> {
    if (left.aspect - right.aspect).abs()
        > MAX_ASPECT_DIFF.max(left.aspect.max(right.aspect) * 0.02)
    {
        return None;
    }
    let distance = (left.hash ^ right.hash).count_ones();
    if distance > HASH_DISTANCE {
        return None;
    }
    if color_distance(left.avg_rgb, right.avg_rgb) > MAX_COLOR_DISTANCE {
        return None;
    }
    Some(distance)
}

fn color_distance(left: [u8; 3], right: [u8; 3]) -> u32 {
    let dr = left[0] as i32 - right[0] as i32;
    let dg = left[1] as i32 - right[1] as i32;
    let db = left[2] as i32 - right[2] as i32;
    (dr * dr + dg * dg + db * db) as u32
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: HashMap<String, Fingerprint>,
}

pub struct VisionCache {
    path: PathBuf,
    records: Mutex<HashMap<String, Fingerprint>>,
    save_lock: Mutex<()>,
    dirty: AtomicBool,
}

impl VisionCache {
    pub fn load(path: PathBuf) -> Self {
        let records = read_cache(&path);
        Self {
            path,
            records: Mutex::new(records),
            save_lock: Mutex::new(()),
            dirty: AtomicBool::new(false),
        }
    }

    pub fn lookup(&self, path: &Path, size: u64, modified_ns: i64) -> Option<Fingerprint> {
        let records = self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fingerprint = records.get(&path_key(path))?;
        if fingerprint.size != size || fingerprint.modified_ns != modified_ns {
            return None;
        }
        Some(fingerprint.clone())
    }

    pub fn put(&self, path: &Path, fingerprint: Fingerprint) {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(path_key(path), fingerprint);
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn save(&self) {
        if !self.dirty.load(Ordering::Relaxed) {
            return;
        }
        let _guard = self
            .save_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = {
            let mut records = self
                .records
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if records.len() > MAX_RECORDS {
                let extra = records.len() - MAX_RECORDS;
                let keys: Vec<String> = records.keys().take(extra).cloned().collect();
                for key in keys {
                    records.remove(&key);
                }
            }
            self.dirty.store(false, Ordering::Release);
            records.clone()
        };
        if write_cache(&self.path, snapshot).is_err() {
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

fn read_cache(path: &Path) -> HashMap<String, Fingerprint> {
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

fn write_cache(path: &Path, entries: HashMap<String, Fingerprint>) -> Result<(), std::io::Error> {
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::other("视觉缓存路径没有父目录"));
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

pub fn detect_npu() -> Option<String> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Some("Apple Neural Engine（Core ML 接口可用）".to_string());
    }
    #[cfg(target_os = "linux")]
    {
        if Path::new("/sys/class/accel").is_dir()
            && fs::read_dir("/sys/class/accel")
                .ok()
                .is_some_and(|mut entries| entries.next().is_some())
        {
            return Some("系统 NPU 设备（/sys/class/accel）".to_string());
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(cpu) = std::env::var("PROCESSOR_IDENTIFIER") {
            let cpu = cpu.to_lowercase();
            if cpu.contains("snapdragon") {
                return Some("Qualcomm Hexagon NPU".to_string());
            }
            if cpu.contains("ryzen ai") {
                return Some("AMD XDNA NPU".to_string());
            }
            if cpu.contains("intel") && cpu.contains("ultra") {
                return Some("Intel NPU".to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn gradient(red: bool) -> DynamicImage {
        let buffer: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(160, 100, |x, y| {
            if red {
                Rgb([(x * 2) as u8, (y * 2) as u8, 80])
            } else {
                Rgb([80, (y * 2) as u8, (x * 2) as u8])
            }
        });
        DynamicImage::ImageRgb8(buffer)
    }

    #[test]
    fn identical_images_have_identical_fingerprints() {
        let image = gradient(true);
        let a = fingerprint_image(&image, 160, 100);
        let b = fingerprint_image(&image, 160, 100);
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.avg_rgb, b.avg_rgb);
        assert!(a.quality > 0.0);
    }

    #[test]
    fn groups_only_similar_fingerprints() {
        let base = fingerprint_image(&gradient(true), 160, 100);
        let mut nearby = base.clone();
        nearby.hash ^= 0b11;
        nearby.quality += 0.1;
        let far = fingerprint_image(&gradient(false), 160, 100);
        let items = vec![
            (
                PathBuf::from("a.jpg"),
                Fingerprint {
                    size: 1,
                    modified_ns: 1,
                    features: base,
                },
            ),
            (
                PathBuf::from("b.jpg"),
                Fingerprint {
                    size: 1,
                    modified_ns: 1,
                    features: nearby,
                },
            ),
            (
                PathBuf::from("c.jpg"),
                Fingerprint {
                    size: 1,
                    modified_ns: 1,
                    features: far,
                },
            ),
        ];
        let groups = duplicate_groups(&items);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].paths.len(), 2);
        assert_eq!(groups[0].preferred, PathBuf::from("b.jpg"));
    }

    #[test]
    fn cache_round_trips_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vision.json");
        let cache = VisionCache::load(path.clone());
        let photo = Path::new("/photos/a.jpg");
        let features = fingerprint_image(&gradient(true), 160, 100);
        cache.put(
            photo,
            Fingerprint {
                size: 42,
                modified_ns: 7,
                features,
            },
        );
        cache.save();
        assert!(path.exists());

        let reloaded = VisionCache::load(path);
        let fingerprint = reloaded.lookup(photo, 42, 7).unwrap();
        assert_eq!(fingerprint.size, 42);
        assert!(reloaded.lookup(photo, 42, 8).is_none());
    }
}
