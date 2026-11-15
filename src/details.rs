use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use crate::exif_io::{self, EditFields, ExifField, ExifGroup};
use crate::model::{
    MediaEntry, MediaKind, format_fps, format_friendly_datetime, is_writable_extension,
};

pub struct DetailsState {
    pub path: PathBuf,
    pub groups: Vec<ExifGroup>,
    pub editable: Option<EditFields>,
    pub original: Option<EditFields>,
    pub is_writable: bool,
    pub exif_error: Option<String>,
    pub write_error: Option<String>,
    pub last_saved: Option<String>,
    pub brand_custom: bool,
}

pub struct DetailsResult {
    pub tab_id: u64,
    pub path: PathBuf,
    pub details: DetailsState,
}

struct Request {
    tab_id: u64,
    entry: MediaEntry,
}

pub struct DetailsService {
    requests: Sender<Request>,
    results: Receiver<DetailsResult>,
}

impl Default for DetailsService {
    fn default() -> Self {
        Self::new()
    }
}

impl DetailsService {
    pub fn new() -> Self {
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        let (result_tx, result_rx) = mpsc::channel::<DetailsResult>();
        std::thread::Builder::new()
            .name("exif-details".to_string())
            .spawn(move || {
                while let Ok(request) = request_rx.recv() {
                    let details = build_details(&request.entry);
                    if result_tx
                        .send(DetailsResult {
                            tab_id: request.tab_id,
                            path: request.entry.path,
                            details,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            })
            .expect("无法启动元数据详情线程");
        Self {
            requests: request_tx,
            results: result_rx,
        }
    }

    pub fn request(&self, tab_id: u64, entry: MediaEntry) {
        let _ = self.requests.send(Request { tab_id, entry });
    }

    pub fn poll(&self) -> Vec<DetailsResult> {
        self.results.try_iter().collect()
    }
}

pub fn build_details(entry: &MediaEntry) -> DetailsState {
    match entry.kind {
        MediaKind::Image => {
            let mut exif_error = None;
            let groups = match exif_io::read_image_details(&entry.path) {
                Ok(groups) => groups,
                Err(error) => {
                    if !error.contains("No Exif data found") {
                        exif_error = Some(error);
                    }
                    Vec::new()
                }
            };
            let writable = is_writable_extension(&entry.ext);
            let (editable, original, write_error) = if writable {
                match exif_io::read_editable(&entry.path) {
                    Ok(fields) => (Some(fields.clone()), Some(fields), None),
                    Err(error) => (None, None, Some(error)),
                }
            } else {
                (None, None, None)
            };
            DetailsState {
                path: entry.path.clone(),
                groups,
                editable,
                original,
                is_writable: writable,
                exif_error,
                write_error,
                last_saved: None,
                brand_custom: false,
            }
        }
        MediaKind::Video => DetailsState {
            path: entry.path.clone(),
            groups: video_groups(entry),
            editable: None,
            original: None,
            is_writable: false,
            exif_error: entry.read_error.clone(),
            write_error: None,
            last_saved: None,
            brand_custom: false,
        },
    }
}

fn video_groups(entry: &MediaEntry) -> Vec<ExifGroup> {
    let Some(video) = &entry.video else {
        return Vec::new();
    };
    let dash = || "—".to_string();
    let mut summary = vec![
        ExifField {
            label: "编码格式".to_string(),
            value: video.codec.clone().unwrap_or_else(dash),
        },
        ExifField {
            label: "封装格式".to_string(),
            value: video.container.clone().unwrap_or_else(dash),
        },
        ExifField {
            label: "帧率".to_string(),
            value: video.fps.map(format_fps).unwrap_or_else(dash),
        },
        ExifField {
            label: "分辨率".to_string(),
            value: video.resolution_label().unwrap_or_else(dash),
        },
    ];
    if let Some(duration) = video.duration_label() {
        summary.push(ExifField {
            label: "时长".to_string(),
            value: duration,
        });
    }
    if let Some(device) = video.camera_label() {
        summary.push(ExifField {
            label: "设备".to_string(),
            value: device,
        });
    }
    if let Some(created) = video.created_at {
        summary.push(ExifField {
            label: "创建时间".to_string(),
            value: format_friendly_datetime(&created),
        });
    }
    if let Some(gps) = video.gps {
        summary.push(ExifField {
            label: "GPS".to_string(),
            value: format!("{:.6}, {:.6}", gps.0, gps.1),
        });
    }

    let mut groups = vec![ExifGroup {
        name: "视频概要".to_string(),
        fields: summary,
    }];
    if !video.tags.is_empty() {
        groups.push(ExifGroup {
            name: "容器元数据标签".to_string(),
            fields: video
                .tags
                .iter()
                .map(|(label, value)| ExifField {
                    label: label.clone(),
                    value: value.clone(),
                })
                .collect(),
        });
    }
    groups
}
