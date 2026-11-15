use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use eframe::egui::ColorImage;
use image::{DynamicImage, ImageFormat};

use crate::cache::CacheStore;
use crate::exif_io::read_embedded_thumbnail;
use crate::model::is_video_extension;
use crate::video_poster;

const EMBEDDED_THUMBNAIL_MAX: u32 = 256;

pub struct ImageJob {
    pub path: PathBuf,
    pub max_size: u32,
    pub orientation: u16,
    pub cache_key: u64,
}

pub struct ImageResult {
    pub path: PathBuf,
    pub max_size: u32,
    pub cache_key: u64,
    pub result: Result<ColorImage, String>,
}

pub struct ImageService {
    tx: Sender<ImageJob>,
    rx: Receiver<ImageResult>,
}

impl ImageService {
    pub fn new(name: &str, workers: usize, cache: Arc<CacheStore>) -> Self {
        let (tx, rx) = mpsc::channel::<ImageJob>();
        let (result_tx, result_rx) = mpsc::channel::<ImageResult>();
        let jobs = Arc::new(Mutex::new(rx));
        for index in 0..workers.max(1) {
            let jobs = jobs.clone();
            let result_tx = result_tx.clone();
            let cache = cache.clone();
            std::thread::Builder::new()
                .name(format!("{name}-{index}"))
                .spawn(move || {
                    loop {
                        let job = {
                            let jobs = jobs.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                            jobs.recv()
                        };
                        let Ok(job) = job else {
                            return;
                        };
                        let result = load_job(&job, &cache);
                        if result_tx
                            .send(ImageResult {
                                path: job.path,
                                max_size: job.max_size,
                                cache_key: job.cache_key,
                                result,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                })
                .expect("failed to start image worker");
        }
        Self { tx, rx: result_rx }
    }

    pub fn request(&self, job: ImageJob) {
        let _ = self.tx.send(job);
    }

    pub fn poll(&self) -> Vec<ImageResult> {
        let mut results = Vec::new();
        while let Ok(result) = self.rx.try_recv() {
            results.push(result);
        }
        results
    }
}

fn load_job(job: &ImageJob, cache: &CacheStore) -> Result<ColorImage, String> {
    if let Some(bytes) = cache.load(job.cache_key)
        && let Ok(image) = image::load_from_memory(&bytes)
    {
        return Ok(to_color_image(image));
    }

    let decoded = if is_video(&job.path) {
        video_poster::load_poster(&job.path, job.max_size)?
    } else {
        decode_source(&job.path, job.max_size)?
    };
    let oriented = apply_orientation(decoded, job.orientation);
    let thumb = oriented.thumbnail(job.max_size, job.max_size);
    store_thumbnail(cache, job.cache_key, &thumb);
    Ok(to_color_image(thumb))
}

fn decode_source(path: &Path, max_size: u32) -> Result<DynamicImage, String> {
    if max_size <= EMBEDDED_THUMBNAIL_MAX
        && is_jpeg(path)
        && let Some(bytes) = read_embedded_thumbnail(path)
        && let Ok(image) = image::load_from_memory(&bytes)
    {
        return Ok(image);
    }
    let reader = image::ImageReader::open(path)
        .map_err(|error| format!("无法打开图片：{error}"))?
        .with_guessed_format()
        .map_err(|error| format!("无法识别图片格式：{error}"))?;
    reader
        .decode()
        .map_err(|error| format!("无法解码预览：{error}"))
}

fn store_thumbnail(cache: &CacheStore, key: u64, image: &DynamicImage) {
    let format = if image.color().has_alpha() {
        ImageFormat::Png
    } else {
        ImageFormat::Jpeg
    };
    let mut bytes = Vec::new();
    if image.write_to(&mut Cursor::new(&mut bytes), format).is_ok() {
        cache.store(key, &bytes);
    }
}

fn to_color_image(image: DynamicImage) -> ColorImage {
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    ColorImage::from_rgba_unmultiplied([width as usize, height as usize], rgba.as_raw())
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

fn is_video(path: &Path) -> bool {
    path.extension()
        .map(|value| is_video_extension(&value.to_string_lossy().to_lowercase()))
        .unwrap_or(false)
}
