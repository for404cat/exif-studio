use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::model::{MediaEntry, MediaKind, format_datetime};

const DEFAULT_LIMIT: usize = 200;
const MAX_LIMIT: usize = 1000;

#[derive(Clone, Serialize)]
pub struct ApiFile {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub format: String,
    pub size: u64,
    pub modified: Option<String>,
    pub resolution: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub taken_at: Option<String>,
    pub iso: Option<u32>,
    pub aperture: Option<f64>,
    pub exposure_time: Option<f64>,
    pub focal_length: Option<f64>,
    pub gps: Option<(f64, f64)>,
    pub codec: Option<String>,
    pub container: Option<String>,
    pub fps: Option<f64>,
    pub duration_secs: Option<f64>,
    pub read_error: Option<String>,
}

impl ApiFile {
    pub(crate) fn from_entry(entry: &MediaEntry) -> Self {
        let image = entry.image.as_ref();
        let video = entry.video.as_ref();
        ApiFile {
            path: entry.path.to_string_lossy().into_owned(),
            name: entry.name.clone(),
            kind: match entry.kind {
                MediaKind::Image => "image".to_string(),
                MediaKind::Video => "video".to_string(),
            },
            format: entry.format_label(),
            size: entry.size,
            modified: entry.modified.map(|value| value.to_rfc3339()),
            resolution: entry.resolution_label(),
            width: image
                .and_then(|i| i.width)
                .or_else(|| video.and_then(|v| v.width)),
            height: image
                .and_then(|i| i.height)
                .or_else(|| video.and_then(|v| v.height)),
            camera: entry.camera_label(),
            lens: image.and_then(|i| i.lens.clone()),
            taken_at: entry.taken_at().map(|value| format_datetime(&value)),
            iso: image.and_then(|i| i.iso),
            aperture: image.and_then(|i| i.f_number),
            exposure_time: image.and_then(|i| i.exposure_time),
            focal_length: image.and_then(|i| i.focal_length),
            gps: image
                .and_then(|i| i.gps)
                .or_else(|| video.and_then(|v| v.gps)),
            codec: video.and_then(|v| v.codec.clone()),
            container: video.and_then(|v| v.container.clone()),
            fps: video.and_then(|v| v.fps),
            duration_secs: video.and_then(|v| v.duration_secs),
            read_error: entry.read_error.clone(),
        }
    }
}

#[derive(Clone, Serialize)]
pub struct ApiFolder {
    pub id: u64,
    pub path: String,
    pub scanning: bool,
    pub files: Vec<ApiFile>,
}

#[derive(Clone, Serialize)]
struct ApiFolderSummary {
    id: u64,
    path: String,
    scanning: bool,
    file_count: usize,
}

#[derive(Default)]
pub struct ApiRegistry {
    folders: RwLock<Vec<ApiFolder>>,
}

impl ApiRegistry {
    pub fn publish(&self, folders: Vec<ApiFolder>) {
        let mut guard = self
            .folders
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = folders;
    }

    fn summaries(&self) -> Vec<ApiFolderSummary> {
        let guard = self
            .folders
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .iter()
            .map(|folder| ApiFolderSummary {
                id: folder.id,
                path: folder.path.clone(),
                scanning: folder.scanning,
                file_count: folder.files.len(),
            })
            .collect()
    }

    fn files(
        &self,
        folder: Option<u64>,
        query: &str,
        kind: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Option<(usize, Vec<ApiFile>)> {
        let guard = self
            .folders
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let folders: Vec<&ApiFolder> = match folder {
            Some(id) => {
                let found = guard.iter().find(|item| item.id == id)?;
                vec![found]
            }
            None => guard.iter().collect(),
        };
        let needle = query.to_lowercase();
        let mut matched = Vec::new();
        for item in folders {
            for file in &item.files {
                if let Some(kind) = kind
                    && !file.kind.eq_ignore_ascii_case(kind)
                {
                    continue;
                }
                if !needle.is_empty() && !matches_query(file, &needle) {
                    continue;
                }
                matched.push(file.clone());
            }
        }
        let total = matched.len();
        let files = matched.into_iter().skip(offset).take(limit).collect();
        Some((total, files))
    }

    fn file(&self, path: &str) -> Option<ApiFile> {
        let guard = self
            .folders
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .iter()
            .flat_map(|folder| folder.files.iter())
            .find(|file| file.path == path)
            .cloned()
    }
}

fn matches_query(file: &ApiFile, needle: &str) -> bool {
    file.name.to_lowercase().contains(needle)
        || file.path.to_lowercase().contains(needle)
        || file
            .camera
            .as_deref()
            .map(|value| value.to_lowercase().contains(needle))
            .unwrap_or(false)
        || file
            .lens
            .as_deref()
            .map(|value| value.to_lowercase().contains(needle))
            .unwrap_or(false)
}

pub struct ApiServer {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    pub port: u16,
}

impl ApiServer {
    pub fn start(port: u16, token: String, registry: Arc<ApiRegistry>) -> Result<Self, String> {
        let server = Server::http(("127.0.0.1", port))
            .map_err(|error| format!("无法监听 127.0.0.1:{port}：{error}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let join = std::thread::Builder::new()
            .name("exif-api".to_string())
            .spawn(move || {
                while !thread_stop.load(Ordering::Relaxed) {
                    match server.recv_timeout(Duration::from_millis(250)) {
                        Ok(Some(request)) => handle_request(request, &token, &registry),
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
            })
            .map_err(|error| format!("无法启动 API 线程：{error}"))?;
        Ok(Self {
            stop,
            join: Some(join),
            port,
        })
    }
}

impl Drop for ApiServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn handle_request(request: Request, token: &str, registry: &ApiRegistry) {
    if !request.method().eq(&Method::Get) {
        respond_json(request, 405, &serde_json::json!({"error": "只支持 GET"}));
        return;
    }
    if !authorized(&request, token) {
        let response = Response::from_string("{\"error\":\"unauthorized\"}")
            .with_status_code(StatusCode(401))
            .with_header(json_header())
            .with_header(no_store_header())
            .with_header(
                Header::from_bytes("WWW-Authenticate", "Bearer").expect("static auth header"),
            );
        let _ = request.respond(response);
        return;
    }

    let (path, query) = split_url(request.url());
    match path {
        "/v1/ping" => respond_json(
            request,
            200,
            &serde_json::json!({
                "ok": true,
                "name": "Exif Studio",
                "version": env!("CARGO_PKG_VERSION"),
            }),
        ),
        "/v1/folders" => respond_json(
            request,
            200,
            &serde_json::json!({"folders": registry.summaries()}),
        ),
        "/v1/files" => {
            let folder = query.get("folder").and_then(|value| value.parse().ok());
            let kind = query
                .get("kind")
                .map(|value| value.to_lowercase())
                .filter(|value| value == "image" || value == "video");
            let text = query.get("q").cloned().unwrap_or_default();
            let limit = query
                .get("limit")
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_LIMIT)
                .clamp(1, MAX_LIMIT);
            let offset = query
                .get("offset")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            match registry.files(folder, &text, kind.as_deref(), limit, offset) {
                Some((total, files)) => respond_json(
                    request,
                    200,
                    &serde_json::json!({"total": total, "files": files}),
                ),
                None => respond_json(request, 404, &serde_json::json!({"error": "文件夹不存在"})),
            }
        }
        "/v1/file" => {
            let Some(path) = query.get("path") else {
                respond_json(
                    request,
                    400,
                    &serde_json::json!({"error": "缺少 path 参数"}),
                );
                return;
            };
            match registry.file(path) {
                Some(file) => respond_json(request, 200, &serde_json::json!({"file": file})),
                None => respond_json(
                    request,
                    404,
                    &serde_json::json!({"error": "文件不在当前打开的文件夹中"}),
                ),
            }
        }
        _ => respond_json(request, 404, &serde_json::json!({"error": "未知接口"})),
    }
}

fn authorized(request: &Request, token: &str) -> bool {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Authorization"))
        .and_then(|header| header.value.as_str().strip_prefix("Bearer "))
        .map(|provided| constant_time_eq(token.as_bytes(), provided.trim().as_bytes()))
        .unwrap_or(false)
}

fn constant_time_eq(expected: &[u8], provided: &[u8]) -> bool {
    if expected.len() != provided.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in expected.iter().zip(provided) {
        difference |= left ^ right;
    }
    difference == 0
}

fn respond_json(request: Request, status: u16, value: &impl Serialize) {
    let body = serde_json::to_string(value)
        .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".to_string());
    let response = Response::from_string(body)
        .with_status_code(StatusCode(status))
        .with_header(json_header())
        .with_header(no_store_header())
        .with_header(
            Header::from_bytes("X-Content-Type-Options", "nosniff")
                .expect("static security header"),
        );
    let _ = request.respond(response);
}

fn json_header() -> Header {
    Header::from_bytes("Content-Type", "application/json; charset=utf-8")
        .expect("static content type")
}

fn no_store_header() -> Header {
    Header::from_bytes("Cache-Control", "no-store").expect("static cache header")
}

fn split_url(url: &str) -> (&str, HashMap<String, String>) {
    let (path, raw_query) = url.split_once('?').unwrap_or((url, ""));
    let mut query = HashMap::new();
    for pair in raw_query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(percent_decode(key), percent_decode(value));
    }
    (path, query)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                    out.push(high * 16 + low);
                    index += 3;
                    continue;
                }
                out.push(bytes[index]);
            }
            b'+' => out.push(b' '),
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    if getrandom::fill(&mut bytes).is_err() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0);
        for chunk in 0..4 {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_u128(now);
            hasher.write_u32(std::process::id());
            hasher.write_usize(chunk);
            let value = hasher.finish().to_le_bytes();
            bytes[chunk * 8..chunk * 8 + 8].copy_from_slice(&value);
        }
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_query_values() {
        let (path, query) = split_url("/v1/file?path=C%3A%5CPhotos%5Ca%20b.jpg&x=1");
        assert_eq!(path, "/v1/file");
        assert_eq!(
            query.get("path").map(String::as_str),
            Some(r"C:\Photos\a b.jpg")
        );
    }

    #[test]
    fn compares_tokens_without_prefix_match() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn generates_stable_length_tokens() {
        let token = generate_token();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|value| value.is_ascii_hexdigit()));
        assert_ne!(token, generate_token());
    }
}
