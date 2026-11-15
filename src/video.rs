use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use chrono::{DateTime, NaiveDateTime};

use crate::model::VideoInfo;

const MAX_MOOV_SIZE: u64 = 256 * 1024 * 1024;
const MAC_EPOCH_OFFSET: i64 = 2_082_844_800;

#[derive(Clone, Copy, Debug)]
struct BmffBox<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}

pub fn read_video_info(path: &Path) -> (VideoInfo, Option<String>) {
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let container = read_container(path);
    let moov = match read_moov(path) {
        Ok(moov) => moov,
        Err(err) => return (VideoInfo::default(), Some(err)),
    };
    match parse_moov(&moov, file_size) {
        Ok(mut info) => {
            info.container = container;
            (info, None)
        }
        Err(err) => (VideoInfo::default(), Some(err)),
    }
}

fn read_container(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut buffer = vec![0u8; 4096];
    let read = file.read(&mut buffer).ok()?;
    buffer.truncate(read);
    let brand = boxes(&buffer)
        .into_iter()
        .find(|item| &item.kind == b"ftyp")
        .and_then(|item| {
            item.body
                .get(0..4)
                .map(|value| [value[0], value[1], value[2], value[3]])
        });
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_uppercase())
        .unwrap_or_default();
    if let Some(brand) = brand {
        let label = match &brand {
            b"qt  " => Some("QuickTime MOV"),
            b"isom" | b"iso2" | b"iso4" | b"iso5" | b"iso6" | b"mp41" | b"mp42" | b"avc1"
            | b"M4V " | b"M4A " | b"dash" | b"msnv" => Some("MP4"),
            b"3gp4" | b"3gp5" | b"3gp6" | b"3g2a" => Some("3GPP"),
            _ => None,
        };
        if let Some(label) = label {
            return Some(label.to_string());
        }
    }
    match extension.as_str() {
        "MOV" => Some("QuickTime MOV".to_string()),
        "MP4" | "M4V" => Some("MP4".to_string()),
        "3GP" | "3G2" => Some("3GPP".to_string()),
        "" => None,
        other => Some(other.to_string()),
    }
}

fn read_moov(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|e| format!("无法打开视频：{e}"))?;
    let length = file
        .metadata()
        .map_err(|e| format!("无法读取文件信息：{e}"))?
        .len();
    let mut pos = 0u64;

    while pos + 8 <= length {
        file.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
        let mut head = [0u8; 8];
        file.read_exact(&mut head).map_err(|e| e.to_string())?;
        let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as u64;
        let kind = [head[4], head[5], head[6], head[7]];
        let (header_len, box_size) = match size32 {
            0 => (8u64, length - pos),
            1 => {
                let mut large = [0u8; 8];
                file.read_exact(&mut large).map_err(|e| e.to_string())?;
                (16u64, u64::from_be_bytes(large))
            }
            n => (8u64, n),
        };
        if box_size < header_len || pos.saturating_add(box_size) > length {
            break;
        }
        if &kind == b"moov" {
            let body_len = box_size - header_len;
            if body_len > MAX_MOOV_SIZE {
                return Err("moov 数据过大，已跳过解析".to_string());
            }
            let mut body = vec![0u8; body_len as usize];
            file.read_exact(&mut body).map_err(|e| e.to_string())?;
            return Ok(body);
        }
        pos = pos.saturating_add(box_size);
    }

    Err("文件中未找到 MP4/MOV 的 moov 元数据".to_string())
}

fn parse_moov(moov: &[u8], file_size: u64) -> Result<VideoInfo, String> {
    let mut info = VideoInfo::default();
    let mut movie_creation: Option<i64> = None;
    let mut movie_duration: Option<f64> = None;
    let mut video_track: Option<TrackInfo> = None;
    let mut audio_codec: Option<String> = None;
    let mut all_tags: Vec<(String, String)> = Vec::new();

    for item in boxes(moov) {
        match &item.kind {
            b"mvhd" => {
                if let Some(header) = parse_mvhd(item.body) {
                    movie_creation = header.creation;
                    if let (Some(timescale), Some(duration)) = (header.timescale, header.duration)
                        && timescale > 0
                    {
                        movie_duration = Some(duration as f64 / timescale as f64);
                    }
                }
            }
            b"trak" => {
                if let Some(track) = parse_trak(item.body) {
                    match track.kind {
                        TrackKind::Video => {
                            if video_track.is_none() {
                                video_track = Some(track);
                            }
                        }
                        TrackKind::Audio => {
                            if audio_codec.is_none() {
                                audio_codec = track.codec.clone();
                            }
                        }
                        TrackKind::Unknown => {}
                    }
                }
            }
            b"udta" => collect_udta(item.body, &mut all_tags),
            b"meta" => collect_meta(item.body, &mut all_tags),
            _ => {}
        }
    }

    if let Some(track) = &video_track {
        info.width = track.width;
        info.height = track.height;
        info.codec = track.codec.clone();
        info.fps = track.fps;
        if info.width.is_none() {
            info.width = track.stsd_width;
        }
        if info.height.is_none() {
            info.height = track.stsd_height;
        }
    }
    if let Some(codec) = audio_codec {
        all_tags.push(("音频编码".to_string(), codec));
    }

    info.duration_secs = movie_duration.or_else(|| video_track.as_ref().and_then(|t| t.duration));

    if let Some(creation) = movie_creation {
        info.created_at = mac_timestamp_to_datetime(creation);
    }

    let mut make_key = None;
    let mut model_key = None;
    let mut date_key = None;
    let mut gps_key = None;
    for (key, value) in &all_tags {
        if value.is_empty() {
            continue;
        }
        match key.as_str() {
            "设备品牌" => make_key = Some(value.clone()),
            "设备型号" => model_key = Some(value.clone()),
            "创建时间" => date_key = Some(value.clone()),
            "位置" => gps_key = Some(value.clone()),
            _ => {}
        }
    }
    info.make = make_key;
    info.model = model_key;
    if let Some(date) = date_key
        && let Some(parsed) = parse_iso_datetime(&date)
    {
        info.created_at = Some(parsed);
    }
    if let Some(gps) = gps_key {
        info.gps = parse_iso6709(&gps);
    }

    info.tags = dedup_tags(all_tags);
    if info.duration_secs.unwrap_or(0.0) > 0.0 {
        let bitrate = (file_size as f64 * 8.0 / info.duration_secs.unwrap()) as u64;
        info.tags
            .push(("平均码率".to_string(), format_bitrate(bitrate)));
    }
    if let Some(res) = info.resolution_label() {
        info.tags.push(("分辨率".to_string(), res));
    }
    if let Some(fps) = info.fps {
        info.tags
            .push(("帧率".to_string(), format!("{fps:.2} fps")));
    }
    if let Some(duration) = info.duration_secs {
        info.tags
            .push(("时长".to_string(), crate::model::format_duration(duration)));
    }

    Ok(info)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TrackKind {
    Video,
    Audio,
    Unknown,
}

struct TrackInfo {
    kind: TrackKind,
    codec: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    stsd_width: Option<u32>,
    stsd_height: Option<u32>,
    fps: Option<f64>,
    duration: Option<f64>,
}

fn parse_trak(trak: &[u8]) -> Option<TrackInfo> {
    let mut kind = TrackKind::Unknown;
    let mut codec = None;
    let mut width = None;
    let mut height = None;
    let mut stsd_width = None;
    let mut stsd_height = None;
    let mut fps = None;
    let mut duration = None;

    for child in boxes(trak) {
        match &child.kind {
            b"tkhd" => {
                let parsed = parse_tkhd(child.body);
                width = parsed.width;
                height = parsed.height;
            }
            b"mdia" => {
                let mdia_body = child.body;
                for media in boxes(mdia_body) {
                    match &media.kind {
                        b"mdhd" => {
                            if let Some(header) = parse_mvhd_like(media.body)
                                && let (Some(timescale), Some(dur)) =
                                    (header.timescale, header.duration)
                                && timescale > 0
                            {
                                duration = Some(dur as f64 / timescale as f64);
                            }
                        }
                        b"hdlr" if media.body.len() >= 12 => {
                            kind = match &media.body[8..12] {
                                b"vide" => TrackKind::Video,
                                b"soun" => TrackKind::Audio,
                                _ => TrackKind::Unknown,
                            };
                        }
                        b"minf" => {
                            for minf in boxes(media.body) {
                                if &minf.kind != b"stbl" {
                                    continue;
                                }
                                for stbl in boxes(minf.body) {
                                    match &stbl.kind {
                                        b"stsd" => {
                                            let parsed = parse_stsd(stbl.body);
                                            codec = parsed.codec;
                                            stsd_width = parsed.width;
                                            stsd_height = parsed.height;
                                        }
                                        b"stts" if kind == TrackKind::Video => {
                                            fps = parse_stts_fps(stbl.body, mdia_body);
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    if kind == TrackKind::Unknown {
        return None;
    }
    Some(TrackInfo {
        kind,
        codec,
        width,
        height,
        stsd_width,
        stsd_height,
        fps,
        duration,
    })
}

struct VideoHeader {
    creation: Option<i64>,
    timescale: Option<u32>,
    duration: Option<u64>,
}

fn parse_mvhd(body: &[u8]) -> Option<VideoHeader> {
    parse_mvhd_like(body)
}

fn parse_mvhd_like(body: &[u8]) -> Option<VideoHeader> {
    if body.len() < 20 {
        return None;
    }
    let version = body[0];
    if version == 1 {
        if body.len() < 32 {
            return None;
        }
        let creation = u64::from_be_bytes(body[4..12].try_into().ok()?);
        let timescale = u32::from_be_bytes(body[20..24].try_into().ok()?);
        let duration = u64::from_be_bytes(body[24..32].try_into().ok()?);
        Some(VideoHeader {
            creation: Some(creation as i64),
            timescale: Some(timescale),
            duration: Some(duration),
        })
    } else {
        let creation = u32::from_be_bytes(body[4..8].try_into().ok()?);
        let timescale = u32::from_be_bytes(body[12..16].try_into().ok()?);
        let duration = u32::from_be_bytes(body[16..20].try_into().ok()?);
        Some(VideoHeader {
            creation: Some(creation as i64),
            timescale: Some(timescale),
            duration: Some(duration as u64),
        })
    }
}

fn parse_tkhd(body: &[u8]) -> TrackDimensions {
    let mut dims = TrackDimensions::default();
    if body.len() < 8 {
        return dims;
    }
    let version = body[0];
    let offset = if version == 1 { 88 } else { 76 };
    if body.len() >= offset + 8 {
        let w = u32::from_be_bytes(body[offset..offset + 4].try_into().unwrap());
        let h = u32::from_be_bytes(body[offset + 4..offset + 8].try_into().unwrap());
        if w > 0 {
            dims.width = Some(w >> 16);
        }
        if h > 0 {
            dims.height = Some(h >> 16);
        }
    }
    dims
}

#[derive(Default)]
struct TrackDimensions {
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Default)]
struct SampleDescription {
    codec: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

fn parse_stsd(body: &[u8]) -> SampleDescription {
    let mut description = SampleDescription::default();
    if body.len() < 16 {
        return description;
    }
    let entry_start = 8usize;
    if body.len() < entry_start + 8 {
        return description;
    }
    let format = [
        body[entry_start + 4],
        body[entry_start + 5],
        body[entry_start + 6],
        body[entry_start + 7],
    ];
    description.codec = Some(codec_label(&format));
    let data_start = entry_start + 8;
    if body.len() >= data_start + 28 {
        let width = u16::from_be_bytes([body[data_start + 24], body[data_start + 25]]);
        let height = u16::from_be_bytes([body[data_start + 26], body[data_start + 27]]);
        if width > 0 {
            description.width = Some(width as u32);
        }
        if height > 0 {
            description.height = Some(height as u32);
        }
    }
    description
}

fn parse_stts_fps(stts: &[u8], mdia: &[u8]) -> Option<f64> {
    if stts.len() < 16 {
        return None;
    }
    let timescale = find_box(mdia, b"mdhd")
        .and_then(parse_mvhd_like)
        .and_then(|h| h.timescale)?;
    if timescale == 0 {
        return None;
    }
    let count = u32::from_be_bytes(stts[4..8].try_into().ok()?) as usize;
    let mut total_samples = 0u64;
    let mut total_ticks = 0u64;
    let mut pos = 8;
    for _ in 0..count {
        if pos + 8 > stts.len() {
            break;
        }
        let samples = u32::from_be_bytes(stts[pos..pos + 4].try_into().ok()?) as u64;
        let delta = u32::from_be_bytes(stts[pos + 4..pos + 8].try_into().ok()?) as u64;
        total_samples += samples;
        total_ticks += samples * delta;
        pos += 8;
    }
    if total_ticks == 0 {
        return None;
    }
    let fps = total_samples as f64 * timescale as f64 / total_ticks as f64;
    if fps.is_finite() && fps > 0.0 {
        Some((fps * 100.0).round() / 100.0)
    } else {
        None
    }
}

fn collect_meta(body: &[u8], tags: &mut Vec<(String, String)>) {
    let content = if body.len() >= 8 && looks_like_boxes(&body[4..]) {
        &body[4..]
    } else if looks_like_boxes(body) {
        body
    } else if body.len() > 4 {
        &body[4..]
    } else {
        return;
    };

    let keys = find_box(content, b"keys")
        .map(parse_keys)
        .unwrap_or_default();
    let Some(ilst) = find_box(content, b"ilst") else {
        return;
    };

    for item in boxes(ilst) {
        let index = u32::from_be_bytes(item.kind);
        let key = if item.kind[0] == 0 {
            keys.get(index.saturating_sub(1) as usize).cloned()
        } else {
            Some(fourcc(&item.kind))
        };
        let Some(key) = key else { continue };
        if let Some(value) = parse_ilst_item(item.body)
            && !value.is_empty()
        {
            tags.push((metadata_label(&key).unwrap_or(&key).to_string(), value));
        }
    }
}

fn parse_keys(body: &[u8]) -> Vec<String> {
    let mut result = Vec::new();
    if body.len() < 8 {
        return result;
    }
    let count = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
    let mut pos = 8;
    for _ in 0..count {
        if pos + 8 > body.len() {
            break;
        }
        let size = u32::from_be_bytes(body[pos..pos + 4].try_into().unwrap()) as usize;
        if size < 8 || pos + size > body.len() {
            break;
        }
        let key = &body[pos + 8..pos + size];
        let key = String::from_utf8_lossy(key)
            .trim_end_matches('\0')
            .trim()
            .to_string();
        result.push(key);
        pos += size;
    }
    result
}

fn parse_ilst_item(body: &[u8]) -> Option<String> {
    for child in boxes(body) {
        if &child.kind != b"data" || child.body.len() < 8 {
            continue;
        }
        let data_type = u32::from_be_bytes(child.body[0..4].try_into().ok()?);
        let payload = &child.body[8..];
        return match data_type {
            1 | 0 => Some(
                String::from_utf8_lossy(payload)
                    .trim_end_matches('\0')
                    .trim()
                    .to_string(),
            ),
            2 => Some(
                String::from_utf16_lossy(
                    &payload
                        .chunks_exact(2)
                        .map(|c| u16::from_be_bytes([c[0], c[1]]))
                        .collect::<Vec<_>>(),
                )
                .trim_end_matches('\0')
                .trim()
                .to_string(),
            ),
            21 | 22 if payload.len() <= 8 => {
                let mut value = 0u64;
                for byte in payload {
                    value = (value << 8) | *byte as u64;
                }
                Some(value.to_string())
            }
            23 if payload.len() >= 4 => {
                Some(f32::from_be_bytes(payload[0..4].try_into().ok()?).to_string())
            }
            24 if payload.len() >= 8 => {
                Some(f64::from_be_bytes(payload[0..8].try_into().ok()?).to_string())
            }
            _ => None,
        };
    }
    None
}

fn collect_udta(body: &[u8], tags: &mut Vec<(String, String)>) {
    for child in boxes(body) {
        if &child.kind == b"meta" {
            collect_meta(child.body, tags);
            continue;
        }
        let label = fourcc(&child.kind);
        if !label.starts_with('©') {
            continue;
        }
        if let Some(value) = parse_udta_text(child.body)
            && !value.is_empty()
        {
            tags.push((metadata_label(&label).unwrap_or(&label).to_string(), value));
        }
    }
}

fn parse_udta_text(body: &[u8]) -> Option<String> {
    if body.len() > 4 {
        let text = String::from_utf8_lossy(&body[4..])
            .trim_end_matches('\0')
            .trim()
            .to_string();
        let text = text.trim_start_matches(|c: char| c.is_control());
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }
    None
}

fn find_box<'a>(buf: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(buf)
        .into_iter()
        .find(|item| &item.kind == kind)
        .map(|item| item.body)
}

fn boxes(buf: &[u8]) -> Vec<BmffBox<'_>> {
    let mut result = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= buf.len() {
        let size32 = u32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap()) as u64;
        let kind = [buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]];
        let (header_len, box_size) = match size32 {
            0 => (8usize, buf.len() as u64 - pos as u64),
            1 => {
                if pos + 16 > buf.len() {
                    break;
                }
                let large = u64::from_be_bytes(buf[pos + 8..pos + 16].try_into().unwrap());
                (16usize, large)
            }
            n => (8usize, n),
        };
        if box_size < header_len as u64 {
            break;
        }
        let end = match (pos as u64).checked_add(box_size) {
            Some(end) if end <= buf.len() as u64 => end as usize,
            _ => break,
        };
        let body_start = pos + header_len;
        if body_start > end {
            break;
        }
        result.push(BmffBox {
            kind,
            body: &buf[body_start..end],
        });
        pos = end;
    }
    result
}

fn looks_like_boxes(buf: &[u8]) -> bool {
    if buf.len() < 8 {
        return false;
    }
    let size = u32::from_be_bytes(buf[0..4].try_into().unwrap());
    let kind = [buf[4], buf[5], buf[6], buf[7]];
    (size == 0 || size >= 8) && kind.iter().all(|b| b.is_ascii_graphic() || *b == 0)
}

fn fourcc(kind: &[u8; 4]) -> String {
    let mut text = String::new();
    for byte in kind {
        match *byte {
            0xA9 => text.push('©'),
            b if b.is_ascii_graphic() => text.push(b as char),
            0 | b' ' => {}
            _ => text.push('?'),
        }
    }
    text.trim().to_string()
}

fn codec_label(code: &[u8; 4]) -> String {
    match code {
        b"avc1" | b"avc3" | b"avc2" => "H.264".to_string(),
        b"hvc1" | b"hev1" => "H.265 / HEVC".to_string(),
        b"av01" => "AV1".to_string(),
        b"vp09" => "VP9".to_string(),
        b"mp4a" => "AAC".to_string(),
        b"alac" => "ALAC".to_string(),
        b"ac-3" => "AC-3".to_string(),
        b"ec-3" => "E-AC-3".to_string(),
        b"Opus" => "Opus".to_string(),
        b"fLaC" => "FLAC".to_string(),
        b"encv" => "加密视频".to_string(),
        other => fourcc(other).to_uppercase(),
    }
}

fn metadata_label(key: &str) -> Option<&'static str> {
    match key {
        "com.apple.quicktime.make" | "©mak" => Some("设备品牌"),
        "com.apple.quicktime.model" | "©mod" => Some("设备型号"),
        "com.apple.quicktime.creationdate" | "©day" => Some("创建时间"),
        "com.apple.quicktime.location.ISO6709" | "©xyz" => Some("位置"),
        "com.apple.quicktime.software" | "©swr" => Some("软件"),
        "com.apple.quicktime.artist" | "©ART" => Some("作者"),
        "com.apple.quicktime.title" | "©nam" => Some("标题"),
        "com.apple.quicktime.comment" | "©cmt" => Some("备注"),
        "©too" => Some("编码软件"),
        "©gen" => Some("类别"),
        "com.android.version" => Some("安卓版本"),
        "com.android.capture.fps" => Some("拍摄帧率"),
        "com.android.manufacturer" => Some("设备品牌"),
        "com.android.model" => Some("设备型号"),
        _ => None,
    }
}

fn dedup_tags(tags: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut result: Vec<(String, String)> = Vec::new();
    for (key, value) in tags {
        if let Some(existing) = result.iter_mut().find(|(k, _)| k == &key) {
            existing.1 = value;
        } else {
            result.push((key, value));
        }
    }
    result
}

fn mac_timestamp_to_datetime(seconds: i64) -> Option<NaiveDateTime> {
    if seconds <= 0 {
        return None;
    }
    let unix = seconds - MAC_EPOCH_OFFSET;
    DateTime::from_timestamp(unix, 0).map(|dt| dt.naive_utc())
}

fn parse_iso_datetime(value: &str) -> Option<NaiveDateTime> {
    let value = value.trim();
    let offset_formats = [
        "%Y-%m-%dT%H:%M:%S%z",
        "%Y-%m-%dT%H:%M:%S%.f%z",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y:%m:%d %H:%M:%S",
    ];
    for format in offset_formats {
        if let Ok(dt) = DateTime::parse_from_str(value, format) {
            return Some(dt.naive_local());
        }
        if let Ok(dt) = NaiveDateTime::parse_from_str(value, format) {
            return Some(dt);
        }
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
        return Some(dt.naive_local());
    }
    None
}

fn parse_iso6709(value: &str) -> Option<(f64, f64)> {
    let text = value.trim().trim_end_matches('/');
    if text.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    for (index, ch) in text.chars().enumerate() {
        if (ch == '+' || ch == '-') && index > 0 {
            parts.push(current.clone());
            current.clear();
        }
        current.push(ch);
    }
    parts.push(current);
    if parts.len() < 2 {
        return None;
    }
    let lat: f64 = parts[0].parse().ok()?;
    let lon: f64 = parts[1].parse().ok()?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    Some((lat, lon))
}

fn format_bitrate(bits_per_second: u64) -> String {
    let mbps = bits_per_second as f64 / 1_000_000.0;
    format!("{mbps:.2} Mbps")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iso6709_decimal() {
        assert_eq!(
            parse_iso6709("+37.7749-122.4194+010.000/"),
            Some((37.7749, -122.4194))
        );
        assert_eq!(
            parse_iso6709("+31.2304+121.4737/"),
            Some((31.2304, 121.4737))
        );
        assert_eq!(
            parse_iso6709("-33.8688+151.2093/"),
            Some((-33.8688, 151.2093))
        );
        assert_eq!(parse_iso6709("bad"), None);
    }

    #[test]
    fn parses_metadata_date() {
        assert!(parse_iso_datetime("2024-05-01T18:23:11+0800").is_some());
        assert!(parse_iso_datetime("2024-05-01T18:23:11Z").is_some());
        assert!(parse_iso_datetime("2024:05:01 18:23:11").is_some());
    }

    #[test]
    fn reads_basic_boxes() {
        let buf = [
            0, 0, 0, 16, b'f', b't', b'y', b'p', b'm', b'p', b'4', b'2', 0, 0, 0, 0, 0, 0, 0, 8,
            b'f', b'r', b'e', b'e',
        ];
        let list = boxes(&buf);
        assert_eq!(list.len(), 2);
        assert_eq!(&list[0].kind, b"ftyp");
        assert_eq!(&list[1].kind, b"free");
    }

    #[test]
    fn codec_names() {
        assert_eq!(codec_label(b"hvc1"), "H.265 / HEVC");
        assert_eq!(codec_label(b"avc1"), "H.264");
    }

    #[test]
    fn computes_fps_from_stts() {
        fn make_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(body);
            out
        }

        let mut mdhd = vec![0u8, 0, 0, 0];
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&30_000u32.to_be_bytes());
        mdhd.extend_from_slice(&30_030u32.to_be_bytes());

        let mut hdlr = vec![0u8; 12];
        hdlr[8..12].copy_from_slice(b"vide");

        let mut stts = vec![0u8, 0, 0, 0];
        stts.extend_from_slice(&1u32.to_be_bytes());
        stts.extend_from_slice(&1001u32.to_be_bytes());
        stts.extend_from_slice(&1001u32.to_be_bytes());

        let stbl = make_box(b"stbl", &make_box(b"stts", &stts));
        let minf = make_box(b"minf", &stbl);
        let mut mdia = make_box(b"mdhd", &mdhd);
        mdia.extend_from_slice(&make_box(b"hdlr", &hdlr));
        mdia.extend_from_slice(&minf);
        let mdia_box = make_box(b"mdia", &mdia);
        let children = boxes(&mdia_box[8..]);
        assert_eq!(children.len(), 3);
        assert_eq!(&children[1].kind, b"hdlr");
        assert_eq!(&children[1].body[8..12], b"vide");
        let trak = make_box(b"trak", &mdia_box);

        let track = parse_trak(&trak[8..]).expect("video track");
        let fps = track.fps.expect("fps");
        assert!((fps - 29.97).abs() < 0.01, "fps={fps}");
    }

    #[test]
    fn parses_mp4_metadata_structure() {
        fn make_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(body);
            out
        }

        let mut mvhd = vec![0u8, 0, 0, 0];
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&1000u32.to_be_bytes());
        mvhd.extend_from_slice(&5000u32.to_be_bytes());
        mvhd.resize(100, 0);
        let mut moov = make_box(b"mvhd", &mvhd);

        let mut keys_body = vec![0u8, 0, 0, 0, 0, 0, 0, 2];
        for key in ["com.apple.quicktime.make", "com.apple.quicktime.model"] {
            let mut entry = Vec::new();
            entry.extend_from_slice(&((key.len() + 8) as u32).to_be_bytes());
            entry.extend_from_slice(b"mdta");
            entry.extend_from_slice(key.as_bytes());
            keys_body.extend_from_slice(&entry);
        }
        let keys = make_box(b"keys", &keys_body);
        let hdlr = make_box(b"hdlr", &[0u8; 25]);

        let mut ilst_body = Vec::new();
        for (index, value) in [(1u32, "Apple"), (2u32, "iPhone 16 Pro")] {
            let mut data = Vec::new();
            data.extend_from_slice(&1u32.to_be_bytes());
            data.extend_from_slice(&0u32.to_be_bytes());
            data.extend_from_slice(value.as_bytes());
            let data = make_box(b"data", &data);
            let item = make_box(&index.to_be_bytes(), &data);
            ilst_body.extend_from_slice(&item);
        }
        let ilst = make_box(b"ilst", &ilst_body);
        let mut meta_body = vec![0u8, 0, 0, 0];
        meta_body.extend_from_slice(&hdlr);
        meta_body.extend_from_slice(&keys);
        meta_body.extend_from_slice(&ilst);
        let meta = make_box(b"meta", &meta_body);
        let udta = make_box(b"udta", &meta);
        moov.extend_from_slice(&udta);

        let info = parse_moov(&moov, 1_000_000).unwrap();
        assert_eq!(info.make.as_deref(), Some("Apple"));
        assert_eq!(info.model.as_deref(), Some("iPhone 16 Pro"));
        assert_eq!(info.duration_secs, Some(5.0));
    }
}
