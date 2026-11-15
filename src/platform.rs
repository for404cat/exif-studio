use std::path::{Path, PathBuf};
use std::process::Command;

const APP_DIR: &str = "exif-studio";

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR)
}

pub fn default_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(APP_DIR)
}

pub fn cjk_font_candidates() -> &'static [&'static str] {
    #[cfg(target_os = "windows")]
    {
        &[
            r"C:\Windows\Fonts\Deng.ttf",
            r"C:\Windows\Fonts\simhei.ttf",
            r"C:\Windows\Fonts\msyh.ttc",
            r"C:\Windows\Fonts\msyh.ttf",
        ]
    }
    #[cfg(target_os = "macos")]
    {
        &[
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/Library/Fonts/Arial Unicode.ttf",
        ]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
        ]
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        &[]
    }
}

pub fn reveal_in_file_manager(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("explorer").arg("/select,").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg("-R").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = path.parent().unwrap_or(path);
        let _ = Command::new("xdg-open").arg(target).spawn();
    }
}

pub fn open_directory(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("explorer").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(path).spawn();
    }
}

pub fn open_file(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("explorer").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(path).spawn();
    }
}
