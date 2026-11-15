use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDateTime};
use exif::{Context, Field, In, Reader, Tag, Value};
use little_exif::exif_tag::ExifTag;
use little_exif::metadata::Metadata;
use little_exif::rational::uR64;

use crate::model::{
    ImageInfo, format_datetime, format_exposure, format_friendly_datetime, trim_float,
};

#[derive(Clone, Debug, Default)]
pub struct ExifField {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, Default)]
pub struct ExifGroup {
    pub name: String,
    pub fields: Vec<ExifField>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditFields {
    pub description: String,
    pub artist: String,
    pub copyright: String,
    pub software: String,
    pub make: String,
    pub model: String,
    pub lens_make: String,
    pub lens_model: String,
    pub date_time_original: String,
    pub offset_time_original: String,
    pub user_comment: String,
    pub iso: String,
    pub f_number: String,
    pub exposure_time: String,
    pub focal_length: String,
    pub focal_length_35mm: String,
    pub orientation: u16,
    pub gps_latitude: String,
    pub gps_longitude: String,
    pub gps_altitude: String,
}

pub fn read_image_info(path: &Path) -> (ImageInfo, Option<String>) {
    let mut info = ImageInfo::default();
    let mut error = None;

    let exif = match read_exif(path) {
        Ok(exif) => Some(exif),
        Err(err) => {
            if !err.contains("No Exif data found") {
                error = Some(err);
            }
            None
        }
    };

    if let Some(exif) = &exif {
        info.has_exif = exif.fields().len() > 0;
        info.make = find_ascii(exif, Tag::Make);
        info.model = find_ascii(exif, Tag::Model);
        info.lens = find_ascii(exif, Tag::LensModel);
        info.iso = find_uint(exif, Tag::PhotographicSensitivity)
            .or_else(|| find_uint(exif, Tag::ISOSpeed));
        info.f_number = find_rational(exif, Tag::FNumber);
        info.exposure_time = find_rational(exif, Tag::ExposureTime);
        info.focal_length = find_rational(exif, Tag::FocalLength);
        info.focal_length_35mm = find_uint(exif, Tag::FocalLengthIn35mmFilm);
        info.orientation = find_uint(exif, Tag::Orientation)
            .filter(|v| (1..=8).contains(v))
            .map(|v| v as u16)
            .unwrap_or(1);
        info.gps = find_gps(exif);
        info.taken_at = find_ascii(exif, Tag::DateTimeOriginal)
            .as_deref()
            .and_then(parse_exif_datetime);
        info.width =
            find_uint(exif, Tag::PixelXDimension).or_else(|| find_uint(exif, Tag::ImageWidth));
        info.height =
            find_uint(exif, Tag::PixelYDimension).or_else(|| find_uint(exif, Tag::ImageLength));
    }

    // kamadak-exif 无法读取 PNG 等容器中由 little_exif 写入的 EXIF，退回读取可编辑字段。
    if !info.has_exif
        && supports_editable_fallback(path)
        && let Ok(fields) = read_editable(path)
        && editable_has_content(&fields)
    {
        apply_editable_info(&mut info, &fields);
        error = None;
    }

    if (info.width.is_none() || info.height.is_none())
        && let Ok((w, h)) = image::image_dimensions(path)
    {
        info.width = Some(w);
        info.height = Some(h);
    }

    (info, error)
}

pub fn read_image_details(path: &Path) -> Result<Vec<ExifGroup>, String> {
    let (groups, error) = match read_exif(path) {
        Ok(exif) => (groups_from_exif(&exif), None),
        Err(err) => (Vec::new(), Some(err)),
    };
    if !groups.is_empty() {
        return Ok(groups);
    }
    if let Ok(fields) = read_editable(path) {
        let groups = groups_from_editable(&fields);
        if !groups.is_empty() {
            return Ok(groups);
        }
    }
    match error {
        Some(err) => Err(err),
        None => Ok(groups),
    }
}

fn groups_from_exif(exif: &exif::Exif) -> Vec<ExifGroup> {
    let mut base = Vec::new();
    let mut shooting = Vec::new();
    let mut location = Vec::new();
    let mut interop = Vec::new();

    for field in exif.fields() {
        if field.ifd_num == In::THUMBNAIL || is_noise_tag(field.tag) {
            continue;
        }
        let Some(value) = field_text(field, exif) else {
            continue;
        };
        let tag_name = field.tag.to_string();
        let label = tag_label(&tag_name);
        let item = ExifField {
            label: label.to_string(),
            value: friendly_datetime_value(label, value),
        };
        match field.tag.context() {
            Context::Tiff => base.push(item),
            Context::Exif => shooting.push(item),
            Context::Gps => location.push(item),
            Context::Interop => interop.push(item),
            _ => base.push(item),
        }
    }

    let mut groups = Vec::new();
    for (name, fields) in [
        ("基本与设备", base),
        ("拍摄参数", shooting),
        ("位置信息", location),
        ("兼容信息", interop),
    ] {
        if !fields.is_empty() {
            groups.push(ExifGroup {
                name: name.to_string(),
                fields,
            });
        }
    }
    groups
}

fn friendly_datetime_value(label: &str, value: String) -> String {
    if !matches!(label, "拍摄时间" | "数字化时间" | "修改时间") {
        return value;
    }
    NaiveDateTime::parse_from_str(value.trim(), "%Y:%m:%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M:%S"))
        .map(|parsed| format_friendly_datetime(&parsed))
        .unwrap_or(value)
}

fn field_text(field: &Field, exif: &exif::Exif) -> Option<String> {
    if let Value::Ascii(parts) = &field.value {
        let text = parts
            .iter()
            .filter_map(|bytes| {
                let value = String::from_utf8_lossy(bytes);
                let value = value.trim_matches('\0').trim();
                (!value.is_empty()).then(|| value.to_string())
            })
            .collect::<Vec<_>>()
            .join(", ");
        return (!text.is_empty()).then_some(text);
    }
    let value = field.display_value().with_unit(exif).to_string();
    let value = value.trim().trim_end_matches('\0').trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn editable_has_content(fields: &EditFields) -> bool {
    fields.orientation != 1
        || [
            &fields.description,
            &fields.artist,
            &fields.copyright,
            &fields.software,
            &fields.make,
            &fields.model,
            &fields.lens_make,
            &fields.lens_model,
            &fields.date_time_original,
            &fields.user_comment,
            &fields.iso,
            &fields.f_number,
            &fields.exposure_time,
            &fields.focal_length,
            &fields.focal_length_35mm,
            &fields.gps_latitude,
            &fields.gps_longitude,
            &fields.gps_altitude,
        ]
        .iter()
        .any(|value| !value.trim().is_empty())
}

fn apply_editable_info(info: &mut ImageInfo, fields: &EditFields) {
    info.has_exif = true;
    let text = |value: &str| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    };
    info.make = text(&fields.make);
    info.model = text(&fields.model);
    info.lens = text(&fields.lens_model).or_else(|| text(&fields.lens_make));
    info.iso = fields.iso.trim().parse().ok();
    info.f_number = parse_number(fields.f_number.trim()).ok();
    info.exposure_time = parse_exposure(fields.exposure_time.trim()).ok();
    info.focal_length = parse_number(fields.focal_length.trim()).ok();
    info.focal_length_35mm = fields.focal_length_35mm.trim().parse().ok();
    if (1..=8).contains(&fields.orientation) {
        info.orientation = fields.orientation;
    }
    info.taken_at = parse_exif_datetime(&fields.date_time_original);
    let lat = parse_number(fields.gps_latitude.trim()).ok();
    let lon = parse_number(fields.gps_longitude.trim()).ok();
    if let (Some(lat), Some(lon)) = (lat, lon) {
        info.gps = Some((lat, lon));
    }
}

fn groups_from_editable(fields: &EditFields) -> Vec<ExifGroup> {
    if !editable_has_content(fields) {
        return Vec::new();
    }
    let mut base = Vec::new();
    let mut shooting = Vec::new();
    let mut location = Vec::new();
    let push = |group: &mut Vec<ExifField>, label: &str, value: &str| {
        let value = value.trim();
        if !value.is_empty() {
            group.push(ExifField {
                label: label.to_string(),
                value: value.to_string(),
            });
        }
    };

    push(&mut base, "图像描述", &fields.description);
    push(&mut base, "作者", &fields.artist);
    push(&mut base, "版权", &fields.copyright);
    push(&mut base, "软件", &fields.software);
    push(&mut base, "品牌", &fields.make);
    push(&mut base, "型号", &fields.model);
    push(&mut base, "镜头品牌", &fields.lens_make);
    push(&mut base, "镜头型号", &fields.lens_model);
    if fields.orientation != 1 {
        push(&mut base, "方向", &fields.orientation.to_string());
    }

    push(
        &mut shooting,
        "拍摄时间",
        &friendly_datetime_value("拍摄时间", fields.date_time_original.clone()),
    );
    push(&mut shooting, "时区", &fields.offset_time_original);
    push(&mut shooting, "用户备注", &fields.user_comment);
    push(&mut shooting, "感光度", &fields.iso);
    if let Ok(value) = parse_number(fields.f_number.trim()) {
        push(
            &mut shooting,
            "光圈",
            &format!("f/{}", trim_float(value, 1)),
        );
    }
    push(&mut shooting, "快门速度", &fields.exposure_time);
    if let Ok(value) = parse_number(fields.focal_length.trim()) {
        push(
            &mut shooting,
            "焦距",
            &format!("{} mm", trim_float(value, 1)),
        );
    }
    push(&mut shooting, "35mm 等效焦距", &fields.focal_length_35mm);

    push(&mut location, "纬度", &fields.gps_latitude);
    push(&mut location, "经度", &fields.gps_longitude);
    if let Ok(value) = parse_number(fields.gps_altitude.trim()) {
        push(
            &mut location,
            "海拔",
            &format!("{} m", trim_float(value, 1)),
        );
    }

    let mut groups = Vec::new();
    for (name, fields) in [
        ("基本与设备", base),
        ("拍摄参数", shooting),
        ("位置信息", location),
    ] {
        if !fields.is_empty() {
            groups.push(ExifGroup {
                name: name.to_string(),
                fields,
            });
        }
    }
    groups
}

fn read_exif(path: &Path) -> Result<exif::Exif, String> {
    let file = File::open(path).map_err(|e| format!("无法打开文件：{e}"))?;
    let mut reader = BufReader::new(file);
    Reader::new()
        .continue_on_error(true)
        .read_from_container(&mut reader)
        .map_err(|e| format!("未找到可读取的 EXIF：{e}"))
}

fn supports_editable_fallback(path: &Path) -> bool {
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    matches!(
        extension.as_str(),
        "png" | "webp" | "heic" | "heif" | "hif" | "avif" | "jxl"
    )
}

/// 相机 JPEG 通常内嵌一张 160–1024px 的预览图，缩略图列表优先复用它，
/// 避免为每张小图完整解码 2000 万像素的原始文件。
pub fn read_embedded_thumbnail(path: &Path) -> Option<Vec<u8>> {
    let exif = read_exif(path).ok()?;
    let offset = exif
        .get_field(Tag::JPEGInterchangeFormat, In::THUMBNAIL)?
        .value
        .get_uint(0)? as usize;
    let length = exif
        .get_field(Tag::JPEGInterchangeFormatLength, In::THUMBNAIL)?
        .value
        .get_uint(0)? as usize;
    if length == 0 || length > 4 * 1024 * 1024 {
        return None;
    }
    exif.buf()
        .get(offset..offset.checked_add(length)?)
        .map(|bytes| bytes.to_vec())
}

fn find_field(exif: &exif::Exif, tag: Tag) -> Option<&Field> {
    exif.fields()
        .find(|f| f.tag == tag && f.ifd_num == In::PRIMARY)
        .or_else(|| exif.fields().find(|f| f.tag == tag))
}

fn find_ascii(exif: &exif::Exif, tag: Tag) -> Option<String> {
    let field = find_field(exif, tag)?;
    ascii_value(field)
}

fn ascii_value(field: &Field) -> Option<String> {
    match &field.value {
        Value::Ascii(parts) => parts.first().and_then(|bytes| {
            let text = String::from_utf8_lossy(bytes);
            let text = text.trim_matches('\0').trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        }),
        _ => None,
    }
}

fn find_uint(exif: &exif::Exif, tag: Tag) -> Option<u32> {
    find_field(exif, tag)?.value.get_uint(0)
}

fn find_rational(exif: &exif::Exif, tag: Tag) -> Option<f64> {
    let field = find_field(exif, tag)?;
    match &field.value {
        Value::Rational(values) => values.first().map(|v| v.to_f64()),
        _ => field.value.get_uint(0).map(f64::from),
    }
}

fn find_gps(exif: &exif::Exif) -> Option<(f64, f64)> {
    let lat = gps_coordinate(exif, Tag::GPSLatitude, Tag::GPSLatitudeRef)?;
    let lon = gps_coordinate(exif, Tag::GPSLongitude, Tag::GPSLongitudeRef)?;
    Some((lat, lon))
}

fn gps_coordinate(exif: &exif::Exif, coord: Tag, reference: Tag) -> Option<f64> {
    let field = find_field(exif, coord)?;
    let values = match &field.value {
        Value::Rational(values) if values.len() >= 3 => values,
        _ => return None,
    };
    let degrees = values[0].to_f64() + values[1].to_f64() / 60.0 + values[2].to_f64() / 3600.0;
    let reference = find_ascii(exif, reference).unwrap_or_default();
    let sign = match reference.chars().next() {
        Some('S') | Some('W') => -1.0,
        _ => 1.0,
    };
    Some(degrees * sign)
}

fn is_noise_tag(tag: Tag) -> bool {
    matches!(
        tag,
        Tag::StripOffsets
            | Tag::StripByteCounts
            | Tag::RowsPerStrip
            | Tag::JPEGInterchangeFormat
            | Tag::JPEGInterchangeFormatLength
    )
}

fn tag_label(name: &str) -> &str {
    match name {
        "Make" => "品牌",
        "Model" => "型号",
        "LensMake" => "镜头品牌",
        "LensModel" => "镜头型号",
        "LensInfo" => "镜头参数",
        "LensSerialNumber" => "镜头序列号",
        "SerialNumber" => "机身序列号",
        "OwnerName" => "相机所有者",
        "ImageDescription" => "图像描述",
        "Artist" => "作者",
        "Copyright" => "版权",
        "Software" => "软件",
        "DateTime" => "修改时间",
        "DateTimeOriginal" => "拍摄时间",
        "DateTimeDigitized" => "数字化时间",
        "OffsetTimeOriginal" => "时区",
        "ExposureTime" => "快门速度",
        "FNumber" => "光圈",
        "ApertureValue" => "光圈值",
        "MaxApertureValue" => "最大光圈",
        "ShutterSpeedValue" => "快门速度值",
        "ISO" => "ISO",
        "ISOSpeed" => "ISO 速度",
        "PhotographicSensitivity" => "ISO",
        "ExposureProgram" => "曝光程序",
        "ExposureMode" => "曝光模式",
        "ExposureBiasValue" => "曝光补偿",
        "MeteringMode" => "测光模式",
        "Flash" => "闪光灯",
        "FocalLength" => "焦距",
        "FocalLengthIn35mmFilm" => "35mm 等效焦距",
        "WhiteBalance" => "白平衡",
        "LightSource" => "光源",
        "SceneCaptureType" => "场景类型",
        "Contrast" => "对比度",
        "Saturation" => "饱和度",
        "Sharpness" => "锐度",
        "DigitalZoomRatio" => "数码变焦",
        "SubjectDistance" => "主体距离",
        "Orientation" => "方向",
        "ResolutionUnit" => "分辨率单位",
        "XResolution" => "水平分辨率",
        "YResolution" => "垂直分辨率",
        "ColorSpace" => "色彩空间",
        "PixelXDimension" => "像素宽度",
        "PixelYDimension" => "像素高度",
        "ImageWidth" => "图像宽度",
        "ImageHeight" => "图像高度",
        "BitsPerSample" => "位深",
        "Compression" => "压缩方式",
        "ColorModel" => "色彩模式",
        "GPSLatitude" => "纬度",
        "GPSLongitude" => "经度",
        "GPSAltitude" => "海拔",
        "GPSAltitudeRef" => "海拔参考",
        "GPSVersionID" => "GPS 版本",
        "GPSTimeStamp" => "GPS 时间",
        "GPSDateStamp" => "GPS 日期",
        "GPSImgDirection" => "拍摄方向",
        "GPSSpeed" => "速度",
        "GPSProcessingMethod" => "定位方式",
        "ExifVersion" => "EXIF 版本",
        "FlashpixVersion" => "Flashpix 版本",
        "UserComment" => "用户备注",
        "ImageUniqueID" => "图像唯一 ID",
        "InteroperabilityIndex" => "互操作性索引",
        other => other,
    }
}

fn load_metadata(path: &Path) -> Result<Metadata, String> {
    match Metadata::new_from_path(path) {
        Ok(meta) => Ok(meta),
        Err(error) if is_missing_metadata(&error) => Ok(Metadata::new()),
        Err(error) => Err(format!("无法解析 EXIF：{error}")),
    }
}

fn is_missing_metadata(error: &std::io::Error) -> bool {
    let text = error.to_string();
    [
        "No EXIF data found",
        "No metadata found",
        "No EXIF item found",
        "No meta box found",
        "No iinf box found",
        "No iloc box found",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

pub fn read_editable(path: &Path) -> Result<EditFields, String> {
    let meta = load_metadata(path)?;
    let mut fields = EditFields {
        orientation: 1,
        ..Default::default()
    };

    fields.description = string_tag(&meta, &ExifTag::ImageDescription(String::new()));
    fields.artist = string_tag(&meta, &ExifTag::Artist(String::new()));
    fields.copyright = string_tag(&meta, &ExifTag::Copyright(String::new()));
    fields.software = string_tag(&meta, &ExifTag::Software(String::new()));
    fields.make = string_tag(&meta, &ExifTag::Make(String::new()));
    fields.model = string_tag(&meta, &ExifTag::Model(String::new()));
    fields.lens_make = string_tag(&meta, &ExifTag::LensMake(String::new()));
    fields.lens_model = string_tag(&meta, &ExifTag::LensModel(String::new()));
    fields.date_time_original = string_tag(&meta, &ExifTag::DateTimeOriginal(String::new()));
    fields.offset_time_original = string_tag(&meta, &ExifTag::OffsetTimeOriginal(String::new()));
    fields.user_comment = user_comment_tag(&meta);

    if let Some(v) = u16_tag(&meta, &ExifTag::ISO(Vec::new())).and_then(|v| v.first().copied()) {
        fields.iso = v.to_string();
    }
    if let Some(v) =
        rational_tag(&meta, &ExifTag::FNumber(Vec::new())).and_then(|v| v.into_iter().next())
    {
        fields.f_number = trim_float(f64::from(v), 1);
    }
    if let Some(v) =
        rational_tag(&meta, &ExifTag::ExposureTime(Vec::new())).and_then(|v| v.into_iter().next())
    {
        let seconds = f64::from(v);
        fields.exposure_time = if seconds > 0.0 && seconds < 1.0 {
            format_exposure(seconds).trim_end_matches('s').to_string()
        } else {
            trim_float(seconds, 3)
        };
    }
    if let Some(v) =
        rational_tag(&meta, &ExifTag::FocalLength(Vec::new())).and_then(|v| v.into_iter().next())
    {
        fields.focal_length = trim_float(f64::from(v), 1);
    }
    if let Some(v) = u16_tag(&meta, &ExifTag::FocalLengthIn35mmFormat(Vec::new()))
        .and_then(|v| v.first().copied())
    {
        fields.focal_length_35mm = v.to_string();
    }
    if let Some(v) =
        u16_tag(&meta, &ExifTag::Orientation(Vec::new())).and_then(|v| v.first().copied())
        && (1..=8).contains(&v)
    {
        fields.orientation = v;
    }

    let lat = rational_tag(&meta, &ExifTag::GPSLatitude(Vec::new()));
    let lat_ref = string_tag(&meta, &ExifTag::GPSLatitudeRef(String::new()));
    if let Some(values) = lat
        && values.len() >= 3
    {
        let mut value = dms_to_decimal(&values);
        if lat_ref.starts_with('S') {
            value = -value;
        }
        fields.gps_latitude = trim_float(value, 6);
    }
    let lon = rational_tag(&meta, &ExifTag::GPSLongitude(Vec::new()));
    let lon_ref = string_tag(&meta, &ExifTag::GPSLongitudeRef(String::new()));
    if let Some(values) = lon
        && values.len() >= 3
    {
        let mut value = dms_to_decimal(&values);
        if lon_ref.starts_with('W') {
            value = -value;
        }
        fields.gps_longitude = trim_float(value, 6);
    }
    if let Some(values) = rational_tag(&meta, &ExifTag::GPSAltitude(Vec::new()))
        && let Some(v) = values.into_iter().next()
    {
        let mut meters = f64::from(v);
        let below = u8_tag(&meta, &ExifTag::GPSAltitudeRef(Vec::new()))
            .and_then(|v| v.first().copied())
            .unwrap_or(0)
            == 1;
        if below {
            meters = -meters;
        }
        fields.gps_altitude = trim_float(meters, 1);
    }

    Ok(fields)
}

pub struct SaveOutcome {
    pub backup: Option<PathBuf>,
}

pub fn save_edits(
    path: &Path,
    fields: &EditFields,
    make_backup: bool,
) -> Result<SaveOutcome, String> {
    let mut meta =
        load_metadata(path).map_err(|e| format!("无法读取现有 EXIF，已取消写入：{e}"))?;

    if (&meta).into_iter().count() == 0
        && let Ok(groups) = read_image_details(path)
        && groups.iter().any(|g| !g.fields.is_empty())
    {
        return Err("该文件的 EXIF 结构无法被安全解析，已取消写入以避免损坏数据。".to_string());
    }

    validate_fields(fields)?;

    set_or_remove_string(&mut meta, fields.description.trim(), |v| {
        ExifTag::ImageDescription(v)
    });
    set_or_remove_string(&mut meta, fields.artist.trim(), ExifTag::Artist);
    set_or_remove_string(&mut meta, fields.copyright.trim(), ExifTag::Copyright);
    set_or_remove_string(&mut meta, fields.software.trim(), ExifTag::Software);
    set_or_remove_string(&mut meta, fields.make.trim(), ExifTag::Make);
    set_or_remove_string(&mut meta, fields.model.trim(), ExifTag::Model);
    set_or_remove_string(&mut meta, fields.lens_make.trim(), ExifTag::LensMake);
    set_or_remove_string(&mut meta, fields.lens_model.trim(), ExifTag::LensModel);
    set_or_remove_string(&mut meta, fields.date_time_original.trim(), |v| {
        ExifTag::DateTimeOriginal(v)
    });
    set_or_remove_string(&mut meta, fields.offset_time_original.trim(), |v| {
        ExifTag::OffsetTimeOriginal(v)
    });

    if fields.user_comment.trim().is_empty() {
        meta.remove_tag(ExifTag::UserComment(Vec::new()));
    } else {
        let mut raw = b"ASCII\0\0\0".to_vec();
        raw.extend_from_slice(fields.user_comment.trim().as_bytes());
        meta.set_tag(ExifTag::UserComment(raw));
    }

    if fields.iso.trim().is_empty() {
        meta.remove_tag(ExifTag::ISO(Vec::new()));
    } else {
        let iso: u16 = fields
            .iso
            .trim()
            .parse()
            .map_err(|_| "ISO 必须是 0–65535 的整数".to_string())?;
        meta.set_tag(ExifTag::ISO(vec![iso]));
    }

    set_or_remove_rational(&mut meta, fields.f_number.trim(), ExifTag::FNumber)?;
    set_exposure(&mut meta, fields.exposure_time.trim())?;
    set_or_remove_rational(&mut meta, fields.focal_length.trim(), |v| {
        ExifTag::FocalLength(v)
    })?;

    if fields.focal_length_35mm.trim().is_empty() {
        meta.remove_tag(ExifTag::FocalLengthIn35mmFormat(Vec::new()));
    } else {
        let value: u16 = fields
            .focal_length_35mm
            .trim()
            .parse()
            .map_err(|_| "35mm 等效焦距必须是整数".to_string())?;
        meta.set_tag(ExifTag::FocalLengthIn35mmFormat(vec![value]));
    }

    if fields.orientation == 0 {
        meta.remove_tag(ExifTag::Orientation(Vec::new()));
    } else {
        meta.set_tag(ExifTag::Orientation(vec![fields.orientation]));
    }

    set_gps(
        &mut meta,
        fields.gps_latitude.trim(),
        fields.gps_longitude.trim(),
        fields.gps_altitude.trim(),
    )?;

    let backup = if make_backup {
        let file_name = path
            .file_name()
            .map(|v| v.to_string_lossy().to_string())
            .unwrap_or_else(|| "image".to_string());
        let stamp = Local::now().format("%Y%m%d-%H%M%S");
        let mut backup_path = path.with_file_name(format!("{file_name}.{stamp}.bak"));
        let mut index = 1;
        while backup_path.exists() {
            backup_path = path.with_file_name(format!("{file_name}.{stamp}-{index}.bak"));
            index += 1;
        }
        fs::copy(path, &backup_path).map_err(|e| format!("创建备份失败：{e}"))?;
        Some(backup_path)
    } else {
        None
    };

    meta.write_to_file(path)
        .map_err(|e| format!("写入 EXIF 失败：{e}"))?;
    Ok(SaveOutcome { backup })
}

fn validate_fields(fields: &EditFields) -> Result<(), String> {
    if !fields.iso.trim().is_empty() {
        fields
            .iso
            .trim()
            .parse::<u16>()
            .map_err(|_| "ISO 必须是 0–65535 的整数".to_string())?;
    }
    if !fields.f_number.trim().is_empty() {
        let v = parse_number(fields.f_number.trim().trim_start_matches("f/"))
            .map_err(|_| "光圈必须是有效数字".to_string())?;
        if !(0.0..=128.0).contains(&v) {
            return Err("光圈值应在 0–128 之间".to_string());
        }
    }
    if !fields.exposure_time.trim().is_empty() {
        let v = parse_exposure(fields.exposure_time.trim())?;
        if v <= 0.0 {
            return Err("快门速度必须大于 0".to_string());
        }
    }
    if !fields.focal_length.trim().is_empty() {
        let v = parse_number(fields.focal_length.trim())
            .map_err(|_| "焦距必须是有效数字".to_string())?;
        if !(0.0..=10000.0).contains(&v) {
            return Err("焦距超出合理范围".to_string());
        }
    }
    if !fields.focal_length_35mm.trim().is_empty() {
        fields
            .focal_length_35mm
            .trim()
            .parse::<u16>()
            .map_err(|_| "35mm 等效焦距必须是整数".to_string())?;
    }
    if !fields.date_time_original.trim().is_empty() {
        NaiveDateTime::parse_from_str(fields.date_time_original.trim(), "%Y:%m:%d %H:%M:%S")
            .map_err(|_| "拍摄时间格式应为 YYYY:MM:DD HH:MM:SS".to_string())?;
    }
    if fields.orientation > 8 {
        return Err("方向必须是 1–8".to_string());
    }
    let lat = fields.gps_latitude.trim();
    let lon = fields.gps_longitude.trim();
    if lat.is_empty() != lon.is_empty() {
        return Err("纬度和经度需要同时填写或同时留空".to_string());
    }
    if !lat.is_empty() {
        let lat_value = parse_number(lat).map_err(|_| "纬度必须是十进制数字".to_string())?;
        let lon_value = parse_number(lon).map_err(|_| "经度必须是十进制数字".to_string())?;
        if !(-90.0..=90.0).contains(&lat_value) {
            return Err("纬度应在 -90 到 90 之间".to_string());
        }
        if !(-180.0..=180.0).contains(&lon_value) {
            return Err("经度应在 -180 到 180 之间".to_string());
        }
    }
    if !fields.gps_altitude.trim().is_empty() {
        parse_number(fields.gps_altitude.trim()).map_err(|_| "海拔必须是有效数字".to_string())?;
    }
    Ok(())
}

fn string_tag(meta: &Metadata, tag: &ExifTag) -> String {
    match meta.get_tag(tag).next() {
        Some(ExifTag::Artist(v))
        | Some(ExifTag::Copyright(v))
        | Some(ExifTag::ImageDescription(v))
        | Some(ExifTag::Software(v))
        | Some(ExifTag::Make(v))
        | Some(ExifTag::Model(v))
        | Some(ExifTag::LensMake(v))
        | Some(ExifTag::LensModel(v))
        | Some(ExifTag::DateTimeOriginal(v))
        | Some(ExifTag::OffsetTimeOriginal(v))
        | Some(ExifTag::GPSLatitudeRef(v))
        | Some(ExifTag::GPSLongitudeRef(v)) => v.trim_matches('\0').trim().to_string(),
        _ => String::new(),
    }
}

fn user_comment_tag(meta: &Metadata) -> String {
    match meta.get_tag(&ExifTag::UserComment(Vec::new())).next() {
        Some(ExifTag::UserComment(raw)) => {
            let body =
                if raw.len() > 8 && (&raw[..8] == b"ASCII\0\0\0" || &raw[..8] == b"UNICODE\0") {
                    &raw[8..]
                } else {
                    raw.as_slice()
                };
            String::from_utf8_lossy(body)
                .trim_matches('\0')
                .trim()
                .to_string()
        }
        _ => String::new(),
    }
}

fn u16_tag(meta: &Metadata, tag: &ExifTag) -> Option<Vec<u16>> {
    match meta.get_tag(tag).next() {
        Some(ExifTag::ISO(v))
        | Some(ExifTag::Orientation(v))
        | Some(ExifTag::FocalLengthIn35mmFormat(v)) => Some(v.clone()),
        _ => None,
    }
}

fn u8_tag(meta: &Metadata, tag: &ExifTag) -> Option<Vec<u8>> {
    match meta.get_tag(tag).next() {
        Some(ExifTag::GPSAltitudeRef(v)) | Some(ExifTag::GPSVersionID(v)) => Some(v.clone()),
        _ => None,
    }
}

fn rational_tag(meta: &Metadata, tag: &ExifTag) -> Option<Vec<uR64>> {
    match meta.get_tag(tag).next() {
        Some(ExifTag::FNumber(v))
        | Some(ExifTag::ExposureTime(v))
        | Some(ExifTag::FocalLength(v))
        | Some(ExifTag::GPSLatitude(v))
        | Some(ExifTag::GPSLongitude(v))
        | Some(ExifTag::GPSAltitude(v)) => Some(v.clone()),
        _ => None,
    }
}

fn set_or_remove_string<F>(meta: &mut Metadata, value: &str, ctor: F)
where
    F: Fn(String) -> ExifTag,
{
    if value.is_empty() {
        meta.remove_tag(ctor(String::new()));
    } else {
        meta.set_tag(ctor(value.to_string()));
    }
}

fn set_or_remove_rational<F>(meta: &mut Metadata, value: &str, ctor: F) -> Result<(), String>
where
    F: Fn(Vec<uR64>) -> ExifTag,
{
    if value.is_empty() {
        meta.remove_tag(ctor(Vec::new()));
        return Ok(());
    }
    let number = parse_number(value.trim_start_matches("f/"))
        .map_err(|_| format!("“{value}”不是有效的数字"))?;
    meta.set_tag(ctor(vec![uR64::from(number)]));
    Ok(())
}

fn set_exposure(meta: &mut Metadata, value: &str) -> Result<(), String> {
    if value.is_empty() {
        meta.remove_tag(ExifTag::ExposureTime(Vec::new()));
        return Ok(());
    }
    let seconds = parse_exposure(value)?;
    meta.set_tag(ExifTag::ExposureTime(vec![uR64::from(seconds)]));
    Ok(())
}

fn parse_exposure(value: &str) -> Result<f64, String> {
    if let Some((num, denom)) = value.split_once('/') {
        let num: f64 = num
            .trim()
            .parse()
            .map_err(|_| "快门速度格式应为 1/125 或秒数".to_string())?;
        let denom: f64 = denom
            .trim()
            .parse()
            .map_err(|_| "快门速度格式应为 1/125 或秒数".to_string())?;
        if denom == 0.0 || num <= 0.0 {
            return Err("快门速度必须大于 0".to_string());
        }
        Ok(num / denom)
    } else {
        let seconds: f64 = value
            .trim_end_matches('s')
            .trim()
            .parse()
            .map_err(|_| "快门速度格式应为 1/125 或秒数".to_string())?;
        if seconds <= 0.0 {
            return Err("快门速度必须大于 0".to_string());
        }
        Ok(seconds)
    }
}

fn parse_number(value: &str) -> Result<f64, std::num::ParseFloatError> {
    value.trim().parse()
}

fn dms_to_decimal(values: &[uR64]) -> f64 {
    f64::from(values[0].clone())
        + f64::from(values[1].clone()) / 60.0
        + f64::from(values[2].clone()) / 3600.0
}

fn decimal_to_dms(value: f64) -> Vec<uR64> {
    let value = value.abs();
    let degrees = value.floor();
    let minutes_full = (value - degrees) * 60.0;
    let minutes = minutes_full.floor();
    let seconds = (minutes_full - minutes) * 60.0;
    vec![
        uR64::from(degrees),
        uR64::from(minutes),
        uR64::from(seconds),
    ]
}

fn set_gps(meta: &mut Metadata, lat: &str, lon: &str, altitude: &str) -> Result<(), String> {
    if lat.is_empty() && lon.is_empty() {
        meta.remove_tag(ExifTag::GPSLatitude(Vec::new()));
        meta.remove_tag(ExifTag::GPSLatitudeRef(String::new()));
        meta.remove_tag(ExifTag::GPSLongitude(Vec::new()));
        meta.remove_tag(ExifTag::GPSLongitudeRef(String::new()));
    } else {
        let lat_value: f64 = parse_number(lat).map_err(|_| "纬度格式无效".to_string())?;
        let lon_value: f64 = parse_number(lon).map_err(|_| "经度格式无效".to_string())?;
        meta.set_tag(ExifTag::GPSVersionID(vec![2, 3, 0, 0]));
        meta.set_tag(ExifTag::GPSLatitudeRef(
            if lat_value >= 0.0 { "N" } else { "S" }.to_string(),
        ));
        meta.set_tag(ExifTag::GPSLatitude(decimal_to_dms(lat_value)));
        meta.set_tag(ExifTag::GPSLongitudeRef(
            if lon_value >= 0.0 { "E" } else { "W" }.to_string(),
        ));
        meta.set_tag(ExifTag::GPSLongitude(decimal_to_dms(lon_value)));
    }

    if altitude.is_empty() {
        meta.remove_tag(ExifTag::GPSAltitude(Vec::new()));
        meta.remove_tag(ExifTag::GPSAltitudeRef(Vec::new()));
    } else {
        let meters: f64 = parse_number(altitude).map_err(|_| "海拔格式无效".to_string())?;
        meta.set_tag(ExifTag::GPSAltitudeRef(vec![if meters < 0.0 {
            1
        } else {
            0
        }]));
        meta.set_tag(ExifTag::GPSAltitude(vec![uR64::from(meters.abs())]));
    }
    Ok(())
}

fn parse_exif_datetime(text: &str) -> Option<NaiveDateTime> {
    let text = text.trim().trim_matches('\0');
    NaiveDateTime::parse_from_str(text, "%Y:%m:%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S"))
        .ok()
}

/// Reads orientation + a compact summary for video-less preview fallbacks.
pub fn image_exif_summary(info: &ImageInfo) -> String {
    let mut parts = Vec::new();
    if let Some(camera) = info.camera_label() {
        parts.push(camera);
    }
    if let Some(at) = info.taken_at {
        parts.push(format_datetime(&at));
    }
    if let Some(exposure) = info.exposure_label() {
        parts.push(exposure);
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn sample_jpeg(dir: &Path) -> PathBuf {
        let path = dir.join("sample.jpg");
        let image: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(16, 12, |x, y| Rgb([(x * 10) as u8, (y * 15) as u8, 120]));
        image.save(&path).unwrap();
        path
    }

    fn full_fields() -> EditFields {
        EditFields {
            description: "Sunset over the river".into(),
            artist: "Ada".into(),
            copyright: "2026 Ada".into(),
            software: "Exif Studio".into(),
            make: "TestCam".into(),
            model: "X1".into(),
            lens_make: "TestOptics".into(),
            lens_model: "35mm F2".into(),
            date_time_original: "2026:10:22 10:30:00".into(),
            offset_time_original: "+08:00".into(),
            user_comment: "hello".into(),
            iso: "200".into(),
            f_number: "2.8".into(),
            exposure_time: "1/125".into(),
            focal_length: "35".into(),
            focal_length_35mm: "52".into(),
            orientation: 6,
            gps_latitude: "31.2304".into(),
            gps_longitude: "121.4737".into(),
            gps_altitude: "12.5".into(),
        }
    }

    #[test]
    fn adds_exif_to_fresh_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let path = sample_jpeg(dir.path());

        let editable = read_editable(&path).unwrap();
        assert_eq!(editable.orientation, 1);

        let outcome = save_edits(&path, &full_fields(), true).unwrap();
        assert!(outcome.backup.as_deref().is_some_and(Path::exists));

        let loaded = read_editable(&path).unwrap();
        assert_eq!(loaded.description, "Sunset over the river");
        assert_eq!(loaded.artist, "Ada");
        assert_eq!(loaded.make, "TestCam");
        assert_eq!(loaded.iso, "200");
        assert_eq!(loaded.orientation, 6);
        assert_eq!(loaded.date_time_original, "2026:10:22 10:30:00");
        let lat: f64 = loaded.gps_latitude.parse().unwrap();
        let lon: f64 = loaded.gps_longitude.parse().unwrap();
        assert!((lat - 31.2304).abs() < 1e-4, "lat = {lat}");
        assert!((lon - 121.4737).abs() < 1e-4, "lon = {lon}");
        assert_eq!(loaded.gps_altitude, "12.5");

        let (info, error) = read_image_info(&path);
        assert!(error.is_none());
        assert!(info.has_exif);
        assert_eq!(info.make.as_deref(), Some("TestCam"));
        assert_eq!(info.model.as_deref(), Some("X1"));
        assert_eq!(info.iso, Some(200));
        assert_eq!(info.width, Some(16));
        assert_eq!(info.height, Some(12));

        let groups = read_image_details(&path).unwrap();
        let base = groups
            .iter()
            .find(|g| g.name == "基本与设备")
            .expect("缺少基本与设备分组");
        let field = |label: &str| {
            base.fields
                .iter()
                .find(|field| field.label == label)
                .map(|field| field.value.as_str())
        };
        assert_eq!(field("品牌"), Some("TestCam"));
        assert_eq!(field("作者"), Some("Ada"));
        assert_eq!(field("版权"), Some("2026 Ada"));
        assert!(
            groups
                .iter()
                .any(|g| g.name == "拍摄参数" && !g.fields.is_empty())
        );
        assert!(
            groups
                .iter()
                .any(|g| g.name == "位置信息" && !g.fields.is_empty())
        );
    }

    #[test]
    fn formats_datetime_labels_for_display() {
        assert_eq!(
            friendly_datetime_value("拍摄时间", "2025:08:18 16:54:55".to_string()),
            "2025.08.18（星期一）16:54"
        );
        assert_eq!(
            friendly_datetime_value("时区", "+08:00".to_string()),
            "+08:00"
        );
    }

    #[test]
    fn skips_empty_ascii_identity_tags() {
        let dir = tempfile::tempdir().unwrap();
        let path = sample_jpeg(dir.path());
        let mut meta = Metadata::new();
        meta.set_tag(ExifTag::Make("TestCam".to_string()));
        meta.set_tag(ExifTag::Artist(String::new()));
        meta.set_tag(ExifTag::Copyright(String::new()));
        meta.write_to_file(&path).unwrap();

        let groups = read_image_details(&path).unwrap();
        let base = groups
            .iter()
            .find(|g| g.name == "基本与设备")
            .expect("缺少基本与设备分组");
        assert!(
            !base
                .fields
                .iter()
                .any(|field| field.label == "作者" || field.label == "版权")
        );
        assert!(
            base.fields
                .iter()
                .any(|field| field.label == "品牌" && field.value == "TestCam")
        );
    }

    #[test]
    fn reads_png_metadata_via_little_exif_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.png");
        let image: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(16, 12, |x, y| Rgb([(x * 10) as u8, (y * 15) as u8, 90]));
        image.save(&path).unwrap();

        save_edits(&path, &full_fields(), false).unwrap();

        let (info, error) = read_image_info(&path);
        assert!(error.is_none(), "error = {error:?}");
        assert!(info.has_exif);
        assert_eq!(info.make.as_deref(), Some("TestCam"));
        assert_eq!(info.model.as_deref(), Some("X1"));
        assert_eq!(info.lens.as_deref(), Some("35mm F2"));
        assert_eq!(info.iso, Some(200));
        assert_eq!(info.f_number, Some(2.8));
        assert!((info.exposure_time.unwrap() - 1.0 / 125.0).abs() < 1e-9);
        assert_eq!(info.focal_length, Some(35.0));
        assert_eq!(info.focal_length_35mm, Some(52));
        assert_eq!(info.orientation, 6);
        assert_eq!(info.width, Some(16));
        assert_eq!(info.height, Some(12));
        assert_eq!(
            info.taken_at.map(|v| v.to_string()),
            Some("2026-10-22 10:30:00".to_string())
        );
        let (lat, lon) = info.gps.unwrap();
        assert!((lat - 31.2304).abs() < 1e-4);
        assert!((lon - 121.4737).abs() < 1e-4);

        let groups = read_image_details(&path).unwrap();
        assert!(
            groups
                .iter()
                .any(|g| g.name == "基本与设备" && !g.fields.is_empty())
        );
        assert!(
            groups
                .iter()
                .any(|g| g.name == "拍摄参数" && !g.fields.is_empty())
        );
        assert!(
            groups
                .iter()
                .any(|g| g.name == "位置信息" && !g.fields.is_empty())
        );
    }

    #[test]
    fn clears_fields_when_left_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = sample_jpeg(dir.path());
        save_edits(&path, &full_fields(), false).unwrap();

        let mut cleared = read_editable(&path).unwrap();
        cleared.description.clear();
        cleared.artist.clear();
        cleared.user_comment.clear();
        cleared.iso.clear();
        cleared.gps_latitude.clear();
        cleared.gps_longitude.clear();
        cleared.gps_altitude.clear();
        save_edits(&path, &cleared, false).unwrap();

        let loaded = read_editable(&path).unwrap();
        assert!(loaded.description.is_empty());
        assert!(loaded.artist.is_empty());
        assert!(loaded.user_comment.is_empty());
        assert!(loaded.iso.is_empty());
        assert!(loaded.gps_latitude.is_empty());
        assert!(loaded.gps_longitude.is_empty());
        assert!(loaded.gps_altitude.is_empty());
        assert_eq!(loaded.make, "TestCam");
    }

    #[test]
    fn rejects_invalid_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = sample_jpeg(dir.path());

        let mut fields = full_fields();
        fields.gps_longitude.clear();
        assert!(save_edits(&path, &fields, false).is_err());

        let mut fields = full_fields();
        fields.iso = "abc".into();
        assert!(save_edits(&path, &fields, false).is_err());

        let mut fields = full_fields();
        fields.orientation = 9;
        assert!(save_edits(&path, &fields, false).is_err());
    }
}
