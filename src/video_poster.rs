use std::path::Path;
use std::process::Command;

use image::DynamicImage;

/// Extract a poster frame for a video. On Windows the shell thumbnail cache is
/// used (it handles hardware codecs without shipping one); macOS falls back to
/// Quick Look and Linux to ffmpeg/ffmpegthumbnailer when available.
pub fn load_poster(path: &Path, max_size: u32) -> Result<DynamicImage, String> {
    let size = max_size.clamp(96, 2048);

    #[cfg(target_os = "windows")]
    {
        let shell = windows_shell::load(path, size);
        if shell.is_ok() {
            return shell;
        }
        ffmpeg(path, size).map_err(|_| {
            shell
                .err()
                .unwrap_or_else(|| "系统未提供可用的视频解码器".to_string())
        })
    }

    #[cfg(target_os = "macos")]
    {
        let quick = quicklook(path, size);
        if quick.is_ok() {
            return quick;
        }
        ffmpeg(path, size).map_err(|_| {
            quick
                .err()
                .unwrap_or_else(|| "Quick Look 无法生成视频封面".to_string())
        })
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        ffmpeg(path, size).or_else(|_| ffmpeg_thumbnailer(path, size))
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        let _ = (path, size);
        Err("当前平台暂不支持视频封面".to_string())
    }
}

fn read_image(bytes: &[u8]) -> Result<DynamicImage, String> {
    image::load_from_memory(bytes).map_err(|error| format!("无法解码视频封面：{error}"))
}

#[cfg(any(
    target_os = "windows",
    target_os = "macos",
    all(unix, not(target_os = "macos"))
))]
fn ffmpeg(path: &Path, size: u32) -> Result<DynamicImage, String> {
    let filter = format!("scale='min({size},iw)':-2");
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", "1", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &filter,
            "-f",
            "image2pipe",
            "-vcodec",
            "png",
            "-",
        ])
        .output()
        .map_err(|error| format!("未找到 ffmpeg：{error}"))?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err("ffmpeg 未能提取视频封面".to_string());
    }
    read_image(&output.stdout)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn ffmpeg_thumbnailer(path: &Path, size: u32) -> Result<DynamicImage, String> {
    let target = std::env::temp_dir().join(format!(
        "exif-studio-poster-{}-{}.png",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let output = Command::new("ffmpegthumbnailer")
        .arg("-i")
        .arg(path)
        .args(["-s", &size.to_string(), "-o"])
        .arg(&target)
        .output()
        .map_err(|error| format!("未找到 ffmpegthumbnailer：{error}"))?;
    if !output.status.success() {
        return Err("ffmpegthumbnailer 未能提取视频封面".to_string());
    }
    let bytes = std::fs::read(&target).map_err(|error| format!("无法读取视频封面：{error}"))?;
    let _ = std::fs::remove_file(&target);
    read_image(&bytes)
}

#[cfg(target_os = "macos")]
fn quicklook(path: &Path, size: u32) -> Result<DynamicImage, String> {
    let directory = std::env::temp_dir().join(format!(
        "exif-studio-ql-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&directory).map_err(|error| format!("无法创建临时目录：{error}"))?;
    let output = Command::new("qlmanage")
        .args(["-t", "-s", &size.to_string(), "-o"])
        .arg(&directory)
        .arg(path)
        .output()
        .map_err(|error| format!("无法启动 Quick Look：{error}"))?;
    let result = (|| {
        if !output.status.success() {
            return Err("Quick Look 未能生成视频封面".to_string());
        }
        let entry = std::fs::read_dir(&directory)
            .map_err(|error| format!("无法读取临时目录：{error}"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|ext| ext == "png"))
            .ok_or_else(|| "Quick Look 没有输出封面图".to_string())?;
        let bytes = std::fs::read(&entry).map_err(|error| format!("无法读取封面：{error}"))?;
        read_image(&bytes)
    })();
    let _ = std::fs::remove_dir_all(&directory);
    result
}

#[cfg(target_os = "windows")]
mod windows_shell {
    use std::ffi::c_void;
    use std::mem::size_of;

    use image::{DynamicImage, RgbaImage};
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GetDC, GetDIBits, GetObjectW, HGDIOBJ, ReleaseDC, SelectObject,
    };
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize, IBindCtx,
    };
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_THUMBNAILONLY,
    };
    use windows::core::HSTRING;

    struct ComGuard(bool);

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    pub fn load(path: &std::path::Path, size: u32) -> Result<DynamicImage, String> {
        unsafe {
            let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            let _guard = ComGuard(initialized);
            let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
            let wide = HSTRING::from(absolute.to_string_lossy().as_ref());
            let factory: IShellItemImageFactory =
                SHCreateItemFromParsingName(&wide, None::<&IBindCtx>)
                    .map_err(|error| format!("Shell 缩略图不可用：{error}"))?;
            let bitmap = factory
                .GetImage(
                    SIZE {
                        cx: size as i32,
                        cy: size as i32,
                    },
                    SIIGBF_THUMBNAILONLY,
                )
                .map_err(|error| format!("系统无法为视频生成封面：{error}"))?;
            to_image(bitmap)
        }
    }

    unsafe fn to_image(
        bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
    ) -> Result<DynamicImage, String> {
        unsafe {
            let mut info = BITMAP::default();
            if GetObjectW(
                HGDIOBJ(bitmap.0),
                size_of::<BITMAP>() as i32,
                Some((&mut info as *mut BITMAP).cast()),
            ) == 0
            {
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                return Err("无法读取 Shell 封面尺寸".to_string());
            }
            let width = info.bmWidth.abs();
            let height = info.bmHeight.abs();
            if width <= 0 || height <= 0 || width > 8192 || height > 8192 {
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                return Err("Shell 封面尺寸无效".to_string());
            }

            let screen = GetDC(None);
            let memory = CreateCompatibleDC(Some(screen));
            let previous = SelectObject(memory, HGDIOBJ(bitmap.0));
            let mut header = BITMAPINFO::default();
            header.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            header.bmiHeader.biWidth = width;
            header.bmiHeader.biHeight = -height;
            header.bmiHeader.biPlanes = 1;
            header.bmiHeader.biBitCount = 32;
            header.bmiHeader.biCompression = BI_RGB.0;

            let mut pixels = vec![0u8; width as usize * height as usize * 4];
            let lines = GetDIBits(
                memory,
                bitmap,
                0,
                height as u32,
                Some(pixels.as_mut_ptr().cast::<c_void>()),
                &mut header,
                DIB_RGB_COLORS,
            );
            SelectObject(memory, previous);
            let _ = DeleteDC(memory);
            ReleaseDC(None, screen);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));

            if lines == 0 {
                return Err("无法读取 Shell 封面像素".to_string());
            }
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.swap(0, 2);
                if pixel[3] == 0 {
                    pixel[3] = 255;
                }
            }
            RgbaImage::from_raw(width as u32, height as u32, pixels)
                .map(DynamicImage::ImageRgba8)
                .ok_or_else(|| "Shell 封面数据不完整".to_string())
        }
    }
}
