# 架构与实现

本文说明代码如何组织、状态如何流动，以及修改时需要遵守的约束。用户功能和构建步骤见 [README](../README.md)。

## 1. 总体结构

Exif Studio 是单进程桌面程序。UI 使用 `eframe`/`egui` 原生绘制，没有 WebView、前端资源包或数据库。目录中的媒体文件是唯一数据源，应用只维护内存索引和可删除的缓存。

```text
用户选择目录
    │
    ▼
FolderTab ──► scan ──► entries（媒体条目）
    │                     │
    │                     ├─► filtered（筛选/排序后的索引）──► 虚拟化网格
    │                     ├─► DetailsService ──► EXIF 详情/编辑
    │                     └─► Vision ──► 相似组/优选星标
    │
    ├─► ImageService ──► 缩略图/预览纹理
    └─► ApiRegistry ──► 本机只读 HTTP API
```

模块职责：

| 模块 | 职责 |
| --- | --- |
| `main.rs` | 命令行参数、窗口、wgpu 后端初始化 |
| `app.rs` | 应用状态、标签页、所有视图、交互与轮询调度 |
| `model.rs` | `MediaEntry`、`ImageInfo`、`VideoInfo`、格式列表与格式化 |
| `scan.rs` | 流式目录发现、并行元数据读取、扫描消息 |
| `details.rs` | 详情线程，加载完整 EXIF 与可编辑字段 |
| `exif_io.rs` | EXIF 读取、分组、缩略图提取、字段校验与写入 |
| `video.rs` | MP4/MOV 家族容器解析，只读 |
| `video_poster.rs` | 视频首帧：Windows Shell、macOS Quick Look、Linux ffmpeg |
| `vision.rs` | dHash 指纹、画质评分、相似组聚类、指纹缓存 |
| `watermark.rs` | LSB 水印、PNG 副本、视觉索引与截图识别 |
| `stats.rs` | 统计聚合 |
| `cache.rs` | 缩略图磁盘缓存、容量上限与 LRU |
| `metacache.rs` | 文件元数据 JSON 缓存 |
| `catalog.rs` | 星标持久化 |
| `settings.rs` | 配置持久化与默认值 |
| `api.rs` | 回环 HTTP 服务、Token 鉴权、只读查询 |
| `platform.rs` | 配置目录、字体、文件管理器/播放器调用 |
| `widgets.rs` | 主题、通用绘制组件、滚动条样式 |

## 2. 线程模型

| 工作 | 线程 | 结果如何回到 UI |
| --- | --- | --- |
| 目录发现 | 每个扫描任务 1 个线程 | `ScanMsg::Discovering/Started` |
| 元数据读取 | 4–24 个工作线程 | 有序 `ScanMsg::Entries`，批量 32 条 |
| 缩略图 | 4–8 个工作线程 | `ImageResult`，UI 侧限制每帧上传 6 张纹理 |
| 大图预览 | 1 个工作线程 | `ImageResult` |
| 完整 EXIF 详情 | 1 个线程 | `DetailsResult` |
| 视觉分析 | 1 个协调线程 + 4–16 个计算线程 | `VisionMsg::Progress/Finished/Failed` |
| 水印写入/识别 | 每次操作 1 个临时线程 | `mpsc` 结果，对话框轮询 |
| 本地 API | 1 个服务线程 | 共享 `ApiRegistry` |

UI 线程只在 `ExifStudioApp::ui` 中轮询这些通道。扫描、解码、EXIF 写入和视觉分析不会在 UI 线程执行；少数工作仍留在 UI 线程，例如缓存命中查询、视觉报告重建、配置写盘和 API 快照生成。这些路径都使用有界数据或被节流，新增功能时不要把大文件 IO 放回 UI 线程。

## 3. 启动与主循环

`main.rs` 解析参数：

```text
exif-studio <目录或文件>
exif-studio <目录> --stats
```

Windows 上 wgpu 只启用 DX12/Vulkan，避免部分 Intel 驱动的 WGL 后端栈溢出。窗口最小尺寸为 1080×700。

`ExifStudioApp::ui` 每帧的顺序基本固定：

1. 检查是否有模态窗口打开，更新毛玻璃背景状态。
2. 处理快捷键、拖入文件、文件夹选择器。
3. 轮询扫描、详情、视觉、水印、缩略图。
4. 绘制顶部栏、标签条、状态栏和当前标签页。
5. 绘制设置、水印、未保存确认、外部打开确认等窗口。
6. 绘制提示条、发布本地 API 快照。
7. 根据忙碌状态请求下一帧。

有扫描、缩略图请求、动画或提示条时，UI 会按 100ms 或更短间隔重绘；完全空闲时依赖 egui 的按需重绘。

模态弹窗背景通过 `ctx.request_screenshot` 截图、降采样和盒式模糊生成纹理。截图完成前会跳过底层的绘制，避免把遮罩也截进去。

### `with_tab_mut`

`ExifStudioApp` 同时持有全局服务（缩略图服务、缓存、设置）和 `Vec<FolderTab>`。在需要同时访问 `&mut self` 和某个标签页时，代码使用：

```rust
let mut tab = std::mem::replace(&mut self.tabs[index], FolderTab::placeholder());
let result = action(self, is_active, &mut tab);
self.tabs[index] = tab;
```

这样可以把标签页临时移出向量，避免借用整个 `self.tabs`。新增会跨标签页访问的 UI 时，沿用这个模式，不要在标签页闭包里再次索引 `self.tabs`。

## 4. 标签页状态

`FolderTab` 是主要的状态单元。字段可以按用途分组：

| 组 | 代表字段 | 说明 |
| --- | --- | --- |
| 来源 | `folder`, `canonical`, `recursive` | 目录与是否递归；路径比较使用词法归一化，不在 UI 线程做 `canonicalize` |
| 扫描 | `scan`, `scanning`, `scan_total`, `scan_done`, `scan_discovered` | 扫描句柄与进度 |
| 数据 | `entries`, `image_count`, `total_bytes` | 已读取的媒体条目和增量统计 |
| 筛选 | `search`, `kind_filter`, `favorites_only`, `folder_filter`, `filtered` | `filtered` 存的是 `entries` 的下标 |
| 排序 | `filtered_time_range`, `browse_time_cursor` | 时间排序时的时间轴状态 |
| 选择 | `selected`, `details`, `details_request`, `detail_tab` | 选中路径与详情加载状态 |
| 查重 | `vision_report`, `duplicate_order`, `duplicates_page`, `duplicates_start`, `duplicates_loaded` | 报告、展示顺序、分页/瀑布流位置 |
| 缓存标记 | `filter_dirty`, `stats_dirty`, `favorites_dirty`, `folder_groups_dirty`, `entry_index_dirty` | 决定哪块数据需要重算 |

关键不变量：

- `filtered` 中的每个值都必须是 `entries` 的有效下标。扫描追加条目时只更新标记，重建由 `refresh_filter` 完成。
- `selected` 是路径而不是下标；条目被重新读取或排序后仍然有效。`entry_index` 可用时是 O(1) 查找，否则退化为线性扫描。
- `entry_index` 在扫描期间保持 dirty，扫描结束后一次性构建；否则每批新条目都会重建整张路径表。
- `duplicates_page`、`duplicates_start` 和 `duplicates_loaded` 都针对“展示顺序”，不是 `report.groups` 的原始下标。展示顺序由 `duplicate_order` 映射。
- `duplicates_revision` 在报告、排序或扫描状态改变时递增；`duplicate_order_revision` 落后时重建顺序和时间轴数据。
- 修改 `entries`、星标或范围过滤后，必须同步更新对应 dirty 标记，否则统计、优选数或目录分组会显示旧值。

### 筛选与排序

`refresh_filter` 先按搜索、图片/视频、优选、子目录过滤，再按 `PhotoSort` 排序：

- `Filename`：依赖扫描阶段已按文件名排序的 `entries` 顺序，不重复排序。
- `TakenTime`：按拍摄时间降序，没有拍摄时间时用修改时间；同时计算 `filtered_time_range`，供浏览时间轴使用。
- `Resolution`：按像素数降序。

扫描过程中，筛选重建被限制在约 700ms 一次；目录 chip 和优选数约 900ms 一次。扫描结束后会强制刷新一次。这个节流是浏览 3.9 万文件时保持窗口缩放流畅的关键。

时间使用墙上时钟：EXIF 的 `DateTimeOriginal` 本身不带可靠时区，文件修改时间也按本地墙上时间处理。时间轴标签直接格式化这个值，不再做一次本地时区转换。

## 5. 扫描管线

`scan::start_scan` 分两阶段：

1. **发现**：`WalkDir` 只读目录项，按扩展名过滤，不调用 `metadata`。每 180ms 发送一次 `Discovering { files }`，界面因此能在网络共享上立即显示“已找到 N 个文件”。
2. **读取**：发现完成后按文件名排序并发送 `Started { total }`，最多 24 个线程并行执行 `metadata`、缓存查询和 EXIF/视频解析。

工作线程把 `(position, Option<MediaEntry>)` 发回协调线程，协调线程用 `BTreeMap` 按位置重排，再以 32 条为一批发给 UI。这样即使用户看到的是文件名顺序，元数据解析仍然是并行乱序完成的。

全局最多同时运行 2 个文件夹扫描；其余标签页进入排队状态。关闭标签页会 drop `ScanHandle`，通过 `AtomicBool` 通知所有工作线程退出。

发现阶段会先做一次 `read_dir` 检查，文件夹不可访问时直接发 `Failed`。单个文件读取失败会被跳过，不阻塞整次扫描。扫描线程在正常结束后保存元数据缓存；中途取消不会补写，切换缓存目录等设置操作会主动保存当前内存中的记录。

## 6. 缓存层

| 缓存 | 位置 | 键 | 失效条件 |
| --- | --- | --- | --- |
| 元数据 | `metadata.json` | 路径十六进制 + 文件大小 + 修改时间纳秒 | 文件大小/修改时间变化、超过 90 天、超 50,000 条或 128MB |
| 视觉指纹 | `vision.json` | 路径十六进制 + 大小 + 修改时间 | 文件大小/修改时间变化、超过 100,000 条或 64MB |
| 缩略图/预览 | `thumbs/<2位分片>/<hash>.cache` | FNV-1a(路径 + 大小 + 修改时间 + variant) | 文件变化导致 key 变化；按总字节数 LRU 清理 |

variant 常量在 `app.rs`：

- `THUMB_VARIANT`：112px 小图。
- `THUMB_LARGE_VARIANT`：256px 大图。
- `PREVIEW_VARIANT`：1600px 预览。

缓存文件使用临时文件 + 原子重命名写入。缩略图解码优先读取 JPEG 内嵌 EXIF 缩略图，只有尺寸不够或格式不支持时才解码原图。视频封面不进入元数据缓存，只进入图片缓存。

修改缓存内容格式时必须提升对应 `CACHE_VERSION`，否则旧 JSON 会被当作有效数据读入。缩略图缓存没有 JSON 版本字段，版本写在 `cache.rs` 的 FNV 盐值里；改变缩略图编码约定时也要同步修改它。

## 7. 缩略图与预览

`ImageService` 是通用图片工作池：

```rust
ImageJob { path, max_size, orientation, cache_key }
ImageResult { path, max_size, cache_key, result: Result<ColorImage, String> }
```

流程：

1. 先查磁盘缓存；命中时只做 `ColorImage` 转换。
2. JPEG 且请求尺寸不超过 256px 时，尝试读取内嵌缩略图。
3. 否则解码原图，应用 EXIF 方向，缩放并写回磁盘缓存。
4. 视频使用 `video_poster::load_poster`。
5. UI 把结果放入 `thumb_ready` 队列，每帧最多上传 6 张纹理；纹理总量上限 256，超出时按 LRU 淘汰。

网格请求只针对 `ScrollArea::show_rows` 当前可见的行。`MAX_THUMB_INFLIGHT` 限制在途任务，避免快速拖动滚动条时向工作池灌入过多请求。

## 8. EXIF 详情与写入

扫描阶段只读取列表需要的字段。选中文件后，`DetailsService` 再读取完整 EXIF 分组和可编辑字段，避免为 39k 个文件一次性构建完整详情。

读取路径：

- `kamadak-exif` 读取容器中的 EXIF。
- PNG/WebP/HEIF/AVIF/JXL 等格式在 kamadak-exif 不支持时，回退到 `little_exif::Metadata`。
- 宽高缺失时使用 `image::image_dimensions` 兜底。
- 损坏或不支持的图片返回简洁提示，原始错误只放在悬停提示里。

写入路径（`exif_io::save_edits`）：

1. 读取并解析现有 EXIF；如果文件已有无法安全解析的结构，直接拒绝写入。
2. 校验 ISO、光圈、快门、焦距、方向、GPS 等字段。
3. 按设置创建 `.bak` 备份副本。
4. 使用 `little_exif` 写回原文件。

写入不是数据库事务；备份是失败后的恢复手段。任何修改写入逻辑的改动都必须保留“先解析、再校验、必要时备份”的顺序，并补充对应测试。

可编辑字段定义在 `exif_io::EditFields`。新增字段需要同时处理读取、校验、写入、分组展示和编辑表单，具体步骤见 [DEVELOPMENT.md](DEVELOPMENT.md)。

## 9. 视频

`video.rs` 是手写的 BMFF 盒子解析器，不调用 ffmpeg 读元数据。支持 MP4、M4V、MOV、3GP、3G2，解析：

- `mvhd`：创建时间、时间刻度、时长。
- `tkhd`：轨道宽高。
- `stsd`/`stts`：编码 FourCC 与帧率。
- `udta`/`meta`/`ilst`：相机、创建时间、GPS 等标签。

所有切片在访问前都检查边界。视频元数据严格只读。

封面由 `video_poster.rs` 处理：Windows 优先 Shell 缩略图，失败后回退 ffmpeg；macOS 优先 Quick Look；Linux 使用 ffmpeg 或 ffmpegthumbnailer。封面缺失不影响元数据展示。

## 10. 查重

视觉指纹由 `vision.rs` 计算：

- 9×8 灰度图得到 64 位 dHash。
- 64×64 灰度图计算亮度、对比度和拉普拉斯锐度。
- 8×8 平均 RGB 用于颜色距离。
- 质量分综合锐度、对比度、曝光、分辨率和过曝/欠曝比例。

聚类使用 4 个 16 位桶加鸽巢探测：距离阈值 6 的候选对至少会在一个 16 位块中相差不超过 1 位，因此每个块只探测 17 个 key，避免 37k 图片两两比较。另有长宽比和平均颜色阈值过滤误匹配。

每组内按“分辨率 0.45、锐度 0.33、对比度 0.12、曝光 0.10”的综合分选出优选。报告完成后，`duplicate_order` 才根据用户选择的重复数量、时间或分辨率排序生成，`duplicates_revision` 用于让旧顺序失效。

视觉分析只在打开“查重”后启动，并且在切换视图动画结束后开始。扫描完成时如果缓存中已有报告，会先构建一个临时报告，让浏览视图的自动优选星标立即可见。dHash 和画质评分是 CPU 位运算，GPU 不参与计算。

## 11. 统计、星标、水印与 API

- `stats.rs` 在 `stats_dirty` 时对整个 `entries` 做一次聚合，结果缓存在 `FolderTab::stats`。
- `catalog.rs` 把手动星标和“取消自动优选”保存到 `ratings.json`，不修改照片文件。
- `watermark.rs` 将文本写入 PNG 副本的 LSB。格式为 9 字节头（`EXSW` + 版本 + 长度）加文本；口令用于打散位序，不是加密。视觉索引保存 dHash、pHash 和平均颜色，用于截图/压缩后的本机匹配，索引上限 2000 条、16MB，搜索预算 2.2 秒。
- `api.rs` 的 `ApiRegistry` 由 UI 线程发布快照，最多每秒一次。HTTP 线程只读这个快照，不持有 `FolderTab`，因此 API 不会与 UI 争用扫描状态。启用 API 且目录很大时，快照生成本身会遍历所有条目，这是已知成本。

## 12. 性能敏感点

修改浏览/查重 UI 时，优先检查以下位置：

1. 不要在每个 frame 遍历 `entries`。计数、目录分组、优选数和路径索引都有缓存或 dirty 标记。
2. 网格必须继续使用 `ScrollArea::show_rows` 或等价的行虚拟化；不要把所有缩略图一次性加入 UI。
3. 纹理上传必须保留每帧上限。网络共享上解码结果可能集中到达，一次性上传几十张会直接造成掉帧。
4. 时间排序的排序和筛选重建在扫描期间要保留节流；否则每批 32 个文件都会重新排序 39k 条。
5. 增量加载列表使用 `stick_to_bottom` 时要注意与 `vertical_scroll_offset` 的交互：显式回顶的帧要关闭贴底，下一帧再恢复，否则旧的“贴底”状态会把视图拉回底部。
6. 滚动条最小长度由 `widgets::app_theme` 的 `handle_min_length` 控制。列表很长时不要改回默认值，否则滑块会缩小到难以点击。
7. 网络共享上的性能测试必须用 release 构建；debug 构建受 `image` 解码和 egui 布局影响很大。

本仓库在一台 Windows 机器上对 39,496 个文件的 SMB 目录做过基线测试：扫描约 37 秒，发现进度在 1 秒内出现；打开约 3.9 万条目的浏览页后连续缩放窗口，UI 线程消息响应保持在毫秒级。具体数字会随网络、磁盘和 CPU 变化，但测试方法可以复现。

## 13. 常见改动位置

| 需求 | 主要文件 |
| --- | --- |
| 新 EXIF 字段 | `model.rs`、`exif_io.rs`、`app.rs`（编辑表单） |
| 新图片格式 | `model.rs`、`Cargo.toml`、`thumbs.rs`、`vision.rs` |
| 新视频容器 | `model.rs`、`video.rs`、`video_poster.rs` |
| 新视图/面板 | `app.rs` 的 `ViewTab`、`view_switch`、`central_*` |
| 新排序 | `settings.rs`、`app.rs` 的 `refresh_filter` / duplicate order |
| 新设置 | `settings.rs`、`app.rs` 的设置窗口与 `apply_settings` |
| 新 API 字段/端点 | `api.rs`、`docs/API.md` |
| 新缓存格式 | 对应模块和 `CACHE_VERSION` |

更具体的操作步骤和测试要求见 [DEVELOPMENT.md](DEVELOPMENT.md)。
