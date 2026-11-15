use std::path::Path;

use exif_studio::exif_io::{EditFields, save_edits};
use image::{ImageBuffer, Rgb, RgbImage};

struct Sample {
    name: &'static str,
    width: u32,
    height: u32,
    palette: [[u8; 3]; 3],
    fields: EditFields,
}

fn gradient(width: u32, height: u32, palette: [[u8; 3]; 3]) -> RgbImage {
    ImageBuffer::from_fn(width, height, |x, y| {
        let fx = x as f32 / width as f32;
        let fy = y as f32 / height as f32;
        let t = (fy * 0.7 + fx * 0.3).clamp(0.0, 1.0);
        let (a, b) = if t < 0.5 {
            (&palette[0], &palette[1])
        } else {
            (&palette[1], &palette[2])
        };
        let local = if t < 0.5 { t * 2.0 } else { (t - 0.5) * 2.0 };
        Rgb([
            (a[0] as f32 * (1.0 - local) + b[0] as f32 * local) as u8,
            (a[1] as f32 * (1.0 - local) + b[1] as f32 * local) as u8,
            (a[2] as f32 * (1.0 - local) + b[2] as f32 * local) as u8,
        ])
    })
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new("samples").to_path_buf());
    std::fs::create_dir_all(&out).unwrap();

    let samples = vec![
        Sample {
            name: "sunset_sky.jpg",
            width: 1400,
            height: 1050,
            palette: [[244, 148, 82], [196, 82, 104], [48, 42, 88]],
            fields: EditFields {
                description: "Sunset over the Huangpu River".into(),
                artist: "Lin Wei".into(),
                copyright: "2024 Lin Wei".into(),
                software: "Exif Studio".into(),
                make: "Canon".into(),
                model: "EOS R5".into(),
                lens_make: "Canon".into(),
                lens_model: "RF 24-70mm F2.8 L IS USM".into(),
                date_time_original: "2024:05:18 18:42:11".into(),
                offset_time_original: "+08:00".into(),
                iso: "100".into(),
                f_number: "8".into(),
                exposure_time: "1/125".into(),
                focal_length: "35".into(),
                focal_length_35mm: "35".into(),
                orientation: 1,
                gps_latitude: "31.2304".into(),
                gps_longitude: "121.4737".into(),
                gps_altitude: "12.5".into(),
                ..Default::default()
            },
        },
        Sample {
            name: "street_night.jpg",
            width: 1200,
            height: 1600,
            palette: [[14, 16, 38], [58, 48, 96], [232, 168, 80]],
            fields: EditFields {
                description: "Neon alley after rain".into(),
                artist: "Lin Wei".into(),
                make: "Sony".into(),
                model: "ILCE-7M4".into(),
                lens_model: "FE 35mm F1.4 GM".into(),
                date_time_original: "2024:11:02 21:15:44".into(),
                iso: "3200".into(),
                f_number: "1.4".into(),
                exposure_time: "1/60".into(),
                focal_length: "35".into(),
                focal_length_35mm: "35".into(),
                orientation: 1,
                ..Default::default()
            },
        },
        Sample {
            name: "portrait_studio.jpg",
            width: 1400,
            height: 1050,
            palette: [[230, 208, 188], [180, 142, 122], [92, 64, 58]],
            fields: EditFields {
                description: "Studio portrait, soft key light".into(),
                make: "FUJIFILM".into(),
                model: "X-T5".into(),
                lens_model: "XF 56mm F1.2 R".into(),
                date_time_original: "2025:01:12 14:20:05".into(),
                iso: "400".into(),
                f_number: "2".into(),
                exposure_time: "1/250".into(),
                focal_length: "56".into(),
                focal_length_35mm: "84".into(),
                orientation: 6,
                ..Default::default()
            },
        },
        Sample {
            name: "mountain_hike.jpg",
            width: 1600,
            height: 1200,
            palette: [[176, 208, 224], [120, 150, 132], [56, 72, 64]],
            fields: EditFields {
                description: "Above the clouds on Huangshan".into(),
                make: "Apple".into(),
                model: "iPhone 15 Pro".into(),
                lens_model: "iPhone 15 Pro back triple camera 6.86mm f/1.78".into(),
                date_time_original: "2025:07:21 09:05:30".into(),
                offset_time_original: "+08:00".into(),
                iso: "64".into(),
                f_number: "1.78".into(),
                exposure_time: "1/1200".into(),
                focal_length: "6.86".into(),
                focal_length_35mm: "24".into(),
                orientation: 1,
                gps_latitude: "30.1333".into(),
                gps_longitude: "118.1667".into(),
                gps_altitude: "1860".into(),
                ..Default::default()
            },
        },
        Sample {
            name: "beach_morning.jpg",
            width: 1500,
            height: 1000,
            palette: [[204, 226, 236], [120, 184, 196], [222, 200, 160]],
            fields: EditFields {
                description: "Low tide at dawn".into(),
                make: "NIKON CORPORATION".into(),
                model: "NIKON Z 8".into(),
                lens_model: "NIKKOR Z 24-120mm f/4 S".into(),
                date_time_original: "2023:08:14 06:45:19".into(),
                iso: "200".into(),
                f_number: "5.6".into(),
                exposure_time: "1/500".into(),
                focal_length: "85".into(),
                focal_length_35mm: "85".into(),
                orientation: 1,
                ..Default::default()
            },
        },
        Sample {
            name: "city_dusk.jpg",
            width: 1600,
            height: 1067,
            palette: [[46, 62, 102], [134, 92, 122], [240, 178, 110]],
            fields: EditFields {
                description: "CBD skyline at blue hour".into(),
                make: "Panasonic".into(),
                model: "DC-S5M2".into(),
                lens_model: "LUMIX S 50mm F1.8".into(),
                date_time_original: "2024:03:29 19:12:00".into(),
                iso: "800".into(),
                f_number: "2.8".into(),
                exposure_time: "1/80".into(),
                focal_length: "50".into(),
                focal_length_35mm: "50".into(),
                orientation: 1,
                gps_latitude: "39.9042".into(),
                gps_longitude: "116.4074".into(),
                ..Default::default()
            },
        },
        Sample {
            name: "forest_stream.jpg",
            width: 1400,
            height: 1050,
            palette: [[130, 162, 118], [82, 116, 92], [36, 58, 52]],
            fields: EditFields {
                description: "Mossy stream in the bamboo forest".into(),
                make: "Canon".into(),
                model: "EOS R6 Mark II".into(),
                lens_model: "RF 15-35mm F2.8 L IS USM".into(),
                date_time_original: "2025:10:04 10:11:22".into(),
                iso: "640".into(),
                f_number: "4".into(),
                exposure_time: "1/100".into(),
                focal_length: "15".into(),
                focal_length_35mm: "15".into(),
                orientation: 1,
                gps_latitude: "30.2741".into(),
                gps_longitude: "120.1551".into(),
                ..Default::default()
            },
        },
        Sample {
            name: "snow_cabin.jpg",
            width: 1500,
            height: 1000,
            palette: [[220, 230, 242], [148, 174, 198], [92, 74, 70]],
            fields: EditFields {
                description: "Cabin under first snow".into(),
                make: "Sony".into(),
                model: "ILCE-7RM5".into(),
                lens_model: "FE 24-105mm F4 G OSS".into(),
                date_time_original: "2022:12:23 16:05:55".into(),
                iso: "100".into(),
                f_number: "9".into(),
                exposure_time: "1/200".into(),
                focal_length: "24".into(),
                focal_length_35mm: "24".into(),
                orientation: 1,
                gps_latitude: "45.8038".into(),
                gps_longitude: "126.5349".into(),
                gps_altitude: "150".into(),
                ..Default::default()
            },
        },
        Sample {
            name: "abstract_render.png",
            width: 1200,
            height: 1200,
            palette: [[30, 34, 50], [90, 60, 140], [240, 190, 90]],
            fields: EditFields {
                description: "Procedural render study 04".into(),
                software: "Blender 4.2".into(),
                artist: "Mo Chen".into(),
                date_time_original: "2025:06:30 23:41:09".into(),
                ..Default::default()
            },
        },
    ];

    for sample in samples {
        let path = out.join(sample.name);
        let image = gradient(sample.width, sample.height, sample.palette);
        image.save(&path).unwrap();
        let result = save_edits(&path, &sample.fields, false).unwrap();
        assert!(result.backup.is_none());
        println!("created {}", path.display());
    }

    for (name, width, height, palette) in [
        (
            "scanned_map.tif",
            1100u32,
            800u32,
            [[222, 208, 176], [190, 168, 122], [128, 108, 76]],
        ),
        (
            "old_print.tif",
            900u32,
            1200u32,
            [[212, 198, 182], [160, 140, 124], [96, 84, 76]],
        ),
    ] {
        let path = out.join(name);
        gradient(width, height, palette).save(&path).unwrap();
        println!("created {}", path.display());
    }

    write_video(
        &out.join("phone_clip.mp4"),
        VideoSpec {
            codec: *b"avc1",
            width: 1920,
            height: 1080,
            fps: 30,
            duration: 12,
            make: "Apple",
            model: "iPhone 15 Pro",
            created: "2025-07-21T09:06:12+0800",
            location: "+31.2304+121.4737+012.500/",
            software: "17.5.1",
        },
    );
    write_video(
        &out.join("drone_flight.mov"),
        VideoSpec {
            codec: *b"hvc1",
            width: 3840,
            height: 2160,
            fps: 24,
            duration: 30,
            make: "DJI",
            model: "Mavic 3 Pro",
            created: "2024-09-15T17:32:08+0800",
            location: "+30.1333+118.1667+1860.000/",
            software: "03.02.0600",
        },
    );
    write_video(
        &out.join("vlog_camera.mp4"),
        VideoSpec {
            codec: *b"avc1",
            width: 1280,
            height: 720,
            fps: 60,
            duration: 45,
            make: "Sony",
            model: "ILCE-7M4",
            created: "2024-11-02T21:15:44+0800",
            location: "+39.9042+116.4074+043.000/",
            software: "3.00",
        },
    );
}

struct VideoSpec {
    codec: [u8; 4],
    width: u32,
    height: u32,
    fps: u32,
    duration: u32,
    make: &'static str,
    model: &'static str,
    created: &'static str,
    location: &'static str,
    software: &'static str,
}

fn be32(value: u32) -> [u8; 4] {
    value.to_be_bytes()
}

fn put_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&be32(value));
}

fn put_u16(buf: &mut [u8], offset: usize, value: u16) {
    buf[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(&be32((body.len() + 8) as u32));
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![
        version,
        (flags >> 16) as u8,
        (flags >> 8) as u8,
        flags as u8,
    ];
    payload.extend_from_slice(body);
    mp4_box(kind, &payload)
}

fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(part);
    }
    out
}

const MAC_EPOCH_2025: u32 = 3_818_534_400;

fn make_mvhd(timescale: u32, duration: u32) -> Vec<u8> {
    let mut body = vec![0u8; 100];
    put_u32(&mut body, 4, MAC_EPOCH_2025);
    put_u32(&mut body, 8, MAC_EPOCH_2025);
    put_u32(&mut body, 12, timescale);
    put_u32(&mut body, 16, duration * timescale);
    put_u32(&mut body, 20, 0x0001_0000);
    put_u16(&mut body, 24, 0x0100);
    put_u32(&mut body, 36, 0x0001_0000);
    put_u32(&mut body, 52, 0x0001_0000);
    put_u32(&mut body, 68, 0x4000_0000);
    put_u32(&mut body, 96, 3);
    mp4_box(b"mvhd", &body)
}

fn make_tkhd(track_id: u32, width: u32, height: u32, duration: u32) -> Vec<u8> {
    let mut body = vec![0u8; 84];
    put_u32(&mut body, 4, MAC_EPOCH_2025);
    put_u32(&mut body, 8, MAC_EPOCH_2025);
    put_u32(&mut body, 12, track_id);
    put_u32(&mut body, 20, duration * 1000);
    if width == 0 {
        put_u16(&mut body, 36, 0x0100);
    }
    put_u32(&mut body, 40, 0x0001_0000);
    put_u32(&mut body, 56, 0x0001_0000);
    put_u32(&mut body, 72, 0x4000_0000);
    put_u32(&mut body, 76, width << 16);
    put_u32(&mut body, 80, height << 16);
    mp4_box(b"tkhd", &body)
}

fn make_mdhd(timescale: u32, duration: u32) -> Vec<u8> {
    let mut body = vec![0u8; 24];
    put_u32(&mut body, 4, MAC_EPOCH_2025);
    put_u32(&mut body, 8, MAC_EPOCH_2025);
    put_u32(&mut body, 12, timescale);
    put_u32(&mut body, 16, duration * timescale);
    put_u16(&mut body, 20, 0x55c4);
    mp4_box(b"mdhd", &body)
}

fn make_hdlr(kind: &[u8; 4]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&[0u8; 4]);
    body.extend_from_slice(kind);
    body.extend_from_slice(&[0u8; 12]);
    body.extend_from_slice(b"handler\0");
    full_box(b"hdlr", 0, 0, &body)
}

fn make_stsd_video(codec: &[u8; 4], width: u32, height: u32) -> Vec<u8> {
    let mut entry = vec![0u8; 86];
    put_u32(&mut entry, 0, 86);
    entry[4..8].copy_from_slice(codec);
    put_u16(&mut entry, 14, 1);
    put_u16(&mut entry, 32, width as u16);
    put_u16(&mut entry, 34, height as u16);
    put_u32(&mut entry, 36, 0x0048_0000);
    put_u32(&mut entry, 40, 0x0048_0000);
    put_u16(&mut entry, 48, 1);
    put_u16(&mut entry, 74, 0x0018);
    put_u16(&mut entry, 76, 0xffff);
    let mut body = vec![0u8; 4];
    put_u32(&mut body, 0, 1);
    body.extend_from_slice(&entry);
    full_box(b"stsd", 0, 0, &body)
}

fn make_stsd_audio() -> Vec<u8> {
    let mut entry = vec![0u8; 36];
    put_u32(&mut entry, 0, 36);
    entry[4..8].copy_from_slice(b"mp4a");
    put_u16(&mut entry, 14, 1);
    put_u16(&mut entry, 24, 2);
    put_u16(&mut entry, 26, 16);
    put_u32(&mut entry, 32, 48_000 << 16);
    let mut body = vec![0u8; 4];
    put_u32(&mut body, 0, 1);
    body.extend_from_slice(&entry);
    full_box(b"stsd", 0, 0, &body)
}

fn make_stts(samples: u32, delta: u32) -> Vec<u8> {
    let mut body = vec![0u8; 12];
    put_u32(&mut body, 0, 1);
    put_u32(&mut body, 4, samples);
    put_u32(&mut body, 8, delta);
    full_box(b"stts", 0, 0, &body)
}

fn make_video_trak(spec: &VideoSpec) -> Vec<u8> {
    let media_timescale = spec.fps * 1000;
    let samples = spec.fps * spec.duration;
    let stbl = mp4_box(
        b"stbl",
        &concat(&[
            make_stsd_video(&spec.codec, spec.width, spec.height),
            make_stts(samples, 1000),
        ]),
    );
    let minf = mp4_box(b"minf", &stbl);
    let mdia = mp4_box(
        b"mdia",
        &concat(&[
            make_mdhd(media_timescale, spec.duration),
            make_hdlr(b"vide"),
            minf,
        ]),
    );
    mp4_box(
        b"trak",
        &concat(&[make_tkhd(1, spec.width, spec.height, spec.duration), mdia]),
    )
}

fn make_audio_trak(duration: u32) -> Vec<u8> {
    let stbl = mp4_box(b"stbl", &make_stsd_audio());
    let minf = mp4_box(b"minf", &stbl);
    let mdia = mp4_box(
        b"mdia",
        &concat(&[make_mdhd(48_000, duration), make_hdlr(b"soun"), minf]),
    );
    mp4_box(b"trak", &concat(&[make_tkhd(2, 0, 0, duration), mdia]))
}

fn make_meta_tags(spec: &VideoSpec) -> Vec<u8> {
    let tags = [
        ("com.apple.quicktime.make", spec.make),
        ("com.apple.quicktime.model", spec.model),
        ("com.apple.quicktime.creationdate", spec.created),
        ("com.apple.quicktime.location.ISO6709", spec.location),
        ("com.apple.quicktime.software", spec.software),
    ];

    let mut keys_body = vec![0u8; 8];
    put_u32(&mut keys_body, 4, tags.len() as u32);
    let mut ilst_body = Vec::new();
    for (index, (key, value)) in tags.iter().enumerate() {
        keys_body.extend_from_slice(&be32((key.len() + 8) as u32));
        keys_body.extend_from_slice(b"mdta");
        keys_body.extend_from_slice(key.as_bytes());

        let mut data_body = vec![0u8; 8];
        put_u32(&mut data_body, 0, 1);
        data_body.extend_from_slice(value.as_bytes());
        let item = mp4_box(&be32(index as u32 + 1), &mp4_box(b"data", &data_body));
        ilst_body.extend_from_slice(&item);
    }

    let hdlr = {
        let mut body = vec![0u8; 4];
        body.extend_from_slice(b"mdta");
        body.extend_from_slice(&[0u8; 12]);
        body.extend_from_slice(b"mdtahdlr\0");
        full_box(b"hdlr", 0, 0, &body)
    };
    let meta = mp4_box(
        b"meta",
        &concat(&[
            vec![0u8; 4],
            hdlr,
            mp4_box(b"keys", &keys_body),
            mp4_box(b"ilst", &ilst_body),
        ]),
    );
    mp4_box(b"udta", &meta)
}

fn write_video(path: &Path, spec: VideoSpec) {
    let mut ftyp_body = Vec::new();
    ftyp_body.extend_from_slice(b"isom");
    ftyp_body.extend_from_slice(&be32(0x200));
    ftyp_body.extend_from_slice(b"isomiso2mp41avc1");
    let ftyp = mp4_box(b"ftyp", &ftyp_body);

    let moov = mp4_box(
        b"moov",
        &concat(&[
            make_mvhd(1000, spec.duration),
            make_video_trak(&spec),
            make_audio_trak(spec.duration),
            make_meta_tags(&spec),
        ]),
    );
    let mdat = mp4_box(b"mdat", &vec![0u8; 64 * 1024]);
    std::fs::write(path, concat(&[ftyp, moov, mdat])).unwrap();
    println!("created {}", path.display());
}
