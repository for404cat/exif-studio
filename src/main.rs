#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use eframe::egui;
use exif_studio::app::ExifStudioApp;

fn main() -> eframe::Result<()> {
    let mut initial: Option<PathBuf> = None;
    let mut open_stats = false;
    for arg in std::env::args().skip(1) {
        if arg == "--stats" {
            open_stats = true;
        } else if !arg.starts_with('-') && initial.is_none() {
            initial = Some(PathBuf::from(arg));
        }
    }

    // 部分 Intel 驱动上 wgpu 的 WGL 后端会栈溢出，仅启用 DX12/Vulkan。
    let mut wgpu_setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    wgpu_setup.instance_descriptor.backends = eframe::wgpu::Backends::PRIMARY;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Exif Studio")
            .with_inner_size([1380.0, 900.0])
            .with_min_inner_size([1080.0, 700.0]),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(wgpu_setup),
            ..Default::default()
        },
        ..Default::default()
    };

    eframe::run_native(
        "Exif Studio",
        options,
        Box::new(move |cc| Ok(Box::new(ExifStudioApp::new(cc, initial, open_stats)))),
    )
}
