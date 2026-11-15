use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, UNIX_EPOCH};

use chrono::{DateTime, Local};
use walkdir::WalkDir;

use crate::exif_io::read_image_info;
use crate::metacache::MetadataCache;
use crate::model::{MediaEntry, MediaKind, kind_for_extension};
use crate::video::read_video_info;

const BATCH_SIZE: usize = 32;
const WORKER_LIMIT: usize = 24;

pub enum ScanMsg {
    Discovering { files: usize },
    Started { total: usize },
    Entries(Vec<MediaEntry>),
    Finished { total: usize, elapsed: Duration },
    Failed(String),
}

pub struct ScanHandle {
    pub rx: Receiver<ScanMsg>,
    pub cancel: Arc<AtomicBool>,
}

impl Drop for ScanHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

struct Candidate {
    path: PathBuf,
    kind: MediaKind,
}

pub fn start_scan(
    folder: PathBuf,
    recursive: bool,
    cache: Arc<MetadataCache>,
) -> Result<ScanHandle, String> {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let thread_cancel = cancel.clone();

    std::thread::Builder::new()
        .name("exif-scan".to_string())
        .spawn(move || {
            let started = Instant::now();
            // Discovery only reads directory entries. Metadata (size/mtime) and
            // EXIF are fetched by the parallel workers later, so the progress
            // bar appears immediately even on a slow SMB share.
            let _ = tx.send(ScanMsg::Discovering { files: 0 });
            if let Err(error) = std::fs::read_dir(&folder) {
                let _ = tx.send(ScanMsg::Failed(format!("无法访问文件夹：{error}")));
                return;
            }
            let walker = if recursive {
                WalkDir::new(&folder)
            } else {
                WalkDir::new(&folder).max_depth(1)
            };

            let mut candidates: Vec<Candidate> = Vec::new();
            let mut last_report = Instant::now();
            for entry in walker.into_iter().filter_map(Result::ok) {
                if thread_cancel.load(Ordering::Relaxed) {
                    return;
                }
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.into_path();
                let Some(kind) = kind_for_extension(
                    &path
                        .extension()
                        .map(|value| value.to_string_lossy().to_lowercase())
                        .unwrap_or_default(),
                ) else {
                    continue;
                };
                candidates.push(Candidate { path, kind });
                if last_report.elapsed() >= Duration::from_millis(180) {
                    if tx
                        .send(ScanMsg::Discovering {
                            files: candidates.len(),
                        })
                        .is_err()
                    {
                        return;
                    }
                    last_report = Instant::now();
                }
            }

            candidates.sort_by_cached_key(|candidate| {
                (
                    candidate
                        .path
                        .file_name()
                        .map(|name| name.to_string_lossy().to_lowercase())
                        .unwrap_or_default(),
                    candidate.path.clone(),
                )
            });

            let total = candidates.len();
            if tx.send(ScanMsg::Discovering { files: total }).is_err() {
                return;
            }
            if tx.send(ScanMsg::Started { total }).is_err() {
                return;
            }

            if total > 0 {
                let mut worker_error = None;
                let candidates = Arc::new(candidates);
                let next = Arc::new(AtomicUsize::new(0));
                let (result_tx, result_rx) = mpsc::channel::<(usize, Option<MediaEntry>)>();
                let workers = std::thread::available_parallelism()
                    .map(|value| value.get())
                    .unwrap_or(4)
                    .clamp(4, WORKER_LIMIT)
                    .min(total);

                for index in 0..workers {
                    let candidates = candidates.clone();
                    let next = next.clone();
                    let cancel = thread_cancel.clone();
                    let cache = cache.clone();
                    let result_tx = result_tx.clone();
                    let spawn = std::thread::Builder::new()
                        .name(format!("exif-scan-{index}"))
                        .spawn(move || {
                            loop {
                                if cancel.load(Ordering::Relaxed) {
                                    return;
                                }
                                let position = next.fetch_add(1, Ordering::Relaxed);
                                let Some(candidate) = candidates.get(position) else {
                                    return;
                                };
                                let entry = load_candidate(candidate, &cache);
                                if result_tx.send((position, entry)).is_err() {
                                    return;
                                }
                            }
                        });
                    if let Err(error) = spawn {
                        worker_error = Some(format!("无法启动扫描线程：{error}"));
                        break;
                    }
                }
                drop(result_tx);

                if let Some(error) = worker_error {
                    let _ = tx.send(ScanMsg::Failed(error));
                    return;
                }

                let mut pending = BTreeMap::new();
                let mut next_index = 0usize;
                let mut batch = Vec::with_capacity(BATCH_SIZE);
                while next_index < total {
                    if thread_cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    match result_rx.recv_timeout(Duration::from_millis(100)) {
                        Ok((position, entry)) => {
                            pending.insert(position, entry);
                            while let Some(entry) = pending.remove(&next_index) {
                                if let Some(entry) = entry {
                                    batch.push(entry);
                                }
                                next_index += 1;
                                if batch.len() >= BATCH_SIZE
                                    && tx
                                        .send(ScanMsg::Entries(std::mem::take(&mut batch)))
                                        .is_err()
                                {
                                    return;
                                }
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                for (_, entry) in pending {
                    if let Some(entry) = entry {
                        batch.push(entry);
                    }
                }
                if !batch.is_empty() && tx.send(ScanMsg::Entries(batch)).is_err() {
                    return;
                }
            }

            let _ = tx.send(ScanMsg::Finished {
                total,
                elapsed: started.elapsed(),
            });
            cache.save();
        })
        .map_err(|error| format!("无法启动扫描线程：{error}"))?;

    Ok(ScanHandle { rx, cancel })
}

pub fn load_entry(path: &Path, cache: &MetadataCache) -> MediaEntry {
    let ext = path
        .extension()
        .map(|value| value.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let kind = kind_for_extension(&ext).unwrap_or(MediaKind::Image);
    let metadata = std::fs::metadata(path).ok();
    let size = metadata.as_ref().map(|value| value.len()).unwrap_or(0);
    let (modified, modified_ns) = metadata.as_ref().map(file_times).unwrap_or((None, 0));
    build_entry(path, kind, size, modified, modified_ns, cache)
}

fn load_candidate(candidate: &Candidate, cache: &MetadataCache) -> Option<MediaEntry> {
    let metadata = std::fs::metadata(&candidate.path).ok()?;
    let (modified, modified_ns) = file_times(&metadata);
    Some(build_entry(
        &candidate.path,
        candidate.kind,
        metadata.len(),
        modified,
        modified_ns,
        cache,
    ))
}

fn build_entry(
    path: &Path,
    kind: MediaKind,
    size: u64,
    modified: Option<DateTime<Local>>,
    modified_ns: i64,
    cache: &MetadataCache,
) -> MediaEntry {
    let mut entry = MediaEntry {
        path: path.to_path_buf(),
        kind,
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
        ext: path
            .extension()
            .map(|value| value.to_string_lossy().to_lowercase())
            .unwrap_or_default(),
        size,
        modified,
        image: None,
        video: None,
        read_error: None,
    };

    if let Some(cached) = cache.lookup(path, size, modified_ns) {
        let matches = match kind {
            MediaKind::Image => cached.image.is_some(),
            MediaKind::Video => cached.video.is_some(),
        };
        if matches {
            entry.image = cached.image;
            entry.video = cached.video;
            return entry;
        }
    }

    match kind {
        MediaKind::Image => {
            let (info, error) = read_image_info(path);
            if error.is_none() {
                cache.put(path, size, modified_ns, Some(info.clone()), None);
            }
            entry.image = Some(info);
            entry.read_error = error;
        }
        MediaKind::Video => {
            let (info, error) = read_video_info(path);
            if error.is_none() {
                cache.put(path, size, modified_ns, None, Some(info.clone()));
            }
            entry.video = Some(info);
            entry.read_error = error;
        }
    }
    entry
}

fn file_times(metadata: &std::fs::Metadata) -> (Option<DateTime<Local>>, i64) {
    let modified_system = metadata.modified().ok();
    let modified = modified_system.map(DateTime::<Local>::from);
    let modified_ns = modified_system
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos() as i64)
        .unwrap_or(0);
    (modified, modified_ns)
}
