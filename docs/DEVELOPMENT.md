# 开发与维护

本文面向准备修改代码的人。先读 [ARCHITECTURE.md](ARCHITECTURE.md) 了解模块和状态不变量，再按这里的步骤操作。

## 1. 工具链

- Rust stable，edition 2024，最低 1.85。
- Windows 是当前主要开发和测试平台。
- macOS/Linux 使用同一套 Rust 代码；平台差异集中在 `platform.rs` 和 `video_poster.rs`。

常用命令：

```bash
cargo check
cargo fmt
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

release 构建在 Windows 上是 GUI 子系统，运行时不显示控制台。需要看 panic/backtrace 时使用 debug 构建：

```bash
RUST_BACKTRACE=1 cargo run -- "D:\\Photos"
```

## 2. 本地测试素材

仓库不提交二进制测试素材。`samples/` 和 `_shots/` 都在 `.gitignore` 中。

```bash
# 生成包含 JPEG、PNG、TIFF、MP4/MOV 的本地样例
cargo run --example make_samples
# 默认写入 ./samples
```

这些样例只覆盖基本路径。涉及缩略图、EXIF 写入、视频解析或 SMB 性能时，应该再用真实相机文件、不同扩展名和损坏文件测试。

## 3. 功能改动指南

### 3.1 新增一个 EXIF 字段

以新增一个只读字段为例：

1. 在 `model.rs::ImageInfo` 增加字段，并给 `Default` 补值。
2. 在 `exif_io::read_image_info` 中解析。常用辅助函数是 `find_ascii`、`find_uint`、`find_rational`。
3. 如果该字段需要展示，在 `groups_from_exif` 中加入到对应分组；折叠摘要由 `app.rs::collapsed_summary` 控制。
4. 如果字段来自 `little_exif` 的 fallback，在 `read_editable` 和 `EditFields` 中补充读取状态，并在 `apply_editable_info` 中写入 `ImageInfo`。
5. 如果需要可编辑，完整改动包括：
   - `EditFields` 新字段；
   - `read_editable`；
   - `save_edits` 及 `validate_fields`；
   - `groups_from_editable`；
   - `app.rs::show_edit_tab` 的表单和保存逻辑；
   - `exif_io` 的往返测试。
6. 如果 API 应该暴露该字段，在 `api.rs::ApiFile` 和 `from_entry` 中增加映射，并更新 `docs/API.md`。

EXIF 写入的底线：已有 EXIF 无法安全解析时拒绝写入；字段校验在写盘前完成；默认创建备份。不要在 UI 线程直接写文件。

### 3.2 新增图片格式

格式列表分散在几个地方，漏掉任何一处都会造成“能扫描但没缩略图”或“能预览但不参与查重”：

1. `model.rs::image_extensions`。
2. `Cargo.toml` 的 `image` crate features（如果需要内置解码）。
3. `app.rs::is_previewable`。
4. `model.rs::is_writable_extension`（如果支持写入）。
5. `vision.rs::is_visual_extension`（如果参与查重）。
6. `exif_io::supports_editable_fallback`（如果 `little_exif` 能读该容器）。
7. README 的格式表和 `docs/API.md` 的格式说明。

RAW 目前不支持。如果以后加入 RAW，需要同时考虑内嵌预览、相机 EXIF 方言和写入安全，不能只把扩展名加进列表。

### 3.3 新增视频容器

视频元数据由 `video.rs` 的 BMFF 解析器读取，不依赖 ffmpeg：

1. `model.rs::video_extensions`。
2. `video.rs` 的 box 解析、轨道解析和标签映射。
3. `video_poster.rs` 的封面回退策略。
4. 测试 `video.rs` 的解析测试，覆盖截断、超大长度字段和缺失 box。

视频始终按只读处理。不要在视频路径上增加写元数据功能。

### 3.4 新增视图或页面

1. 在 `app.rs` 的 `ViewTab` 增加枚举值。
2. 更新 `view_switch` 的 `ITEMS`。
3. 在 `handle_shortcuts` 的 `views` 数组增加快捷键。
4. 在 `show_workspace_for` 的 `match` 中调用新的 `central_*` 方法。
5. 如果该视图需要独立状态，加到 `FolderTab` 并在 `FolderTab::new`、`scan_tab` 中初始化和重置。
6. 确认 `sync_tab_content` 不会在每次重绘时做重 IO。

视图切换会使用胶囊动画；如果新视图有首次加载成本，参考查重视图，先延迟分析再显示进度，而不是在切换的同一帧启动全部工作。

### 3.5 新增排序方式

1. `settings.rs` 增加枚举和默认值，并加入 `Settings` 字段；`#[serde(default)]` 保证旧配置可读。
2. `app.rs::refresh_filter` 实现排序键和 `filtered_time_range` 的维护。
3. 在浏览侧栏或查重标题栏增加下拉框。
4. 排序变化时重置滚动位置；时间轴数据必须一起失效。
5. 补充 `settings.rs` 的缺字段测试和 `app.rs` 的排序/时间轴单元测试。

浏览排序作用于 `filtered`，不要直接重排 `entries`。查重排序作用于 `duplicate_order`，不要直接改 `VisionReport::groups`，否则优选星标和原始分组会被破坏。

### 3.6 新增设置项

1. `settings.rs` 加字段、默认值、`normalize` 校验。
2. 设置窗口里加控件，并同步 `settings_draft`。
3. 在 `apply_settings` 中处理即时生效和需要重建服务的变更。
4. 如果改动会影响缓存目录、缩略图服务或扫描，先检查是否有扫描/视觉分析正在运行。
5. 增加旧配置缺字段的测试。

设置文件在配置目录，不要把它写进仓库。修改缓存语义时提升对应 `CACHE_VERSION`。

### 3.7 新增本地 API 端点

1. 在 `api.rs::handle_request` 加路由，保持只读 `GET`。
2. 查询数据必须来自 `ApiRegistry` 的标签页快照，不要直接持有 UI 状态。
3. 如果需要新字段，在 `ApiFile`/`ApiFolder` 和发布逻辑中增加。
4. 更新 `docs/API.md`，说明参数、返回字段和错误码。
5. 不要在未鉴权路径返回文件内容、缩略图或完整目录列表。

本地 API 是可选功能，默认关闭。不得把它改成非回环监听或增加写入能力而不做额外的授权设计。

### 3.8 界面文案与本地化

当前界面文案直接写在 Rust 代码中，以中文为主。若要做多语言：

1. 先建立集中的文案表（例如新增 `src/text.rs`），用稳定的 key 替代散落在 `app.rs` 的字符串。
2. 先迁移高频界面（顶部栏、浏览、设置、错误提示），再迁移统计和查重。
3. 保留调试/错误上下文中的原始技术信息，但用户可见提示要使用简短文案。
4. CSV/JSON 等导出字段如果需要稳定，不要跟随界面语言变化。

## 4. 性能规则

这些规则来自 SMB 上 3.9 万文件的实测，改动浏览路径时请保留：

- **不要每帧遍历全部条目**。使用 `image_count`、`total_bytes`、`favorite_count`、`folder_group_cache`、`entry_index` 等缓存；修改条目后设置 dirty 标记。
- **不要每帧重建路径索引**。扫描期间保持 dirty，扫描结束后用 `rebuild_entry_index` 一次构建。
- **网格只渲染可见行**。使用 `ScrollArea::show_rows`。
- **限制纹理上传**。`MAX_TEXTURE_UPLOADS_PER_FRAME` 是 6；不要因为“照片出得慢”就把它调到几十。
- **扫描期间节流筛选**。`SCAN_FILTER_INTERVAL`、`SCAN_CHIP_INTERVAL` 和 `FAVORITE_REFRESH_INTERVAL` 分别控制筛选、目录 chip 和优选数。
- **增量加载列表不要回跳**。`stick_to_bottom` 只在用户位于底部时生效；显式回到顶部后要经过一帧再恢复贴底（见 `duplicates_stick_ready`）。
- **性能测试用 release**。debug 构建的 `image` 解码和 egui 布局不能代表实际体验。
- **网络共享测试**要记录：文件数量、图片/视频比例、是否冷缓存、窗口缩放是否卡顿、首屏进度出现时间。不要只测本地 SSD。

常用测试路径：

```text
\\server\share\photos
```

测试前确认共享凭据已在本机建立。测试时不要提交真实照片、共享地址或包含个人信息的截图。

## 5. 测试与检查

每次提交至少运行：

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

测试重点：

- `exif_io`：EXIF 往返、PNG/WebP fallback、GPS、清空字段、备份。
- `video`：截断 box、错误长度、正常 MP4/MOV。
- `vision`：相同图指纹一致、相似组聚类、缓存往返。
- `watermark`：无损往返、错误口令、二次压缩识别、原图不变。
- `cache`/`metacache`：TTL、LRU、超限、原子写入。
- `api`：Token 比较、URL 解码。

UI 改动没有自动化视觉回归；至少用 release 构建手动检查浏览、统计、查重、设置、编辑和弹窗布局。涉及滚动/缩放时，在真实大目录上测试，而不是只看 `samples`。

## 6. 发布检查表

1. 更新 `Cargo.toml` 版本。
2. 运行格式化、clippy、全部测试。
3. `cargo build --release`。
4. 在 Windows 上启动 release 构建，检查：
   - 打开目录、扫描进度和窗口缩放；
   - 图片缩略图、预览、EXIF 详情；
   - 视频封面和外部打开确认；
   - 查重分页/瀑布流、排序、时间轴；
   - 设置、缓存目录、星标、水印；
   - 本地 API 默认关闭。
5. 复制产物到 `dist/ExifStudio.exe`（`dist/` 不提交）。
6. 推送 tag 前确认仓库没有 `samples/`、`_shots/`、缓存、设置文件、备份文件或个人路径。

## 7. 常见坑

- **借用冲突**：同时需要 `&mut ExifStudioApp` 和 `&mut FolderTab` 时，用 `with_tab_mut` 的临时替换模式，不要强行拆分借用到 UI 闭包里。
- **扫描后忘记 dirty**：条目追加、删除、重新读取、修改星标、切换排序，都可能影响筛选/统计/查重顺序。按数据依赖设置对应标记。
- **时间口径**：EXIF 时间没有时区，文件修改时间按本地墙上时间处理。时间轴和排序不要混用 UTC 时间戳与本地墙上时间。
- **滚动回跳**：`vertical_scroll_offset(0.0)` 与 `stick_to_bottom(true)` 同帧使用时，旧贴底状态可能在下一次布局把视图拉回去。回顶帧关闭贴底，下一帧再恢复。
- **Windows release 无控制台**：release 使用 GUI 子系统，panic 和日志默认看不到；调试时用 debug 构建或重定向输出。
- **缓存版本**：修改 `MetadataCache`、`VisionCache` 或缩略图缓存内容格式时，提升 `CACHE_VERSION`，否则旧缓存会被错误复用。
- **EXIF 写入**：不要绕过 `save_edits` 的解析和校验直接写文件；不要让失败路径跳过备份。
- **隐私**：问题复现、截图和 PR 中不要包含真实照片、共享地址、用户名、API Token 或个人信息。`_shots/` 已加入 `.gitignore`，不要强行提交。
