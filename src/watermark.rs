use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use image::imageops::FilterType;
use image::{DynamicImage, GrayImage, ImageFormat, ImageReader, Limits, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::platform;

const MAGIC: &[u8; 4] = b"EXSW";
const VERSION: u8 = 1;
const HEADER_BYTES: usize = 9;
const MAX_TEXT_BYTES: usize = 4096;
const INDEX_VERSION: u32 = 1;
const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_INDEX_RECORDS: usize = 2000;
const VISUAL_SEARCH_LIMIT: usize = 60;
const VISUAL_SEARCH_BUDGET: Duration = Duration::from_millis(2200);

#[derive(Clone, Serialize, Deserialize)]
pub struct VisualHash {
    dhash: u64,
    phash: u64,
    avg_rgb: [u8; 3],
}

#[derive(Clone, Serialize, Deserialize)]
pub struct VisualRecord {
    pub text: String,
    pub output: PathBuf,
    pub width: u32,
    pub height: u32,
    hashes: Vec<VisualHash>,
    #[serde(default)]
    key_hash: u64,
    created_at: u64,
}

#[derive(Clone, Debug)]
pub enum Recognition {
    Embedded(String),
    Visual(String),
}

pub struct EmbedOutcome {
    pub path: PathBuf,
    pub metadata_copied: bool,
    pub record: VisualRecord,
}

pub fn validate_folder_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("输出子文件夹名称不能为空".to_string());
    }
    let path = Path::new(name);
    let mut components = path.components();
    let valid = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !name.contains(['/', '\\', ':', '\0']);
    if !valid {
        return Err("输出子文件夹只能是单个文件夹名称，不能包含路径分隔符".to_string());
    }
    Ok(name.to_string())
}

pub fn default_output_path(source: &Path, folder: &str) -> Result<PathBuf, String> {
    let folder = validate_folder_name(folder)?;
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let output_dir = parent.join(folder);
    let stem = source
        .file_stem()
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "image".to_string());
    let mut candidate = output_dir.join(format!("{stem}.watermarked.png"));
    let mut suffix = 1u32;
    while candidate.exists() {
        candidate = output_dir.join(format!("{stem}.watermarked-{suffix}.png"));
        suffix += 1;
    }
    Ok(candidate)
}

pub fn embed(
    source: &Path,
    output: &Path,
    text: &str,
    passphrase: &str,
) -> Result<EmbedOutcome, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("水印内容不能为空".to_string());
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(format!("水印内容过长（最多 {MAX_TEXT_BYTES} 字节）"));
    }
    if output == source {
        return Err("不会覆盖原文件，请选择另一个输出路径".to_string());
    }
    let orientation = crate::exif_io::read_image_info(source).0.orientation;
    let image = apply_orientation(decode(source)?, orientation);
    let (width, height) = (image.width(), image.height());
    let mut rgba = image.to_rgba8();
    let payload = build_payload(text.as_bytes());
    let geometry = Geometry::new(width, height, passphrase);
    if payload.len() * 8 > geometry.capacity {
        return Err(format!(
            "水印容量不足：图片最多容纳 {} 字节",
            geometry.capacity / 8
        ));
    }
    write_payload(&mut rgba, &payload, &geometry);
    let record = VisualRecord {
        text: text.to_string(),
        output: output.to_path_buf(),
        width,
        height,
        hashes: visual_hashes(&rgba),
        key_hash: if passphrase.is_empty() {
            0
        } else {
            seed_from(passphrase)
        },
        created_at: unix_seconds(),
    };

    let Some(parent) = output.parent() else {
        return Err("输出路径没有父目录".to_string());
    };
    fs::create_dir_all(parent).map_err(|error| format!("无法创建输出目录：{error}"))?;
    let temporary = output.with_extension(format!("tmp-{}", std::process::id()));
    let result = (|| -> Result<(), String> {
        let mut file =
            File::create(&temporary).map_err(|error| format!("无法创建水印文件：{error}"))?;
        DynamicImage::ImageRgba8(rgba)
            .write_to(&mut file, ImageFormat::Png)
            .map_err(|error| format!("无法写入 PNG：{error}"))?;
        file.sync_all()
            .map_err(|error| format!("无法同步水印文件：{error}"))?;
        fs::rename(&temporary, output).map_err(|error| format!("无法保存水印文件：{error}"))
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    let metadata_copied = copy_metadata(source, output);
    Ok(EmbedOutcome {
        path: output.to_path_buf(),
        metadata_copied,
        record,
    })
}

pub fn extract(path: &Path, passphrase: &str) -> Result<Option<String>, String> {
    let image = decode(path)?;
    let rgba = image.to_rgba8();
    let geometry = Geometry::new(rgba.width(), rgba.height(), passphrase);
    if geometry.capacity < HEADER_BYTES * 8 {
        return Ok(None);
    }
    let mut header = vec![0u8; HEADER_BYTES];
    read_payload(&rgba, &mut header, &geometry);
    if &header[0..4] != MAGIC || header[4] != VERSION {
        return Ok(None);
    }
    let length = u32::from_le_bytes([header[5], header[6], header[7], header[8]]) as usize;
    if length == 0 || length > MAX_TEXT_BYTES {
        return Ok(None);
    }
    let total = HEADER_BYTES + length + 4;
    if total * 8 > geometry.capacity {
        return Ok(None);
    }
    let mut payload = vec![0u8; total];
    read_payload(&rgba, &mut payload, &geometry);
    let text = &payload[HEADER_BYTES..HEADER_BYTES + length];
    let expected = u32::from_le_bytes(
        payload[total - 4..]
            .try_into()
            .map_err(|_| "水印长度无效".to_string())?,
    );
    if crc32(text) != expected {
        return Ok(None);
    }
    let text = String::from_utf8(text.to_vec()).map_err(|_| "水印内容不是有效文本".to_string())?;
    Ok(Some(text))
}

pub fn recognize(
    path: &Path,
    passphrase: &str,
    records: &[VisualRecord],
) -> Result<Option<Recognition>, String> {
    if let Some(text) = extract(path, passphrase)? {
        return Ok(Some(Recognition::Embedded(text)));
    }
    if records.is_empty() {
        return Ok(None);
    }
    let image = decode(path)?.to_rgba8();
    Ok(find_visual_match(&image, passphrase, records).map(Recognition::Visual))
}

fn find_visual_match(
    query: &RgbaImage,
    passphrase: &str,
    records: &[VisualRecord],
) -> Option<String> {
    let key = if passphrase.is_empty() {
        0
    } else {
        seed_from(passphrase)
    };
    let direct = visual_hashes(query);
    for record in records.iter().rev() {
        if record.key_hash != 0 && record.key_hash != key {
            continue;
        }
        if direct.iter().any(|query_hash| {
            record
                .hashes
                .iter()
                .any(|record_hash| hash_similar(query_hash, record_hash))
        }) {
            return Some(record.text.clone());
        }
    }

    let mut working = query.clone();
    let max_dimension = working.width().max(working.height());
    if max_dimension > 480 {
        let scale = 480.0 / max_dimension as f32;
        let width = (working.width() as f32 * scale).round().max(1.0) as u32;
        let height = (working.height() as f32 * scale).round().max(1.0) as u32;
        working = image::imageops::resize(&working, width, height, FilterType::Triangle);
    }

    let started = Instant::now();
    for record in records.iter().rev().take(VISUAL_SEARCH_LIMIT) {
        if record.key_hash != 0 && record.key_hash != key {
            continue;
        }
        if started.elapsed() > VISUAL_SEARCH_BUDGET {
            break;
        }
        if scan_record(&working, record) {
            return Some(record.text.clone());
        }
    }
    None
}

fn scan_record(query: &RgbaImage, record: &VisualRecord) -> bool {
    let query_width = query.width() as f32;
    let query_height = query.height() as f32;
    let aspect = if record.height > 0 {
        record.width as f32 / record.height as f32
    } else {
        1.0
    };
    for scale in [
        1.0f32, 0.9, 0.8, 0.7, 0.6, 0.5, 0.42, 0.35, 0.28, 0.22, 0.18,
    ] {
        let mut width = query_width * scale;
        let mut height = width / aspect;
        if height > query_height * scale {
            height = query_height * scale;
            width = height * aspect;
        }
        if width < 32.0 || height < 32.0 {
            continue;
        }
        let max_x = (query_width - width).max(0.0);
        let max_y = (query_height - height).max(0.0);
        let x_step = (max_x / 6.0).max(1.0);
        let y_step = (max_y / 6.0).max(1.0);
        let mut y = 0.0;
        while y <= max_y + 0.5 {
            let mut x = 0.0;
            while x <= max_x + 0.5 {
                let window = image::imageops::crop_imm(
                    query,
                    x as u32,
                    y as u32,
                    width as u32,
                    height as u32,
                )
                .to_image();
                let dhash = dhash_view(&window);
                if record
                    .hashes
                    .iter()
                    .any(|hash| (dhash ^ hash.dhash).count_ones() <= 10)
                {
                    let hash = hash_view(&window);
                    if record
                        .hashes
                        .iter()
                        .any(|candidate| hash_similar(&hash, candidate))
                    {
                        return true;
                    }
                }
                x += x_step;
            }
            y += y_step;
        }
    }
    false
}

fn hash_similar(left: &VisualHash, right: &VisualHash) -> bool {
    (left.dhash ^ right.dhash).count_ones() <= 10
        && (left.phash ^ right.phash).count_ones() <= 30
        && color_distance(left.avg_rgb, right.avg_rgb) <= 500
}

fn visual_hashes(image: &RgbaImage) -> Vec<VisualHash> {
    let mut hashes = vec![hash_view(image)];
    for fraction in [0.8f32, 0.6] {
        let width = (image.width() as f32 * fraction).round().max(16.0) as u32;
        let height = (image.height() as f32 * fraction).round().max(16.0) as u32;
        let x = (image.width() - width) / 2;
        let y = (image.height() - height) / 2;
        hashes.push(hash_view(
            &image::imageops::crop_imm(image, x, y, width, height).to_image(),
        ));
    }
    hashes
}

fn hash_view(view: &RgbaImage) -> VisualHash {
    let gray =
        DynamicImage::ImageRgba8(image::imageops::resize(view, 32, 32, FilterType::Triangle))
            .to_luma8();
    let small = image::imageops::resize(&gray, 9, 8, FilterType::Triangle);
    VisualHash {
        dhash: dhash_of(&small),
        phash: phash_of(&gray),
        avg_rgb: average_rgb(view),
    }
}

fn dhash_view(view: &RgbaImage) -> u64 {
    let small = DynamicImage::ImageRgba8(image::imageops::resize(view, 9, 8, FilterType::Triangle))
        .to_luma8();
    dhash_of(&small)
}

fn dhash_of(gray: &GrayImage) -> u64 {
    let mut hash = 0u64;
    for y in 0..8u32 {
        for x in 0..8u32 {
            if gray.get_pixel(x, y)[0] > gray.get_pixel(x + 1, y)[0] {
                hash |= 1u64 << (y * 8 + x);
            }
        }
    }
    hash
}

fn phash_of(gray: &GrayImage) -> u64 {
    let mut cosine = [[0f64; 8]; 32];
    for (x, row) in cosine.iter_mut().enumerate() {
        for (u, value) in row.iter_mut().enumerate() {
            *value = (((2 * x + 1) as f64 * u as f64 * std::f64::consts::PI) / 64.0).cos();
        }
    }
    let mut coefficients = Vec::with_capacity(63);
    for v in 0..8usize {
        for u in 0..8usize {
            if u == 0 && v == 0 {
                continue;
            }
            let mut sum = 0f64;
            for y in 0..32usize {
                for x in 0..32usize {
                    sum +=
                        gray.get_pixel(x as u32, y as u32)[0] as f64 * cosine[x][u] * cosine[y][v];
                }
            }
            coefficients.push(sum);
        }
    }
    let mut sorted = coefficients.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    coefficients
        .iter()
        .enumerate()
        .fold(0u64, |hash, (index, value)| {
            if *value > median {
                hash | (1u64 << index)
            } else {
                hash
            }
        })
}

fn average_rgb(view: &RgbaImage) -> [u8; 3] {
    let image = image::imageops::resize(view, 8, 8, FilterType::Triangle);
    let mut sums = [0u64; 3];
    for pixel in image.pixels() {
        sums[0] += pixel[0] as u64;
        sums[1] += pixel[1] as u64;
        sums[2] += pixel[2] as u64;
    }
    let count = (image.width() * image.height()).max(1) as u64;
    [
        (sums[0] / count) as u8,
        (sums[1] / count) as u8,
        (sums[2] / count) as u8,
    ]
}

fn color_distance(left: [u8; 3], right: [u8; 3]) -> u32 {
    let dr = left[0] as i32 - right[0] as i32;
    let dg = left[1] as i32 - right[1] as i32;
    let db = left[2] as i32 - right[2] as i32;
    (dr * dr + dg * dg + db * db) as u32
}

fn build_payload(text: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(HEADER_BYTES + text.len() + 4);
    payload.extend_from_slice(MAGIC);
    payload.push(VERSION);
    payload.extend_from_slice(&(text.len() as u32).to_le_bytes());
    payload.extend_from_slice(text);
    payload.extend_from_slice(&crc32(text).to_le_bytes());
    payload
}

fn decode(path: &Path) -> Result<DynamicImage, String> {
    let file = File::open(path).map_err(|error| format!("无法打开图片：{error}"))?;
    let mut reader = ImageReader::new(BufReader::new(file))
        .with_guessed_format()
        .map_err(|error| format!("无法识别图片格式：{error}"))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(768 * 1024 * 1024);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|error| format!("无法解码图片：{error}"))
}

fn copy_metadata(source: &Path, output: &Path) -> bool {
    let Ok(mut metadata) = little_exif::metadata::Metadata::new_from_path(source) else {
        return false;
    };
    // The pixels were already rotated into display orientation, so avoid double rotation.
    metadata.set_tag(little_exif::exif_tag::ExifTag::Orientation(vec![1]));
    metadata.write_to_file(output).is_ok()
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

struct Geometry {
    width: u32,
    capacity: usize,
    start: u64,
    step: u64,
    key: u64,
}

impl Geometry {
    fn new(width: u32, height: u32, passphrase: &str) -> Self {
        let capacity = (width as u64 * height as u64 * 3) as usize;
        let seed = seed_from(passphrase);
        if capacity == 0 {
            return Self {
                width,
                capacity,
                start: 0,
                step: 1,
                key: seed,
            };
        }
        let start = splitmix64(seed ^ 0x243f_6a88_85a3_08d3) % capacity as u64;
        let mut step = splitmix64(seed ^ 0x1319_8a2e_0370_7344) % capacity as u64;
        if step == 0 {
            step = 1;
        }
        while gcd(step, capacity as u64) != 1 {
            step = step % capacity as u64 + 1;
        }
        Self {
            width,
            capacity,
            start,
            step,
            key: splitmix64(seed ^ 0xa409_3822_299f_31d0),
        }
    }

    fn position(&self, bit_index: usize) -> usize {
        ((self.start + bit_index as u64 * self.step) % self.capacity as u64) as usize
    }
}

fn write_payload(rgba: &mut RgbaImage, payload: &[u8], geometry: &Geometry) {
    let mut key = geometry.key;
    for (bit_index, bit_value) in payload
        .iter()
        .enumerate()
        .flat_map(|(index, byte)| (0..8).map(move |bit| (index * 8 + bit, (byte >> (7 - bit)) & 1)))
    {
        key = splitmix64(key);
        let value = bit_value ^ (key & 1) as u8;
        let position = geometry.position(bit_index);
        let pixel_index = position / 3;
        let channel = position % 3;
        let x = (pixel_index % geometry.width as usize) as u32;
        let y = (pixel_index / geometry.width as usize) as u32;
        let pixel = rgba.get_pixel_mut(x, y);
        pixel[channel] = (pixel[channel] & 0xfe) | value;
    }
}

fn read_payload(rgba: &RgbaImage, payload: &mut [u8], geometry: &Geometry) {
    let mut key = geometry.key;
    for bit_index in 0..payload.len() * 8 {
        key = splitmix64(key);
        let position = geometry.position(bit_index);
        let pixel_index = position / 3;
        let channel = position % 3;
        let x = (pixel_index % geometry.width as usize) as u32;
        let y = (pixel_index / geometry.width as usize) as u32;
        let value = (rgba.get_pixel(x, y)[channel] & 1) ^ (key & 1) as u8;
        payload[bit_index / 8] |= value << (7 - (bit_index % 8));
    }
}

fn seed_from(passphrase: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in passphrase.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^ 0x9e37_79b9_7f4a_7c15
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = value;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let rest = left % right;
        left = right;
        right = rest;
    }
    left
}

#[derive(Serialize, Deserialize)]
struct IndexFile {
    version: u32,
    records: Vec<VisualRecord>,
}

pub struct WatermarkIndex {
    path: PathBuf,
    records: Vec<VisualRecord>,
    dirty: bool,
}

impl WatermarkIndex {
    pub fn load() -> Self {
        Self::load_from(platform::config_dir().join("watermarks.json"))
    }

    pub fn load_from(path: PathBuf) -> Self {
        let records = read_index(&path);
        Self {
            path,
            records,
            dirty: false,
        }
    }

    pub fn records(&self) -> &[VisualRecord] {
        &self.records
    }

    pub fn add(&mut self, record: VisualRecord) {
        self.records.push(record);
        self.dirty = true;
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        if self.records.len() > MAX_INDEX_RECORDS {
            let excess = self.records.len() - MAX_INDEX_RECORDS;
            self.records.drain(0..excess);
        }
        let index = IndexFile {
            version: INDEX_VERSION,
            records: self.records.clone(),
        };
        let Ok(bytes) = serde_json::to_vec(&index) else {
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

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn clear(&mut self) {
        self.records.clear();
        self.dirty = false;
        let _ = fs::remove_file(&self.path);
    }
}

impl Drop for WatermarkIndex {
    fn drop(&mut self) {
        self.save();
    }
}

fn read_index(path: &Path) -> Vec<VisualRecord> {
    let Ok(metadata) = fs::metadata(path) else {
        return Vec::new();
    };
    if metadata.len() > MAX_INDEX_BYTES {
        return Vec::new();
    }
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    let Ok(index) = serde_json::from_slice::<IndexFile>(&bytes) else {
        return Vec::new();
    };
    if index.version != INDEX_VERSION {
        return Vec::new();
    }
    index.records
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn sample_png(path: &Path) {
        let image: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(240, 160, |x, y| {
            Rgb([(x % 255) as u8, (y % 255) as u8, ((x + y) % 255) as u8])
        });
        image.save(path).unwrap();
    }

    #[test]
    fn embeds_and_extracts_lsb_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.png");
        let output = dir.path().join("source.watermarked.png");
        sample_png(&source);
        let before = fs::read(&source).unwrap();
        let outcome = embed(&source, &output, "版权：Exif Studio", "").unwrap();
        assert_eq!(outcome.path, output);
        assert_eq!(fs::read(&source).unwrap(), before);

        let watermark = extract(&output, "").unwrap();
        assert_eq!(watermark.as_deref(), Some("版权：Exif Studio"));
        assert!(extract(&output, "wrong").unwrap().is_none());

        let original = image::open(&source).unwrap().to_rgb8();
        let embedded = image::open(&output).unwrap().to_rgb8();
        assert_eq!(original.dimensions(), embedded.dimensions());
        let mut differences = 0usize;
        for (a, b) in original.pixels().zip(embedded.pixels()) {
            for channel in 0..3 {
                assert!((a[channel] as i32 - b[channel] as i32).abs() <= 1);
                if a[channel] != b[channel] {
                    differences += 1;
                }
            }
        }
        assert!(differences > 0);
    }

    #[test]
    fn png_roundtrip_preserves_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roundtrip.png");
        let rgba = RgbaImage::from_fn(240, 160, |x, y| {
            image::Rgba([(x % 255) as u8, (y % 255) as u8, ((x + y) % 255) as u8, 255])
        });
        let mut file = File::create(&path).unwrap();
        DynamicImage::ImageRgba8(rgba.clone())
            .write_to(&mut file, ImageFormat::Png)
            .unwrap();
        drop(file);
        let decoded = image::open(&path).unwrap().to_rgba8();
        assert_eq!(rgba.as_raw(), decoded.as_raw());
    }

    #[test]
    fn in_memory_roundtrip() {
        let mut rgba = RgbaImage::from_fn(240, 160, |x, y| {
            image::Rgba([(x % 255) as u8, (y % 255) as u8, ((x + y) % 255) as u8, 255])
        });
        let payload = build_payload("版权：Exif Studio".as_bytes());
        let geometry = Geometry::new(240, 160, "");
        write_payload(&mut rgba, &payload, &geometry);
        let mut read = vec![0u8; payload.len()];
        read_payload(&rgba, &mut read, &geometry);
        assert_eq!(read, payload);
    }

    #[test]
    fn copies_metadata_and_keeps_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("meta.png");
        let output = dir.path().join("meta.watermarked.png");
        sample_png(&source);
        let mut metadata = little_exif::metadata::Metadata::new();
        metadata.set_tag(little_exif::exif_tag::ExifTag::Make("TestCam".to_string()));
        metadata.write_to_file(&source).unwrap();

        let outcome = embed(&source, &output, "metadata + watermark", "").unwrap();
        assert!(outcome.metadata_copied);
        assert_eq!(
            extract(&output, "").unwrap().as_deref(),
            Some("metadata + watermark")
        );
        let (info, error) = crate::exif_io::read_image_info(&output);
        assert!(error.is_none(), "error = {error:?}");
        assert_eq!(info.make.as_deref(), Some("TestCam"));
    }

    #[test]
    fn copies_jpeg_metadata_to_png_copy() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("meta.jpg");
        let output = dir.path().join("meta.watermarked.png");
        let image: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(240, 160, |x, y| Rgb([x as u8, y as u8, 90]));
        image.save(&source).unwrap();
        let mut metadata = little_exif::metadata::Metadata::new();
        metadata.set_tag(little_exif::exif_tag::ExifTag::Make("TestCam".to_string()));
        metadata.write_to_file(&source).unwrap();

        let outcome = embed(&source, &output, "jpeg watermark", "").unwrap();
        assert!(outcome.metadata_copied);
        assert_eq!(
            extract(&output, "").unwrap().as_deref(),
            Some("jpeg watermark")
        );
        let (info, error) = crate::exif_io::read_image_info(&output);
        assert!(error.is_none(), "error = {error:?}");
        assert_eq!(info.make.as_deref(), Some("TestCam"));
    }

    #[test]
    fn rejects_oversized_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("tiny.png");
        let image: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(8, 8, |_, _| Rgb([10, 20, 30]));
        image.save(&source).unwrap();
        let output = dir.path().join("tiny.watermarked.png");
        let result = embed(&source, &output, &"x".repeat(1024), "");
        assert!(result.is_err());
    }

    #[test]
    fn default_output_does_not_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("photo.jpg");
        fs::write(&source, b"x").unwrap();
        let first = default_output_path(&source, "watermarked").unwrap();
        fs::create_dir_all(first.parent().unwrap()).unwrap();
        fs::write(&first, b"x").unwrap();
        let second = default_output_path(&source, "watermarked").unwrap();
        assert_ne!(first, second);
        assert!(first.parent().unwrap().ends_with("watermarked"));
        assert!(second.to_string_lossy().contains("watermarked-1"));
        assert!(default_output_path(&source, "../bad").is_err());
    }

    #[test]
    fn finds_watermark_in_recompressed_screenshot() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.png");
        let output = dir
            .path()
            .join("watermarked")
            .join("source.watermarked.png");
        let image: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(320, 200, |x, y| {
            let stripe = ((x / 20) % 2) as u8 * 100;
            Rgb([
                (x as u8).wrapping_add(stripe),
                (y as u8).wrapping_add(40),
                (x as u8 / 3).wrapping_add(y as u8 / 2),
            ])
        });
        image.save(&source).unwrap();
        let outcome = embed(&source, &output, "screenshot robust", "secret").unwrap();

        let mut canvas = RgbaImage::from_pixel(900, 700, image::Rgba([25, 28, 34, 255]));
        let art = image::imageops::resize(
            &image::open(&output).unwrap().to_rgba8(),
            480,
            300,
            FilterType::Triangle,
        );
        image::imageops::replace(&mut canvas, &art, 135, 170);
        // Simulate a window screenshot and a second-generation JPEG upload.
        let screenshot = dir.path().join("screenshot.jpg");
        DynamicImage::ImageRgba8(canvas)
            .save_with_format(&screenshot, ImageFormat::Jpeg)
            .unwrap();

        let records = [outcome.record.clone()];
        let recognized = recognize(&screenshot, "secret", &records).unwrap();
        assert!(
            matches!(recognized, Some(Recognition::Visual(ref text)) if text == "screenshot robust")
        );
        assert!(recognize(&screenshot, "wrong", &records).unwrap().is_none());
    }

    #[test]
    fn visual_index_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watermarks.json");
        let source = dir.path().join("source.png");
        let output = dir.path().join("out.png");
        sample_png(&source);
        let outcome = embed(&source, &output, "index test", "").unwrap();
        {
            let mut index = WatermarkIndex::load_from(path.clone());
            index.add(outcome.record);
            index.save();
        }
        let reloaded = WatermarkIndex::load_from(path);
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded.records()[0].text, "index test");
    }
}
