use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::task::{Context as TaskContext, Poll, Waker};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Button, Color32, ComboBox, FontData, FontDefinitions, FontFamily, FontId,
    Frame, Id, Label, Layout, Margin, Order, ProgressBar, Rect, RichText, ScrollArea, Sense,
    Stroke, TextEdit, Ui, Vec2,
};

use crate::api::{ApiFile, ApiFolder, ApiRegistry, ApiServer, generate_token};
use crate::cache::{CacheStore, file_key};
use crate::catalog::RatingCatalog;
use crate::details::{self, DetailsService, DetailsState};
use crate::exif_io::{self, ExifField, ExifGroup};
use crate::metacache::MetadataCache;
use crate::model::{
    CAMERA_BRANDS, MediaEntry, MediaKind, format_bytes, format_duration, format_fps,
    format_friendly_datetime, format_friendly_local, is_writable_extension, known_camera_brand,
    trim_float,
};
use crate::platform;
use crate::scan::{self, ScanHandle, ScanMsg};
use crate::settings::{self, DuplicateSort, DuplicatesLayout, PhotoSort, Settings};
use crate::stats::{self, Stats};
use crate::thumbs::{ImageJob, ImageService};
use crate::vision::{self, DuplicateGroup, VisionCache, VisionHandle, VisionMsg, VisionReport};
use crate::watermark::{self, Recognition, WatermarkIndex};
use crate::widgets;

const THUMB_SIZE: u32 = 112;
const PREVIEW_MAX: u32 = 1600;
const MAX_THUMB_TEXTURES: usize = 256;
const MAX_THUMB_INFLIGHT: usize = 48;
const THUMB_VARIANT: u32 = 1;
const THUMB_LARGE_VARIANT: u32 = 3;
const THUMB_LARGE_SIZE: u32 = 256;
const PREVIEW_VARIANT: u32 = 2;
const GRID_SPACING: f32 = 8.0;
const DUPLICATES_PAGE_SIZE: usize = 20;
const GROUP_PREVIEW_LIMIT: usize = 12;
const DUPLICATES_WATERFALL_CHUNK: usize = 24;
const API_PUBLISH_INTERVAL: Duration = Duration::from_millis(1000);
const MAX_CONCURRENT_SCANS: usize = 2;
const MAX_SCAN_BATCHES_PER_FRAME: usize = 64;
const MAX_TEXTURE_UPLOADS_PER_FRAME: usize = 6;
const SCAN_FILTER_INTERVAL: Duration = Duration::from_millis(700);
const SCAN_CHIP_INTERVAL: Duration = Duration::from_millis(900);
const FAVORITE_REFRESH_INTERVAL: Duration = Duration::from_millis(900);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewTab {
    Browse,
    Stats,
    Duplicates,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailTab {
    Info,
    Edit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum KindFilter {
    All,
    Images,
    Videos,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToastKind {
    Info,
    Success,
    Error,
}

struct Toast {
    kind: ToastKind,
    text: String,
    created: Instant,
}

struct WatermarkDialog {
    tab_id: u64,
    path: PathBuf,
    folder: String,
    output_path: Option<PathBuf>,
    text: String,
    passphrase: String,
    busy: bool,
    receiver: Option<Receiver<Result<WatermarkOutcome, String>>>,
    message: Option<(bool, String)>,
    extracted: Option<String>,
}

impl WatermarkDialog {
    fn new(tab_id: u64, path: PathBuf, folder: String) -> Self {
        Self {
            tab_id,
            path,
            folder,
            output_path: None,
            text: String::new(),
            passphrase: String::new(),
            busy: false,
            receiver: None,
            message: None,
            extracted: None,
        }
    }
}

enum WatermarkOutcome {
    Embedded(watermark::EmbedOutcome),
    Extracted(Option<Recognition>),
}

#[derive(Default)]
struct PreviewState {
    path: Option<PathBuf>,
    key: u64,
    texture: Option<egui::TextureHandle>,
    error: Option<String>,
    loading: bool,
}

#[derive(Clone)]
enum TabAction {
    Select(PathBuf),
    Rescan,
    Close,
}

struct PendingAction {
    tab_id: u64,
    action: TabAction,
}

type FolderPicker = Pin<Box<dyn Future<Output = Option<Vec<rfd::FileHandle>>> + Send>>;

struct VisionHardware {
    gpu: String,
    npu: Option<String>,
    adapters: Vec<String>,
}

struct FolderTab {
    id: u64,
    folder: PathBuf,
    canonical: PathBuf,
    entries: Vec<MediaEntry>,
    filtered: Vec<usize>,
    filter_dirty: bool,
    favorites_only: bool,
    folder_filter: Option<PathBuf>,
    folder_groups_expanded: bool,
    search: String,
    kind_filter: KindFilter,
    selected: Option<PathBuf>,
    scan: Option<ScanHandle>,
    scanning: bool,
    scan_queued: bool,
    scan_total: usize,
    scan_done: usize,
    scan_discovered: usize,
    image_count: usize,
    total_bytes: u64,
    favorites_dirty: bool,
    favorite_count: usize,
    favorites_built_at: Instant,
    folder_group_cache: Vec<(PathBuf, String, usize)>,
    folder_groups_dirty: bool,
    folder_groups_built_at: Instant,
    last_filter_refresh: Instant,
    filtered_time_range: Option<(i64, i64)>,
    browse_time_cursor: f32,
    entry_index: Arc<HashMap<PathBuf, usize>>,
    entry_index_dirty: bool,
    pending_select: Option<PathBuf>,
    recursive: bool,
    view: ViewTab,
    detail_tab: DetailTab,
    details: Option<DetailsState>,
    details_request: Option<PathBuf>,
    vision: Option<VisionHandle>,
    vision_report: Option<VisionReport>,
    vision_progress: Option<(usize, usize)>,
    vision_error: Option<String>,
    vision_attempted: bool,
    stats: Option<Stats>,
    stats_dirty: bool,
    reset_list_scroll: bool,
    duplicates_page: usize,
    duplicates_scroll_top: bool,
    duplicates_stick_ready: bool,
    duplicates_notice: Option<(String, Instant)>,
    duplicates_bounce_at: Option<Instant>,
    duplicates_loaded: usize,
    duplicates_start: usize,
    duplicates_revision: u64,
    duplicate_order: Vec<usize>,
    duplicate_order_revision: u64,
    duplicate_order_sort: DuplicateSort,
    duplicate_group_times: Vec<i64>,
    duplicate_group_pixels: Vec<u64>,
    duplicate_time_range: Option<(i64, i64)>,
    duplicates_time_cursor: f32,
    duplicates_cursor_anchor: (usize, usize),
    vision_start_at: f64,
    vision_cache_loaded: bool,
}

impl FolderTab {
    fn new(id: u64, folder: PathBuf, canonical: PathBuf) -> Self {
        Self {
            id,
            folder,
            canonical,
            entries: Vec::new(),
            filtered: Vec::new(),
            filter_dirty: true,
            favorites_only: false,
            folder_filter: None,
            folder_groups_expanded: false,
            search: String::new(),
            kind_filter: KindFilter::All,
            selected: None,
            scan: None,
            scanning: false,
            scan_queued: false,
            scan_total: 0,
            scan_done: 0,
            scan_discovered: 0,
            image_count: 0,
            total_bytes: 0,
            favorites_dirty: true,
            favorite_count: 0,
            favorites_built_at: Instant::now() - FAVORITE_REFRESH_INTERVAL,
            folder_group_cache: Vec::new(),
            folder_groups_dirty: false,
            folder_groups_built_at: Instant::now() - SCAN_CHIP_INTERVAL,
            last_filter_refresh: Instant::now() - SCAN_FILTER_INTERVAL,
            filtered_time_range: None,
            browse_time_cursor: 1.0,
            entry_index: Arc::new(HashMap::new()),
            entry_index_dirty: true,
            pending_select: None,
            recursive: true,
            view: ViewTab::Browse,
            detail_tab: DetailTab::Info,
            details: None,
            details_request: None,
            vision: None,
            vision_report: None,
            vision_progress: None,
            vision_error: None,
            vision_attempted: false,
            stats: None,
            stats_dirty: true,
            reset_list_scroll: true,
            duplicates_page: 0,
            duplicates_scroll_top: false,
            duplicates_stick_ready: false,
            duplicates_notice: None,
            duplicates_bounce_at: None,
            duplicates_loaded: DUPLICATES_WATERFALL_CHUNK,
            duplicates_start: 0,
            duplicates_revision: 0,
            duplicate_order: Vec::new(),
            duplicate_order_revision: u64::MAX,
            duplicate_order_sort: DuplicateSort::Count,
            duplicate_group_times: Vec::new(),
            duplicate_group_pixels: Vec::new(),
            duplicate_time_range: None,
            duplicates_time_cursor: 0.5,
            duplicates_cursor_anchor: (usize::MAX, usize::MAX),
            vision_start_at: 0.0,
            vision_cache_loaded: false,
        }
    }

    fn placeholder() -> Self {
        Self::new(0, PathBuf::new(), PathBuf::new())
    }

    fn edit_is_dirty(&self) -> bool {
        self.details
            .as_ref()
            .map(|details| details.editable.is_some() && details.editable != details.original)
            .unwrap_or(false)
    }

    fn selected_entry(&self) -> Option<&MediaEntry> {
        let selected = self.selected.as_ref()?;
        if !self.entry_index_dirty
            && let Some(&index) = self.entry_index.get(selected)
        {
            return self.entries.get(index);
        }
        self.entries.iter().find(|entry| &entry.path == selected)
    }

    fn rebuild_entry_index(&mut self) {
        self.entry_index = Arc::new(
            self.entries
                .iter()
                .enumerate()
                .map(|(index, entry)| (entry.path.clone(), index))
                .collect(),
        );
        self.entry_index_dirty = false;
    }

    fn touch_duplicates(&mut self) {
        self.duplicates_revision = self.duplicates_revision.wrapping_add(1);
        self.duplicate_order_revision = u64::MAX;
        self.duplicates_start = 0;
        self.duplicates_page = 0;
        self.duplicates_loaded = DUPLICATES_WATERFALL_CHUNK;
        self.duplicates_scroll_top = true;
        self.duplicates_stick_ready = false;
        self.duplicates_time_cursor = 0.5;
        self.duplicates_cursor_anchor = (usize::MAX, usize::MAX);
    }
}

pub struct ExifStudioApp {
    tabs: Vec<FolderTab>,
    active: Option<usize>,
    next_tab_id: u64,
    start_in_stats: bool,
    folder_picker: Option<FolderPicker>,
    detail_loader: DetailsService,
    ratings: RatingCatalog,
    watermark_index: WatermarkIndex,
    vision_cache: Arc<VisionCache>,
    vision_hardware: VisionHardware,
    thumbs: ImageService,
    previews: ImageService,
    thumb_textures: HashMap<u64, egui::TextureHandle>,
    thumb_ready: VecDeque<(u64, egui::ColorImage)>,
    thumb_pending: HashSet<u64>,
    thumb_failed: HashSet<u64>,
    thumb_lru: VecDeque<u64>,
    preview: PreviewState,
    cache: Arc<CacheStore>,
    meta_cache: Arc<MetadataCache>,
    settings: Settings,
    settings_draft: Settings,
    show_settings: bool,
    watermark_dialog: Option<WatermarkDialog>,
    pending_open_video: Option<PathBuf>,
    collapsed: HashSet<String>,
    toast: Option<Toast>,
    pending_action: Option<PendingAction>,
    api_registry: Arc<ApiRegistry>,
    api_server: Option<ApiServer>,
    api_error: Option<String>,
    api_dirty: bool,
    api_last_publish: Instant,
    modal_backdrop: Option<egui::TextureHandle>,
    modal_backdrop_source: Arc<Mutex<Option<Arc<egui::ColorImage>>>>,
    modal_backdrop_requested: bool,
    modal_wait_frames: u8,
}

impl ExifStudioApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        initial: Option<PathBuf>,
        open_stats: bool,
    ) -> Self {
        install_fonts(&cc.egui_ctx);
        widgets::app_theme(&cc.egui_ctx);

        let settings = Settings::load();
        let cache_root = settings.cache_dir();
        let cache = Arc::new(CacheStore::new(
            cache_root.clone(),
            settings.cache_limit_bytes(),
        ));
        let meta_cache = Arc::new(MetadataCache::load(metadata_cache_path(&cache_root)));
        let workers = image_worker_count();
        let settings_draft = settings.clone();
        let ratings = RatingCatalog::load();
        let watermark_index = WatermarkIndex::load();
        let vision_cache = Arc::new(VisionCache::load(vision_cache_path(&cache_root)));
        let vision_hardware = detect_vision_hardware(cc);
        let mut app = Self {
            tabs: Vec::new(),
            active: None,
            next_tab_id: 1,
            start_in_stats: open_stats,
            folder_picker: None,
            detail_loader: DetailsService::new(),
            ratings,
            watermark_index,
            vision_cache,
            vision_hardware,
            thumbs: ImageService::new("exif-thumbnails", workers, cache.clone()),
            previews: ImageService::new("exif-preview", 1, cache.clone()),
            thumb_textures: HashMap::new(),
            thumb_ready: VecDeque::new(),
            thumb_pending: HashSet::new(),
            thumb_failed: HashSet::new(),
            thumb_lru: VecDeque::new(),
            preview: PreviewState::default(),
            cache,
            meta_cache,
            settings,
            settings_draft,
            show_settings: false,
            watermark_dialog: None,
            pending_open_video: None,
            collapsed: default_collapsed_groups(),
            toast: None,
            pending_action: None,
            api_registry: Arc::new(ApiRegistry::default()),
            api_server: None,
            api_error: None,
            api_dirty: false,
            api_last_publish: Instant::now(),
            modal_backdrop: None,
            modal_backdrop_source: Arc::new(Mutex::new(None)),
            modal_backdrop_requested: false,
            modal_wait_frames: 0,
        };

        if let Some(path) = initial {
            app.open_path(path);
        }
        if app.settings.api_enabled {
            if app.settings.api_token.is_empty() {
                app.settings.ensure_api_token();
                app.settings.save();
            }
            if let Err(error) = app.restart_api() {
                app.set_toast(ToastKind::Error, format!("本地 API 启动失败：{error}"));
            }
        }
        app.settings_draft = app.settings.clone();
        app
    }

    fn open_path(&mut self, path: PathBuf) {
        let path = std::path::absolute(&path).unwrap_or(path);
        let extension = path
            .extension()
            .map(|value| value.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if crate::model::kind_for_extension(&extension).is_some() {
            let parent = path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            self.open_folder(parent, Some(path));
        } else {
            self.open_folder(path, None);
        }
    }

    fn open_folder(&mut self, folder: PathBuf, select: Option<PathBuf>) {
        // Do not touch the filesystem here: is_dir/canonicalize can block for
        // seconds on an SMB share. The scan thread reports unreachable paths.
        let canonical = std::path::absolute(&folder).unwrap_or_else(|_| folder.clone());
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| same_folder(&tab.canonical, &canonical))
        {
            self.active = Some(index);
            if self.start_in_stats {
                self.tabs[index].view = ViewTab::Stats;
                self.tabs[index].stats_dirty = true;
                self.start_in_stats = false;
            }
            if let Some(path) = select {
                self.request_action_at(index, TabAction::Select(path));
            } else {
                let name = folder_title(&folder);
                self.set_toast(ToastKind::Info, format!("{name} 已在标签页中打开"));
            }
            return;
        }

        let id = self.next_tab_id;
        self.next_tab_id = self.next_tab_id.saturating_add(1);
        let mut tab = FolderTab::new(id, folder, canonical);
        if self.start_in_stats {
            tab.view = ViewTab::Stats;
            self.start_in_stats = false;
        }
        self.tabs.push(tab);
        let index = self.tabs.len() - 1;
        self.active = Some(index);
        self.scan_tab_at(index, select);
    }

    fn choose_folder(&mut self, ctx: &egui::Context) {
        if self.folder_picker.is_some() {
            self.set_toast(ToastKind::Info, "文件夹选择窗口已打开");
            return;
        }
        let mut dialog = rfd::AsyncFileDialog::new().set_title("打开照片 / 视频文件夹");
        if let Some(folder) = self.active.and_then(|index| self.tabs.get(index))
            && folder.folder.is_dir()
        {
            dialog = dialog.set_directory(&folder.folder);
        }
        self.folder_picker = Some(Box::pin(dialog.pick_folders()));
        ctx.request_repaint();
    }

    fn poll_folder_picker(&mut self, ctx: &egui::Context) {
        let Some(future) = self.folder_picker.as_mut() else {
            return;
        };
        let waker = Waker::noop();
        let mut task = TaskContext::from_waker(waker);
        match future.as_mut().poll(&mut task) {
            Poll::Ready(result) => {
                self.folder_picker = None;
                if let Some(handles) = result {
                    for handle in handles {
                        self.open_folder(handle.path().to_path_buf(), None);
                    }
                }
                ctx.request_repaint();
            }
            Poll::Pending => ctx.request_repaint_after(Duration::from_millis(100)),
        }
    }

    fn scan_tab_at(&mut self, index: usize, select: Option<PathBuf>) {
        let _ = self.with_tab_mut(index, |app, is_active, tab| {
            app.scan_tab(tab, is_active, select);
        });
    }

    fn scan_tab(&mut self, tab: &mut FolderTab, is_active: bool, select: Option<PathBuf>) {
        tab.scan = None;
        tab.entries.clear();
        tab.filtered.clear();
        tab.filter_dirty = true;
        tab.folder_filter = None;
        tab.folder_groups_expanded = false;
        tab.selected = None;
        tab.details = None;
        tab.details_request = None;
        tab.detail_tab = DetailTab::Info;
        tab.vision = None;
        tab.vision_report = None;
        tab.vision_progress = None;
        tab.vision_error = None;
        tab.vision_attempted = false;
        tab.stats = None;
        tab.stats_dirty = true;
        tab.scanning = true;
        tab.scan_queued = false;
        tab.scan_total = 0;
        tab.scan_done = 0;
        tab.scan_discovered = 0;
        tab.image_count = 0;
        tab.total_bytes = 0;
        tab.favorites_dirty = true;
        tab.favorite_count = 0;
        tab.folder_group_cache.clear();
        tab.folder_groups_dirty = false;
        tab.entry_index = Arc::new(HashMap::new());
        tab.entry_index_dirty = true;
        tab.filtered_time_range = None;
        tab.browse_time_cursor = 1.0;
        tab.last_filter_refresh = Instant::now() - SCAN_FILTER_INTERVAL;
        tab.pending_select = select;
        tab.reset_list_scroll = true;
        tab.duplicates_page = 0;
        tab.duplicates_loaded = DUPLICATES_WATERFALL_CHUNK;
        tab.duplicates_start = 0;
        tab.duplicates_revision = tab.duplicates_revision.wrapping_add(1);
        tab.duplicate_order.clear();
        tab.duplicate_order_revision = u64::MAX;
        tab.duplicate_group_times.clear();
        tab.duplicate_group_pixels.clear();
        tab.duplicate_time_range = None;
        tab.duplicates_time_cursor = 0.5;
        tab.duplicates_cursor_anchor = (usize::MAX, usize::MAX);
        tab.duplicates_scroll_top = true;
        tab.duplicates_stick_ready = false;
        tab.duplicates_notice = None;
        tab.duplicates_bounce_at = None;
        tab.vision_start_at = 0.0;
        tab.vision_cache_loaded = false;
        if is_active {
            self.preview = PreviewState::default();
        }

        let active_scans = self.tabs.iter().filter(|item| item.scan.is_some()).count();
        if active_scans >= MAX_CONCURRENT_SCANS {
            tab.scan_queued = true;
        } else {
            self.try_start_scan(tab);
        }
        self.api_dirty = true;
    }

    fn try_start_scan(&mut self, tab: &mut FolderTab) -> bool {
        let cache = self.meta_cache.clone();
        let result = scan::start_scan(tab.folder.clone(), tab.recursive, cache);
        match result {
            Ok(handle) => {
                tab.scan = Some(handle);
                tab.scan_queued = false;
                tab.scanning = true;
                true
            }
            Err(error) => {
                tab.scan = None;
                tab.scan_queued = false;
                tab.scanning = false;
                self.set_toast(ToastKind::Error, error);
                false
            }
        }
    }

    fn launch_queued_scans(&mut self) {
        let mut active_scans = self.tabs.iter().filter(|tab| tab.scan.is_some()).count();
        if active_scans >= MAX_CONCURRENT_SCANS {
            return;
        }
        let queued: Vec<usize> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.scanning && tab.scan_queued && tab.scan.is_none())
            .map(|(index, _)| index)
            .collect();
        for index in queued {
            if active_scans >= MAX_CONCURRENT_SCANS {
                break;
            }
            let started = self
                .with_tab_mut(index, |app, _is_active, tab| app.try_start_scan(tab))
                .unwrap_or(false);
            if started {
                active_scans += 1;
                self.api_dirty = true;
            }
        }
    }

    fn with_tab_mut<R>(
        &mut self,
        index: usize,
        action: impl FnOnce(&mut Self, bool, &mut FolderTab) -> R,
    ) -> Option<R> {
        if index >= self.tabs.len() {
            return None;
        }
        let is_active = self.active == Some(index);
        let mut tab = std::mem::replace(&mut self.tabs[index], FolderTab::placeholder());
        let result = action(self, is_active, &mut tab);
        self.tabs[index] = tab;
        Some(result)
    }

    fn request_action_at(&mut self, index: usize, action: TabAction) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if tab.edit_is_dirty() {
            self.pending_action = Some(PendingAction {
                tab_id: tab.id,
                action,
            });
        } else {
            self.perform_action_at(index, action);
        }
    }

    fn perform_action_at(&mut self, index: usize, action: TabAction) {
        match action {
            TabAction::Close => self.close_tab(index),
            TabAction::Rescan => {
                let _ = self.with_tab_mut(index, |app, is_active, tab| {
                    let select = tab.selected.clone();
                    app.scan_tab(tab, is_active, select);
                });
            }
            TabAction::Select(path) => {
                let _ = self.with_tab_mut(index, |app, is_active, tab| {
                    app.load_selection(tab, &path, is_active);
                });
            }
        }
    }

    fn perform_pending_action(&mut self) {
        let Some(pending) = self.pending_action.take() else {
            return;
        };
        let Some(index) = self.tabs.iter().position(|tab| tab.id == pending.tab_id) else {
            return;
        };
        self.perform_action_at(index, pending.action);
    }

    fn activate_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = Some(index);
        }
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = None;
            self.preview = PreviewState::default();
        } else if self.active == Some(index) {
            let next = index.min(self.tabs.len() - 1);
            self.active = Some(next);
            self.preview = PreviewState::default();
        } else if let Some(active) = self.active
            && active > index
        {
            self.active = Some(active - 1);
        }
        self.api_dirty = true;
        self.launch_queued_scans();
    }

    fn request_close_tab(&mut self, index: usize) {
        self.request_action_at(index, TabAction::Close);
    }

    fn load_selection(&mut self, tab: &mut FolderTab, path: &Path, is_active: bool) {
        if tab.selected.as_deref() == Some(path) {
            return;
        }
        if tab.edit_is_dirty() {
            self.pending_action = Some(PendingAction {
                tab_id: tab.id,
                action: TabAction::Select(path.to_path_buf()),
            });
            return;
        }
        let Some(entry) = tab.entries.iter().find(|entry| entry.path == path).cloned() else {
            return;
        };
        if (entry.kind == MediaKind::Video || !is_writable_extension(&entry.ext))
            && tab.detail_tab == DetailTab::Edit
        {
            tab.detail_tab = DetailTab::Info;
        }
        tab.selected = Some(path.to_path_buf());
        self.ensure_details(tab, &entry);
        if is_active {
            self.request_preview(&entry);
        }
    }

    fn ensure_details(&mut self, tab: &mut FolderTab, entry: &MediaEntry) {
        if (entry.kind == MediaKind::Video || !is_writable_extension(&entry.ext))
            && tab.detail_tab == DetailTab::Edit
        {
            tab.detail_tab = DetailTab::Info;
        }
        if tab
            .details
            .as_ref()
            .is_some_and(|details| details.path == entry.path)
            || tab.details_request.as_ref() == Some(&entry.path)
        {
            return;
        }
        tab.details = None;
        if entry.kind == MediaKind::Video {
            tab.details_request = None;
            tab.details = Some(details::build_details(entry));
            return;
        }
        tab.details_request = Some(entry.path.clone());
        self.detail_loader.request(tab.id, entry.clone());
    }

    fn poll_details(&mut self, ctx: &egui::Context) {
        for result in self.detail_loader.poll() {
            let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == result.tab_id) else {
                continue;
            };
            if tab.details_request.as_ref() != Some(&result.path) {
                continue;
            }
            tab.details_request = None;
            if tab.selected.as_ref() == Some(&result.path) {
                tab.details = Some(result.details);
            }
            ctx.request_repaint();
        }
    }

    fn start_vision(&mut self, tab: &mut FolderTab) {
        if tab.vision.is_some() {
            return;
        }
        tab.vision_error = None;
        tab.vision_progress = None;
        let analyzable = tab
            .entries
            .iter()
            .filter(|entry| {
                entry.kind == MediaKind::Image && vision::is_visual_extension(&entry.ext)
            })
            .count();
        if analyzable == 0 {
            tab.vision_attempted = true;
            tab.vision_error =
                Some("没有可分析的图片（支持 JPEG、PNG、TIFF、WebP、BMP、GIF）".to_string());
            return;
        }
        tab.vision_attempted = true;
        match vision::start_entries(&tab.entries, self.vision_cache.clone()) {
            Some(handle) => {
                tab.vision_progress = Some((0, analyzable));
                tab.vision = Some(handle);
            }
            None => {
                tab.vision_error = Some("没有可分析的图片".to_string());
            }
        }
    }

    fn restart_vision(&mut self, tab: &mut FolderTab) {
        tab.vision = None;
        tab.vision_report = None;
        tab.vision_progress = None;
        tab.vision_error = None;
        tab.vision_attempted = false;
        tab.favorites_dirty = true;
        tab.touch_duplicates();
        tab.vision_start_at = 0.0;
    }

    fn cancel_vision(&mut self, tab: &mut FolderTab) {
        tab.vision = None;
        tab.vision_progress = None;
        tab.vision_report = None;
        tab.vision_error = Some("视觉分析已取消，可随时重新分析".to_string());
        tab.vision_attempted = true;
        tab.favorites_dirty = true;
        tab.touch_duplicates();
    }

    fn poll_vision(&mut self, ctx: &egui::Context) {
        let mut changed = false;
        for tab in &mut self.tabs {
            let (messages, disconnected) = {
                let Some(handle) = tab.vision.as_ref() else {
                    continue;
                };
                let mut messages = Vec::new();
                let mut disconnected = false;
                loop {
                    match handle.rx.try_recv() {
                        Ok(message) => messages.push(message),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
                (messages, disconnected)
            };
            if messages.is_empty() && !disconnected {
                continue;
            }
            changed = true;
            let mut finished = false;
            for message in messages {
                match message {
                    VisionMsg::Progress { done, total } => {
                        tab.vision_progress = Some((done, total));
                    }
                    VisionMsg::Finished(report) => {
                        tab.vision_progress = None;
                        let group_count = report.groups.len();
                        tab.vision_report = Some(report);
                        tab.favorites_dirty = true;
                        tab.touch_duplicates();
                        tab.duplicates_notice = Some((
                            format!("分析完成 · 共 {group_count} 个相似组"),
                            Instant::now(),
                        ));
                        tab.filter_dirty = true;
                        finished = true;
                    }
                    VisionMsg::Failed(error) => {
                        tab.vision_progress = None;
                        tab.vision_error = Some(error);
                        finished = true;
                    }
                }
            }
            if finished || disconnected {
                tab.vision = None;
                if disconnected && tab.vision_report.is_none() {
                    tab.vision_error = Some("视觉分析线程意外结束".to_string());
                }
            }
        }
        if changed {
            ctx.request_repaint();
        }
    }

    fn request_preview(&mut self, entry: &MediaEntry) {
        let key = entry_key(entry, PREVIEW_VARIANT);
        self.preview = PreviewState {
            path: Some(entry.path.clone()),
            key,
            ..Default::default()
        };
        if entry.kind == MediaKind::Video {
            if !crate::model::is_video_extension(&entry.ext) {
                self.preview.error =
                    Some("此视频容器暂不支持生成封面，仍可查看容器元数据。".to_string());
                return;
            }
            self.preview.loading = true;
            self.previews.request(ImageJob {
                path: entry.path.clone(),
                max_size: PREVIEW_MAX,
                orientation: 1,
                cache_key: key,
            });
            return;
        }
        if !is_previewable(&entry.ext) {
            self.preview.error = Some("此格式暂不支持内置预览，但 EXIF 信息仍可查看。".to_string());
            return;
        }
        let orientation = entry
            .image
            .as_ref()
            .map(|info| info.orientation)
            .unwrap_or(1);
        self.preview.loading = true;
        self.previews.request(ImageJob {
            path: entry.path.clone(),
            max_size: PREVIEW_MAX,
            orientation,
            cache_key: key,
        });
    }

    fn save_selected(&mut self, tab: &mut FolderTab) {
        let Some(details) = tab.details.as_ref() else {
            return;
        };
        let Some(fields) = details.editable.clone() else {
            return;
        };
        let path = details.path.clone();
        let outcome = match exif_io::save_edits(&path, &fields, self.settings.backup) {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Some(details) = tab.details.as_mut() {
                    details.write_error = Some(error.clone());
                }
                self.set_toast(ToastKind::Error, error);
                return;
            }
        };
        let backup_name = outcome
            .backup
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_string());

        let old_entry = tab.entries.iter().find(|entry| entry.path == path).cloned();
        self.meta_cache.forget(&path);
        let new_entry = scan::load_entry(&path, &self.meta_cache);
        self.meta_cache.save();
        if let Some(old_entry) = &old_entry {
            self.invalidate_entry_visuals(old_entry);
        }
        if let Some(index) = tab.entries.iter().position(|entry| entry.path == path) {
            tab.entries[index] = new_entry.clone();
        }
        if let Some(old_entry) = &old_entry {
            tab.total_bytes = tab
                .total_bytes
                .saturating_sub(old_entry.size)
                .saturating_add(new_entry.size);
        }
        let mut new_details = details::build_details(&new_entry);
        new_details.last_saved = Some(match &backup_name {
            Some(name) => format!("已写入文件 · 备份 {name}"),
            None => "已写入文件".to_string(),
        });
        tab.selected = Some(path);
        tab.details_request = None;
        tab.details = Some(new_details);
        tab.detail_tab = DetailTab::Edit;
        tab.filter_dirty = true;
        tab.stats_dirty = true;
        self.request_preview(&new_entry);
        self.api_dirty = true;

        let message = match backup_name {
            Some(name) => format!("EXIF 已保存，原文件备份为 {name}"),
            None => "EXIF 已保存".to_string(),
        };
        self.set_toast(ToastKind::Success, message);
    }

    fn reload_selected(&mut self, tab: &mut FolderTab) {
        if tab.edit_is_dirty() {
            self.set_toast(
                ToastKind::Info,
                "存在未保存的修改，请先保存或恢复后再重新读取。",
            );
            return;
        }
        let Some(path) = tab.selected.clone() else {
            return;
        };
        let old_entry = tab.entries.iter().find(|entry| entry.path == path).cloned();
        self.meta_cache.forget(&path);
        let new_entry = scan::load_entry(&path, &self.meta_cache);
        if let Some(old_entry) = &old_entry {
            self.invalidate_entry_visuals(old_entry);
        }
        if let Some(index) = tab.entries.iter().position(|entry| entry.path == path) {
            tab.entries[index] = new_entry.clone();
        }
        if let Some(old_entry) = &old_entry {
            tab.total_bytes = tab
                .total_bytes
                .saturating_sub(old_entry.size)
                .saturating_add(new_entry.size);
        }
        tab.details_request = None;
        tab.details = Some(details::build_details(&new_entry));
        tab.filter_dirty = true;
        tab.stats_dirty = true;
        self.request_preview(&new_entry);
        self.api_dirty = true;
        self.set_toast(ToastKind::Info, "已重新读取文件元数据");
    }

    fn invalidate_entry_visuals(&mut self, entry: &MediaEntry) {
        let key = entry_key(entry, THUMB_VARIANT);
        self.thumb_pending.remove(&key);
        self.thumb_failed.remove(&key);
        self.thumb_ready.retain(|(queued, _)| *queued != key);
        if self.thumb_textures.remove(&key).is_some() {
            self.thumb_lru.retain(|item| *item != key);
        }
    }

    fn poll_scans(&mut self, ctx: &egui::Context) {
        let active_id = self
            .active
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.id);
        let mut active_finished: Option<(String, usize, Duration, Option<String>)> = None;
        let mut messages_received = false;
        let mut api_changed = false;
        let ratings = &self.ratings;
        let photo_sort = self.settings.photo_sort;

        for tab in &mut self.tabs {
            let (messages, disconnected) = {
                let Some(handle) = tab.scan.as_ref() else {
                    continue;
                };
                let mut messages = Vec::new();
                let mut disconnected = false;
                loop {
                    if messages.len() >= MAX_SCAN_BATCHES_PER_FRAME {
                        break;
                    }
                    match handle.rx.try_recv() {
                        Ok(message) => messages.push(message),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
                (messages, disconnected)
            };
            if messages.is_empty() && !disconnected {
                continue;
            }
            messages_received = true;

            let mut finished = false;
            let mut failure = None;
            let mut total = 0usize;
            let mut elapsed = Duration::ZERO;
            for message in messages {
                match message {
                    ScanMsg::Discovering { files } => {
                        tab.scan_discovered = files;
                    }
                    ScanMsg::Started { total: count } => {
                        tab.scan_total = count;
                        tab.scan_discovered = count;
                    }
                    ScanMsg::Entries(batch) => {
                        tab.scan_done += batch.len();
                        for entry in &batch {
                            if entry.kind == MediaKind::Image {
                                tab.image_count += 1;
                            }
                            tab.total_bytes = tab.total_bytes.saturating_add(entry.size);
                        }
                        tab.entries.extend(batch);
                        tab.filter_dirty = true;
                        tab.favorites_dirty = true;
                        tab.folder_groups_dirty = true;
                        tab.entry_index_dirty = true;
                        tab.stats_dirty = true;
                        api_changed = true;
                    }
                    ScanMsg::Finished {
                        total: count,
                        elapsed: time,
                    } => {
                        finished = true;
                        total = count;
                        elapsed = time;
                        tab.scan_done = count;
                        tab.scan_discovered = count;
                        tab.scanning = false;
                    }
                    ScanMsg::Failed(error) => {
                        failure = Some(error);
                        tab.scanning = false;
                    }
                }
            }
            if disconnected && !finished && failure.is_none() {
                failure = Some("扫描线程意外结束".to_string());
            }
            if finished || failure.is_some() {
                tab.scan = None;
                tab.scan_queued = false;
                tab.scanning = false;
                tab.favorites_dirty = true;
                tab.folder_groups_dirty = true;
                tab.entry_index_dirty = true;
                refresh_filter(tab, ratings, photo_sort);
                tab.stats_dirty = true;
                if let Some(path) = tab.pending_select.take()
                    && tab.entries.iter().any(|entry| entry.path == path)
                {
                    tab.selected = Some(path);
                }
                if tab.selected.is_none() {
                    tab.selected = tab
                        .filtered
                        .first()
                        .map(|index| tab.entries[*index].path.clone());
                }
                tab.details = None;
                if Some(tab.id) == active_id {
                    let name = folder_title(&tab.folder);
                    active_finished = Some((name, total, elapsed, failure));
                }
                api_changed = true;
            }
        }

        if api_changed {
            self.api_dirty = true;
        }
        if messages_received {
            self.launch_queued_scans();
            if let Some((name, total, elapsed, failure)) = active_finished {
                if let Some(error) = failure {
                    self.set_toast(ToastKind::Error, error);
                } else if total == 0 {
                    self.set_toast(
                        ToastKind::Info,
                        format!("{name} 中没有找到支持的图片或视频"),
                    );
                } else {
                    self.set_toast(
                        ToastKind::Success,
                        format!(
                            "{name} 扫描完成 · {total} 个文件 · {:.1} 秒",
                            elapsed.as_secs_f64()
                        ),
                    );
                }
            }
            ctx.request_repaint();
        }
    }

    fn poll_images(&mut self, ctx: &egui::Context) {
        for result in self.thumbs.poll() {
            if !self.thumb_pending.remove(&result.cache_key) {
                continue;
            }
            match result.result {
                Ok(color) => {
                    self.thumb_failed.remove(&result.cache_key);
                    self.thumb_ready.push_back((result.cache_key, color));
                }
                Err(_) => {
                    self.thumb_failed.insert(result.cache_key);
                }
            }
        }
        // Uploading dozens of textures in one frame is a visible hitch while
        // scrolling a network share. Spread them across frames instead.
        for _ in 0..MAX_TEXTURE_UPLOADS_PER_FRAME {
            let Some((key, color)) = self.thumb_ready.pop_front() else {
                break;
            };
            let texture = ctx.load_texture(
                format!("thumb-{key:016x}"),
                color,
                egui::TextureOptions::LINEAR,
            );
            self.insert_thumb(key, texture);
        }

        for result in self.previews.poll() {
            if result.max_size != PREVIEW_MAX
                || self.preview.key != result.cache_key
                || self.preview.path.as_ref() != Some(&result.path)
            {
                continue;
            }
            self.preview.loading = false;
            match result.result {
                Ok(color) => {
                    self.preview.texture = Some(ctx.load_texture(
                        format!("preview-{:016x}", result.cache_key),
                        color,
                        egui::TextureOptions::LINEAR,
                    ));
                    self.preview.error = None;
                }
                Err(error) => {
                    self.preview.texture = None;
                    self.preview.error = Some(error);
                }
            }
        }
        self.enforce_thumb_budget();
    }

    fn insert_thumb(&mut self, key: u64, texture: egui::TextureHandle) {
        self.thumb_textures.insert(key, texture);
        self.thumb_lru.retain(|item| *item != key);
        self.thumb_lru.push_back(key);
    }

    fn touch_thumb(&mut self, key: u64) {
        if !self.thumb_textures.contains_key(&key) {
            return;
        }
        self.thumb_lru.retain(|item| *item != key);
        self.thumb_lru.push_back(key);
    }

    fn enforce_thumb_budget(&mut self) {
        while self.thumb_textures.len() > MAX_THUMB_TEXTURES {
            let Some(key) = self.thumb_lru.pop_front() else {
                break;
            };
            self.thumb_textures.remove(&key);
        }
    }

    fn set_toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toast = Some(Toast {
            kind,
            text: text.into(),
            created: Instant::now(),
        });
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let open = ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::O));
        if open {
            self.choose_folder(ctx);
            return;
        }
        let Some(index) = self.active else {
            return;
        };
        if self.pending_action.is_some() {
            return;
        }
        let rescan = ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::F5));
        let close =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::W));
        let save = ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::S));
        let next_tab =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::Tab));
        let views = [
            (egui::Key::Num1, ViewTab::Browse),
            (egui::Key::Num2, ViewTab::Stats),
            (egui::Key::Num3, ViewTab::Duplicates),
        ];
        let mut requested_view = None;
        for (key, view) in views {
            if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, key)) {
                requested_view = Some(view);
            }
        }

        if let Some(view) = requested_view
            && let Some(tab) = self.tabs.get_mut(index)
            && tab.view != view
        {
            tab.view = view;
            if view == ViewTab::Stats {
                tab.stats_dirty = true;
            } else if view == ViewTab::Duplicates {
                tab.duplicates_page = 0;
                tab.duplicates_loaded = DUPLICATES_WATERFALL_CHUNK;
                tab.duplicates_scroll_top = true;
                tab.vision_start_at = ctx.input(|input| input.time) + 0.24;
            }
        }

        if let Some(tab) = self.tabs.get_mut(index)
            && tab.view == ViewTab::Duplicates
            && self.settings.duplicates_layout == DuplicatesLayout::Paged
        {
            let total = duplicates_total_pages(
                tab.vision_report
                    .as_ref()
                    .map_or(0, |report| report.groups.len()),
            );
            if total > 1 {
                let page = tab.duplicates_page.min(total - 1);
                let mouse_previous =
                    ctx.input(|input| input.pointer.button_clicked(egui::PointerButton::Extra1));
                let mouse_next =
                    ctx.input(|input| input.pointer.button_clicked(egui::PointerButton::Extra2));
                let typing = ctx.egui_wants_keyboard_input();
                let key_previous = !typing
                    && ctx.input_mut(|input| {
                        input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp)
                    });
                let key_next = !typing
                    && ctx.input_mut(|input| {
                        input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown)
                    });
                if mouse_previous || key_previous {
                    go_duplicates_page(tab, page.saturating_sub(1), total);
                } else if mouse_next || key_next {
                    go_duplicates_page(tab, page + 1, total);
                }
            }
        }

        if rescan {
            self.request_action_at(index, TabAction::Rescan);
        }
        if close {
            self.request_close_tab(index);
        }
        if next_tab && !self.tabs.is_empty() {
            self.active = Some((index + 1) % self.tabs.len());
        }
        if save {
            let _ = self.with_tab_mut(index, |app, _is_active, tab| {
                if tab.view == ViewTab::Browse && tab.edit_is_dirty() {
                    app.save_selected(tab);
                }
            });
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|input| input.raw.dropped_files.clone());
        for file in dropped {
            let path = file.path().to_path_buf();
            if path.is_dir() {
                self.open_folder(path, None);
            } else if path.is_file() {
                let parent = path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."));
                self.open_folder(parent, Some(path));
            }
        }
    }

    fn open_settings(&mut self) {
        self.settings_draft = self.settings.clone();
        self.show_settings = true;
    }

    fn modal_open(&self) -> bool {
        self.show_settings
            || self.watermark_dialog.is_some()
            || self.pending_action.is_some()
            || self.pending_open_video.is_some()
    }

    fn prepare_modal(&mut self, ctx: &egui::Context) {
        let image = self
            .modal_backdrop_source
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(image) = image {
            self.modal_backdrop = Some(blurred_backdrop(ctx, &image));
            self.modal_wait_frames = 0;
        }
        if !self.modal_open() {
            self.modal_backdrop = None;
            self.modal_backdrop_requested = false;
            self.modal_wait_frames = 0;
            if let Ok(mut slot) = self.modal_backdrop_source.lock() {
                *slot = None;
            }
        }
    }

    /// Ask for a screenshot of the unobscured frame and wait until it can be
    /// blurred. The dialog is skipped for a frame or two while this happens, so
    /// the captured image never contains the dialog itself.
    fn modal_ready(&mut self, ctx: &egui::Context) -> bool {
        if !self.modal_open() {
            return false;
        }
        if self.modal_backdrop.is_some() {
            return true;
        }
        if !self.modal_backdrop_requested {
            self.modal_backdrop_requested = true;
            self.modal_wait_frames = 3;
            let slot = self.modal_backdrop_source.clone();
            let repaint = ctx.clone();
            ctx.request_screenshot(move |image| {
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(image);
                }
                repaint.request_repaint();
            });
            return false;
        }
        if self.modal_wait_frames > 0 {
            self.modal_wait_frames -= 1;
            return false;
        }
        true
    }

    fn draw_modal_backdrop(&self, ctx: &egui::Context) {
        if !self.modal_open() {
            return;
        }
        if self.modal_backdrop.is_none()
            && self.modal_backdrop_requested
            && self.modal_wait_frames > 0
        {
            // The screenshot used for the frosted glass must not contain our
            // own dimming layer, so stay out of the frame while it is captured.
            return;
        }
        let screen = ctx.content_rect();
        egui::Area::new(Id::unique("modal-backdrop"))
            .order(Order::Middle)
            .fixed_pos(screen.min)
            .interactable(true)
            .show(ctx, |ui| {
                ui.set_clip_rect(screen);
                let (rect, _) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
                let painter = ui.painter().clone();
                painter.rect_filled(rect, 0, Color32::from_rgb(8, 10, 14));
                if let Some(texture) = &self.modal_backdrop {
                    painter.image(
                        texture.id(),
                        rect,
                        Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                        Color32::from_rgba_unmultiplied(235, 238, 244, 255),
                    );
                }
                painter.rect_filled(rect, 0, Color32::from_rgba_unmultiplied(9, 11, 15, 148));
                let glow = Rect::from_center_size(
                    rect.center(),
                    Vec2::new(
                        (rect.width() * 0.72).max(320.0),
                        (rect.height() * 0.8).max(280.0),
                    ),
                );
                painter.rect_filled(glow, 40, Color32::from_rgba_unmultiplied(20, 24, 31, 42));
            });
    }

    fn clear_caches(&mut self) {
        self.cache.clear_files();
        self.meta_cache.clear();
        self.vision_cache.clear();
        for tab in &mut self.tabs {
            tab.vision = None;
            tab.vision_report = None;
            tab.vision_progress = None;
            tab.vision_error = Some("视觉缓存已清理，点击“重新分析”可重新计算".to_string());
            tab.vision_attempted = true;
            tab.touch_duplicates();
        }
        self.thumb_pending.clear();
        self.thumb_failed.clear();
        self.thumb_ready.clear();
        self.thumb_textures.clear();
        self.thumb_lru.clear();
        self.preview = PreviewState::default();
    }

    fn recreate_image_services(&mut self) {
        self.thumbs =
            ImageService::new("exif-thumbnails", image_worker_count(), self.cache.clone());
        self.previews = ImageService::new("exif-preview", 1, self.cache.clone());
        self.thumb_pending.clear();
        self.thumb_failed.clear();
        self.thumb_ready.clear();
        self.preview = PreviewState::default();
    }

    fn restart_api(&mut self) -> Result<(), String> {
        self.api_server = None;
        self.api_error = None;
        if !self.settings.api_enabled {
            return Ok(());
        }
        if self.settings.api_token.is_empty() {
            self.settings.ensure_api_token();
            self.settings.save();
        }
        match ApiServer::start(
            self.settings.api_port,
            self.settings.api_token.clone(),
            self.api_registry.clone(),
        ) {
            Ok(server) => {
                self.api_server = Some(server);
                self.api_dirty = true;
                self.publish_api(true);
                Ok(())
            }
            Err(error) => {
                self.api_error = Some(error.clone());
                Err(error)
            }
        }
    }

    fn api_folders(&self) -> Vec<ApiFolder> {
        self.tabs
            .iter()
            .map(|tab| ApiFolder {
                id: tab.id,
                path: tab.folder.to_string_lossy().into_owned(),
                scanning: tab.scanning,
                files: tab.entries.iter().map(ApiFile::from_entry).collect(),
            })
            .collect()
    }

    fn publish_api(&mut self, force: bool) {
        if self.api_server.is_none() {
            return;
        }
        if !force && (!self.api_dirty || self.api_last_publish.elapsed() < API_PUBLISH_INTERVAL) {
            return;
        }
        self.api_registry.publish(self.api_folders());
        self.api_dirty = false;
        self.api_last_publish = Instant::now();
    }

    fn apply_settings(&mut self) {
        let mut next = self.settings_draft.clone();
        next.normalize();
        if next.api_enabled {
            next.ensure_api_token();
        }

        let cache_dir_changed = next.cache_dir != self.settings.cache_dir;
        let cache_limit_changed = next.cache_limit_bytes() != self.settings.cache_limit_bytes();
        if cache_dir_changed
            && (self.tabs.iter().any(|tab| tab.scanning)
                || self.tabs.iter().any(|tab| tab.vision.is_some()))
        {
            self.set_toast(
                ToastKind::Error,
                "仍有扫描或视觉分析在进行，请等待结束后再更改缓存目录。",
            );
            return;
        }
        if cache_dir_changed {
            let root = next.cache_dir();
            if let Err(error) = std::fs::create_dir_all(&root) {
                self.set_toast(ToastKind::Error, format!("无法创建缓存目录：{error}"));
                return;
            }
            self.meta_cache.save();
            self.vision_cache.save();
            self.cache = Arc::new(CacheStore::new(root.clone(), next.cache_limit_bytes()));
            self.meta_cache = Arc::new(MetadataCache::load(metadata_cache_path(&root)));
            self.vision_cache = Arc::new(VisionCache::load(vision_cache_path(&root)));
            self.recreate_image_services();
        } else if cache_limit_changed {
            self.cache.set_limit_bytes(next.cache_limit_bytes());
        }

        let previous = self.settings.clone();
        let api_changed = previous.api_enabled != next.api_enabled
            || previous.api_port != next.api_port
            || previous.api_token != next.api_token;
        let api_retry = next.api_enabled && self.api_server.is_none();

        self.settings = next.clone();
        self.settings_draft = next;
        self.settings.save();
        self.api_dirty = true;
        if previous.duplicates_layout != self.settings.duplicates_layout {
            for tab in &mut self.tabs {
                tab.duplicates_page = 0;
                tab.duplicates_start = 0;
                tab.duplicates_loaded = DUPLICATES_WATERFALL_CHUNK;
                tab.duplicates_scroll_top = true;
                tab.duplicates_notice = Some((
                    format!("已切换为{}", self.settings.duplicates_layout.label()),
                    Instant::now(),
                ));
            }
        }
        if previous.photo_sort != self.settings.photo_sort {
            for tab in &mut self.tabs {
                tab.filter_dirty = true;
                tab.reset_list_scroll = true;
            }
        }
        if previous.duplicates_sort != self.settings.duplicates_sort {
            for tab in &mut self.tabs {
                tab.touch_duplicates();
            }
        }

        let mut api_failure = None;
        if (api_changed || api_retry)
            && let Err(error) = self.restart_api()
        {
            api_failure = Some(error);
        }
        self.publish_api(true);

        if let Some(error) = api_failure {
            self.show_settings = true;
            self.set_toast(ToastKind::Error, format!("本地 API 启动失败：{error}"));
            return;
        }
        self.show_settings = false;
        let message = if cache_dir_changed {
            "设置已保存 · 缓存目录已切换，旧目录不会自动删除"
        } else {
            "设置已保存"
        };
        self.set_toast(ToastKind::Success, message);
    }

    fn show_top_bar(&mut self, root: &mut Ui) {
        let active_view = self
            .active
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.view);
        let mut open_folder = false;
        let mut open_settings = false;
        let mut next_view = None;

        egui::Panel::top("app-top-bar")
            .exact_size(58.0)
            .frame(
                Frame::new()
                    .fill(widgets::BG)
                    .inner_margin(Margin::symmetric(18, 8)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    let (logo_rect, _) =
                        ui.allocate_exact_size(Vec2::new(32.0, 32.0), Sense::hover());
                    ui.painter().rect_filled(logo_rect, 9, widgets::ACCENT);
                    ui.painter().text(
                        logo_rect.center(),
                        Align2::CENTER_CENTER,
                        "E",
                        FontId::proportional(19.0),
                        widgets::BG,
                    );
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.add_space(1.0);
                        ui.label(
                            RichText::new("Exif Studio")
                                .size(18.0)
                                .strong()
                                .color(widgets::TEXT),
                        );
                        ui.label(
                            RichText::new("影像元数据工作台")
                                .size(10.5)
                                .color(widgets::MUTED),
                        );
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                Button::new(
                                    RichText::new("打开文件夹").size(13.5).color(widgets::BG),
                                )
                                .fill(widgets::ACCENT),
                            )
                            .clicked()
                        {
                            open_folder = true;
                        }
                        if tab_button(ui, "设置", self.show_settings).clicked() {
                            open_settings = true;
                        }
                        if let Some(view) = active_view
                            && let Some(next) = view_switch(ui, view)
                        {
                            next_view = Some(next);
                        }
                    });
                });
            });

        if open_folder {
            self.choose_folder(root.ctx());
        }
        if open_settings {
            self.open_settings();
        }
        if let (Some(index), Some(view)) = (self.active, next_view)
            && let Some(tab) = self.tabs.get_mut(index)
            && tab.view != view
        {
            tab.view = view;
            if view == ViewTab::Stats {
                tab.stats_dirty = true;
            } else if view == ViewTab::Duplicates {
                tab.duplicates_page = 0;
                tab.vision_start_at = root.ctx().input(|input| input.time) + 0.24;
            }
        }
    }

    fn show_tab_strip(&mut self, root: &mut Ui) {
        let mut activate = None;
        let mut close = None;
        let mut new_folder = false;
        egui::Panel::top("folder-tab-strip")
            .exact_size(46.0)
            .frame(
                Frame::new()
                    .fill(widgets::BG)
                    .inner_margin(Margin::symmetric(12, 4)),
            )
            .show(root, |ui| {
                ScrollArea::horizontal()
                    .id_salt("folder-tabs")
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for (index, tab) in self.tabs.iter().enumerate() {
                                let active = self.active == Some(index);
                                let (select, closes) = tab_chip(ui, tab, active);
                                if select {
                                    activate = Some(index);
                                }
                                if closes {
                                    close = Some(index);
                                }
                            }
                            if ui
                                .add(
                                    Button::new(
                                        RichText::new("+").size(19.0).color(widgets::MUTED),
                                    )
                                    .frame(false)
                                    .min_size(Vec2::new(32.0, 30.0)),
                                )
                                .hover_tip("打开另一个文件夹")
                                .clicked()
                            {
                                new_folder = true;
                            }
                            if self.tabs.is_empty() {
                                ui.label(
                                    RichText::new("每个文件夹会作为一个标签页打开")
                                        .size(11.0)
                                        .color(widgets::MUTED),
                                );
                            }
                        });
                    });
            });

        if let Some(index) = close {
            self.request_close_tab(index);
        } else if let Some(index) = activate {
            self.activate_tab(index);
        }
        if new_folder {
            self.choose_folder(root.ctx());
        }
    }

    fn show_status_bar(&mut self, root: &mut Ui) {
        let left;
        let mut right = String::new();
        let mut scanning = false;
        if let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) {
            if tab.scanning {
                scanning = true;
                left = if tab.scan_queued {
                    "等待扫描资源…".to_string()
                } else if tab.scan_total == 0 {
                    format!("正在发现文件… 已找到 {} 个", tab.scan_discovered)
                } else {
                    format!("正在扫描  {} / {}", tab.scan_done, tab.scan_total)
                };
            } else {
                let images = tab.image_count;
                let videos = tab.entries.len().saturating_sub(images);
                left = format!(
                    "{} 个文件  ·  图片 {images}  ·  视频 {videos}  ·  {}",
                    tab.entries.len(),
                    format_bytes(tab.total_bytes)
                );
            }
            if let Some(entry) = tab.selected_entry() {
                right = widgets::truncate(&entry.name, 54);
            }
        } else {
            left = "就绪 · 打开一个文件夹开始".to_string();
        }
        if let Some(server) = &self.api_server {
            right = format!("{}    本地 API 127.0.0.1:{}", right, server.port);
        } else if self.api_error.is_some() {
            right = format!("{right}    API 未启动");
        }

        egui::Panel::bottom("app-status-bar")
            .exact_size(30.0)
            .frame(
                Frame::new()
                    .fill(widgets::BG)
                    .inner_margin(Margin::symmetric(18, 6)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    if scanning {
                        ui.spinner();
                    }
                    ui.label(RichText::new(left).size(11.5).color(widgets::MUTED));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if !right.trim().is_empty() {
                            ui.label(RichText::new(right).size(11.5).color(widgets::MUTED));
                        }
                    });
                });
            });
    }

    fn show_welcome(&mut self, ui: &mut Ui) {
        let mut open_folder = false;
        let height = ui.available_height();
        ui.add_space((height * 0.1).min(86.0));
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(84.0), Sense::hover());
            let center = rect.center();
            ui.painter()
                .circle_stroke(center, 36.0, Stroke::new(2.0, widgets::ACCENT));
            ui.painter()
                .circle_stroke(center, 22.0, Stroke::new(1.2, widgets::BORDER));
            ui.painter()
                .circle_filled(center, 8.0, widgets::accent_soft(220));
            for index in 0..6 {
                let angle = index as f32 / 6.0 * std::f32::consts::TAU;
                let start = center + Vec2::angled(angle) * 15.0;
                let end = center + Vec2::angled(angle) * 34.0;
                ui.painter()
                    .line_segment([start, end], Stroke::new(1.4, widgets::accent_soft(130)));
            }
            ui.add_space(22.0);
            ui.label(
                RichText::new("让每一张照片都有据可查")
                    .size(28.0)
                    .strong()
                    .color(widgets::TEXT),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new("查看、编辑与统计照片和视频的元数据")
                    .size(14.0)
                    .color(widgets::MUTED),
            );
            ui.add_space(24.0);
            if ui
                .add(
                    Button::new(RichText::new("打开文件夹").size(15.0).color(widgets::BG))
                        .fill(widgets::ACCENT)
                        .min_size(Vec2::new(180.0, 42.0)),
                )
                .clicked()
            {
                open_folder = true;
            }
            ui.add_space(10.0);
            ui.label(
                RichText::new("支持一次选择多个文件夹，每个文件夹独立标签页")
                    .size(11.5)
                    .color(widgets::MUTED),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new("或将文件夹或图片直接拖入窗口")
                    .size(11.5)
                    .color(widgets::MUTED),
            );
            ui.add_space(36.0);
            ui.label(
                RichText::new("JPEG  ·  PNG  ·  TIFF  ·  WebP  ·  HEIF  ·  MP4  ·  MOV")
                    .size(11.0)
                    .color(Color32::from_rgb(105, 113, 126)),
            );
        });
        if open_folder {
            self.choose_folder(ui.ctx());
        }
    }

    fn show_workspace_for(&mut self, root: &mut Ui, tab: &mut FolderTab, is_active: bool) {
        self.sync_tab_content(tab, is_active);
        if tab.view == ViewTab::Browse {
            self.show_left_panel(root, tab);
        }
        let view_key = match tab.view {
            ViewTab::Browse => 1u8,
            ViewTab::Stats => 2,
            ViewTab::Duplicates => 3,
        };
        let fade = root.ctx().animate_bool_with_time(
            Id::unique(("view-fade", tab.id, view_key)),
            true,
            0.22,
        );
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(widgets::BG)
                    .inner_margin(Margin::symmetric(22, 18)),
            )
            .show(root, |ui| {
                ui.set_opacity(fade);
                if fade < 1.0 {
                    ui.add_space((1.0 - fade) * 6.0);
                }
                match tab.view {
                    ViewTab::Browse => self.central_browse(ui, tab),
                    ViewTab::Stats => self.central_stats(ui, tab),
                    ViewTab::Duplicates => self.central_duplicates(ui, tab),
                }
            });
    }

    fn sync_tab_content(&mut self, tab: &mut FolderTab, is_active: bool) {
        if tab.filter_dirty
            && (!tab.scanning || tab.last_filter_refresh.elapsed() >= SCAN_FILTER_INTERVAL)
        {
            refresh_filter(tab, &self.ratings, self.settings.photo_sort);
        }
        if !tab.scanning && tab.entry_index_dirty {
            tab.rebuild_entry_index();
        }
        let selection_missing = tab.selected.as_ref().is_some_and(|path| {
            if !tab.entry_index_dirty {
                !tab.entry_index.contains_key(path)
            } else {
                !tab.entries.iter().any(|entry| &entry.path == path)
            }
        });
        if selection_missing {
            tab.selected = None;
            tab.details = None;
            tab.details_request = None;
        }
        if tab.selected.is_none()
            && !tab.scanning
            && let Some(path) = tab
                .filtered
                .first()
                .map(|index| tab.entries[*index].path.clone())
        {
            self.load_selection(tab, &path, is_active);
        }
        if let Some(path) = tab.selected.clone() {
            if let Some(entry) = tab.entries.iter().find(|entry| entry.path == path).cloned() {
                self.ensure_details(tab, &entry);
            } else {
                tab.selected = None;
                tab.details = None;
                tab.details_request = None;
            }
        } else {
            tab.details = None;
            tab.details_request = None;
        }

        if !tab.scanning
            && !tab.vision_cache_loaded
            && !tab.vision_attempted
            && !tab.entries.is_empty()
        {
            tab.vision_cache_loaded = true;
            tab.vision_report = vision::cached_report(&tab.entries, &self.vision_cache);
            if tab.vision_report.is_some() {
                tab.favorites_dirty = true;
                tab.touch_duplicates();
            }
        }

        if is_active {
            if let Some(entry) = tab.selected_entry().cloned() {
                let key = entry_key(&entry, PREVIEW_VARIANT);
                if self.preview.path.as_ref() != Some(&entry.path) || self.preview.key != key {
                    self.request_preview(&entry);
                }
            } else if self.preview.path.is_some() {
                self.preview = PreviewState::default();
            }
        }
    }

    fn show_left_panel(&mut self, root: &mut Ui, tab: &mut FolderTab) {
        let mut clicked: Option<PathBuf> = None;
        let mut star_toggle: Option<PathBuf> = None;
        let mut rescan = false;
        let mut recursive_changed = false;
        let mut zoom_commit = false;
        let mut zoom = self.settings.thumbnail_zoom.clamp(0.0, 1.0);
        let mut needed: Vec<(u64, PathBuf, u16, u32)> = Vec::new();
        let mut visible_keys: HashSet<u64> = HashSet::new();
        let mut photo_sort = self.settings.photo_sort;
        let mut sort_changed = false;
        let auto_preferred = tab
            .vision_report
            .as_ref()
            .map(|report| report.preferred.clone())
            .unwrap_or_default();

        let textures = &self.thumb_textures;
        let pending = &self.thumb_pending;
        let failed = &self.thumb_failed;
        let ratings = &self.ratings;
        if tab.favorites_dirty
            && (!tab.scanning || tab.favorites_built_at.elapsed() >= FAVORITE_REFRESH_INTERVAL)
        {
            tab.favorite_count = tab
                .entries
                .iter()
                .filter(|entry| {
                    ratings.is_favorite(&entry.path, auto_preferred.contains(&entry.path))
                })
                .count();
            tab.favorites_dirty = false;
            tab.favorites_built_at = Instant::now();
        }
        let favorite_count = tab.favorite_count;
        if tab.folder_groups_dirty
            && (!tab.scanning || tab.folder_groups_built_at.elapsed() >= SCAN_CHIP_INTERVAL)
        {
            tab.folder_group_cache = folder_groups(tab);
            tab.folder_groups_dirty = false;
            tab.folder_groups_built_at = Instant::now();
        }

        egui::Panel::left("file-list-panel")
            .resizable(true)
            .default_size(342.0)
            .size_range(290.0..=480.0)
            .frame(
                Frame::new()
                    .fill(widgets::PANEL)
                    .inner_margin(Margin::symmetric(14, 14)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("当前文件夹").size(11.0).color(widgets::MUTED));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(Button::new("刷新").small().frame(false)).clicked() {
                            rescan = true;
                        }
                    });
                });
                let name = folder_title(&tab.folder);
                ui.label(
                    RichText::new(widgets::truncate(&name, 40))
                        .size(15.0)
                        .strong()
                        .color(widgets::TEXT),
                );
                let full_path = tab.folder.display().to_string();
                ui.add(
                    Label::new(
                        RichText::new(widgets::truncate(&full_path, 44))
                            .size(10.5)
                            .color(widgets::MUTED),
                    )
                    .truncate(),
                )
                .hover_tip(full_path);
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let mut recursive = tab.recursive;
                    if ui
                        .checkbox(&mut recursive, RichText::new("包含子文件夹").size(11.5))
                        .changed()
                    {
                        tab.recursive = recursive;
                        recursive_changed = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                Button::new(
                                    RichText::new(format!("★ 优选照片 {favorite_count}"))
                                        .size(10.5)
                                        .color(if tab.favorites_only {
                                            widgets::ACCENT
                                        } else {
                                            widgets::MUTED
                                        }),
                                )
                                .fill(if tab.favorites_only {
                                    widgets::PANEL_SOFT
                                } else {
                                    Color32::TRANSPARENT
                                })
                                .stroke(Stroke::new(
                                    1.0,
                                    if tab.favorites_only {
                                        widgets::ACCENT
                                    } else {
                                        widgets::BORDER
                                    },
                                )),
                            )
                            .hover_tip("只显示已手动星标或自动优选的照片")
                            .clicked()
                        {
                            tab.favorites_only = !tab.favorites_only;
                            tab.filter_dirty = true;
                            tab.reset_list_scroll = true;
                            tab.selected = None;
                            tab.details = None;
                            tab.details_request = None;
                        }
                        if tab.edit_is_dirty() {
                            widgets::pill(ui, "有未保存修改", widgets::ACCENT);
                        }
                    });
                });
                ui.add_space(8.0);

                if ui
                    .add(
                        TextEdit::singleline(&mut tab.search)
                            .hint_text("搜索文件名、相机、镜头…")
                            .desired_width(f32::INFINITY),
                    )
                    .changed()
                {
                    tab.filter_dirty = true;
                }
                ui.add_space(6.0);

                let total = tab.entries.len();
                let images = tab.image_count;
                let videos = total.saturating_sub(images);
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(
                            tab.kind_filter == KindFilter::All,
                            RichText::new(format!("全部 {total}")).size(11.5),
                        )
                        .clicked()
                    {
                        tab.kind_filter = KindFilter::All;
                        tab.filter_dirty = true;
                    }
                    if ui
                        .selectable_label(
                            tab.kind_filter == KindFilter::Images,
                            RichText::new(format!("图片 {images}")).size(11.5),
                        )
                        .clicked()
                    {
                        tab.kind_filter = KindFilter::Images;
                        tab.filter_dirty = true;
                    }
                    if ui
                        .selectable_label(
                            tab.kind_filter == KindFilter::Videos,
                            RichText::new(format!("视频 {videos}")).size(11.5),
                        )
                        .clicked()
                    {
                        tab.kind_filter = KindFilter::Videos;
                        tab.filter_dirty = true;
                    }
                });

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("排序").size(10.5).color(widgets::MUTED));
                    ComboBox::from_id_salt(("photo-sort", tab.id))
                        .selected_text(photo_sort.label())
                        .width(158.0)
                        .show_ui(ui, |ui| {
                            for option in [
                                PhotoSort::Filename,
                                PhotoSort::TakenTime,
                                PhotoSort::Resolution,
                            ] {
                                ui.selectable_value(&mut photo_sort, option, option.label());
                            }
                        });
                });
                sort_changed = photo_sort != self.settings.photo_sort;

                let folder_groups = std::mem::take(&mut tab.folder_group_cache);
                if !folder_groups.is_empty() {
                    ui.add_space(4.0);
                    show_folder_group_chips(ui, tab, &folder_groups);
                }
                tab.folder_group_cache = folder_groups;

                if tab.scanning {
                    ui.add_space(8.0);
                    let fraction = if tab.scan_total > 0 {
                        tab.scan_done as f32 / tab.scan_total as f32
                    } else {
                        0.0
                    };
                    ui.add(
                        ProgressBar::new(fraction)
                            .desired_height(5.0)
                            .fill(widgets::ACCENT)
                            .animate(true),
                    );
                    let progress = if tab.scan_queued {
                        "排队等待扫描，最多同时读取 2 个文件夹".to_string()
                    } else if tab.scan_total == 0 {
                        format!("正在发现文件… 已找到 {} 个", tab.scan_discovered)
                    } else if tab.scan_done < tab.scan_total {
                        format!("已读取 {} / {} 个文件", tab.scan_done, tab.scan_total)
                    } else {
                        "正在整理文件…".to_string()
                    };
                    ui.label(RichText::new(progress).size(10.5).color(widgets::MUTED));
                }

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("缩放").size(10.5).color(widgets::MUTED));
                    let width = (ui.available_width() - 6.0).max(80.0);
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::new(width, 20.0), Sense::click_and_drag());
                    if response.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    let track = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 6.0));
                    ui.painter().rect_filled(track, 3, widgets::BORDER);
                    let knob_x = egui::lerp(track.left()..=track.right(), zoom.clamp(0.0, 1.0));
                    ui.painter().rect_filled(
                        Rect::from_min_max(
                            track.left_top(),
                            egui::Pos2::new(knob_x, track.bottom()),
                        ),
                        3,
                        widgets::ACCENT,
                    );
                    ui.painter().circle_filled(
                        egui::Pos2::new(knob_x, track.center().y),
                        7.0,
                        widgets::ACCENT,
                    );
                    ui.painter().circle_stroke(
                        egui::Pos2::new(knob_x, track.center().y),
                        7.5,
                        Stroke::new(1.0, widgets::BG),
                    );
                    if (response.dragged() || response.clicked())
                        && let Some(position) = response.interact_pointer_pos()
                    {
                        zoom = ((position.x - track.left()) / track.width()).clamp(0.0, 1.0);
                    }
                    if response.drag_stopped() || response.clicked() {
                        zoom_commit = true;
                    }
                    response.hover_tip("向左缩小以并排显示更多照片，向右放大");
                });

                if let Some((done, total)) = tab.vision_progress {
                    ui.add_space(2.0);
                    ui.add(
                        ProgressBar::new(done as f32 / total.max(1) as f32)
                            .desired_height(3.0)
                            .fill(widgets::TEAL),
                    );
                    ui.label(
                        RichText::new(format!("视觉分析 {done} / {total}"))
                            .size(10.0)
                            .color(widgets::MUTED),
                    );
                } else if let Some(report) = &tab.vision_report {
                    if !report.groups.is_empty() {
                        ui.label(
                            RichText::new(format!(
                                "查重 {} 组 · 自动优选 {} 张",
                                report.groups.len(),
                                report.preferred.len()
                            ))
                            .size(10.0)
                            .color(widgets::ACCENT),
                        );
                    }
                } else if let Some(error) = &tab.vision_error {
                    ui.label(RichText::new(error).size(10.0).color(widgets::MUTED));
                }

                ui.add_space(8.0);
                widgets::divider(ui);
                ui.add_space(4.0);

                if tab.filtered.is_empty() {
                    let message = if tab.scanning {
                        "正在读取元数据…"
                    } else if tab.entries.is_empty() {
                        "没有找到支持的图片或视频"
                    } else if tab.favorites_only {
                        "还没有优选照片；可点击缩略图右上角的星标"
                    } else {
                        "没有匹配的文件"
                    };
                    ui.add_space(30.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(message).size(12.0).color(widgets::MUTED));
                    });
                    return;
                }

                let available = ui.available_width().max(120.0);
                let target = 88.0 + zoom * 240.0;
                let columns = (((available + GRID_SPACING) / (target + GRID_SPACING)).floor()
                    as usize)
                    .max(1);
                let cell_width = ((available - GRID_SPACING * columns.saturating_sub(1) as f32)
                    / columns as f32)
                    .clamp(72.0, 360.0);
                let image_height = cell_width.min(220.0);
                let cell_height = image_height + 30.0;
                let row_height = cell_height + GRID_SPACING;
                let (variant, thumb_size) = if cell_width >= 150.0 {
                    (THUMB_LARGE_VARIANT, THUMB_LARGE_SIZE)
                } else {
                    (THUMB_VARIANT, THUMB_SIZE)
                };

                let mut jump_position = None;
                if photo_sort == PhotoSort::TakenTime && tab.filtered_time_range.is_some() {
                    ui.add_space(2.0);
                    jump_position = browse_time_axis(ui, tab);
                    if let Some(position) = jump_position
                        && let Some(&entry_index) = tab.filtered.get(position)
                    {
                        clicked = Some(tab.entries[entry_index].path.clone());
                    }
                    ui.add_space(4.0);
                }

                let entries = &tab.entries;
                let filtered = &tab.filtered;
                let selected_path = tab.selected.clone();
                let reset_scroll = std::mem::take(&mut tab.reset_list_scroll);
                let mut scroll_area = ScrollArea::vertical()
                    .id_salt(("file-list-scroll", tab.id))
                    .auto_shrink([false, false])
                    .stick_to_bottom(tab.scanning && !reset_scroll && jump_position.is_none());
                if let Some(position) = jump_position {
                    let row = position / columns;
                    scroll_area = scroll_area.vertical_scroll_offset(row as f32 * row_height);
                } else if reset_scroll {
                    scroll_area = scroll_area.vertical_scroll_offset(0.0);
                }
                let rows = filtered.len().div_ceil(columns);
                scroll_area.show_rows(ui, row_height, rows, |ui, range| {
                    ui.spacing_mut().item_spacing = Vec2::new(GRID_SPACING, GRID_SPACING);
                    for row in range {
                        ui.horizontal(|ui| {
                            for column in 0..columns {
                                let position = row * columns + column;
                                let Some(&entry_index) = filtered.get(position) else {
                                    break;
                                };
                                let entry = &entries[entry_index];
                                let key = entry_key(entry, variant);
                                let texture = textures.get(&key);
                                let is_selected =
                                    selected_path.as_deref() == Some(entry.path.as_path());
                                let is_auto = auto_preferred.contains(&entry.path);
                                let favorite = ratings.is_favorite(&entry.path, is_auto);
                                let auto_badge =
                                    is_auto && ratings.manual_state(&entry.path) != Some(false);
                                let (response, star_clicked) = photo_tile(
                                    ui,
                                    entry,
                                    texture,
                                    is_selected,
                                    favorite,
                                    auto_badge,
                                    cell_width,
                                    image_height,
                                );
                                if star_clicked {
                                    star_toggle = Some(entry.path.clone());
                                } else if response.clicked() {
                                    clicked = Some(entry.path.clone());
                                }
                                if texture.is_some() {
                                    visible_keys.insert(key);
                                } else if can_thumbnail(entry)
                                    && !pending.contains(&key)
                                    && !failed.contains(&key)
                                    && needed.len() < 16
                                {
                                    let orientation = entry
                                        .image
                                        .as_ref()
                                        .map(|info| info.orientation)
                                        .unwrap_or(1);
                                    needed.push((key, entry.path.clone(), orientation, thumb_size));
                                }
                            }
                        });
                    }
                });
            });

        self.settings.thumbnail_zoom = zoom;
        if self.show_settings {
            self.settings_draft.thumbnail_zoom = zoom;
        }
        if zoom_commit {
            self.settings.save();
        }
        if sort_changed {
            self.settings.photo_sort = photo_sort;
            self.settings.save();
            refresh_filter(tab, &self.ratings, photo_sort);
            if photo_sort == PhotoSort::TakenTime {
                tab.browse_time_cursor = 1.0;
            }
            tab.reset_list_scroll = true;
            root.ctx().request_repaint();
        }
        if let Some(path) = star_toggle {
            let auto = auto_preferred.contains(&path);
            self.ratings.toggle(&path, auto);
            tab.filter_dirty = true;
            tab.favorites_dirty = true;
            root.ctx().request_repaint();
        }

        if recursive_changed {
            if tab.edit_is_dirty() {
                tab.recursive = !tab.recursive;
                self.set_toast(ToastKind::Info, "存在未保存修改，已暂缓切换扫描范围。");
            } else {
                let select = tab.selected.clone();
                self.scan_tab(tab, true, select);
            }
            return;
        }
        if rescan {
            if tab.edit_is_dirty() {
                self.pending_action = Some(PendingAction {
                    tab_id: tab.id,
                    action: TabAction::Rescan,
                });
            } else {
                let select = tab.selected.clone();
                self.scan_tab(tab, true, select);
            }
            return;
        }
        if let Some(path) = clicked {
            self.load_selection(tab, &path, true);
        }
        for key in visible_keys {
            self.touch_thumb(key);
        }
        for (key, path, orientation, max_size) in needed {
            if self.thumb_pending.len() >= MAX_THUMB_INFLIGHT {
                break;
            }
            if self.thumb_pending.contains(&key) || self.thumb_failed.contains(&key) {
                continue;
            }
            self.thumb_pending.insert(key);
            self.thumbs.request(ImageJob {
                path,
                max_size,
                orientation,
                cache_key: key,
            });
        }
    }

    fn central_browse(&mut self, ui: &mut Ui, tab: &mut FolderTab) {
        let Some(entry) = tab.selected_entry().cloned() else {
            ui.add_space(90.0);
            ui.vertical_centered(|ui| {
                if tab.scanning {
                    ui.spinner();
                    ui.add_space(12.0);
                    let message = if tab.scan_queued {
                        "当前标签页正在排队，等待其他文件夹扫描完成…".to_string()
                    } else if tab.scan_total == 0 {
                        format!("正在发现文件… 已找到 {} 个", tab.scan_discovered)
                    } else {
                        format!(
                            "正在读取文件夹中的元数据… {} / {}",
                            tab.scan_done, tab.scan_total
                        )
                    };
                    ui.label(RichText::new(message).size(13.0).color(widgets::MUTED));
                } else {
                    ui.label(
                        RichText::new("从左侧选择一个文件查看详情")
                            .size(13.0)
                            .color(widgets::MUTED),
                    );
                }
            });
            return;
        };

        ScrollArea::vertical()
            .id_salt(("detail-scroll", tab.id))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.show_detail_header(ui, tab, &entry);
                ui.add_space(14.0);
                self.show_preview(ui, &entry);
                ui.add_space(12.0);
                show_quick_facts(ui, &entry);
                ui.add_space(18.0);
                self.show_detail_tabs(ui, tab, &entry);
                ui.add_space(12.0);
                match tab.detail_tab {
                    DetailTab::Info => self.show_info_tab(ui, tab, &entry),
                    DetailTab::Edit => self.show_edit_tab(ui, tab, &entry),
                }
                ui.add_space(24.0);
            });
    }

    fn central_stats(&mut self, ui: &mut Ui, tab: &mut FolderTab) {
        if tab.stats_dirty || tab.stats.is_none() {
            tab.stats = Some(Stats::compute(&tab.entries));
            tab.stats_dirty = false;
        }
        if let Some(stats) = tab.stats.as_ref() {
            show_stats_view(ui, stats, tab.id, tab.scanning);
        }
    }

    fn central_duplicates(&mut self, ui: &mut Ui, tab: &mut FolderTab) {
        let mut restart = false;
        let mut cancel = false;
        let mut start_analysis = false;
        let mut page_target: Option<usize> = None;
        let mut autoload = false;
        let mut actions = DuplicateActions::default();
        let now = ui.ctx().input(|input| input.time);
        if !tab.scanning && !tab.vision_attempted {
            if tab.vision_start_at <= 0.0 {
                tab.vision_start_at = now + 0.24;
            } else if now >= tab.vision_start_at {
                start_analysis = true;
            }
        }
        let layout = self.settings.duplicates_layout;
        let sort = self.settings.duplicates_sort;
        let mut sort_choice = sort;
        let mut sort_changed = false;
        if tab.duplicate_order_revision != tab.duplicates_revision
            || tab.duplicate_order_sort != sort
        {
            rebuild_duplicate_view(tab, sort);
        }
        let group_count = tab
            .vision_report
            .as_ref()
            .map(|report| report.groups.len())
            .unwrap_or(0);
        let total_pages = duplicates_total_pages(group_count);
        let page = tab.duplicates_page.min(total_pages.saturating_sub(1));
        tab.duplicates_page = page;
        let scroll_top = std::mem::take(&mut tab.duplicates_scroll_top);
        if scroll_top {
            tab.duplicates_stick_ready = false;
        }
        let notice = tab
            .duplicates_notice
            .as_ref()
            .filter(|(_, created)| created.elapsed() < Duration::from_millis(2400))
            .map(|(text, created)| (text.clone(), created.elapsed().as_secs_f32()));
        let (bounce, bouncing) = duplicate_bounce_offset(tab.duplicates_bounce_at);
        if bouncing {
            ui.ctx().request_repaint();
        }
        let auto_preferred = tab
            .vision_report
            .as_ref()
            .map(|report| report.preferred.clone())
            .unwrap_or_default();
        let entry_index = tab.entry_index.clone();
        let render_ctx = DuplicateRenderCtx {
            entries: &tab.entries,
            entry_index: entry_index.as_ref(),
            textures: &self.thumb_textures,
            pending: &self.thumb_pending,
            failed: &self.thumb_failed,
            ratings: &self.ratings,
            auto_preferred: auto_preferred.as_ref(),
        };
        let gpu = self.vision_hardware.gpu.clone();
        let npu = self.vision_hardware.npu.clone();
        let adapter_count = self.vision_hardware.adapters.len();
        let start = if layout == DuplicatesLayout::Waterfall {
            tab.duplicates_start.min(group_count.saturating_sub(1))
        } else {
            page * DUPLICATES_PAGE_SIZE
        };
        let waterfall_loaded = tab.duplicates_loaded.min(group_count.saturating_sub(start));
        let waterfall_end = start + waterfall_loaded;
        let more_waterfall = layout == DuplicatesLayout::Waterfall && waterfall_end < group_count;
        let user_scrolled = ui
            .ctx()
            .input(|input| input.smooth_scroll_delta.y.abs() > 0.4);
        let mut timeline_target: Option<(usize, i64)> = None;
        if sort == DuplicateSort::TakenTime
            && group_count > 0
            && tab.duplicates_cursor_anchor != (page, start)
        {
            let top = if layout == DuplicatesLayout::Waterfall {
                start
            } else {
                page * DUPLICATES_PAGE_SIZE
            };
            if let Some(&group) = tab.duplicate_order.get(top)
                && let Some(&time) = tab.duplicate_group_times.get(group)
                && time != i64::MIN
                && let Some(range) = tab.duplicate_time_range
            {
                tab.duplicates_time_cursor = time_fraction(time, range);
            }
            tab.duplicates_cursor_anchor = (page, start);
        }

        let mut scroll_area = ScrollArea::vertical()
            .id_salt(("duplicates-scroll", tab.id))
            .auto_shrink([false, false])
            .stick_to_bottom(more_waterfall && tab.duplicates_stick_ready);
        if scroll_top {
            scroll_area = scroll_area.vertical_scroll_offset(0.0);
        }
        scroll_area.show(ui, |ui| {
            ui.add_space(bounce.abs());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("照片查重")
                        .size(24.0)
                        .strong()
                        .color(widgets::TEXT),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new("感知指纹 · 画质优选 · 本地分析")
                        .size(11.5)
                        .color(widgets::MUTED),
                );
                if layout == DuplicatesLayout::Paged && total_pages > 1 {
                    ui.add_space(8.0);
                    widgets::pill(
                        ui,
                        &format!("第 {} / {} 页", page + 1, total_pages),
                        widgets::TEAL,
                    );
                } else if layout == DuplicatesLayout::Waterfall && group_count > 0 {
                    ui.add_space(8.0);
                    widgets::pill(
                        ui,
                        &format!(
                            "已显示 {}–{} / {} 组",
                            start + 1,
                            waterfall_end.min(group_count),
                            group_count
                        ),
                        widgets::TEAL,
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if tab.vision.is_some() {
                        if ui.button("取消分析").clicked() {
                            cancel = true;
                        }
                    } else if ui.button("重新分析").clicked() {
                        restart = true;
                    }
                });
            });
            if group_count > 0 {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("排序").size(11.0).color(widgets::MUTED));
                    ComboBox::from_id_salt(("duplicates-sort", tab.id))
                        .selected_text(sort.label())
                        .width(174.0)
                        .show_ui(ui, |ui| {
                            for option in [
                                DuplicateSort::Count,
                                DuplicateSort::TakenTime,
                                DuplicateSort::Resolution,
                            ] {
                                ui.selectable_value(&mut sort_choice, option, option.label());
                            }
                        });
                    if sort == DuplicateSort::TakenTime && tab.duplicate_time_range.is_some() {
                        ui.label(
                            RichText::new("拖动时间轴可直接跳到对应时间段")
                                .size(10.0)
                                .color(Color32::from_rgb(105, 113, 126)),
                        );
                    }
                });
                if sort == DuplicateSort::TakenTime
                    && let Some(range) = tab.duplicate_time_range
                    && let Some(target) = duplicate_time_axis(
                        ui,
                        &tab.duplicate_order,
                        &tab.duplicate_group_times,
                        range,
                        &mut tab.duplicates_time_cursor,
                    )
                {
                    let seconds = tab
                        .duplicate_order
                        .get(target)
                        .and_then(|group| tab.duplicate_group_times.get(*group))
                        .copied()
                        .unwrap_or(range.1);
                    timeline_target = Some((target, seconds));
                }
                sort_changed = sort_choice != sort;
            }
            ui.add_space(12.0);

            widgets::card(ui, |ui| {
                widgets::section_header(ui, "视觉引擎", "全部在本机运行，不上传图片");
                ui.label(
                    RichText::new(format!(
                        "指纹计算：CPU 全核并行（{} 线程）· dHash 感知哈希 + 亮度/锐度/分辨率画质评分",
                        vision::worker_count()
                    ))
                    .size(11.5)
                    .color(widgets::TEXT),
                );
                ui.add_space(3.0);
                ui.label(
                    RichText::new(format!(
                        "GPU：{gpu}{}（仅用于界面渲染，不参与指纹计算）",
                        if adapter_count > 1 {
                            format!("   ·   共 {adapter_count} 个适配器")
                        } else {
                            String::new()
                        }
                    ))
                    .size(10.5)
                    .color(widgets::MUTED),
                );
                ui.label(
                    RichText::new(format!(
                        "NPU：{}",
                        npu.as_deref().unwrap_or("未检测到（预留 ONNX / Core ML 后端）")
                    ))
                    .size(10.5)
                    .color(widgets::MUTED),
                );
                ui.add_space(3.0);
                ui.label(
                    RichText::new(
                        "dHash 是纯 CPU 位运算，GPU 占用低是正常的；查重会吃满可用 CPU 线程。缓存每 15 秒落盘一次，避免频繁写盘拖慢分析。",
                    )
                    .size(10.0)
                    .color(Color32::from_rgb(105, 113, 126)),
                );
            });

            ui.add_space(12.0);
            if tab.scanning {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("文件夹仍在扫描，完成后会自动开始视觉分析")
                            .size(11.5)
                            .color(widgets::MUTED),
                    );
                });
            } else if let Some((done, total)) = tab.vision_progress {
                widgets::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new(format!("正在分析 {done} / {total} 张图片"))
                                .size(12.5)
                                .color(widgets::TEXT),
                        );
                    });
                    ui.add_space(6.0);
                    ui.add(
                        ProgressBar::new(done as f32 / total.max(1) as f32)
                            .desired_height(5.0)
                            .fill(widgets::TEAL)
                            .animate(true),
                    );
                });
            } else if let Some(error) = &tab.vision_error {
                widgets::card(ui, |ui| {
                    ui.label(RichText::new(error).size(12.0).color(widgets::MUTED));
                });
            } else if let Some(report) = &tab.vision_report {
                ui.columns(4, |columns| {
                    metric_card(
                        &mut columns[0],
                        "相似组",
                        &report.groups.len().to_string(),
                        widgets::ACCENT,
                    );
                    metric_card(
                        &mut columns[1],
                        "自动优选",
                        &report.preferred.len().to_string(),
                        widgets::ACCENT,
                    );
                    metric_card(
                        &mut columns[2],
                        "已分析",
                        &report.analyzed.to_string(),
                        widgets::TEAL,
                    );
                    metric_card(
                        &mut columns[3],
                        "读取失败",
                        &report.failed.to_string(),
                        widgets::MUTED,
                    );
                });
                if report.groups.is_empty() {
                    ui.add_space(12.0);
                    widgets::card(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(16.0);
                            ui.label(
                                RichText::new("未发现相似照片")
                                    .size(15.0)
                                    .strong()
                                    .color(widgets::TEXT),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new("连拍、副本或轻微裁剪的图片会归为一组并自动标出画质最佳的一张。")
                                    .size(11.0)
                                    .color(widgets::MUTED),
                            );
                            ui.add_space(16.0);
                        });
                    });
                }
            } else if tab.vision_attempted {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("视觉分析尚未运行，点击右上角“重新分析”开始。")
                            .size(11.5)
                            .color(widgets::MUTED),
                    );
                });
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("正在准备视觉分析…")
                            .size(11.5)
                            .color(widgets::MUTED),
                    );
                });
            }

            let Some(report) = &tab.vision_report else {
                ui.add_space(24.0);
                return;
            };
            if layout == DuplicatesLayout::Paged {
                let end = (start + DUPLICATES_PAGE_SIZE).min(group_count);
                for display_index in start..end {
                    let Some(group) = tab
                        .duplicate_order
                        .get(display_index)
                        .and_then(|group| report.groups.get(*group))
                    else {
                        continue;
                    };
                    ui.add_space(12.0);
                    duplicate_group_card(
                        ui,
                        group,
                        display_index,
                        170.0,
                        124.0,
                        &render_ctx,
                        &mut actions,
                    );
                }
                if total_pages > 1 {
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(page > 0, Button::new("上一页").small())
                            .clicked()
                        {
                            page_target = Some(page.saturating_sub(1));
                        }
                        ui.label(
                            RichText::new(format!(
                                "第 {} / {} 页 · 共 {} 个相似组",
                                page + 1,
                                total_pages,
                                report.groups.len()
                            ))
                            .size(11.5)
                            .color(widgets::MUTED),
                        );
                        if ui
                            .add_enabled(page + 1 < total_pages, Button::new("下一页").small())
                            .clicked()
                        {
                            page_target = Some(page + 1);
                        }
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("PageUp / PageDown 或鼠标侧键翻页")
                                .size(10.0)
                                .color(Color32::from_rgb(105, 113, 126)),
                        );
                    });
                }
                ui.add_space(24.0);
            } else {
                if waterfall_loaded == 0 {
                    ui.add_space(24.0);
                    return;
                }
                let column_width = ((ui.available_width() - 12.0) / 2.0).max(160.0);
                let tile_width = 150.0;
                let tile_height = 108.0;
                let mut left: Vec<(&DuplicateGroup, usize)> = Vec::new();
                let mut right: Vec<(&DuplicateGroup, usize)> = Vec::new();
                let mut heights = [0.0f32; 2];
                for display_index in start..waterfall_end {
                    let Some(group) = tab
                        .duplicate_order
                        .get(display_index)
                        .and_then(|group| report.groups.get(*group))
                    else {
                        continue;
                    };
                    let height =
                        duplicate_group_height(group, column_width, tile_width, tile_height);
                    let column = if heights[0] <= heights[1] { 0 } else { 1 };
                    heights[column] += height;
                    if column == 0 {
                        left.push((group, display_index));
                    } else {
                        right.push((group, display_index));
                    }
                }
                ui.add_space(12.0);
                ui.columns(2, |columns| {
                    for (group, display_index) in left {
                        duplicate_group_card(
                            &mut columns[0],
                            group,
                            display_index,
                            tile_width,
                            tile_height,
                            &render_ctx,
                            &mut actions,
                        );
                        columns[0].add_space(12.0);
                    }
                    for (group, display_index) in right {
                        duplicate_group_card(
                            &mut columns[1],
                            group,
                            display_index,
                            tile_width,
                            tile_height,
                            &render_ctx,
                            &mut actions,
                        );
                        columns[1].add_space(12.0);
                    }
                });
                if more_waterfall {
                    ui.add_space(10.0);
                    let response = ui.add(
                        Button::new(format!(
                            "继续加载（剩余 {} 组）",
                            group_count - waterfall_end
                        ))
                        .fill(widgets::PANEL_SOFT),
                    );
                    if response.clicked() || (ui.is_rect_visible(response.rect) && user_scrolled) {
                        autoload = true;
                        ui.ctx().request_repaint();
                    }
                }
                ui.add_space(20.0);
            }
        });
        if !scroll_top && !tab.duplicates_stick_ready {
            tab.duplicates_stick_ready = true;
        }

        if let Some((text, elapsed)) = &notice {
            let opacity =
                (elapsed / 0.12).clamp(0.0, 1.0) * ((2.4 - elapsed) / 0.35).clamp(0.0, 1.0);
            let rect = ui.max_rect();
            let slide = (1.0 - (elapsed / 0.16).clamp(0.0, 1.0)) * 8.0;
            let pos = egui::pos2(
                rect.center().x - 70.0,
                rect.bottom() - 52.0 + slide + bounce,
            );
            egui::Area::new(Id::unique("duplicates-notice"))
                .order(Order::Foreground)
                .fixed_pos(pos)
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    ui.set_opacity(opacity);
                    let color = if text.contains("已经") {
                        widgets::ACCENT
                    } else {
                        widgets::TEAL
                    };
                    widgets::pill(ui, text, color);
                });
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }

        if sort_changed {
            self.settings.duplicates_sort = sort_choice;
            self.settings.save();
            tab.touch_duplicates();
            tab.duplicates_notice =
                Some((format!("已按{}排序", sort_choice.label()), Instant::now()));
            ui.ctx().request_repaint();
        }
        if let Some(target) = page_target {
            go_duplicates_page(tab, target, total_pages);
            if layout == DuplicatesLayout::Paged {
                let top = (target * DUPLICATES_PAGE_SIZE).min(group_count.saturating_sub(1));
                if let (Some(&group), Some(range)) =
                    (tab.duplicate_order.get(top), tab.duplicate_time_range)
                    && let Some(&time) = tab.duplicate_group_times.get(group)
                    && time != i64::MIN
                {
                    tab.duplicates_time_cursor = time_fraction(time, range);
                }
            }
        }
        if let Some((target, seconds)) = timeline_target {
            let date = format_timeline_date(seconds, true);
            if layout == DuplicatesLayout::Paged {
                let target_page =
                    (target / DUPLICATES_PAGE_SIZE).min(total_pages.saturating_sub(1));
                go_duplicates_page(tab, target_page, total_pages);
            } else {
                tab.duplicates_start = target.min(group_count.saturating_sub(1));
                tab.duplicates_loaded = DUPLICATES_WATERFALL_CHUNK.min(group_count);
                tab.duplicates_scroll_top = true;
            }
            if let Some(range) = tab.duplicate_time_range {
                tab.duplicates_time_cursor = time_fraction(seconds, range);
            }
            tab.duplicates_notice = Some((format!("已跳转到 {date}"), Instant::now()));
            ui.ctx().request_repaint();
        }
        if autoload {
            tab.duplicates_loaded = (tab.duplicates_loaded + DUPLICATES_WATERFALL_CHUNK)
                .min(group_count.saturating_sub(tab.duplicates_start).max(1));
        }
        if start_analysis {
            self.start_vision(tab);
        }
        if cancel {
            self.cancel_vision(tab);
        }
        if restart {
            tab.duplicates_page = 0;
            tab.duplicates_loaded = DUPLICATES_WATERFALL_CHUNK;
            tab.duplicates_scroll_top = true;
            self.restart_vision(tab);
            tab.vision_start_at = now;
        }
        if let Some(path) = actions.star_toggle {
            let auto = auto_preferred.contains(&path);
            self.ratings.toggle(&path, auto);
            tab.filter_dirty = true;
            tab.favorites_dirty = true;
            ui.ctx().request_repaint();
        }
        if let Some(path) = actions.open_file
            && tab.entries.iter().any(|entry| entry.path == path)
        {
            tab.view = ViewTab::Browse;
            self.load_selection(tab, &path, true);
        }
        for key in actions.visible_keys {
            self.touch_thumb(key);
        }
        for (key, path, orientation, max_size) in actions.needed {
            if self.thumb_pending.len() >= MAX_THUMB_INFLIGHT {
                break;
            }
            if self.thumb_pending.contains(&key) || self.thumb_failed.contains(&key) {
                continue;
            }
            self.thumb_pending.insert(key);
            self.thumbs.request(ImageJob {
                path,
                max_size,
                orientation,
                cache_key: key,
            });
        }
    }

    fn show_detail_header(&mut self, ui: &mut Ui, tab: &mut FolderTab, entry: &MediaEntry) {
        let mut reveal = false;
        let mut reload = false;
        let mut play_video = false;
        ui.horizontal(|ui| {
            let color = match entry.kind {
                MediaKind::Image => widgets::TEAL,
                MediaKind::Video => widgets::ACCENT,
            };
            widgets::pill(ui, entry.kind.label(), color);
            widgets::pill(ui, &entry.format_label(), widgets::MUTED);
            if tab.scanning {
                widgets::pill(ui, "扫描中", widgets::TEAL);
            }
            ui.add_space(4.0);
            ui.add(
                Label::new(
                    RichText::new(widgets::truncate(&entry.name, 72))
                        .size(20.0)
                        .strong()
                        .color(widgets::TEXT),
                )
                .truncate(),
            )
            .hover_tip(&entry.name);
        });
        let path = entry.path.display().to_string();
        let actions_width = if entry.kind == MediaKind::Video {
            300.0
        } else {
            210.0
        };
        let path_width = (ui.available_width() - actions_width).max(140.0);
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(path_width, 24.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.add(
                        Label::new(RichText::new(&path).size(11.0).color(widgets::MUTED))
                            .truncate(),
                    )
                    .hover_tip(path);
                },
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if entry.kind == MediaKind::Video
                    && ui
                        .add(
                            Button::new(RichText::new("播放").size(11.5).color(widgets::BG))
                                .fill(widgets::ACCENT)
                                .small(),
                        )
                        .hover_tip("在系统默认播放器中打开")
                        .clicked()
                {
                    play_video = true;
                }
                if ui
                    .add(Button::new("重新读取").small().frame(false))
                    .clicked()
                {
                    reload = true;
                }
                if ui
                    .add(Button::new("在文件夹中显示").small().frame(false))
                    .clicked()
                {
                    reveal = true;
                }
            });
        });
        if let Some(error) = &entry.read_error {
            ui.add_space(6.0);
            ui.add(
                Label::new(
                    RichText::new(format!(
                        "无法读取照片元数据：{}",
                        friendly_media_error(error)
                    ))
                    .size(11.0)
                    .color(widgets::RED),
                )
                .wrap(),
            )
            .on_hover_text(error);
        }
        if reveal {
            platform::reveal_in_file_manager(&entry.path);
        }
        if play_video {
            self.pending_open_video = Some(entry.path.clone());
        }
        if reload {
            self.reload_selected(tab);
        }
    }

    fn show_preview(&mut self, ui: &mut Ui, entry: &MediaEntry) {
        let mut retry = false;
        let mut play_video = false;
        Frame::new()
            .fill(Color32::from_rgb(9, 11, 14))
            .stroke(Stroke::new(1.0, widgets::BORDER))
            .corner_radius(14)
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if entry.kind == MediaKind::Video {
                    let error = self.preview.error.as_deref();
                    if show_video_placeholder(
                        ui,
                        entry,
                        self.preview.texture.as_ref(),
                        self.preview.loading,
                        error,
                    ) {
                        play_video = true;
                    }
                    return;
                }
                if let Some(texture) = &self.preview.texture {
                    let available = ui.available_width();
                    let texture_size = texture.size_vec2();
                    let max_height = 380.0;
                    let scale = (available / texture_size.x)
                        .min(max_height / texture_size.y)
                        .clamp(0.01, 1.5);
                    let size = texture_size * scale;
                    let height = (size.y + 16.0).clamp(140.0, max_height + 16.0);
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(available, height), Sense::hover());
                    let image_rect = Rect::from_center_size(rect.center(), size);
                    ui.painter().image(
                        texture.id(),
                        image_rect,
                        Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                } else if let Some(error) = &self.preview.error {
                    let (rect, preview_response) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 170.0),
                        Sense::hover(),
                    );
                    preview_response.on_hover_text(error);
                    ui.painter().text(
                        rect.center() - Vec2::new(0.0, 16.0),
                        Align2::CENTER_CENTER,
                        "无法生成预览",
                        FontId::proportional(16.0),
                        widgets::TEXT,
                    );
                    ui.painter().text(
                        rect.center() + Vec2::new(0.0, 10.0),
                        Align2::CENTER_CENTER,
                        widgets::truncate(&friendly_media_error(error), 72),
                        FontId::proportional(11.5),
                        widgets::MUTED,
                    );
                    let button_rect = Rect::from_center_size(
                        rect.center() + Vec2::new(0.0, 46.0),
                        Vec2::new(80.0, 26.0),
                    );
                    if ui.put(button_rect, Button::new("重试").small()).clicked() {
                        retry = true;
                    }
                } else {
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), 200.0),
                        Layout::top_down(Align::Center),
                        |ui| {
                            ui.add_space(64.0);
                            ui.spinner();
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new("正在生成预览…")
                                    .size(12.0)
                                    .color(widgets::MUTED),
                            );
                        },
                    );
                }
            });
        if retry {
            self.request_preview(entry);
        }
        if play_video {
            self.pending_open_video = Some(entry.path.clone());
        }
    }

    fn show_detail_tabs(&mut self, ui: &mut Ui, tab: &mut FolderTab, entry: &MediaEntry) {
        let writable = tab
            .details
            .as_ref()
            .map(|details| details.is_writable)
            .unwrap_or(false);
        let dirty = tab.edit_is_dirty();
        let mut next_tab = tab.detail_tab;
        ui.horizontal(|ui| {
            if tab_button(ui, "完整信息", tab.detail_tab == DetailTab::Info).clicked() {
                next_tab = DetailTab::Info;
            }
            if entry.kind == MediaKind::Image {
                let response = ui.add_enabled(
                    writable,
                    Button::new(RichText::new("编辑 EXIF").size(14.0).color(
                        if tab.detail_tab == DetailTab::Edit {
                            widgets::ACCENT
                        } else {
                            widgets::MUTED
                        },
                    ))
                    .frame(tab.detail_tab == DetailTab::Edit)
                    .fill(if tab.detail_tab == DetailTab::Edit {
                        widgets::PANEL_SOFT
                    } else {
                        Color32::TRANSPARENT
                    }),
                );
                if response.clicked() {
                    next_tab = DetailTab::Edit;
                }
                if !writable {
                    response.on_disabled_hover_text("该格式不支持写入 EXIF");
                }
            }
            if dirty {
                ui.add_space(6.0);
                widgets::pill(ui, "有未保存的修改", widgets::ACCENT);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if entry.kind == MediaKind::Video {
                    ui.label(
                        RichText::new("视频容器元数据为只读")
                            .size(11.0)
                            .color(widgets::MUTED),
                    );
                }
            });
        });
        tab.detail_tab = next_tab;
        ui.add_space(8.0);
        widgets::divider(ui);
    }

    fn show_info_tab(&mut self, ui: &mut Ui, tab: &mut FolderTab, entry: &MediaEntry) {
        let Some(details) = tab.details.as_ref() else {
            show_details_pending(ui, tab.details_request.is_some());
            return;
        };
        let show_raw = self.settings.show_raw_tags;
        let mut copy = false;
        let mut watermark = false;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("文件信息")
                    .size(15.5)
                    .strong()
                    .color(widgets::TEXT),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(Button::new("复制全部").small().frame(false))
                    .clicked()
                {
                    copy = true;
                }
                if entry.kind == MediaKind::Image
                    && ui
                        .add(Button::new("隐写水印").small().frame(false))
                        .hover_tip("写入或识别不可见水印")
                        .clicked()
                {
                    watermark = true;
                }
            });
        });
        ui.add_space(8.0);
        widgets::card(ui, |ui| {
            info_row(ui, "文件名", &entry.name);
            info_row(ui, "路径", &entry.path.display().to_string());
            info_row(
                ui,
                "类型",
                &format!("{} · {}", entry.kind.label(), entry.format_label()),
            );
            info_row(ui, "大小", &entry.size_label());
            if let Some(modified) = entry.modified {
                info_row(ui, "修改时间", &format_friendly_local(&modified));
            }
            if let Some(resolution) = entry.resolution_label() {
                info_row(ui, "尺寸", &resolution);
            }
            match entry.kind {
                MediaKind::Image => {
                    let has_exif = entry
                        .image
                        .as_ref()
                        .map(|info| info.has_exif)
                        .unwrap_or(false);
                    info_row(ui, "EXIF", if has_exif { "有" } else { "无" });
                    if let Some(gps) = entry.image.as_ref().and_then(|info| info.gps) {
                        info_row(ui, "GPS", &format!("{:.6}, {:.6}", gps.0, gps.1));
                    }
                }
                MediaKind::Video => {
                    if let Some(info) = &entry.video
                        && let Some(gps) = info.gps
                    {
                        info_row(ui, "GPS", &format!("{:.6}, {:.6}", gps.0, gps.1));
                    }
                    info_row(ui, "元数据", "容器标签 · 只读");
                }
            }
        });

        if let Some(error) = &details.exif_error {
            ui.add_space(12.0);
            warning_card(ui, "照片读取提示", &friendly_media_error(error));
        }
        if let Some(error) = &details.write_error {
            ui.add_space(12.0);
            warning_card(ui, "EXIF 写入不可用", error);
        }
        if !details.is_writable && entry.kind == MediaKind::Image {
            ui.add_space(12.0);
            ui.label(
                RichText::new("此图片格式以只读方式展示 EXIF。")
                    .size(11.5)
                    .color(widgets::MUTED),
            );
        }

        if details.groups.is_empty() {
            ui.add_space(12.0);
            widgets::card(ui, |ui| {
                ui.add_space(6.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("这个文件没有可读取的 EXIF 信息")
                            .size(12.5)
                            .color(widgets::MUTED),
                    );
                });
                ui.add_space(6.0);
            });
        } else {
            for group in &details.groups {
                let visible_count = group
                    .fields
                    .iter()
                    .filter(|field| show_raw || !is_raw_label(&field.label))
                    .count();
                if visible_count == 0 {
                    continue;
                }
                ui.add_space(12.0);
                let collapsed = self.collapsed.contains(&group.name);
                let mut toggle = false;
                widgets::card(ui, |ui| {
                    if widgets::collapsible_header(
                        ui,
                        &group.name,
                        &format!("{visible_count} 项"),
                        collapsed,
                    ) {
                        toggle = true;
                    }
                    if collapsed {
                        let summary = collapsed_summary(&group.name, entry, group);
                        show_summary_line(ui, &summary);
                    } else {
                        let mut shown = 0usize;
                        for field in &group.fields {
                            if !show_raw && is_raw_label(&field.label) {
                                continue;
                            }
                            if shown > 0 {
                                ui.add_space(2.0);
                            }
                            info_row(ui, &field.label, &field.value);
                            shown += 1;
                        }
                        if shown == 0 {
                            ui.label(
                                RichText::new("没有可显示的字段")
                                    .size(11.5)
                                    .color(widgets::MUTED),
                            );
                        }
                    }
                });
                if toggle {
                    if collapsed {
                        self.collapsed.remove(&group.name);
                    } else {
                        self.collapsed.insert(group.name.clone());
                    }
                }
            }
        }

        if copy {
            let text = build_info_text(entry, &details.groups, show_raw);
            ui.ctx().copy_text(text);
            self.set_toast(ToastKind::Success, "已复制全部元数据");
        }
        if watermark {
            self.watermark_dialog = Some(WatermarkDialog::new(
                tab.id,
                entry.path.clone(),
                self.settings.watermark_folder.clone(),
            ));
        }
    }

    fn show_edit_tab(&mut self, ui: &mut Ui, tab: &mut FolderTab, _entry: &MediaEntry) {
        let Some(details) = tab.details.as_ref() else {
            show_details_pending(ui, tab.details_request.is_some());
            return;
        };
        if !details.is_writable {
            widgets::card(ui, |ui| {
                ui.label(
                    RichText::new("该格式不支持写入 EXIF")
                        .size(14.0)
                        .strong()
                        .color(widgets::TEXT),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("支持写入的格式：JPEG、PNG、TIFF、WebP、HEIF、AVIF、JXL。")
                        .size(12.0)
                        .color(widgets::MUTED),
                );
            });
            return;
        }
        let Some(original) = details.original.clone() else {
            let error = details
                .write_error
                .clone()
                .unwrap_or_else(|| "无法读取可编辑的 EXIF 字段".to_string());
            warning_card(ui, "EXIF 写入不可用", &error);
            return;
        };
        let mut editable = details.editable.clone().unwrap_or_else(|| original.clone());
        let last_saved = details.last_saved.clone();
        let mut backup = self.settings.backup;
        let mut save_clicked = false;
        let mut reset_clicked = false;
        let mut insert_now = false;
        let matched_brand = known_camera_brand(&editable.make);
        let mut brand_custom =
            details.brand_custom || (!editable.make.trim().is_empty() && matched_brand.is_none());
        let dirty = editable != original;

        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                let actions_width = 250.0;
                let title_width = (ui.available_width() - actions_width).max(180.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(title_width, 46.0),
                    Layout::top_down(Align::LEFT),
                    |ui| {
                        ui.label(
                            RichText::new("编辑 EXIF")
                                .size(15.5)
                                .strong()
                                .color(widgets::TEXT),
                        );
                        ui.add_space(2.0);
                        ui.add(
                            Label::new(
                                RichText::new(
                                    "修改会写入原文件；所有字段均可留空，留空表示不写入该项。",
                                )
                                .size(11.0)
                                .color(widgets::MUTED),
                            )
                            .truncate(),
                        );
                    },
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            dirty,
                            Button::new(RichText::new("保存修改").size(13.5).color(widgets::BG))
                                .fill(widgets::ACCENT),
                        )
                        .hover_tip("把当前修改写入文件")
                        .clicked()
                    {
                        save_clicked = true;
                    }
                    if ui
                        .add_enabled(
                            dirty,
                            Button::new(RichText::new("撤销未保存修改").size(12.0))
                                .fill(widgets::PANEL_SOFT),
                        )
                        .hover_tip(if dirty {
                            "放弃本次编辑，恢复到文件当前内容"
                        } else {
                            "没有未保存的修改"
                        })
                        .clicked()
                    {
                        reset_clicked = true;
                    }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.checkbox(
                    &mut backup,
                    RichText::new("写入前创建 .bak 备份").size(11.5),
                );
                ui.add_space(8.0);
                if dirty {
                    ui.label(
                        RichText::new("有未保存的修改")
                            .size(11.0)
                            .color(widgets::ACCENT),
                    );
                } else if let Some(message) = &last_saved {
                    ui.label(RichText::new(message).size(11.0).color(widgets::GREEN));
                }
            });
        });

        ui.add_space(12.0);
        widgets::card(ui, |ui| {
            widgets::section_header(ui, "描述与署名", "所有字段均为选填，留空即不写入");
            labeled_field(
                ui,
                "图像描述",
                true,
                "例如：傍晚的城市天际线",
                &mut editable.description,
            );
            ui.columns(2, |columns| {
                labeled_field(
                    &mut columns[0],
                    "作者",
                    true,
                    "摄影师姓名",
                    &mut editable.artist,
                );
                labeled_field(
                    &mut columns[1],
                    "版权",
                    true,
                    "例如 © 2026 for404cat",
                    &mut editable.copyright,
                );
                labeled_field(
                    &mut columns[0],
                    "处理软件",
                    true,
                    "例如 Adobe Lightroom",
                    &mut editable.software,
                );
            });
        });

        ui.add_space(12.0);
        widgets::card(ui, |ui| {
            widgets::section_header(ui, "设备与镜头", "从品牌开始填写，型号和镜头可留空");
            ui.columns(2, |columns| {
                let brand = &mut columns[0];
                field_label(brand, "相机品牌", false);
                let selected_brand = if editable.make.trim().is_empty() {
                    "未设置"
                } else if brand_custom {
                    "其他"
                } else {
                    matched_brand.unwrap_or("其他")
                };
                ComboBox::from_id_salt(("camera-brand", tab.id))
                    .selected_text(selected_brand)
                    .width(brand.available_width())
                    .show_ui(brand, |ui| {
                        if ui
                            .selectable_label(selected_brand == "未设置", "未设置（不写入品牌）")
                            .clicked()
                            && !editable.make.trim().is_empty()
                        {
                            editable.make.clear();
                            brand_custom = false;
                        }
                        for candidate in CAMERA_BRANDS {
                            if ui
                                .selectable_label(selected_brand == candidate, candidate)
                                .clicked()
                                && selected_brand != candidate
                            {
                                if candidate == "其他" {
                                    brand_custom = true;
                                } else {
                                    editable.make = candidate.to_string();
                                    brand_custom = false;
                                }
                            }
                        }
                    });
                if brand_custom {
                    brand.add_space(6.0);
                    labeled_field(
                        brand,
                        "自定义品牌",
                        false,
                        "输入相机品牌",
                        &mut editable.make,
                    );
                } else if editable.make.trim().is_empty() {
                    brand.add_space(8.0);
                    brand.label(
                        RichText::new("留空时不会写入 Make 字段")
                            .size(10.0)
                            .color(Color32::from_rgb(104, 112, 124)),
                    );
                    brand.add_space(8.0);
                } else {
                    brand.add_space(8.0);
                    brand.label(
                        RichText::new("写入 Make 字段")
                            .size(10.0)
                            .color(Color32::from_rgb(104, 112, 124)),
                    );
                    brand.add_space(8.0);
                }
                labeled_field(
                    &mut columns[1],
                    "相机型号",
                    true,
                    "例如 ILCE-7M4、EOS R5、X100V",
                    &mut editable.model,
                );
                labeled_field(
                    &mut columns[0],
                    "镜头品牌",
                    true,
                    "例如 SONY、SIGMA",
                    &mut editable.lens_make,
                );
                labeled_field(
                    &mut columns[1],
                    "镜头型号",
                    true,
                    "例如 FE 24-70mm F2.8 GM II",
                    &mut editable.lens_model,
                );
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("方向")
                        .size(11.5)
                        .strong()
                        .color(widgets::MUTED),
                );
                ui.add_space(6.0);
                ComboBox::from_id_salt("orientation")
                    .selected_text(orientation_label(editable.orientation))
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        for (value, label) in orientation_options() {
                            ui.selectable_value(&mut editable.orientation, value, label);
                        }
                    });
                ui.label(
                    RichText::new("仅影响查看器的旋转方向，不会旋转像素本身。")
                        .size(10.0)
                        .color(widgets::MUTED),
                );
            });
        });

        ui.add_space(12.0);
        widgets::card(ui, |ui| {
            widgets::section_header(ui, "拍摄参数", "数值会自动转换为 EXIF 有理数");
            ui.columns(3, |columns| {
                unit_field(
                    &mut columns[0],
                    "ISO 感光度",
                    None,
                    None,
                    "例如 100",
                    &mut editable.iso,
                );
                unit_field(
                    &mut columns[1],
                    "光圈",
                    Some("f /"),
                    None,
                    "例如 2.8",
                    &mut editable.f_number,
                );
                unit_field(
                    &mut columns[2],
                    "快门速度",
                    None,
                    Some("s"),
                    "例如 1/125",
                    &mut editable.exposure_time,
                );
                unit_field(
                    &mut columns[0],
                    "焦距",
                    None,
                    Some("mm"),
                    "例如 35",
                    &mut editable.focal_length,
                );
                unit_field(
                    &mut columns[1],
                    "35mm 等效焦距",
                    None,
                    Some("mm"),
                    "例如 52",
                    &mut editable.focal_length_35mm,
                );
            });
        });

        ui.add_space(12.0);
        widgets::card(ui, |ui| {
            widgets::section_header(ui, "时间信息", "使用日期选择器编辑，写入时自动转换格式");
            datetime_field(ui, &mut editable.date_time_original);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("时区偏移")
                        .size(11.5)
                        .strong()
                        .color(widgets::MUTED),
                );
                ui.add_space(6.0);
                ui.add(
                    TextEdit::singleline(&mut editable.offset_time_original)
                        .hint_text("+08:00")
                        .desired_width(110.0),
                );
                ui.add_space(10.0);
                if ui
                    .add(Button::new("使用当前时间").small().frame(false))
                    .clicked()
                {
                    insert_now = true;
                }
                if !editable.date_time_original.trim().is_empty()
                    && ui
                        .add(Button::new("清除时间").small().frame(false))
                        .clicked()
                {
                    editable.date_time_original.clear();
                }
            });
        });

        ui.add_space(12.0);
        widgets::card(ui, |ui| {
            widgets::section_header(ui, "GPS 位置", "十进制度 · 留空则不写入");
            ui.columns(3, |columns| {
                labeled_field(
                    &mut columns[0],
                    "纬度",
                    true,
                    "31.2304（北纬为正）",
                    &mut editable.gps_latitude,
                );
                labeled_field(
                    &mut columns[1],
                    "经度",
                    true,
                    "121.4737（东经为正）",
                    &mut editable.gps_longitude,
                );
                labeled_field(
                    &mut columns[2],
                    "海拔",
                    true,
                    "12.5（米）",
                    &mut editable.gps_altitude,
                );
            });
            let latitude = editable.gps_latitude.trim().parse::<f64>().ok();
            let longitude = editable.gps_longitude.trim().parse::<f64>().ok();
            let invalid = latitude.is_some_and(|value| !(-90.0..=90.0).contains(&value))
                || longitude.is_some_and(|value| !(-180.0..=180.0).contains(&value));
            ui.horizontal(|ui| {
                if invalid {
                    ui.label(
                        RichText::new("纬度需在 -90…90，经度需在 -180…180。")
                            .size(10.5)
                            .color(widgets::RED),
                    );
                } else {
                    ui.label(
                        RichText::new("北纬和东经为正数；南纬和西经为负数。")
                            .size(10.5)
                            .color(widgets::MUTED),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if (!editable.gps_latitude.trim().is_empty()
                        || !editable.gps_longitude.trim().is_empty()
                        || !editable.gps_altitude.trim().is_empty())
                        && ui
                            .add(Button::new("清除位置").small().frame(false))
                            .clicked()
                    {
                        editable.gps_latitude.clear();
                        editable.gps_longitude.clear();
                        editable.gps_altitude.clear();
                    }
                });
            });
        });

        ui.add_space(12.0);
        widgets::card(ui, |ui| {
            widgets::section_header(ui, "用户备注", "写入 UserComment 字段");
            ui.add(
                TextEdit::multiline(&mut editable.user_comment)
                    .hint_text("例如：拍摄于第一次自驾进藏途中")
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
        });

        ui.add_space(12.0);
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                if dirty {
                    ui.label(
                        RichText::new("修改尚未保存到文件")
                            .size(11.5)
                            .color(widgets::ACCENT),
                    );
                } else {
                    ui.label(
                        RichText::new("当前内容与文件一致")
                            .size(11.5)
                            .color(widgets::MUTED),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            dirty,
                            Button::new(RichText::new("保存修改").size(13.0).color(widgets::BG))
                                .fill(widgets::ACCENT),
                        )
                        .clicked()
                    {
                        save_clicked = true;
                    }
                    if ui
                        .add_enabled(
                            dirty,
                            Button::new(RichText::new("撤销").size(12.0)).fill(widgets::PANEL_SOFT),
                        )
                        .clicked()
                    {
                        reset_clicked = true;
                    }
                });
            });
        });

        if insert_now {
            let now = chrono::Local::now();
            editable.date_time_original = now.format("%Y:%m:%d %H:%M:%S").to_string();
            editable.offset_time_original = now.format("%:z").to_string();
        }
        if backup != self.settings.backup {
            self.settings.backup = backup;
            self.settings_draft.backup = backup;
            self.settings.save();
        }
        if save_clicked {
            if let Some(details) = tab.details.as_mut() {
                details.editable = Some(editable);
            }
            self.save_selected(tab);
            return;
        }
        if reset_clicked {
            if let Some(details) = tab.details.as_mut() {
                details.editable = details.original.clone();
                details.write_error = None;
                details.brand_custom = details.original.as_ref().is_some_and(|fields| {
                    !fields.make.trim().is_empty() && known_camera_brand(&fields.make).is_none()
                });
            }
            self.set_toast(ToastKind::Info, "已撤销未保存的修改");
            ui.ctx().request_repaint();
            return;
        }
        if let Some(details) = tab.details.as_mut() {
            details.editable = Some(editable);
            details.brand_custom = brand_custom;
        }
    }

    fn show_toast(&mut self, ctx: &egui::Context) {
        let Some(toast) = &self.toast else {
            return;
        };
        let duration = match toast.kind {
            ToastKind::Error => Duration::from_secs(9),
            _ => Duration::from_secs(6),
        };
        if toast.created.elapsed() > duration {
            self.toast = None;
            return;
        }
        let color = match toast.kind {
            ToastKind::Info => widgets::TEAL,
            ToastKind::Success => widgets::GREEN,
            ToastKind::Error => widgets::RED,
        };
        let elapsed = toast.created.elapsed().as_secs_f32();
        let remaining = duration.as_secs_f32() - elapsed;
        let appear = (elapsed / 0.18).clamp(0.0, 1.0);
        let appear = 1.0 - (1.0 - appear).powi(3);
        let fade_out = (remaining / 0.25).clamp(0.0, 1.0);
        let opacity = appear.min(fade_out);
        if appear < 1.0 || fade_out < 1.0 {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        egui::Area::new(Id::unique("toast"))
            .anchor(
                Align2::RIGHT_BOTTOM,
                Vec2::new(-20.0 - (1.0 - appear) * 22.0, -44.0),
            )
            .order(Order::Foreground)
            .show(ctx, |ui| {
                ui.set_opacity(opacity);
                Frame::new()
                    .fill(widgets::PANEL)
                    .stroke(Stroke::new(1.0, color))
                    .corner_radius(10)
                    .inner_margin(Margin::symmetric(14, 9))
                    .show(ui, |ui| {
                        ui.set_max_width(480.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").size(11.0).color(color));
                            ui.add(
                                Label::new(
                                    RichText::new(&toast.text).size(12.5).color(widgets::TEXT),
                                )
                                .wrap(),
                            );
                        });
                    });
            });
    }

    fn show_unsaved_dialog(&mut self, ctx: &egui::Context) {
        if self.pending_action.is_none() {
            return;
        }
        if !self.modal_ready(ctx) {
            return;
        }
        let Some(pending) = &self.pending_action else {
            return;
        };
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == pending.tab_id) else {
            self.pending_action = None;
            return;
        };
        let name = folder_title(&tab.folder);
        let action_label = match pending.action {
            TabAction::Select(_) => "切换文件",
            TabAction::Rescan => "重新扫描",
            TabAction::Close => "关闭标签页",
        };
        let mut cancel = false;
        let mut discard = false;
        egui::Window::new("未保存的修改")
            .collapsible(false)
            .resizable(false)
            .order(Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .min_width(400.0)
            .max_width(400.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("标签页「{name}」中有未保存的 EXIF 修改。"))
                        .size(13.0)
                        .color(widgets::TEXT),
                );
                ui.label(
                    RichText::new(format!("继续{action_label}将丢失这些修改。"))
                        .size(12.0)
                        .color(widgets::MUTED),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(Button::new("取消").fill(widgets::PANEL_SOFT))
                        .clicked()
                    {
                        cancel = true;
                    }
                    if ui
                        .add(
                            Button::new(RichText::new("放弃修改并继续").color(widgets::BG))
                                .fill(widgets::RED),
                        )
                        .clicked()
                    {
                        discard = true;
                    }
                });
            });
        if cancel {
            self.pending_action = None;
        }
        if discard {
            self.perform_pending_action();
        }
    }

    fn show_external_open_dialog(&mut self, ctx: &egui::Context) {
        if self.pending_open_video.is_none() {
            return;
        }
        if !self.modal_ready(ctx) {
            return;
        }
        let Some(path) = self.pending_open_video.clone() else {
            return;
        };
        let mut open = true;
        let mut cancel = false;
        let mut confirm = false;
        egui::Window::new("用外部播放器打开")
            .id(Id::unique("external-open-dialog"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(430.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("将在系统默认播放器中打开这个视频")
                        .size(14.0)
                        .strong()
                        .color(widgets::TEXT),
                );
                ui.add_space(6.0);
                ui.add(
                    Label::new(
                        RichText::new(path.display().to_string())
                            .size(11.0)
                            .color(widgets::MUTED),
                    )
                    .truncate(),
                )
                .hover_tip(path.display().to_string());
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Exif Studio 不接管播放器窗口；播放结束后切回本应用即可继续管理元数据。",
                    )
                    .size(10.5)
                    .color(widgets::MUTED),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            Button::new(RichText::new("打开").color(widgets::BG))
                                .fill(widgets::ACCENT),
                        )
                        .clicked()
                    {
                        confirm = true;
                    }
                    if ui
                        .add(Button::new("取消").fill(widgets::PANEL_SOFT))
                        .clicked()
                    {
                        cancel = true;
                    }
                });
            });
        if confirm {
            platform::open_file(&path);
            self.pending_open_video = None;
            self.set_toast(ToastKind::Success, "已交给系统播放器打开");
        } else if cancel || !open {
            self.pending_open_video = None;
        }
    }

    fn start_watermark_embed(&mut self) {
        let Some(dialog) = self.watermark_dialog.as_mut() else {
            return;
        };
        if dialog.busy {
            return;
        }
        let text = dialog.text.trim().to_string();
        if text.is_empty() {
            dialog.message = Some((false, "请先输入水印内容".to_string()));
            return;
        }
        let folder = match watermark::validate_folder_name(&dialog.folder) {
            Ok(folder) => folder,
            Err(error) => {
                dialog.message = Some((false, error));
                return;
            }
        };
        let source = dialog.path.clone();
        let output = match watermark::default_output_path(&source, &folder) {
            Ok(output) => output,
            Err(error) => {
                dialog.message = Some((false, error));
                return;
            }
        };
        let passphrase = dialog.passphrase.clone();
        let (tx, rx) = mpsc::channel();
        dialog.folder = folder.clone();
        dialog.output_path = None;
        dialog.busy = true;
        dialog.receiver = Some(rx);
        dialog.message = None;
        dialog.extracted = None;
        if self.settings.watermark_folder != folder {
            self.settings.watermark_folder = folder;
            self.settings_draft.watermark_folder = self.settings.watermark_folder.clone();
            self.settings.save();
        }
        std::thread::Builder::new()
            .name("exif-watermark-embed".to_string())
            .spawn(move || {
                let result = watermark::embed(&source, &output, &text, &passphrase)
                    .map(WatermarkOutcome::Embedded);
                let _ = tx.send(result);
            })
            .expect("无法启动水印线程");
    }

    fn start_watermark_extract(&mut self) {
        let Some(dialog) = self.watermark_dialog.as_mut() else {
            return;
        };
        if dialog.busy {
            return;
        }
        let path = dialog.path.clone();
        let passphrase = dialog.passphrase.clone();
        let records = self.watermark_index.records().to_vec();
        let (tx, rx) = mpsc::channel();
        dialog.busy = true;
        dialog.receiver = Some(rx);
        dialog.message = None;
        dialog.extracted = None;
        std::thread::Builder::new()
            .name("exif-watermark-extract".to_string())
            .spawn(move || {
                let result = watermark::recognize(&path, &passphrase, &records)
                    .map(WatermarkOutcome::Extracted);
                let _ = tx.send(result);
            })
            .expect("无法启动水印线程");
    }

    fn poll_watermark(&mut self, ctx: &egui::Context) {
        let mut result = None;
        let mut disconnected = false;
        if let Some(dialog) = self.watermark_dialog.as_mut()
            && let Some(receiver) = dialog.receiver.as_ref()
        {
            match receiver.try_recv() {
                Ok(value) => result = Some(value),
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => disconnected = true,
            }
            dialog.busy = false;
            dialog.receiver = None;
        }
        if disconnected {
            if let Some(dialog) = self.watermark_dialog.as_mut() {
                dialog.message = Some((false, "水印任务意外结束".to_string()));
            }
            ctx.request_repaint();
            return;
        }
        let Some(result) = result else {
            return;
        };
        let mut embedded = None;
        let mut record = None;
        if let Some(dialog) = self.watermark_dialog.as_mut() {
            match result {
                Ok(WatermarkOutcome::Embedded(outcome)) => {
                    let path = outcome.path.clone();
                    let message = if outcome.metadata_copied {
                        "水印副本已生成".to_string()
                    } else {
                        "水印副本已生成（原文件没有可复制的 EXIF）".to_string()
                    };
                    dialog.path = path.clone();
                    dialog.output_path = Some(path.clone());
                    dialog.message = Some((true, message));
                    dialog.extracted = None;
                    embedded = Some((dialog.tab_id, path));
                    record = Some(outcome.record);
                }
                Ok(WatermarkOutcome::Extracted(Some(Recognition::Embedded(text)))) => {
                    dialog.message = Some((true, "已识别隐写水印".to_string()));
                    dialog.extracted = Some(text);
                }
                Ok(WatermarkOutcome::Extracted(Some(Recognition::Visual(text)))) => {
                    dialog.message = Some((
                        true,
                        "已通过视觉指纹识别水印（适用于截图/二次压缩）".to_string(),
                    ));
                    dialog.extracted = Some(text);
                }
                Ok(WatermarkOutcome::Extracted(None)) => {
                    dialog.message = Some((false, "未检测到水印（或口令不正确）".to_string()));
                    dialog.extracted = None;
                }
                Err(error) => {
                    dialog.message = Some((false, error));
                    dialog.extracted = None;
                }
            }
        }
        if let Some(record) = record {
            self.watermark_index.add(record);
            self.watermark_index.save();
        }
        if let Some((tab_id, path)) = embedded {
            self.register_watermark_output(tab_id, &path);
        }
        ctx.request_repaint();
    }

    fn register_watermark_output(&mut self, tab_id: u64, path: &Path) {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };
        if !path.starts_with(&tab.folder)
            || (!tab.recursive && path.parent() != Some(tab.folder.as_path()))
            || tab.entries.iter().any(|entry| entry.path == path)
        {
            return;
        }
        let entry = scan::load_entry(path, &self.meta_cache);
        tab.entries.push(entry);
        tab.entries
            .sort_by_cached_key(|entry| (entry.name.to_lowercase(), entry.path.clone()));
        tab.filter_dirty = true;
        tab.stats_dirty = true;
        self.api_dirty = true;
    }

    fn show_watermark_dialog(&mut self, ctx: &egui::Context) {
        if self.watermark_dialog.is_none() {
            return;
        }
        if !self.modal_ready(ctx) {
            return;
        }
        let Some(dialog) = self.watermark_dialog.as_mut() else {
            return;
        };
        let mut open = true;
        let mut embed_clicked = false;
        let mut extract_clicked = false;
        let busy = dialog.busy;
        egui::Window::new("隐写水印")
            .id(Id::unique("watermark-dialog"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.label(RichText::new("文件").size(10.5).color(widgets::MUTED));
                ui.add(
                    Label::new(
                        RichText::new(dialog.path.display().to_string())
                            .size(11.5)
                            .color(widgets::TEXT),
                    )
                    .truncate(),
                )
                .on_hover_text(dialog.path.display().to_string());
                ui.add_space(8.0);
                ui.label(
                    RichText::new("输出子文件夹")
                        .size(11.0)
                        .color(widgets::MUTED),
                );
                ui.add_space(3.0);
                ui.add(
                    TextEdit::singleline(&mut dialog.folder)
                        .hint_text("watermarked")
                        .desired_width(220.0),
                );
                ui.label(
                    RichText::new("会在原图所在目录下创建该子文件夹，名称可自定义。")
                        .size(10.0)
                        .color(widgets::MUTED),
                );
                ui.add_space(10.0);
                ui.label(RichText::new("水印内容").size(11.0).color(widgets::MUTED));
                ui.add_space(3.0);
                ui.add(
                    TextEdit::singleline(&mut dialog.text)
                        .hint_text("例如：© 2026 摄影作者 · 禁止转载")
                        .char_limit(4096)
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(8.0);
                ui.label(RichText::new("口令（可选）").size(11.0).color(widgets::MUTED));
                ui.add_space(3.0);
                ui.add(
                    TextEdit::singleline(&mut dialog.passphrase)
                        .password(true)
                        .hint_text("识别时必须输入相同口令")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "写入会在自定义子文件夹中生成 8-bit PNG 副本，原图不会被修改。图片同时登记本地视觉指纹：截图、缩放或二次压缩可在本机通过视觉匹配识别；口令仅用于打散水印位，不是加密。",
                    )
                    .size(10.5)
                    .color(widgets::MUTED),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !busy && !dialog.text.trim().is_empty(),
                            Button::new(RichText::new("写入水印副本").size(13.0).color(widgets::BG))
                                .fill(widgets::ACCENT),
                        )
                        .clicked()
                    {
                        embed_clicked = true;
                    }
                    if ui
                        .add_enabled(
                            !busy,
                            Button::new(RichText::new("识别水印（含截图匹配）").size(12.5))
                                .fill(widgets::PANEL_SOFT),
                        )
                        .clicked()
                    {
                        extract_clicked = true;
                    }
                    if busy {
                        ui.spinner();
                        ui.label(
                            RichText::new("正在处理图片…").size(11.0).color(widgets::MUTED),
                        );
                    }
                });
                if let Some((success, message)) = &dialog.message {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(message)
                            .size(11.0)
                            .color(if *success { widgets::GREEN } else { widgets::RED }),
                    );
                }
                if let Some(output) = &dialog.output_path {
                    ui.add_space(6.0);
                    ui.label(RichText::new("输出文件").size(10.5).color(widgets::MUTED));
                    let response = ui
                        .add(
                            Label::new(
                                RichText::new(output.display().to_string())
                                    .size(10.5)
                                    .color(widgets::ACCENT)
                                    .underline(),
                            )
                            .sense(Sense::click())
                            .wrap(),
                        )
                        .on_hover_text("点击打开所在文件夹");
                    if response.clicked()
                        && let Some(parent) = output.parent()
                    {
                        platform::open_directory(parent);
                    }
                    if ui.add(Button::new("打开输出文件夹").small()).clicked()
                        && let Some(parent) = output.parent()
                    {
                        platform::open_directory(parent);
                    }
                }
                if let Some(text) = &dialog.extracted {
                    ui.add_space(8.0);
                    ui.label(RichText::new("识别结果").size(11.0).color(widgets::MUTED));
                    ui.add_space(3.0);
                    let mut text = text.clone();
                    ui.add(
                        TextEdit::multiline(&mut text)
                            .interactive(false)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                }
            });
        if !open {
            self.watermark_dialog = None;
            return;
        }
        if embed_clicked {
            self.start_watermark_embed();
        }
        if extract_clicked {
            self.start_watermark_extract();
        }
    }

    fn show_settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        if !self.modal_ready(ctx) {
            return;
        }
        let mut draft = self.settings_draft.clone();
        let saved_token = self.settings.api_token.clone();
        let saved_port = self.settings.api_port;
        let saved_enabled = self.settings.api_enabled;
        let usage = self.cache.usage_bytes();
        let metadata_size = self.meta_cache.file_size();
        let metadata_records = self.meta_cache.len();
        let api_running = self.api_server.is_some();
        let api_port = self.api_server.as_ref().map(|server| server.port);
        let api_error = self.api_error.clone();
        let any_scanning = self.tabs.iter().any(|tab| tab.scanning);

        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let mut choose_cache = false;
        let mut default_cache = false;
        let mut clear_cache = false;
        let mut open_cache = false;
        let mut copy_token = false;
        let mut regenerate_token = false;

        let settings_height = (ctx.content_rect().height() * 0.82).clamp(420.0, 720.0);
        egui::Window::new("设置")
            .id(Id::unique("settings-window"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .fixed_size(Vec2::new(620.0, settings_height))
            .show(ctx, |ui| {
                let footer_height = 46.0;
                ScrollArea::vertical()
                    .id_salt("settings-scroll")
                    .max_height((ui.available_height() - footer_height).max(200.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("缓存")
                                .size(15.5)
                                .strong()
                                .color(widgets::TEXT),
                        );
                        ui.add_space(8.0);
                        widgets::card(ui, |ui| {
                            ui.label(
                                RichText::new("缓存文件位置设置")
                                    .size(12.0)
                                    .strong()
                                    .color(widgets::TEXT),
                            );
                            ui.add_space(3.0);
                            let cache_path = draft.cache_dir().display().to_string();
                            ui.add(
                                Label::new(
                                    RichText::new(&cache_path)
                                        .size(11.5)
                                        .color(widgets::MUTED),
                                )
                                .truncate(),
                            )
                            .on_hover_text(cache_path);
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if ui.button("选择目录…").clicked() {
                                    choose_cache = true;
                                }
                                if ui.button("使用默认位置").clicked() {
                                    default_cache = true;
                                }
                                if ui.button("打开目录").clicked() {
                                    open_cache = true;
                                }
                            });
                            if any_scanning {
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new("扫描进行中，缓存目录暂不可切换。")
                                        .size(10.5)
                                        .color(widgets::ACCENT),
                                );
                            }
                            ui.add_space(12.0);
                            ui.label(
                                RichText::new("缓存大小上限设置")
                                    .size(12.0)
                                    .strong()
                                    .color(widgets::TEXT),
                            );
                            ui.add_space(3.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                for preset in [512u64, 1024, 2048, 4096, 8192] {
                                    let label = if preset >= 1024 {
                                        format!("{} GB", preset / 1024)
                                    } else {
                                        format!("{preset} MB")
                                    };
                                    if ui
                                        .selectable_label(
                                            draft.cache_limit_mb == preset,
                                            RichText::new(label).size(11.5),
                                        )
                                        .clicked()
                                    {
                                        draft.cache_limit_mb = preset;
                                    }
                                }
                            });
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("自定义上限").size(10.5).color(widgets::MUTED),
                                );
                                ui.add(
                                    egui::DragValue::new(&mut draft.cache_limit_mb)
                                        .range(settings::cache_limit_range_mb())
                                        .speed(64.0)
                                        .suffix(" MB"),
                                );
                            });
                            ui.label(
                                RichText::new("限制缩略图与预览的磁盘缓存；元数据与视觉指纹缓存另行限制。")
                                    .size(10.5)
                                    .color(widgets::MUTED),
                            );
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new(format!(
                                    "当前占用：缩略图/预览 {}  ·  元数据 {} / {} 条",
                                    format_bytes(usage),
                                    format_bytes(metadata_size),
                                    metadata_records
                                ))
                                .size(11.0)
                                .color(widgets::MUTED),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "视觉指纹：{} 条",
                                    self.vision_cache.len()
                                ))
                                .size(11.0)
                                .color(widgets::MUTED),
                            );
                            ui.add_space(6.0);
                            if ui
                                .add(
                                    Button::new(
                                        RichText::new("清理当前缓存").color(widgets::RED),
                                    )
                                    .fill(Color32::TRANSPARENT),
                                )
                                .clicked()
                            {
                                clear_cache = true;
                            }
                        });

                        ui.add_space(12.0);
                        ui.label(
                            RichText::new("编辑与显示")
                                .size(15.5)
                                .strong()
                                .color(widgets::TEXT),
                        );
                        ui.add_space(8.0);
                        widgets::card(ui, |ui| {
                            ui.checkbox(
                                &mut draft.backup,
                                RichText::new("写入 EXIF 前创建 .bak 备份").size(12.0),
                            );
                            ui.add_space(4.0);
                            ui.checkbox(
                                &mut draft.show_raw_tags,
                                RichText::new("显示原始 Tag(...) 字段").size(12.0),
                            );
                            ui.label(
                                RichText::new("关闭时仅隐藏未能映射为中文名称的底层标签。")
                                    .size(10.5)
                                    .color(widgets::MUTED),
                            );
                        });

                        ui.add_space(12.0);
                        ui.label(
                            RichText::new("查重展示方式")
                                .size(15.5)
                                .strong()
                                .color(widgets::TEXT),
                        );
                        ui.add_space(8.0);
                        widgets::card(ui, |ui| {
                            ui.columns(2, |columns| {
                                if duplicates_layout_option(
                                    &mut columns[0],
                                    DuplicatesLayout::Waterfall,
                                    draft.duplicates_layout == DuplicatesLayout::Waterfall,
                                )
                                .clicked()
                                {
                                    draft.duplicates_layout = DuplicatesLayout::Waterfall;
                                }
                                if duplicates_layout_option(
                                    &mut columns[1],
                                    DuplicatesLayout::Paged,
                                    draft.duplicates_layout == DuplicatesLayout::Paged,
                                )
                                .clicked()
                                {
                                    draft.duplicates_layout = DuplicatesLayout::Paged;
                                }
                            });
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(
                                    "瀑布流适合快速浏览大量相似组；分页展示适合逐页核对与批量处理。切换后点击“保存并应用”生效。",
                                )
                                .size(10.5)
                                .color(widgets::MUTED),
                            );
                        });

                        ui.add_space(12.0);
                        ui.label(
                            RichText::new("隐写水印")
                                .size(15.5)
                                .strong()
                                .color(widgets::TEXT),
                        );
                        ui.add_space(8.0);
                        widgets::card(ui, |ui| {
                            ui.label(
                                RichText::new("水印输出子文件夹")
                                    .size(12.0)
                                    .strong()
                                    .color(widgets::TEXT),
                            );
                            ui.add_space(3.0);
                            ui.add(
                                TextEdit::singleline(&mut draft.watermark_folder)
                                    .hint_text("watermarked")
                                    .desired_width(220.0),
                            );
                            ui.label(
                                RichText::new(
                                    "水印副本会写入原图所在目录下的该子文件夹；留空或无效名称会恢复为 watermarked。",
                                )
                                .size(10.5)
                                .color(widgets::MUTED),
                            );
                        });

                        ui.add_space(12.0);
                        ui.label(
                            RichText::new("本地 API（开放端口）")
                                .size(15.5)
                                .strong()
                                .color(widgets::TEXT),
                        );
                        ui.add_space(8.0);
                        widgets::card(ui, |ui| {
                            ui.checkbox(
                                &mut draft.api_enabled,
                                RichText::new("启用仅限本机的只读元数据接口").size(12.0),
                            );
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("端口").size(11.5).color(widgets::MUTED),
                                );
                                ui.add(
                                    egui::DragValue::new(&mut draft.api_port)
                                        .range(1024..=65535)
                                        .speed(1.0),
                                );
                                ui.add_space(12.0);
                                let status = if api_running {
                                    format!(
                                        "● 运行中 · 127.0.0.1:{}",
                                        api_port.unwrap_or(saved_port)
                                    )
                                } else if saved_enabled {
                                    "● 未启动".to_string()
                                } else {
                                    "○ 已关闭".to_string()
                                };
                                ui.label(
                                    RichText::new(status).size(11.5).color(
                                        if api_running {
                                            widgets::GREEN
                                        } else if saved_enabled {
                                            widgets::RED
                                        } else {
                                            widgets::MUTED
                                        },
                                    ),
                                );
                            });
                            if let Some(error) = &api_error {
                                ui.label(
                                    RichText::new(error)
                                        .size(10.5)
                                        .color(widgets::RED),
                                );
                            }
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("Bearer Token")
                                    .size(11.5)
                                    .color(widgets::MUTED),
                            );
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(token_hint(&draft.api_token))
                                        .size(11.5)
                                        .monospace()
                                        .color(widgets::TEXT),
                                );
                                if ui
                                    .add_enabled(
                                        !draft.api_token.is_empty(),
                                        Button::new("复制 Token").small(),
                                    )
                                    .clicked()
                                {
                                    copy_token = true;
                                }
                                if ui.add(Button::new("重新生成").small()).clicked() {
                                    regenerate_token = true;
                                }
                            });
                            if draft.api_token != saved_token {
                                ui.label(
                                    RichText::new("Token 已变更，保存后启动或重启接口才会生效。")
                                        .size(10.5)
                                        .color(widgets::ACCENT),
                                );
                            }
                            ui.add_space(10.0);
                            widgets::divider(ui);
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("安全边界")
                                    .size(12.0)
                                    .strong()
                                    .color(widgets::TEXT),
                            );
                            for line in [
                                "仅监听 127.0.0.1 回环地址，不接受外部网络直连。",
                                "所有请求都必须携带 Bearer Token；只提供只读 GET 元数据。",
                                "仅能查询当前标签页中已打开文件夹的元数据，不接受任意文件路径。",
                                "不启用 CORS；不要把该端口反向代理、隧道或转发到公网。",
                                "Token 保存在当前用户配置目录中。同等权限的本机恶意程序仍可能读取配置，因此只在确有需要时开启。",
                            ] {
                                ui.label(
                                    RichText::new(format!("· {line}"))
                                        .size(10.5)
                                        .color(widgets::MUTED),
                                );
                            }
                        });
                    });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(Button::new(RichText::new("保存并应用").color(widgets::BG)).fill(widgets::ACCENT))
                        .clicked()
                    {
                        apply = true;
                    }
                    if ui
                        .add(Button::new("取消").fill(widgets::PANEL_SOFT))
                        .clicked()
                    {
                        cancel = true;
                    }
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("部分设置需要重新扫描才会影响已读取的数据。")
                            .size(10.5)
                            .color(widgets::MUTED),
                    );
                });
            });

        if choose_cache
            && let Some(path) = rfd::FileDialog::new()
                .set_title("选择缓存目录")
                .set_directory(draft.cache_dir())
                .pick_folder()
        {
            draft.cache_dir = Some(path);
        }
        if default_cache {
            draft.cache_dir = None;
        }
        if regenerate_token {
            draft.api_token = generate_token();
        }
        if copy_token {
            let token = if draft.api_token.is_empty() {
                saved_token.clone()
            } else {
                draft.api_token.clone()
            };
            if !token.is_empty() {
                ctx.copy_text(token);
                self.set_toast(ToastKind::Success, "Bearer Token 已复制到剪贴板");
            }
        }
        if open_cache {
            let directory = draft.cache_dir();
            let _ = std::fs::create_dir_all(&directory);
            platform::open_directory(&directory);
        }
        self.settings_draft = draft;

        if clear_cache {
            self.clear_caches();
            self.set_toast(ToastKind::Success, "当前缓存已清理");
        }
        if apply {
            self.apply_settings();
        } else if cancel || !open {
            self.show_settings = false;
        }
    }
}

impl Drop for ExifStudioApp {
    fn drop(&mut self) {
        self.meta_cache.save();
        self.vision_cache.save();
        self.watermark_index.save();
        self.settings.save();
    }
}

impl eframe::App for ExifStudioApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.prepare_modal(&ctx);
        let modal_open = self.modal_open();
        ctx.data_mut(|data| data.insert_temp(Id::unique("exif-modal-open"), modal_open));
        if !modal_open {
            self.handle_shortcuts(&ctx);
        }
        self.handle_dropped_files(&ctx);
        self.poll_folder_picker(&ctx);
        self.poll_scans(&ctx);
        self.poll_details(&ctx);
        self.poll_vision(&ctx);
        self.poll_watermark(&ctx);
        self.poll_images(&ctx);

        let draw_background = !self.modal_open() || self.modal_backdrop.is_none();
        if draw_background {
            self.show_top_bar(ui);
            self.show_tab_strip(ui);
            self.show_status_bar(ui);

            if let Some(index) = self.active {
                let _ = self.with_tab_mut(index, |app, is_active, tab| {
                    app.show_workspace_for(ui, tab, is_active);
                });
            } else {
                egui::CentralPanel::default()
                    .frame(
                        Frame::new()
                            .fill(widgets::BG)
                            .inner_margin(Margin::symmetric(22, 18)),
                    )
                    .show(ui, |ui| self.show_welcome(ui));
            }
        }

        self.show_settings_window(&ctx);
        self.show_watermark_dialog(&ctx);
        self.show_unsaved_dialog(&ctx);
        self.show_external_open_dialog(&ctx);
        self.draw_modal_backdrop(&ctx);
        self.show_toast(&ctx);
        if self.tabs.iter().any(|tab| tab.scan_queued) {
            self.launch_queued_scans();
        }
        self.publish_api(false);

        let busy = self.tabs.iter().any(|tab| tab.scanning)
            || self.tabs.iter().any(|tab| tab.details_request.is_some())
            || self.tabs.iter().any(|tab| tab.vision.is_some())
            || self
                .watermark_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.busy)
            || self.folder_picker.is_some()
            || !self.thumb_pending.is_empty()
            || !self.thumb_ready.is_empty()
            || self.preview.loading
            || self.toast.is_some()
            || self.pending_action.is_some();
        if busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self.api_dirty {
            ctx.request_repaint_after(API_PUBLISH_INTERVAL);
        }
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for candidate in platform::cjk_font_candidates() {
        if let Ok(bytes) = std::fs::read(candidate) {
            fonts
                .font_data
                .insert("cjk".to_string(), Arc::new(FontData::from_owned(bytes)));
            if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
                family.insert(1.min(family.len()), "cjk".to_string());
            }
            if let Some(family) = fonts.families.get_mut(&FontFamily::Monospace) {
                family.insert(1.min(family.len()), "cjk".to_string());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
}

fn image_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4)
        .div_ceil(2)
        .clamp(4, 8)
}

fn metadata_cache_path(root: &Path) -> PathBuf {
    root.join("metadata.json")
}

fn vision_cache_path(root: &Path) -> PathBuf {
    root.join("vision.json")
}

fn detect_vision_hardware(cc: &eframe::CreationContext<'_>) -> VisionHardware {
    let mut adapters = Vec::new();
    let mut gpu = "未检测到可用 GPU".to_string();
    if let Some(state) = &cc.wgpu_render_state {
        let info = state.adapter.get_info();
        gpu = format!(
            "{} · {:?} · {:?}",
            info.name, info.backend, info.device_type
        );
        for adapter in &state.available_adapters {
            let info = adapter.get_info();
            let label = format!("{} · {:?}", info.name, info.backend);
            if !adapters.contains(&label) {
                adapters.push(label);
            }
        }
    }
    VisionHardware {
        gpu,
        npu: vision::detect_npu(),
        adapters,
    }
}

fn default_collapsed_groups() -> HashSet<String> {
    [
        "基本与设备",
        "拍摄参数",
        "位置信息",
        "兼容信息",
        "视频概要",
        "容器元数据标签",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect()
}

fn entry_key(entry: &MediaEntry, variant: u32) -> u64 {
    file_key(&entry.path, entry.size, entry.modified_ns(), variant)
}

fn same_folder(left: &Path, right: &Path) -> bool {
    let normalize = |path: &Path| {
        let text = path.to_string_lossy().replace('/', "\\");
        text.trim_end_matches('\\').to_string()
    };
    let left = normalize(left);
    let right = normalize(right);
    if cfg!(windows) {
        left.eq_ignore_ascii_case(&right)
    } else {
        left == right
    }
}

fn folder_title(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

enum FolderEntry {
    Title,
    All,
    Group {
        path: PathBuf,
        text: String,
        tooltip: String,
        selected: bool,
    },
}

fn show_folder_group_chips(ui: &mut Ui, tab: &mut FolderTab, groups: &[(PathBuf, String, usize)]) {
    let selected = groups
        .iter()
        .position(|(path, _, _)| tab.folder_filter.as_deref() == Some(path.as_path()));
    let mut ordered = Vec::with_capacity(groups.len());
    if let Some(index) = selected {
        ordered.push(&groups[index]);
    }
    for (index, group) in groups.iter().enumerate() {
        if Some(index) != selected {
            ordered.push(group);
        }
    }
    let mut entries = vec![FolderEntry::Title, FolderEntry::All];
    for (path, label, count) in ordered {
        let text = format!("{}  {count}", widgets::truncate(label, 14));
        let tooltip = path
            .strip_prefix(&tab.folder)
            .unwrap_or(path)
            .display()
            .to_string();
        entries.push(FolderEntry::Group {
            path: path.clone(),
            text,
            tooltip,
            selected: tab.folder_filter.as_deref() == Some(path.as_path()),
        });
    }

    let text_width = |ui: &Ui, text: &str, size: f32| {
        ui.painter()
            .layout_no_wrap(text.to_string(), FontId::proportional(size), widgets::MUTED)
            .size()
            .x
    };
    let widths: Vec<f32> = entries
        .iter()
        .map(|entry| match entry {
            FolderEntry::Title => text_width(ui, "目录", 10.5) + 8.0,
            FolderEntry::All => text_width(ui, "全部目录", 10.0) + 22.0,
            FolderEntry::Group { text, .. } => text_width(ui, text, 10.0) + 22.0,
        })
        .collect();

    let available = ui.available_width().max(120.0);
    let spacing = 4.0;
    let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
    let mut used = 0.0;
    for (index, width) in widths.iter().enumerate() {
        let row_is_empty = rows.last().is_none_or(|row| row.is_empty());
        if !row_is_empty && used + width > available {
            rows.push(Vec::new());
            used = 0.0;
        }
        rows.last_mut()
            .expect("folder rows should not be empty")
            .push(index);
        used += width + spacing;
    }

    let expanded = tab.folder_groups_expanded;
    let total_rows = rows.len();
    let visible_rows = if expanded {
        total_rows
    } else {
        total_rows.min(2)
    };
    let hidden = if expanded {
        0
    } else {
        rows.iter()
            .skip(visible_rows)
            .flatten()
            .filter(|index| matches!(entries[**index], FolderEntry::Group { .. }))
            .count()
    };

    let mut render_row = |ui: &mut Ui, row: &[usize]| {
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 26.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = spacing;
                for &index in row {
                    match &entries[index] {
                        FolderEntry::Title => {
                            ui.label(RichText::new("目录").size(10.5).color(widgets::MUTED));
                        }
                        FolderEntry::All => {
                            if folder_chip(ui, "全部目录", tab.folder_filter.is_none()).clicked()
                            {
                                tab.folder_filter = None;
                                tab.filter_dirty = true;
                                tab.reset_list_scroll = true;
                                tab.selected = None;
                                tab.details = None;
                                tab.details_request = None;
                            }
                        }
                        FolderEntry::Group {
                            path,
                            text,
                            tooltip,
                            selected,
                        } => {
                            let response = folder_chip(ui, text, *selected).hover_tip(tooltip);
                            if response.clicked() && !*selected {
                                tab.folder_filter = Some(path.clone());
                                tab.filter_dirty = true;
                                tab.reset_list_scroll = true;
                                tab.selected = None;
                                tab.details = None;
                                tab.details_request = None;
                            }
                        }
                    }
                }
            },
        );
    };

    if expanded {
        // Keep the expanded list in a bounded, virtualized panel so the
        // collapse control stays reachable and the photo grid keeps its space.
        let height = (ui.available_height() * 0.28).clamp(112.0, 184.0);
        ScrollArea::vertical()
            .id_salt(("folder-chips-scroll", tab.id))
            .max_height(height)
            .auto_shrink([false, true])
            .show_rows(ui, 28.0, total_rows, |ui, range| {
                for row in range {
                    if let Some(row) = rows.get(row) {
                        render_row(ui, row);
                    }
                }
            });
        ui.horizontal(|ui| {
            if ui
                .add(
                    Button::new(
                        RichText::new("收起目录分组")
                            .size(10.0)
                            .color(widgets::ACCENT),
                    )
                    .frame(false)
                    .small(),
                )
                .clicked()
            {
                tab.folder_groups_expanded = false;
            }
            ui.label(
                RichText::new(format!("共 {} 个目录，可在上方滚动浏览", groups.len()))
                    .size(9.5)
                    .color(Color32::from_rgb(105, 113, 126)),
            );
        });
    } else {
        for row in rows.iter().take(visible_rows) {
            render_row(ui, row);
        }
        if total_rows > 2 {
            let label = if hidden > 0 {
                format!("展开全部（{hidden} 个目录）")
            } else {
                "展开全部目录".to_string()
            };
            if ui
                .add(
                    Button::new(RichText::new(label).size(10.0).color(widgets::ACCENT))
                        .frame(false)
                        .small(),
                )
                .clicked()
            {
                tab.folder_groups_expanded = true;
            }
        }
    }
}

fn folder_groups(tab: &FolderTab) -> Vec<(PathBuf, String, usize)> {
    if !tab.recursive {
        return Vec::new();
    }
    let mut counts: BTreeMap<PathBuf, usize> = BTreeMap::new();
    for entry in &tab.entries {
        if let Some(parent) = entry.path.parent() {
            *counts.entry(parent.to_path_buf()).or_insert(0) += 1;
        }
    }
    if counts.len() <= 1 {
        return Vec::new();
    }
    let mut groups: Vec<(PathBuf, String, usize)> = counts
        .into_iter()
        .map(|(path, count)| {
            let label = short_folder_label(&tab.folder, &path);
            (path, label, count)
        })
        .collect();
    groups.sort_by(|left, right| {
        let left_root = left.0.as_path() == tab.folder.as_path();
        let right_root = right.0.as_path() == tab.folder.as_path();
        right_root
            .cmp(&left_root)
            .then_with(|| left.1.to_lowercase().cmp(&right.1.to_lowercase()))
    });
    groups
}

fn short_folder_label(root: &Path, folder: &Path) -> String {
    let relative = folder.strip_prefix(root).unwrap_or(folder);
    let parts: Vec<String> = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    match parts.last() {
        Some(last) => last.clone(),
        None => "根目录".to_string(),
    }
}

fn folder_chip(ui: &mut Ui, label: &str, selected: bool) -> egui::Response {
    ui.add(
        Button::new(RichText::new(label).size(10.5).color(if selected {
            widgets::ACCENT
        } else {
            widgets::MUTED
        }))
        .small()
        .fill(if selected {
            widgets::PANEL_SOFT
        } else {
            Color32::from_rgb(25, 29, 36)
        })
        .stroke(Stroke::new(
            1.0,
            if selected {
                widgets::ACCENT
            } else {
                widgets::BORDER
            },
        )),
    )
}

fn can_thumbnail(entry: &MediaEntry) -> bool {
    match entry.kind {
        MediaKind::Video => crate::model::is_video_extension(&entry.ext),
        MediaKind::Image => is_previewable(&entry.ext),
    }
}

fn is_previewable(ext: &str) -> bool {
    matches!(
        ext,
        "jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp" | "bmp" | "gif"
    )
}

fn is_raw_label(label: &str) -> bool {
    label.starts_with("Tag(")
}

fn token_hint(token: &str) -> String {
    if token.is_empty() {
        "未生成（保存时自动生成）".to_string()
    } else if token.len() > 22 {
        format!("{}…{}", &token[..10], &token[token.len() - 6..])
    } else {
        token.to_string()
    }
}

fn refresh_filter(tab: &mut FolderTab, ratings: &RatingCatalog, sort: PhotoSort) {
    let query = tab.search.trim().to_lowercase();
    tab.filtered = tab
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            if tab.kind_filter == KindFilter::Images && entry.kind != MediaKind::Image {
                return false;
            }
            if tab.kind_filter == KindFilter::Videos && entry.kind != MediaKind::Video {
                return false;
            }
            if tab.favorites_only {
                let auto = tab
                    .vision_report
                    .as_ref()
                    .is_some_and(|report| report.preferred.contains(&entry.path));
                if !ratings.is_favorite(&entry.path, auto) {
                    return false;
                }
            }
            if let Some(folder) = &tab.folder_filter
                && entry.path.parent() != Some(folder.as_path())
            {
                return false;
            }
            if query.is_empty() {
                return true;
            }
            if entry.name.to_lowercase().contains(&query)
                || entry.path.to_string_lossy().to_lowercase().contains(&query)
            {
                return true;
            }
            if entry
                .camera_label()
                .map(|camera| camera.to_lowercase().contains(&query))
                .unwrap_or(false)
            {
                return true;
            }
            if let Some(info) = &entry.image
                && info
                    .lens
                    .as_ref()
                    .map(|lens| lens.to_lowercase().contains(&query))
                    .unwrap_or(false)
            {
                return true;
            }
            if let Some(video) = &entry.video {
                let codec_match = video
                    .codec
                    .as_ref()
                    .map(|codec| codec.to_lowercase().contains(&query))
                    .unwrap_or(false);
                let container_match = video
                    .container
                    .as_ref()
                    .map(|container| container.to_lowercase().contains(&query))
                    .unwrap_or(false);
                if codec_match || container_match {
                    return true;
                }
            }
            false
        })
        .map(|(index, _)| index)
        .collect();
    let entries = &tab.entries;
    match sort {
        // Discovery already emits entries in case-insensitive filename order.
        PhotoSort::Filename => {
            tab.filtered_time_range = None;
        }
        PhotoSort::TakenTime => {
            tab.filtered.sort_by_cached_key(|&index| {
                let entry = &entries[index];
                (
                    std::cmp::Reverse(entry_time_seconds(entry)),
                    entry.name.to_lowercase(),
                )
            });
            let mut range: Option<(i64, i64)> = None;
            for &index in &tab.filtered {
                let time = entry_time_seconds(&entries[index]);
                if time == i64::MIN {
                    continue;
                }
                range = Some(match range {
                    None => (time, time),
                    Some((min, max)) => (min.min(time), max.max(time)),
                });
            }
            tab.filtered_time_range = range;
        }
        PhotoSort::Resolution => {
            tab.filtered.sort_by_cached_key(|&index| {
                let entry = &entries[index];
                (
                    std::cmp::Reverse(entry_pixels(entry)),
                    entry.name.to_lowercase(),
                )
            });
            tab.filtered_time_range = None;
        }
    }
    tab.last_filter_refresh = Instant::now();
    tab.filter_dirty = false;
}

fn friendly_media_error(error: &str) -> String {
    let lower = error.to_lowercase();
    let damaged = [
        "unknown image format",
        "format error",
        "decode",
        "decoding",
        "illegal",
        "unexpected end",
        "truncated",
        "corrupt",
        "broken",
        "invalid",
        "malformed",
        "unsupported",
    ];
    if damaged.iter().any(|keyword| lower.contains(keyword)) {
        return "照片文件可能已损坏，或该格式暂不支持".to_string();
    }
    if lower.contains("network")
        || lower.contains("path was not found")
        || lower.contains("unreachable")
        || lower.contains("共享")
    {
        return "无法访问网络共享，请检查连接后重试".to_string();
    }
    if lower.contains("permission") || lower.contains("access is denied") || lower.contains("拒绝")
    {
        return "没有读取该文件的权限".to_string();
    }
    if lower.contains("not found") || lower.contains("找不到") || lower.contains("已移动") {
        return "文件已被移动、重命名或无法访问".to_string();
    }
    if error
        .chars()
        .any(|value| ('\u{4e00}'..='\u{9fff}').contains(&value))
    {
        return error.to_string();
    }
    "无法读取该文件，文件可能已损坏或暂时不可用".to_string()
}

fn entry_time_seconds(entry: &MediaEntry) -> i64 {
    entry
        .taken_at()
        .map(|value| value.and_utc().timestamp())
        .or_else(|| {
            entry
                .modified
                .map(|value| value.naive_local().and_utc().timestamp())
        })
        .unwrap_or(i64::MIN)
}

fn entry_pixels(entry: &MediaEntry) -> u64 {
    let (width, height) = match entry.kind {
        MediaKind::Image => entry
            .image
            .as_ref()
            .map(|info| (info.width, info.height))
            .unwrap_or((None, None)),
        MediaKind::Video => entry
            .video
            .as_ref()
            .map(|info| (info.width, info.height))
            .unwrap_or((None, None)),
    };
    match (width, height) {
        (Some(width), Some(height)) => width as u64 * height as u64,
        _ => 0,
    }
}

fn group_value<'a>(group: &'a ExifGroup, label: &str) -> Option<&'a str> {
    group
        .fields
        .iter()
        .find(|field| field.label == label)
        .map(|field| field.value.as_str())
}

fn collapsed_summary(
    group_name: &str,
    entry: &MediaEntry,
    group: &ExifGroup,
) -> Vec<(String, String)> {
    let value = |label: &str| {
        group_value(group, label)
            .filter(|value| meaningful_summary_value(value))
            .map(ToString::to_string)
    };
    let resolution = || {
        entry
            .resolution_label()
            .filter(|value| meaningful_summary_value(value))
    };
    let items: Vec<(&str, Option<String>)> = match group_name {
        "基本与设备" => vec![
            ("品牌", value("品牌")),
            ("型号", value("型号")),
            ("分辨率", resolution()),
            ("作者", value("作者")),
            ("版权", value("版权")),
        ],
        "拍摄参数" => vec![
            ("拍摄时间", value("拍摄时间")),
            ("光圈", value("光圈")),
            ("快门", value("快门速度")),
            ("焦距", value("焦距")),
            ("ISO", value("ISO")),
        ],
        "位置信息" => vec![
            ("纬度", value("纬度")),
            ("经度", value("经度")),
            ("海拔", value("海拔")),
        ],
        "视频概要" => vec![
            ("编码格式", value("编码格式")),
            ("封装格式", value("封装格式")),
            ("帧率", value("帧率")),
            ("分辨率", value("分辨率")),
        ],
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|(label, value)| value.map(|value| (label.to_string(), value)))
        .collect()
}

fn meaningful_summary_value(value: &str) -> bool {
    let value = value.trim();
    let normalized = value.trim_matches('"').trim().to_lowercase();
    !matches!(
        normalized.as_str(),
        "" | "null" | "(null)" | "none" | "n/a" | "unknown" | "未知" | "—" | "-"
    )
}

fn show_details_pending(ui: &mut Ui, loading: bool) {
    widgets::card(ui, |ui| {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if loading {
                ui.spinner();
            }
            let message = if loading {
                "正在读取文件元数据…"
            } else {
                "没有可显示的文件元数据"
            };
            ui.label(RichText::new(message).size(12.5).color(widgets::MUTED));
        });
        ui.add_space(8.0);
    });
}

fn show_summary_line(ui: &mut Ui, fields: &[(String, String)]) {
    if fields.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (label, value) in fields {
            ui.label(RichText::new(label).size(10.5).color(widgets::MUTED));
            let response = ui.add(Label::new(
                RichText::new(widgets::truncate(value, 34))
                    .size(12.0)
                    .color(widgets::TEXT),
            ));
            if value.chars().count() > 34 {
                response.hover_tip(value);
            }
            ui.add_space(12.0);
        }
    });
}

fn show_quick_facts(ui: &mut Ui, entry: &MediaEntry) {
    let facts: Vec<(&str, String)> = match entry.kind {
        MediaKind::Image => {
            let info = entry.image.as_ref();
            vec![
                (
                    "相机",
                    info.and_then(|item| item.camera_label())
                        .unwrap_or_else(|| "—".to_string()),
                ),
                (
                    "镜头",
                    info.and_then(|item| item.lens.clone())
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or_else(|| "—".to_string()),
                ),
                (
                    "曝光",
                    info.and_then(|item| item.exposure_label())
                        .unwrap_or_else(|| "—".to_string()),
                ),
                (
                    "拍摄时间",
                    info.and_then(|item| item.taken_at)
                        .map(|value| format_friendly_datetime(&value))
                        .unwrap_or_else(|| "—".to_string()),
                ),
            ]
        }
        MediaKind::Video => {
            let info = entry.video.as_ref();
            vec![
                (
                    "编码格式",
                    info.and_then(|item| item.codec.clone())
                        .unwrap_or_else(|| "—".to_string()),
                ),
                (
                    "封装格式",
                    info.and_then(|item| item.container.clone())
                        .unwrap_or_else(|| "—".to_string()),
                ),
                (
                    "帧率",
                    info.and_then(|item| item.fps)
                        .map(format_fps)
                        .unwrap_or_else(|| "—".to_string()),
                ),
                (
                    "分辨率",
                    info.and_then(|item| item.resolution_label())
                        .unwrap_or_else(|| "—".to_string()),
                ),
            ]
        }
    };

    ui.columns(4, |columns| {
        for (column, (label, value)) in columns.iter_mut().zip(facts.iter()) {
            Frame::new()
                .fill(widgets::PANEL)
                .stroke(Stroke::new(1.0, widgets::BORDER))
                .corner_radius(11)
                .inner_margin(Margin::symmetric(12, 10))
                .show(column, |ui| {
                    ui.set_min_height(46.0);
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(*label).size(10.5).color(widgets::MUTED));
                    ui.add_space(3.0);
                    ui.add(
                        Label::new(
                            RichText::new(widgets::truncate(value, 32))
                                .size(13.0)
                                .color(widgets::TEXT),
                        )
                        .truncate(),
                    )
                    .hover_tip(value);
                });
        }
    });
}

fn show_video_placeholder(
    ui: &mut Ui,
    entry: &MediaEntry,
    texture: Option<&egui::TextureHandle>,
    loading: bool,
    error: Option<&str>,
) -> bool {
    let width = ui.available_width().max(160.0);
    let aspect = texture
        .map(|texture| {
            let size = texture.size_vec2();
            (size.x / size.y.max(1.0)).clamp(0.6, 2.6)
        })
        .unwrap_or(16.0 / 9.0);
    let height = (width / aspect).clamp(190.0, 400.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let painter = ui.painter().clone().with_clip_rect(rect);
    painter.rect_filled(rect, 12, Color32::from_rgb(10, 12, 16));

    if let Some(texture) = texture {
        let texture_size = texture.size_vec2();
        let cover =
            (rect.width() / texture_size.x.max(1.0)).max(rect.height() / texture_size.y.max(1.0));
        let cover_rect = Rect::from_center_size(rect.center(), texture_size * cover);
        painter.image(
            texture.id(),
            cover_rect,
            Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            Color32::from_rgba_unmultiplied(42, 46, 54, 210),
        );
        painter.rect_filled(rect, 12, Color32::from_rgba_unmultiplied(6, 8, 12, 80));
        let scale =
            (rect.width() / texture_size.x.max(1.0)).min(rect.height() / texture_size.y.max(1.0));
        let fitted = Rect::from_center_size(rect.center(), texture_size * scale);
        painter.image(
            texture.id(),
            fitted,
            Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        for index in 0..6 {
            let x = rect.left() + rect.width() * (index as f32 + 0.5) / 6.0;
            painter.line_segment(
                [
                    egui::pos2(x, rect.top() + 18.0),
                    egui::pos2(x, rect.bottom() - 18.0),
                ],
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 9)),
            );
        }
        painter.text(
            rect.center() + Vec2::new(0.0, -58.0),
            Align2::CENTER_CENTER,
            "VIDEO",
            FontId::monospace(13.0),
            Color32::from_rgba_unmultiplied(
                widgets::ACCENT.r(),
                widgets::ACCENT.g(),
                widgets::ACCENT.b(),
                120,
            ),
        );
    }

    let shade = Rect::from_min_max(
        egui::pos2(rect.left(), rect.bottom() - 74.0),
        rect.right_bottom(),
    );
    painter.rect_filled(shade, 12, Color32::from_rgba_unmultiplied(6, 8, 11, 190));

    let center = rect.center();
    let hover = ui.ctx().animate_bool_with_time(
        Id::unique(("video-play", ui.scope_id(), &entry.path)),
        response.hovered(),
        0.16,
    );
    painter.circle_filled(
        center,
        29.0 + hover * 3.0,
        Color32::from_rgba_unmultiplied(8, 10, 13, (200.0 + hover * 30.0) as u8),
    );
    painter.circle_stroke(
        center,
        29.0 + hover * 3.0,
        Stroke::new(
            1.0 + hover,
            widgets::accent_soft((110.0 + hover * 90.0) as u8),
        ),
    );
    let scale = 1.0 + hover * 0.1;
    let triangle = vec![
        center + Vec2::new(-4.0, -8.0) * scale,
        center + Vec2::new(-4.0, 8.0) * scale,
        center + Vec2::new(9.0, 0.0) * scale,
    ];
    painter.add(egui::Shape::convex_polygon(
        triangle,
        widgets::ACCENT,
        Stroke::NONE,
    ));

    let mut first = Vec::new();
    if let Some(video) = &entry.video {
        if let Some(codec) = &video.codec {
            first.push(codec.clone());
        }
        if let Some(container) = &video.container {
            first.push(container.clone());
        }
    }
    if first.is_empty() {
        first.push(entry.format_label());
    }
    let mut second = Vec::new();
    if let Some(video) = &entry.video {
        if let Some(duration) = video.duration_label() {
            second.push(duration);
        }
        if let Some(resolution) = video.resolution_label() {
            second.push(resolution);
        }
        if let Some(fps) = video.fps {
            second.push(format_fps(fps));
        }
    }
    painter.text(
        rect.left_bottom() + Vec2::new(14.0, -34.0),
        Align2::LEFT_BOTTOM,
        first.join("  ·  "),
        FontId::proportional(12.5),
        widgets::TEXT,
    );
    if !second.is_empty() {
        painter.text(
            rect.left_bottom() + Vec2::new(14.0, -16.0),
            Align2::LEFT_BOTTOM,
            second.join("  ·  "),
            FontId::proportional(10.5),
            widgets::MUTED,
        );
    }
    painter.text(
        rect.right_bottom() + Vec2::new(-14.0, -22.0),
        Align2::RIGHT_CENTER,
        "点击确认后用系统播放器打开",
        FontId::proportional(10.5),
        if response.hovered() {
            widgets::ACCENT
        } else {
            widgets::MUTED
        },
    );

    if texture.is_none() && loading {
        ui.put(
            Rect::from_center_size(center + Vec2::new(0.0, 46.0), Vec2::splat(18.0)),
            egui::Spinner::new().size(16.0),
        );
        painter.text(
            center + Vec2::new(0.0, 70.0),
            Align2::CENTER_CENTER,
            "正在提取视频封面…",
            FontId::proportional(10.5),
            widgets::MUTED,
        );
    } else if let Some(error) = error {
        painter.text(
            center + Vec2::new(0.0, 60.0),
            Align2::CENTER_CENTER,
            widgets::truncate(&friendly_media_error(error), 64),
            FontId::proportional(10.5),
            widgets::MUTED,
        );
    }

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let response = if let Some(error) = error {
        response.on_hover_text(error)
    } else {
        response
    };
    response.clicked()
}

fn build_info_text(entry: &MediaEntry, groups: &[ExifGroup], show_raw: bool) -> String {
    let mut text = String::new();
    text.push_str(&format!("文件名: {}\n", entry.name));
    text.push_str(&format!("路径: {}\n", entry.path.display()));
    text.push_str(&format!(
        "类型: {} · {}\n",
        entry.kind.label(),
        entry.format_label()
    ));
    text.push_str(&format!("大小: {}\n", entry.size_label()));
    if let Some(modified) = entry.modified {
        text.push_str(&format!("修改时间: {}\n", format_friendly_local(&modified)));
    }
    if let Some(resolution) = entry.resolution_label() {
        text.push_str(&format!("尺寸: {resolution}\n"));
    }
    if let Some(at) = entry.taken_at() {
        text.push_str(&format!(
            "拍摄/创建时间: {}\n",
            format_friendly_datetime(&at)
        ));
    }
    for group in groups {
        let fields: Vec<&ExifField> = group
            .fields
            .iter()
            .filter(|field| show_raw || !is_raw_label(&field.label))
            .collect();
        if fields.is_empty() {
            continue;
        }
        text.push_str(&format!("\n[{}]\n", group.name));
        for field in fields {
            text.push_str(&format!("{}: {}\n", field.label, field.value));
        }
    }
    text
}

fn field_label(ui: &mut Ui, label: &str, optional: bool) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .size(11.5)
                .strong()
                .color(widgets::MUTED),
        );
        if optional {
            ui.label(
                RichText::new("选填")
                    .size(9.5)
                    .color(Color32::from_rgb(104, 112, 124)),
            );
        }
    });
}

fn labeled_field(ui: &mut Ui, label: &str, optional: bool, hint: &str, value: &mut String) {
    field_label(ui, label, optional);
    ui.add(
        TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(f32::INFINITY)
            .margin(Vec2::new(9.0, 6.0)),
    );
    ui.add_space(8.0);
}

fn unit_field(
    ui: &mut Ui,
    label: &str,
    prefix: Option<&str>,
    suffix: Option<&str>,
    hint: &str,
    value: &mut String,
) {
    field_label(ui, label, false);
    ui.horizontal(|ui| {
        if let Some(prefix) = prefix {
            ui.label(RichText::new(prefix).size(12.0).color(widgets::MUTED));
        }
        let reserve = suffix.map_or(0.0, |_| 26.0) + prefix.map_or(0.0, |_| 26.0);
        let width = (ui.available_width() - reserve).clamp(72.0, 260.0);
        ui.add(
            TextEdit::singleline(value)
                .hint_text(hint)
                .desired_width(width)
                .margin(Vec2::new(9.0, 6.0)),
        );
        if let Some(suffix) = suffix {
            ui.label(RichText::new(suffix).size(12.0).color(widgets::MUTED));
        }
    });
    ui.add_space(8.0);
}

fn parse_exif_datetime(value: &str) -> Option<chrono::NaiveDateTime> {
    let value = value.trim();
    [
        "%Y:%m:%d %H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y/%m/%d %H:%M:%S",
    ]
    .iter()
    .find_map(|format| chrono::NaiveDateTime::parse_from_str(value, format).ok())
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    }
}

fn datetime_field(ui: &mut Ui, value: &mut String) {
    field_label(ui, "拍摄时间", false);
    if value.trim().is_empty() {
        ui.horizontal(|ui| {
            ui.label(RichText::new("尚未设置").size(12.0).color(widgets::MUTED));
            if ui.small_button("+ 添加拍摄时间").clicked() {
                *value = chrono::Local::now().format("%Y:%m:%d %H:%M:%S").to_string();
            }
        });
        ui.add_space(8.0);
        return;
    }
    let Some(parsed) = parse_exif_datetime(value) else {
        ui.add(
            TextEdit::singleline(value)
                .hint_text("YYYY:MM:DD HH:MM:SS")
                .desired_width(f32::INFINITY),
        );
        ui.label(
            RichText::new("当前值不是标准 EXIF 时间格式，修改后请保持 YYYY:MM:DD HH:MM:SS。")
                .size(10.0)
                .color(widgets::ACCENT),
        );
        ui.add_space(8.0);
        return;
    };
    use chrono::{Datelike, Timelike};
    let (mut year, mut month, mut day) = (parsed.year(), parsed.month(), parsed.day());
    let (mut hour, mut minute, mut second) = (parsed.hour(), parsed.minute(), parsed.second());
    let changed = ui
        .horizontal(|ui| {
            let year_response = ui.add(
                egui::DragValue::new(&mut year)
                    .range(1970..=2100)
                    .speed(1.0)
                    .suffix(" 年"),
            );
            let month_response = ui.add(
                egui::DragValue::new(&mut month)
                    .range(1..=12)
                    .speed(0.2)
                    .suffix(" 月"),
            );
            let day_response = ui.add(
                egui::DragValue::new(&mut day)
                    .range(1..=days_in_month(year, month))
                    .speed(0.2)
                    .suffix(" 日"),
            );
            ui.add_space(8.0);
            let hour_response = ui.add(
                egui::DragValue::new(&mut hour)
                    .range(0..=23)
                    .speed(0.2)
                    .suffix(" 时"),
            );
            let minute_response = ui.add(
                egui::DragValue::new(&mut minute)
                    .range(0..=59)
                    .speed(0.2)
                    .suffix(" 分"),
            );
            let second_response = ui.add(
                egui::DragValue::new(&mut second)
                    .range(0..=59)
                    .speed(0.2)
                    .suffix(" 秒"),
            );
            year_response.changed()
                || month_response.changed()
                || day_response.changed()
                || hour_response.changed()
                || minute_response.changed()
                || second_response.changed()
        })
        .inner;
    if changed {
        day = day.min(days_in_month(year, month));
        *value = format!("{year:04}:{month:02}:{day:02} {hour:02}:{minute:02}:{second:02}");
        ui.ctx().request_repaint();
    }
    ui.add_space(8.0);
}

fn info_row(ui: &mut Ui, label: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(124.0, 20.0),
            Layout::left_to_right(Align::Min),
            |ui| {
                ui.label(RichText::new(label).size(11.5).color(widgets::MUTED));
            },
        );
        ui.add(
            Label::new(RichText::new(value).size(12.5).color(widgets::TEXT))
                .wrap()
                .selectable(true),
        );
    });
    ui.add_space(3.0);
}

fn warning_card(ui: &mut Ui, title: &str, message: &str) {
    Frame::new()
        .fill(Color32::from_rgba_unmultiplied(
            widgets::RED.r(),
            widgets::RED.g(),
            widgets::RED.b(),
            16,
        ))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(
                widgets::RED.r(),
                widgets::RED.g(),
                widgets::RED.b(),
                80,
            ),
        ))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(12.5).strong().color(widgets::RED));
            ui.add_space(2.0);
            ui.add(Label::new(RichText::new(message).size(11.5).color(widgets::MUTED)).wrap());
        });
}

fn tab_button(ui: &mut Ui, text: &str, selected: bool) -> egui::Response {
    ui.add(
        Button::new(RichText::new(text).size(14.0).color(if selected {
            widgets::ACCENT
        } else {
            widgets::MUTED
        }))
        .frame(selected)
        .fill(if selected {
            widgets::PANEL_SOFT
        } else {
            Color32::TRANSPARENT
        }),
    )
}

trait HoverTipExt {
    fn hover_tip(self, text: impl Into<egui::WidgetText>) -> Self;
}

impl HoverTipExt for egui::Response {
    fn hover_tip(self, text: impl Into<egui::WidgetText>) -> Self {
        let suppressed = self.ctx.data(|data| {
            data.get_temp::<bool>(Id::unique("exif-modal-open"))
                .unwrap_or(false)
        });
        if suppressed {
            self
        } else {
            self.on_hover_text(text)
        }
    }
}

fn duplicates_total_pages(group_count: usize) -> usize {
    group_count.div_ceil(DUPLICATES_PAGE_SIZE).max(1)
}

fn rebuild_duplicate_view(tab: &mut FolderTab, sort: DuplicateSort) {
    if tab.vision_report.is_some() && tab.entry_index_dirty {
        tab.rebuild_entry_index();
    }
    let Some(report) = tab.vision_report.as_ref() else {
        tab.duplicate_order.clear();
        tab.duplicate_group_times.clear();
        tab.duplicate_group_pixels.clear();
        tab.duplicate_time_range = None;
        tab.duplicate_order_revision = tab.duplicates_revision;
        tab.duplicate_order_sort = sort;
        return;
    };
    let entries = &tab.entries;
    let lookup = &tab.entry_index;
    let mut times = Vec::with_capacity(report.groups.len());
    let mut pixels = Vec::with_capacity(report.groups.len());
    for group in &report.groups {
        let mut best_time = i64::MIN;
        let mut best_pixels = 0u64;
        for path in &group.paths {
            if let Some(&index) = lookup.get(path)
                && let Some(entry) = entries.get(index)
            {
                best_time = best_time.max(entry_time_seconds(entry));
                best_pixels = best_pixels.max(entry_pixels(entry));
            }
        }
        times.push(best_time);
        pixels.push(best_pixels);
    }
    let mut order: Vec<usize> = (0..report.groups.len()).collect();
    order.sort_by(|&left, &right| {
        let count = report.groups[right]
            .paths
            .len()
            .cmp(&report.groups[left].paths.len());
        match sort {
            DuplicateSort::Count => count
                .then_with(|| times[right].cmp(&times[left]))
                .then_with(|| pixels[right].cmp(&pixels[left])),
            DuplicateSort::TakenTime => times[right]
                .cmp(&times[left])
                .then(count)
                .then_with(|| pixels[right].cmp(&pixels[left])),
            DuplicateSort::Resolution => pixels[right]
                .cmp(&pixels[left])
                .then(count)
                .then_with(|| times[right].cmp(&times[left])),
        }
        .then_with(|| {
            report.groups[left]
                .preferred
                .cmp(&report.groups[right].preferred)
        })
    });
    let mut range: Option<(i64, i64)> = None;
    for &time in &times {
        if time == i64::MIN {
            continue;
        }
        range = Some(match range {
            None => (time, time),
            Some((min, max)) => (min.min(time), max.max(time)),
        });
    }
    if sort == DuplicateSort::TakenTime
        && let Some(range) = range
        && let Some(group) = order.first()
        && let Some(&time) = times.get(*group)
        && time != i64::MIN
    {
        tab.duplicates_time_cursor = time_fraction(time, range);
    }
    tab.duplicate_group_times = times;
    tab.duplicate_group_pixels = pixels;
    tab.duplicate_time_range = range;
    tab.duplicate_order = order;
    tab.duplicate_order_revision = tab.duplicates_revision;
    tab.duplicate_order_sort = sort;
    let pages = duplicates_total_pages(report.groups.len());
    tab.duplicates_page = tab.duplicates_page.min(pages.saturating_sub(1));
    if tab.duplicates_start >= report.groups.len() {
        tab.duplicates_start = report.groups.len().saturating_sub(1);
    }
}

fn time_fraction(seconds: i64, range: (i64, i64)) -> f32 {
    if range.1 <= range.0 {
        return 1.0;
    }
    ((seconds.saturating_sub(range.0)) as f64 / (range.1 - range.0) as f64).clamp(0.0, 1.0) as f32
}

fn format_timeline_date(seconds: i64, with_day: bool) -> String {
    let Some(value) = chrono::DateTime::from_timestamp(seconds, 0) else {
        return "—".to_string();
    };
    // Photo timestamps are wall-clock values: EXIF has no reliable zone, and
    // file mtimes are stored as local wall time here. Format them as-is.
    if with_day {
        value.format("%Y.%m.%d").to_string()
    } else {
        value.format("%Y.%m").to_string()
    }
}

fn nearest_time_index(order: &[usize], times: &[i64], target: i64) -> Option<usize> {
    order
        .iter()
        .enumerate()
        .filter(|(_, group)| times.get(**group).is_some_and(|time| *time != i64::MIN))
        .min_by_key(|(_, group)| times[**group].abs_diff(target))
        .map(|(index, _)| index)
}

fn timeline_axis(
    ui: &mut Ui,
    range: (i64, i64),
    cursor: &mut f32,
    aria_label: &str,
) -> Option<i64> {
    let width = ui.available_width().max(160.0);
    let compact = width < 420.0;
    let height = if compact { 54.0 } else { 62.0 };
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
    response.widget_info(|| egui::WidgetInfo::slider(true, *cursor as f64, aria_label));
    let inset = if compact { 22.0 } else { 28.0 };
    let track = Rect::from_min_max(
        egui::pos2(rect.left() + inset, rect.top() + 25.0),
        egui::pos2(rect.right() - inset, rect.top() + 31.0),
    );
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(track, 3, widgets::BORDER);
    let last_tick = if compact { 2 } else { 4 };
    for step in 0..=last_tick {
        let fraction = step as f32 / last_tick as f32;
        let x = egui::lerp(track.left()..=track.right(), fraction);
        let seconds = range.0 + ((range.1 - range.0) as f64 * fraction as f64).round() as i64;
        painter.line_segment(
            [
                egui::pos2(x, track.top() - 3.0),
                egui::pos2(x, track.bottom() + 3.0),
            ],
            Stroke::new(1.0, widgets::MUTED),
        );
        painter.text(
            egui::pos2(x, rect.bottom() - 4.0),
            Align2::CENTER_BOTTOM,
            format_timeline_date(seconds, false),
            FontId::proportional(9.5),
            widgets::MUTED,
        );
    }
    if let Some(position) = response.interact_pointer_pos()
        && (response.dragged() || response.clicked())
    {
        *cursor = ((position.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
    }
    let fraction = cursor.clamp(0.0, 1.0);
    let x = egui::lerp(track.left()..=track.right(), fraction);
    painter.rect_filled(
        Rect::from_center_size(egui::pos2(x, track.center().y), Vec2::new(3.0, 20.0)),
        2,
        widgets::ACCENT,
    );
    painter.circle_filled(egui::pos2(x, track.center().y), 6.0, widgets::ACCENT);
    let seconds = range.0 + ((range.1 - range.0) as f64 * fraction as f64).round() as i64;
    let label = format_timeline_date(seconds, true);
    let label_width = 74.0;
    let label_rect = Rect::from_center_size(
        egui::pos2(
            x.clamp(
                rect.left() + label_width * 0.5,
                rect.right() - label_width * 0.5,
            ),
            track.top() - 12.0,
        ),
        Vec2::new(label_width, 18.0),
    );
    painter.rect_filled(
        label_rect,
        9,
        Color32::from_rgba_unmultiplied(232, 182, 102, 34),
    );
    painter.text(
        label_rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(10.0),
        widgets::ACCENT,
    );
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.drag_stopped() || response.clicked() {
        Some(seconds)
    } else {
        None
    }
}

fn duplicate_time_axis(
    ui: &mut Ui,
    order: &[usize],
    times: &[i64],
    range: (i64, i64),
    cursor: &mut f32,
) -> Option<usize> {
    let seconds = timeline_axis(ui, range, cursor, "查重时间轴")?;
    nearest_time_index(order, times, seconds)
}

fn browse_time_axis(ui: &mut Ui, tab: &mut FolderTab) -> Option<usize> {
    let range = tab.filtered_time_range?;
    let seconds = timeline_axis(ui, range, &mut tab.browse_time_cursor, "照片时间轴")?;
    let entries = &tab.entries;
    let mut best: Option<(usize, u64)> = None;
    for (position, &index) in tab.filtered.iter().enumerate() {
        let time = entry_time_seconds(&entries[index]);
        if time == i64::MIN {
            continue;
        }
        let distance = time.abs_diff(seconds);
        if best.is_none_or(|(_, previous)| distance < previous) {
            best = Some((position, distance));
        }
    }
    best.map(|(position, _)| position)
}

fn go_duplicates_page(tab: &mut FolderTab, target: usize, total_pages: usize) {
    let last = total_pages.saturating_sub(1);
    let now = Instant::now();
    if target > last {
        tab.duplicates_scroll_top = true;
        tab.duplicates_notice = Some(("已经是最后一页".to_string(), now));
        tab.duplicates_bounce_at = Some(now);
        return;
    }
    if target == tab.duplicates_page {
        tab.duplicates_scroll_top = true;
        tab.duplicates_notice = Some(("已经是第一页".to_string(), now));
        tab.duplicates_bounce_at = Some(now);
        return;
    }
    tab.duplicates_page = target;
    tab.duplicates_scroll_top = true;
    tab.duplicates_bounce_at = Some(now);
    tab.duplicates_notice = Some((format!("第 {} / {} 页", target + 1, total_pages), now));
}

fn duplicate_bounce_offset(at: Option<Instant>) -> (f32, bool) {
    let Some(at) = at else {
        return (0.0, false);
    };
    let elapsed = at.elapsed().as_secs_f32();
    if elapsed >= 0.5 {
        return (0.0, false);
    }
    let amplitude = (-elapsed * 9.0).exp() * 7.0;
    ((elapsed * 42.0).sin() * amplitude, true)
}

struct DuplicateRenderCtx<'a> {
    entries: &'a [MediaEntry],
    entry_index: &'a HashMap<PathBuf, usize>,
    textures: &'a HashMap<u64, egui::TextureHandle>,
    pending: &'a HashSet<u64>,
    failed: &'a HashSet<u64>,
    ratings: &'a RatingCatalog,
    auto_preferred: &'a HashSet<PathBuf>,
}

#[derive(Default)]
struct DuplicateActions {
    star_toggle: Option<PathBuf>,
    open_file: Option<PathBuf>,
    needed: Vec<(u64, PathBuf, u16, u32)>,
    visible_keys: HashSet<u64>,
}

fn duplicate_group_height(
    group: &DuplicateGroup,
    width: f32,
    tile_width: f32,
    tile_height: f32,
) -> f32 {
    let per_row = (((width + GRID_SPACING) / (tile_width + GRID_SPACING)).floor()).max(1.0);
    let tiles = group.paths.len().min(GROUP_PREVIEW_LIMIT) as f32;
    let rows = (tiles / per_row).ceil();
    let note = if group.paths.len() > GROUP_PREVIEW_LIMIT {
        18.0
    } else {
        0.0
    };
    54.0 + rows * (tile_height + 30.0 + GRID_SPACING) + note + 34.0
}

fn duplicate_group_card(
    ui: &mut Ui,
    group: &DuplicateGroup,
    index: usize,
    tile_width: f32,
    tile_height: f32,
    ctx: &DuplicateRenderCtx<'_>,
    actions: &mut DuplicateActions,
) {
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("相似组 {}", index + 1))
                    .size(15.0)
                    .strong()
                    .color(widgets::TEXT),
            );
            widgets::pill(ui, &format!("{} 张", group.paths.len()), widgets::MUTED);
            widgets::pill(
                ui,
                &format!("相似度 {}%", group.similarity_percent()),
                widgets::TEAL,
            );
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(GRID_SPACING, GRID_SPACING);
            for path in group.paths.iter().take(GROUP_PREVIEW_LIMIT) {
                let Some(entry_index) = ctx.entry_index.get(path.as_path()) else {
                    continue;
                };
                let entry = &ctx.entries[*entry_index];
                let key = entry_key(entry, THUMB_LARGE_VARIANT);
                let texture = ctx.textures.get(&key);
                let is_auto = ctx.auto_preferred.contains(&entry.path);
                let favorite = ctx.ratings.is_favorite(&entry.path, is_auto);
                let auto_badge = is_auto && ctx.ratings.manual_state(&entry.path) != Some(false);
                let (response, star_clicked) = photo_tile(
                    ui,
                    entry,
                    texture,
                    false,
                    favorite,
                    auto_badge,
                    tile_width,
                    tile_height,
                );
                if star_clicked {
                    actions.star_toggle = Some(entry.path.clone());
                } else if response.clicked() {
                    actions.open_file = Some(entry.path.clone());
                }
                if ui.is_rect_visible(response.rect) {
                    if texture.is_some() {
                        actions.visible_keys.insert(key);
                    } else if can_thumbnail(entry)
                        && !ctx.pending.contains(&key)
                        && !ctx.failed.contains(&key)
                        && actions.needed.len() < 16
                    {
                        let orientation = entry
                            .image
                            .as_ref()
                            .map(|info| info.orientation)
                            .unwrap_or(1);
                        actions.needed.push((
                            key,
                            entry.path.clone(),
                            orientation,
                            THUMB_LARGE_SIZE,
                        ));
                    }
                }
            }
        });
        if group.paths.len() > GROUP_PREVIEW_LIMIT {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "另有 {} 张未在预览中展开，点击任意缩略图可进入浏览视图查看",
                    group.paths.len() - GROUP_PREVIEW_LIMIT
                ))
                .size(10.5)
                .color(widgets::MUTED),
            );
        }
    });
}

fn duplicates_layout_option(
    ui: &mut Ui,
    layout: DuplicatesLayout,
    selected: bool,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 156.0), Sense::click());
    let time = ui.ctx().input(|input| input.time) as f32;
    ui.ctx().request_repaint_after(Duration::from_millis(33));
    let painter = ui.painter().clone();
    let background = if selected {
        widgets::PANEL_SOFT
    } else {
        widgets::PANEL
    };
    painter.rect_filled(rect, 12, background);
    painter.rect_stroke(
        rect,
        12,
        Stroke::new(
            if selected { 1.4 } else { 1.0 },
            if selected {
                widgets::ACCENT
            } else {
                widgets::BORDER
            },
        ),
        egui::StrokeKind::Inside,
    );
    let preview = Rect::from_min_max(
        rect.min + Vec2::new(10.0, 10.0),
        egui::pos2(rect.right() - 10.0, rect.top() + 102.0),
    );
    painter.rect_filled(preview, 8, Color32::from_rgb(13, 15, 20));
    painter.rect_stroke(
        preview,
        8,
        Stroke::new(1.0, Color32::from_rgb(35, 40, 49)),
        egui::StrokeKind::Inside,
    );
    let clip = ui.painter().with_clip_rect(preview);
    match layout {
        DuplicatesLayout::Paged => {
            let turn = (time * 0.55).fract();
            let slide = (turn * std::f32::consts::TAU).sin() * 5.0;
            for layer in (0..3).rev() {
                let offset = layer as f32 * 4.0;
                let page = Rect::from_min_max(
                    preview.min
                        + Vec2::new(24.0 + offset + slide * layer as f32 * 0.3, 12.0 - offset),
                    preview.max - Vec2::new(24.0 - offset, 18.0 + offset),
                );
                clip.rect_filled(
                    page,
                    6,
                    if layer == 0 {
                        Color32::from_rgb(38, 44, 55)
                    } else {
                        Color32::from_rgb(24, 28, 35)
                    },
                );
                clip.rect_stroke(
                    page,
                    6,
                    Stroke::new(1.0, Color32::from_rgb(56, 64, 78)),
                    egui::StrokeKind::Inside,
                );
                if layer == 0 {
                    for row in 0..2 {
                        for column in 0..3 {
                            let tile = Rect::from_min_size(
                                page.min
                                    + Vec2::new(
                                        10.0 + column as f32 * 34.0,
                                        10.0 + row as f32 * 26.0,
                                    ),
                                Vec2::new(28.0, 19.0),
                            );
                            clip.rect_filled(tile, 4, widgets::accent_soft(80));
                        }
                    }
                }
            }
            let dots_y = preview.bottom() - 10.0;
            let active = ((time * 0.55).floor() as i32).rem_euclid(3);
            for dot in 0..3 {
                let x = preview.center().x + (dot as f32 - 1.0) * 12.0;
                clip.circle_filled(
                    egui::pos2(x, dots_y),
                    if dot == active { 3.0 } else { 2.0 },
                    if dot == active {
                        widgets::ACCENT
                    } else {
                        widgets::MUTED
                    },
                );
            }
        }
        DuplicatesLayout::Waterfall => {
            let scroll = (time * 18.0) % 34.0;
            for column in 0..2 {
                for index in 0..5 {
                    let y = preview.top() - 34.0 + index as f32 * 34.0 + scroll;
                    if y + 27.0 < preview.top() || y > preview.bottom() {
                        continue;
                    }
                    let x = preview.left() + 16.0 + column as f32 * (preview.width() / 2.0);
                    let height = if (column + index) % 3 == 0 {
                        28.0
                    } else {
                        22.0
                    };
                    let card = Rect::from_min_size(
                        egui::pos2(x, y),
                        Vec2::new(preview.width() / 2.0 - 24.0, height),
                    );
                    clip.rect_filled(card, 5, Color32::from_rgb(32, 37, 47));
                    clip.rect_filled(
                        Rect::from_min_size(
                            card.min + Vec2::new(6.0, 6.0),
                            Vec2::new(22.0, height - 12.0),
                        ),
                        3,
                        if column == 0 {
                            widgets::accent_soft(80)
                        } else {
                            Color32::from_rgba_unmultiplied(
                                widgets::TEAL.r(),
                                widgets::TEAL.g(),
                                widgets::TEAL.b(),
                                70,
                            )
                        },
                    );
                    for line in 0..2 {
                        clip.rect_filled(
                            Rect::from_min_size(
                                card.min + Vec2::new(34.0, 7.0 + line as f32 * 7.0),
                                Vec2::new((card.width() - 42.0).max(8.0), 3.0),
                            ),
                            1,
                            Color32::from_rgb(66, 74, 88),
                        );
                    }
                }
            }
        }
    }
    painter.text(
        rect.left_bottom() + Vec2::new(14.0, -36.0),
        Align2::LEFT_BOTTOM,
        layout.label(),
        FontId::proportional(13.0),
        if selected {
            widgets::ACCENT
        } else {
            widgets::TEXT
        },
    );
    painter.text(
        rect.left_bottom() + Vec2::new(14.0, -20.0),
        Align2::LEFT_BOTTOM,
        match layout {
            DuplicatesLayout::Paged => "每页 20 组，可用 PageUp / PageDown 或鼠标侧键翻页",
            DuplicatesLayout::Waterfall => "双列连续滚动，滚到底部自动加载下一批",
        },
        FontId::proportional(9.5),
        widgets::MUTED,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn blurred_backdrop(ctx: &egui::Context, image: &egui::ColorImage) -> egui::TextureHandle {
    let (source_width, source_height) = (image.size[0], image.size[1]);
    if source_width == 0 || source_height == 0 {
        return ctx.load_texture(
            "modal-backdrop-empty",
            egui::ColorImage::new([1, 1], vec![Color32::from_rgb(12, 14, 18)]),
            egui::TextureOptions::LINEAR,
        );
    }
    let width = (source_width / 14).clamp(24, 240);
    let height = ((width as f32 * source_height as f32 / source_width as f32).round() as usize)
        .clamp(16, 240);
    let mut small = Vec::with_capacity(width * height);
    for y in 0..height {
        let sample_y = (y * source_height / height).min(source_height - 1);
        for x in 0..width {
            let sample_x = (x * source_width / width).min(source_width - 1);
            small.push(image.pixels[sample_y * source_width + sample_x]);
        }
    }
    for _ in 0..2 {
        small = box_blur(small, width, height, 2);
    }
    ctx.load_texture(
        "modal-backdrop-blur",
        egui::ColorImage::new([width, height], small),
        egui::TextureOptions::LINEAR,
    )
}

fn box_blur(pixels: Vec<Color32>, width: usize, height: usize, radius: usize) -> Vec<Color32> {
    let mut horizontal = vec![Color32::BLACK; pixels.len()];
    let radius = radius as i32;
    for y in 0..height {
        for x in 0..width {
            let (mut r, mut g, mut b, mut count) = (0u32, 0u32, 0u32, 0u32);
            for offset in -radius..=radius {
                let sx = (x as i32 + offset).clamp(0, width as i32 - 1) as usize;
                let pixel = pixels[y * width + sx];
                r += pixel.r() as u32;
                g += pixel.g() as u32;
                b += pixel.b() as u32;
                count += 1;
            }
            horizontal[y * width + x] =
                Color32::from_rgb((r / count) as u8, (g / count) as u8, (b / count) as u8);
        }
    }
    let mut vertical = vec![Color32::BLACK; pixels.len()];
    for y in 0..height {
        for x in 0..width {
            let (mut r, mut g, mut b, mut count) = (0u32, 0u32, 0u32, 0u32);
            for offset in -radius..=radius {
                let sy = (y as i32 + offset).clamp(0, height as i32 - 1) as usize;
                let pixel = horizontal[sy * width + x];
                r += pixel.r() as u32;
                g += pixel.g() as u32;
                b += pixel.b() as u32;
                count += 1;
            }
            vertical[y * width + x] =
                Color32::from_rgb((r / count) as u8, (g / count) as u8, (b / count) as u8);
        }
    }
    vertical
}

fn view_switch(ui: &mut Ui, current: ViewTab) -> Option<ViewTab> {
    const ITEMS: [(&str, ViewTab); 3] = [
        ("浏览", ViewTab::Browse),
        ("统计", ViewTab::Stats),
        ("查重", ViewTab::Duplicates),
    ];
    let segment = 64.0;
    let height = 30.0;
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(segment * ITEMS.len() as f32 + 8.0, height),
        Sense::hover(),
    );
    let mut responses = Vec::with_capacity(ITEMS.len());
    for (index, (label, _)) in ITEMS.iter().enumerate() {
        let segment_rect = Rect::from_min_size(
            egui::Pos2::new(rect.left() + 4.0 + index as f32 * segment, rect.top() + 3.0),
            Vec2::new(segment, height - 6.0),
        );
        let response = ui.interact(
            segment_rect,
            Id::unique(("view-segment", index)),
            Sense::click(),
        );
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, *label));
        responses.push(response);
    }
    let selected = ITEMS
        .iter()
        .position(|(_, view)| *view == current)
        .unwrap_or(0);
    let animated =
        ui.ctx()
            .animate_value_with_time(Id::unique("view-switch-pill"), selected as f32, 0.2);
    let track = Rect::from_min_max(
        rect.left_top(),
        egui::Pos2::new(rect.right(), rect.bottom()),
    );
    ui.painter().rect_filled(track, 9, widgets::PANEL);
    ui.painter().rect_stroke(
        track,
        9,
        Stroke::new(1.0, widgets::BORDER),
        egui::StrokeKind::Inside,
    );
    let pill = Rect::from_min_size(
        egui::Pos2::new(track.left() + 4.0 + animated * segment, track.top() + 3.0),
        Vec2::new(segment, height - 6.0),
    );
    ui.painter().rect_filled(pill, 7, widgets::PANEL_SOFT);
    ui.painter().rect_stroke(
        pill,
        7,
        Stroke::new(1.0, widgets::accent_soft(110)),
        egui::StrokeKind::Inside,
    );
    let hovered = responses.iter().position(|response| response.hovered());
    if let Some(index) = hovered
        && index != selected
    {
        let hover_rect = Rect::from_min_size(
            egui::Pos2::new(
                track.left() + 4.0 + index as f32 * segment,
                track.top() + 3.0,
            ),
            Vec2::new(segment, height - 6.0),
        );
        ui.painter().rect_filled(
            hover_rect,
            7,
            Color32::from_rgba_unmultiplied(255, 255, 255, 8),
        );
    }
    for (index, (label, _)) in ITEMS.iter().enumerate() {
        let center = egui::Pos2::new(
            track.left() + 4.0 + index as f32 * segment + segment / 2.0,
            track.center().y,
        );
        ui.painter().text(
            center,
            Align2::CENTER_CENTER,
            *label,
            FontId::proportional(13.0),
            if index == selected {
                widgets::ACCENT
            } else if hovered == Some(index) {
                widgets::TEXT
            } else {
                widgets::MUTED
            },
        );
    }
    if hovered.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    for (index, response) in responses.iter().enumerate() {
        if response.clicked() {
            let next = ITEMS[index].1;
            if next != current {
                return Some(next);
            }
        }
    }
    None
}

fn tab_chip(ui: &mut Ui, tab: &FolderTab, active: bool) -> (bool, bool) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(190.0, 30.0), Sense::click());
    let close_rect = Rect::from_center_size(
        rect.right_center() - Vec2::new(16.0, 0.0),
        Vec2::new(22.0, 24.0),
    );
    let hover_pos = response.hover_pos();
    let close_hovered = hover_pos.is_some_and(|pos| close_rect.contains(pos));
    let dirty = tab.edit_is_dirty();
    let mut select = false;
    let mut close = false;

    if ui.is_rect_visible(rect) {
        let hover_t = ui.ctx().animate_bool_with_time(
            Id::unique(("tab-chip-hover", tab.id)),
            response.hovered(),
            0.16,
        );
        let active_t =
            ui.ctx()
                .animate_bool_with_time(Id::unique(("tab-chip-active", tab.id)), active, 0.18);
        let fill = Color32::from_rgb(18, 21, 26)
            .lerp_to_gamma(widgets::PANEL, hover_t)
            .lerp_to_gamma(widgets::PANEL_SOFT, active_t);
        ui.painter().rect_filled(rect, 8, fill);
        ui.painter().rect_stroke(
            rect,
            8,
            Stroke::new(
                1.0,
                widgets::BORDER.lerp_to_gamma(
                    Color32::from_rgba_unmultiplied(
                        widgets::ACCENT.r(),
                        widgets::ACCENT.g(),
                        widgets::ACCENT.b(),
                        100,
                    ),
                    active_t,
                ),
            ),
            egui::StrokeKind::Inside,
        );
        ui.painter().rect_filled(
            Rect::from_min_size(
                rect.left_bottom() + Vec2::new(10.0, -3.0),
                Vec2::new(rect.width() - 20.0, 2.0),
            ),
            2,
            Color32::from_rgba_unmultiplied(
                widgets::ACCENT.r(),
                widgets::ACCENT.g(),
                widgets::ACCENT.b(),
                (255.0 * active_t) as u8,
            ),
        );

        let mut title_x = rect.left() + 11.0;
        if dirty {
            ui.painter().circle_filled(
                rect.left_center() + Vec2::new(11.0, 0.0),
                3.0,
                widgets::ACCENT,
            );
            title_x += 11.0;
        } else if tab.scanning {
            ui.painter().circle_stroke(
                rect.left_center() + Vec2::new(11.0, 0.0),
                3.2,
                Stroke::new(
                    1.4,
                    if tab.scan_queued {
                        widgets::MUTED
                    } else {
                        widgets::TEAL
                    },
                ),
            );
            title_x += 11.0;
        }

        let text_clip = Rect::from_min_max(
            egui::Pos2::new(title_x, rect.top()),
            egui::Pos2::new(close_rect.left() - 4.0, rect.bottom()),
        );
        ui.painter().with_clip_rect(text_clip).text(
            egui::Pos2::new(title_x, rect.center().y),
            Align2::LEFT_CENTER,
            widgets::truncate(&folder_title(&tab.folder), 22),
            FontId::proportional(12.5),
            widgets::MUTED.lerp_to_gamma(widgets::TEXT, active_t.max(hover_t * 0.5)),
        );
        ui.painter().text(
            close_rect.center(),
            Align2::CENTER_CENTER,
            "×",
            FontId::proportional(14.0),
            if close_hovered {
                widgets::RED
            } else if active {
                widgets::MUTED
            } else {
                Color32::from_rgb(105, 113, 126)
            },
        );
    }

    if response.middle_clicked() {
        close = true;
    } else if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        if close_rect.contains(pos) {
            close = true;
        } else {
            select = true;
        }
    }

    let status = if tab.edit_is_dirty() {
        " · 有未保存修改"
    } else if tab.scanning {
        " · 正在读取"
    } else {
        ""
    };
    let response = response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .hover_tip(format!("{}{}", tab.folder.display(), status));
    let _ = response;
    (select && !close, close)
}

#[allow(clippy::too_many_arguments)]
fn photo_tile(
    ui: &mut Ui,
    entry: &MediaEntry,
    texture: Option<&egui::TextureHandle>,
    selected: bool,
    favorite: bool,
    auto_badge: bool,
    width: f32,
    image_height: f32,
) -> (egui::Response, bool) {
    let height = image_height + 30.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, entry.name.as_str()));
    let mut star_clicked = false;
    let mut star_hovered = false;
    let mut star_label = "设为星标";
    let path_id = entry.path.as_path();
    let hover_t = ui.ctx().animate_bool_with_time(
        Id::unique(("tile-hover", ui.scope_id(), path_id)),
        response.hovered(),
        0.16,
    );
    let select_t = ui.ctx().animate_bool_with_time(
        Id::unique(("tile-selected", ui.scope_id(), path_id)),
        selected,
        0.18,
    );
    let star_t = ui.ctx().animate_bool_with_time(
        Id::unique(("tile-star", ui.scope_id(), path_id)),
        favorite,
        0.2,
    );
    if ui.is_rect_visible(rect) {
        let image_rect = Rect::from_min_size(rect.min, Vec2::new(width, image_height));
        let background = widgets::PANEL
            .lerp_to_gamma(widgets::PANEL_SOFT, hover_t)
            .lerp_to_gamma(widgets::accent_soft(40), select_t * 0.55);
        let glow = (hover_t * 16.0 + select_t * 24.0) as u8;
        if glow > 0 {
            ui.painter()
                .rect_filled(image_rect.expand(1.5), 11, widgets::accent_soft(glow));
        }
        ui.painter().rect_filled(image_rect, 10, background);
        ui.painter().rect_filled(
            Rect::from_min_size(image_rect.left_bottom(), Vec2::new(width, 30.0)),
            10,
            background,
        );

        if let Some(texture) = texture {
            let texture_size = texture.size_vec2();
            let scale = (image_rect.width() / texture_size.x.max(1.0))
                .min(image_rect.height() / texture_size.y.max(1.0));
            let fitted = Rect::from_center_size(image_rect.center(), texture_size * scale);
            ui.painter().image(
                texture.id(),
                fitted,
                Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            let color = match entry.kind {
                MediaKind::Image => widgets::TEAL,
                MediaKind::Video => widgets::ACCENT,
            };
            ui.painter().text(
                image_rect.center(),
                Align2::CENTER_CENTER,
                match entry.kind {
                    MediaKind::Image => "IMG",
                    MediaKind::Video => "VID",
                },
                FontId::monospace(10.0),
                Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 150),
            );
        }

        if entry.kind == MediaKind::Video {
            let center = image_rect.center();
            ui.painter().circle_filled(
                center,
                15.0,
                Color32::from_rgba_unmultiplied(8, 10, 13, 165),
            );
            let triangle = vec![
                center + Vec2::new(-4.0, -7.0),
                center + Vec2::new(-4.0, 7.0),
                center + Vec2::new(8.0, 0.0),
            ];
            ui.painter().add(egui::Shape::convex_polygon(
                triangle,
                widgets::ACCENT,
                Stroke::NONE,
            ));
        }

        let border_color = widgets::BORDER
            .lerp_to_gamma(Color32::from_rgb(80, 88, 102), hover_t * 0.6)
            .lerp_to_gamma(widgets::ACCENT, select_t);
        ui.painter().rect_stroke(
            image_rect,
            10,
            Stroke::new(1.0 + select_t * 0.6, border_color),
            egui::StrokeKind::Inside,
        );

        if auto_badge {
            let badge = Rect::from_min_size(
                image_rect.left_top() + Vec2::new(6.0, 6.0),
                Vec2::new(34.0, 17.0),
            );
            ui.painter().rect_filled(
                badge,
                8,
                Color32::from_rgba_unmultiplied(10, 12, 15, (170.0 * star_t) as u8),
            );
            ui.painter().text(
                badge.center(),
                Align2::CENTER_CENTER,
                "优选",
                FontId::proportional(9.5),
                Color32::from_rgba_unmultiplied(
                    widgets::ACCENT.r(),
                    widgets::ACCENT.g(),
                    widgets::ACCENT.b(),
                    (255.0 * star_t) as u8,
                ),
            );
        }

        let star_rect = Rect::from_center_size(
            image_rect.right_top() + Vec2::new(-17.0, 17.0),
            Vec2::splat(28.0),
        );
        star_hovered = response
            .hover_pos()
            .is_some_and(|position| star_rect.contains(position));
        star_label = if favorite {
            if auto_badge {
                "取消自动优选"
            } else {
                "取消星标"
            }
        } else {
            "设为星标"
        };
        let show_star = favorite || auto_badge || response.hovered();
        if show_star {
            let radius = 11.0 + star_t + hover_t * 0.5;
            ui.painter().circle_filled(
                star_rect.center(),
                radius,
                Color32::from_rgba_unmultiplied(8, 10, 13, 175),
            );
            if star_hovered {
                ui.painter().circle_stroke(
                    star_rect.center(),
                    radius + 0.5,
                    Stroke::new(1.0, widgets::accent_soft(150)),
                );
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            ui.painter().text(
                star_rect.center(),
                Align2::CENTER_CENTER,
                "★",
                FontId::proportional(14.0 + star_t * 3.0 + hover_t * 0.5),
                Color32::from_rgb(172, 178, 188).lerp_to_gamma(widgets::ACCENT, star_t),
            );
            star_clicked = star_hovered
                && response.clicked()
                && response
                    .interact_pointer_pos()
                    .is_some_and(|position| star_rect.contains(position));
        }

        let font_size = if width >= 140.0 { 11.5 } else { 10.5 };
        let max_chars = ((width - 6.0) / (font_size * 0.9)).max(4.0) as usize;
        ui.painter().text(
            rect.left_bottom() + Vec2::new(2.0, -22.0),
            Align2::LEFT_TOP,
            widgets::truncate(&entry.name, max_chars),
            FontId::proportional(font_size),
            widgets::TEXT,
        );
    }
    let hover = if star_hovered {
        star_label.to_string()
    } else {
        match &entry.read_error {
            Some(error) => format!("{}\n无法读取：{}", entry.name, friendly_media_error(error)),
            None => entry.name.clone(),
        }
    };
    (response.hover_tip(hover), star_clicked)
}

fn show_stats_view(ui: &mut Ui, stats: &Stats, tab_id: u64, scanning: bool) {
    ScrollArea::vertical()
        .id_salt(("stats-scroll", tab_id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("统计分析")
                        .size(24.0)
                        .strong()
                        .color(widgets::TEXT),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "{} 个文件 · {} 个解析失败",
                        stats.images + stats.videos,
                        stats.errors
                    ))
                    .size(11.5)
                    .color(widgets::MUTED),
                );
                if scanning {
                    ui.spinner();
                    ui.label(
                        RichText::new("扫描中，统计会持续更新")
                            .size(11.0)
                            .color(widgets::MUTED),
                    );
                }
            });
            ui.add_space(14.0);

            if stats.images + stats.videos == 0 {
                ui.add_space(70.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(if scanning {
                            "正在等待可统计的媒体文件…"
                        } else {
                            "还没有可统计的媒体文件"
                        })
                        .size(14.0)
                        .color(widgets::MUTED),
                    );
                });
                return;
            }

            ui.columns(4, |columns| {
                metric_card(
                    &mut columns[0],
                    "图片",
                    &stats.images.to_string(),
                    widgets::TEAL,
                );
                metric_card(
                    &mut columns[1],
                    "视频",
                    &stats.videos.to_string(),
                    widgets::ACCENT,
                );
                metric_card(
                    &mut columns[2],
                    "总占用",
                    &format_bytes(stats.image_bytes + stats.video_bytes),
                    widgets::TEXT,
                );
                metric_card(
                    &mut columns[3],
                    "带 GPS 的文件",
                    &stats.gps_count.to_string(),
                    widgets::GREEN,
                );
            });

            ui.add_space(8.0);
            let mut extras = Vec::new();
            if let (Some(first), Some(last)) =
                (stats.years.keys().next(), stats.years.keys().next_back())
            {
                extras.push(format!("拍摄跨度 {first} – {last}"));
            }
            if stats.video_seconds > 0.0 {
                extras.push(format!(
                    "视频总时长 {}",
                    format_duration(stats.video_seconds)
                ));
            }
            if !extras.is_empty() {
                ui.label(
                    RichText::new(extras.join("   ·   "))
                        .size(11.0)
                        .color(widgets::MUTED),
                );
            }

            ui.add_space(14.0);
            ui.columns(2, |columns| {
                let left = &mut columns[0];
                stats_card(left, "文件格式", "", |ui| {
                    widgets::bar_rows(
                        ui,
                        &stats::sorted_counts(&stats.formats, 10),
                        widgets::ACCENT,
                    );
                });
                left.add_space(12.0);
                stats_card(left, "相机品牌 Top", "", |ui| {
                    widgets::bar_rows(ui, &stats::sorted_counts(&stats.makes, 8), widgets::TEAL);
                });
                left.add_space(12.0);
                stats_card(left, "相机型号 Top", "", |ui| {
                    widgets::bar_rows(ui, &stats::sorted_counts(&stats.models, 8), widgets::TEAL);
                });
                left.add_space(12.0);
                stats_card(left, "照片分辨率 Top", "", |ui| {
                    widgets::bar_rows(
                        ui,
                        &stats::sorted_counts(&stats.image_resolutions, 6),
                        widgets::MUTED,
                    );
                });

                let right = &mut columns[1];
                stats_card(right, "拍摄年份", "按 EXIF 拍摄时间统计", |ui| {
                    widgets::years_chart(ui, &stats.years, widgets::ACCENT);
                });
                right.add_space(12.0);
                stats_card(right, "月份分布", "全年各月拍摄数量", |ui| {
                    widgets::months_chart(ui, &stats.months, widgets::TEAL);
                });
                right.add_space(12.0);
                stats_card(right, "镜头 Top", "", |ui| {
                    widgets::bar_rows(ui, &stats::sorted_counts(&stats.lenses, 8), widgets::GREEN);
                });
            });

            ui.add_space(12.0);
            stats_card(ui, "曝光参数分布", "", |ui| {
                exposure_section(ui, stats);
            });

            ui.add_space(12.0);
            stats_card(
                ui,
                "位置分布",
                &format!("{} 个文件带 GPS", stats.gps_count),
                |ui| {
                    if stats.gps_points.is_empty() {
                        widgets::gps_scatter(ui, &stats.gps_points);
                    } else {
                        ui.columns(2, |columns| {
                            widgets::gps_scatter(&mut columns[0], &stats.gps_points);
                            if stats.gps_clusters.is_empty() {
                                columns[1].label(
                                    RichText::new("暂无位置聚类")
                                        .size(11.0)
                                        .color(widgets::MUTED),
                                );
                            } else {
                                widgets::bar_rows(
                                    &mut columns[1],
                                    &stats.gps_clusters,
                                    widgets::TEAL,
                                );
                            }
                        });
                    }
                },
            );

            if stats.videos > 0 {
                ui.add_space(12.0);
                stats_card(ui, "视频概览", "", |ui| {
                    video_stats_section(ui, stats);
                });
            }
            ui.add_space(24.0);
        });
}

fn metric_card(ui: &mut Ui, label: &str, value: &str, color: Color32) {
    Frame::new()
        .fill(widgets::PANEL)
        .stroke(Stroke::new(1.0, widgets::BORDER))
        .corner_radius(12)
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(label).size(11.0).color(widgets::MUTED));
            ui.add_space(3.0);
            ui.add(Label::new(RichText::new(value).size(21.0).strong().color(color)).truncate());
        });
}

fn stats_card(ui: &mut Ui, title: &str, hint: &str, add: impl FnOnce(&mut Ui)) {
    widgets::card(ui, |ui| {
        widgets::section_header(ui, title, hint);
        add(ui);
    });
}

fn exposure_section(ui: &mut Ui, stats: &Stats) {
    let iso_data: Vec<(String, usize)> = top_numeric_counts(&stats.isos, 8)
        .into_iter()
        .map(|(value, count)| (format!("ISO {value}"), count))
        .collect();
    let aperture_data: Vec<(String, usize)> = top_numeric_counts(&stats.apertures, 8)
        .into_iter()
        .map(|(value, count)| {
            let text = format!("f/{}", trim_float(value as f64 / 10.0, 1));
            (text, count)
        })
        .collect();
    ui.columns(3, |columns| {
        columns[0].label(RichText::new("ISO").size(11.5).color(widgets::MUTED));
        columns[0].add_space(3.0);
        widgets::bar_rows(&mut columns[0], &iso_data, widgets::TEAL);
        columns[1].label(RichText::new("光圈").size(11.5).color(widgets::MUTED));
        columns[1].add_space(3.0);
        widgets::bar_rows(&mut columns[1], &aperture_data, widgets::ACCENT);
        columns[2].label(RichText::new("焦距").size(11.5).color(widgets::MUTED));
        columns[2].add_space(3.0);
        widgets::bar_rows(&mut columns[2], &stats.focal_bins, widgets::GREEN);
    });
}

fn video_stats_section(ui: &mut Ui, stats: &Stats) {
    ui.horizontal(|ui| {
        widgets::pill(
            ui,
            &format!("总时长 {}", format_duration(stats.video_seconds)),
            widgets::TEAL,
        );
        widgets::pill(ui, &format!("视频 {}", stats.videos), widgets::ACCENT);
    });
    ui.add_space(10.0);
    ui.label(RichText::new("编码").size(11.5).color(widgets::MUTED));
    ui.add_space(3.0);
    widgets::bar_rows(
        ui,
        &stats::sorted_counts(&stats.video_codecs, 8),
        widgets::ACCENT,
    );
    ui.add_space(10.0);
    ui.label(RichText::new("分辨率").size(11.5).color(widgets::MUTED));
    ui.add_space(3.0);
    widgets::bar_rows(
        ui,
        &stats::sorted_counts(&stats.video_resolutions, 8),
        widgets::TEAL,
    );
    ui.add_space(10.0);
    ui.label(RichText::new("拍摄设备").size(11.5).color(widgets::MUTED));
    ui.add_space(3.0);
    widgets::bar_rows(
        ui,
        &stats::sorted_counts(&stats.video_makes, 6),
        widgets::GREEN,
    );
    if !stats.video_years.is_empty() {
        ui.add_space(10.0);
        widgets::years_chart(ui, &stats.video_years, widgets::ACCENT);
    }
}

fn top_numeric_counts(map: &BTreeMap<u32, usize>, limit: usize) -> Vec<(u32, usize)> {
    let mut values: Vec<(u32, usize)> = map.iter().map(|(key, value)| (*key, *value)).collect();
    values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    values.truncate(limit);
    values.sort_by_key(|(key, _)| *key);
    values
}

fn orientation_options() -> Vec<(u16, &'static str)> {
    vec![
        (1, "1 · 正常"),
        (2, "2 · 水平翻转"),
        (3, "3 · 旋转 180°"),
        (4, "4 · 垂直翻转"),
        (5, "5 · 转置"),
        (6, "6 · 顺时针 90°"),
        (7, "7 · 反转置"),
        (8, "8 · 逆时针 90°"),
        (0, "未设置（移除方向标签）"),
    ]
}

fn orientation_label(value: u16) -> &'static str {
    orientation_options()
        .into_iter()
        .find(|(option, _)| *option == value)
        .map(|(_, label)| label)
        .unwrap_or("1 · 正常")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_fraction_and_nearest_index() {
        assert!((time_fraction(200, (100, 300)) - 0.5).abs() < f32::EPSILON);
        assert_eq!(time_fraction(50, (100, 300)), 0.0);
        assert_eq!(time_fraction(400, (100, 300)), 1.0);
        let order = [2, 0, 1];
        let times = [100, 300, 200];
        assert_eq!(nearest_time_index(&order, &times, 210), Some(0));
        assert_eq!(nearest_time_index(&order, &times, 299), Some(2));
        assert_eq!(format_timeline_date(0, true), "1970.01.01");
    }

    #[test]
    fn damaged_image_errors_are_friendly() {
        let message = friendly_media_error(
            "无法解码预览：Format error decoding Jpeg: Error parsing image. Illegal start byte",
        );
        assert_eq!(message, "照片文件可能已损坏，或该格式暂不支持");
        assert_eq!(
            friendly_media_error("未找到可读取的 EXIF: Unknown image format"),
            "照片文件可能已损坏，或该格式暂不支持"
        );
        assert_eq!(
            friendly_media_error("未找到可读取的 EXIF：Broken JPEG file"),
            "照片文件可能已损坏，或该格式暂不支持"
        );
        assert_eq!(
            friendly_media_error("此格式暂不支持内置预览，但 EXIF 信息仍可查看。"),
            "此格式暂不支持内置预览，但 EXIF 信息仍可查看。"
        );
    }
}
