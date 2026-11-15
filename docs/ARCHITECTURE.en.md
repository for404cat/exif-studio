# Architecture and internals

[简体中文](ARCHITECTURE.md) | English

This document explains how the code is organized, how state moves through the application, and which constraints matter when changing it. User-facing features and build steps are in [README.en.md](../README.en.md).

## 1. Overall shape

Exif Studio is a single-process desktop application. The UI is drawn with `eframe`/`egui`; there is no WebView, frontend bundle or database. Media files on disk are the only source of truth. The app keeps an in-memory index and disposable caches.

```text
user selects a folder
    │
    ▼
FolderTab ──► scan ──► entries (media records)
    │                     │
    │                     ├─► filtered (indices after filtering/sorting) ──► virtualized grid
    │                     ├─► DetailsService ──► EXIF details/editing
    │                     └─► Vision ──► duplicate groups / auto picks
    │
    ├─► ImageService ──► thumbnail/preview textures
    └─► ApiRegistry ──► loopback read-only HTTP API
```

Module responsibilities:

| Module | Responsibility |
| --- | --- |
| `main.rs` | CLI arguments, window, wgpu backend setup |
| `app.rs` | application state, tabs, all views, interaction and polling |
| `model.rs` | `MediaEntry`, `ImageInfo`, `VideoInfo`, format lists and formatting |
| `scan.rs` | streaming discovery, parallel metadata reads, scan messages |
| `details.rs` | detail thread for full EXIF and editable fields |
| `exif_io.rs` | EXIF reading, grouping, embedded thumbnails, validation and writing |
| `video.rs` | read-only MP4/MOV-family container parser |
| `video_poster.rs` | video posters: Windows Shell, macOS Quick Look, Linux ffmpeg |
| `vision.rs` | dHash fingerprints, quality scoring, clustering, fingerprint cache |
| `watermark.rs` | LSB watermark, PNG copy, visual index and screenshot matching |
| `stats.rs` | aggregate statistics |
| `cache.rs` | thumbnail disk cache, size limit and LRU |
| `metacache.rs` | file metadata JSON cache |
| `catalog.rs` | persistent ratings |
| `settings.rs` | persisted settings and defaults |
| `api.rs` | loopback HTTP server, token auth, read-only queries |
| `platform.rs` | config directories, fonts, file manager/player launching |
| `widgets.rs` | theme, shared drawing widgets, scrollbar styling |

## 2. Thread model

| Work | Threads | Results reach the UI via |
| --- | --- | --- |
| Directory discovery | one per scan job | `ScanMsg::Discovering/Started` |
| Metadata reads | 4–24 workers | ordered `ScanMsg::Entries`, batches of 32 |
| Thumbnails | 4–8 workers | `ImageResult`; the UI uploads at most 6 textures per frame |
| Preview | 1 worker | `ImageResult` |
| Full EXIF details | 1 thread | `DetailsResult` |
| Vision analysis | 1 coordinator + 4–16 compute workers | `VisionMsg::Progress/Finished/Failed` |
| Watermark write/extract | one temporary thread per operation | `mpsc` result polled by the dialog |
| Local API | 1 server thread | shared `ApiRegistry` |

The UI thread polls these channels in `ExifStudioApp::ui`. Scanning, decoding, EXIF writing and visual analysis do not run on the UI thread. A few tasks still do: cache-hit lookups, rebuilding a cached vision report, writing settings, and generating the API snapshot. They operate on bounded data or are throttled. Do not move large file IO back into the UI thread.

## 3. Startup and the main loop

`main.rs` parses:

```text
exif-studio <folder-or-file>
exif-studio <folder> --stats
```

On Windows, wgpu enables only DX12/Vulkan to avoid a WGL backend stack overflow seen on some Intel drivers. The minimum window size is 1080×700.

Each frame of `ExifStudioApp::ui` roughly follows this order:

1. Check whether a modal is open; update the frosted-glass backdrop state.
2. Handle shortcuts, dropped files and the folder picker.
3. Poll scans, details, vision, watermark and image jobs.
4. Draw the top bar, tab strip, status bar and current tab.
5. Draw Settings, Watermark, unsaved-changes and external-open dialogs.
6. Draw the toast and publish a local API snapshot.
7. Request another frame if work or animations are active.

While scanning, thumbnails, animations or a toast are active, the UI schedules repaints at 100 ms or shorter intervals. When idle, egui repaints on demand.

Modal backdrops are captured with `ctx.request_screenshot`, downsampled and box-blurred into a texture. The underlying UI is skipped for the capture frames so the dimming layer is not embedded in the screenshot.

### `with_tab_mut`

`ExifStudioApp` owns global services (image service, caches, settings) and `Vec<FolderTab>`. To access `&mut self` and one tab at the same time, the code temporarily moves the tab out:

```rust
let mut tab = std::mem::replace(&mut self.tabs[index], FolderTab::placeholder());
let result = action(self, is_active, &mut tab);
self.tabs[index] = tab;
```

Follow this pattern when adding cross-tab UI. Do not re-index `self.tabs` inside the tab closure.

## 4. Per-tab state

`FolderTab` is the main state unit. Its fields fall into these groups:

| Group | Examples | Notes |
| --- | --- | --- |
| Source | `folder`, `canonical`, `recursive` | Path comparison is lexical; the UI thread does not call `canonicalize` |
| Scan | `scan`, `scanning`, `scan_total`, `scan_done`, `scan_discovered` | Scan handle and progress |
| Data | `entries`, `image_count`, `total_bytes` | Loaded media records and incremental totals |
| Filter | `search`, `kind_filter`, `favorites_only`, `folder_filter`, `filtered` | `filtered` stores indices into `entries` |
| Sort | `filtered_time_range`, `browse_time_cursor` | Timeline state for time sorting |
| Selection | `selected`, `details`, `details_request`, `detail_tab` | Selected path and detail loading |
| Duplicates | `vision_report`, `duplicate_order`, `duplicates_page`, `duplicates_start`, `duplicates_loaded` | Report, display order, page/waterfall position |
| Dirty flags | `filter_dirty`, `stats_dirty`, `favorites_dirty`, `folder_groups_dirty`, `entry_index_dirty` | Decide what needs recomputation |

Key invariants:

- Every value in `filtered` is a valid index into `entries`. Appending entries only sets dirty flags; `refresh_filter` rebuilds the list.
- `selected` is a path, not an index. It remains valid after sorting or re-reading a file. `entry_index` gives O(1) lookup when available, otherwise lookup falls back to a linear scan.
- `entry_index` stays dirty during scanning and is built once when scanning finishes. Otherwise every batch of new entries would rebuild a 39k-entry path map.
- `duplicates_page`, `duplicates_start` and `duplicates_loaded` refer to the *display order*, not raw `report.groups` indices. `duplicate_order` maps display order to report indices.
- `duplicates_revision` increases when the report, sort order or scan state changes. `duplicate_order_revision` lags behind until the order and timeline data are rebuilt.
- After modifying `entries`, ratings or range filters, update the corresponding dirty flags. Otherwise counts, picks or folder chips can show stale data.

### Filtering and sorting

`refresh_filter` applies search, image/video, favorites and subfolder filters, then sorts by `PhotoSort`:

- `Filename`: relies on the filename order produced by scanning; it does not sort again.
- `TakenTime`: capture time descending, falling back to file modification time. It also computes `filtered_time_range` for the browse timeline.
- `Resolution`: pixel count descending.

During scanning, filtering is rebuilt at most about every 700 ms; folder chips and favorite counts about every 900 ms. A final refresh runs after scanning. This throttling is what keeps window resizing smooth with 39k entries.

Times are wall-clock values. EXIF `DateTimeOriginal` has no reliable zone, and file modification times are treated as local wall time. Timeline labels format that value directly, without another timezone conversion.

## 5. Scan pipeline

`scan::start_scan` has two phases:

1. **Discovery**: `WalkDir` reads directory entries only, filters by extension, and does not call `metadata`. It sends `Discovering { files }` every 180 ms, so the UI shows a discovered count immediately even on SMB.
2. **Read**: after discovery, candidates are sorted by filename and `Started { total }` is sent. Up to 24 workers run `metadata`, cache lookups and EXIF/video parsing in parallel.

Workers send `(position, Option<MediaEntry>)` back. The coordinator reorders them with a `BTreeMap` and emits batches of 32 to the UI. This gives filename-ordered results even though parsing completes out of order.

At most two folder scans run at once; other tabs queue. Dropping a tab drops its `ScanHandle`, and an `AtomicBool` tells all workers to stop.

Discovery first calls `read_dir` once so an inaccessible folder produces `Failed`. Individual file failures are skipped without stopping the scan. The metadata cache is saved when the scan thread finishes normally. A cancelled scan does not write again; settings changes such as a cache-directory move save the current in-memory records explicitly.

## 6. Cache layers

| Cache | Location | Key | Invalidation |
| --- | --- | --- | --- |
| Metadata | `metadata.json` | hex path + size + mtime nanoseconds | size/mtime change, older than 90 days, more than 50,000 records or 128 MB |
| Visual fingerprints | `vision.json` | hex path + size + mtime | size/mtime change, more than 100,000 records or 64 MB |
| Thumbnails/previews | `thumbs/<2-char shard>/<hash>.cache` | FNV-1a(path + size + mtime + variant) | file change changes the key; total-size LRU eviction |

Variant constants live in `app.rs`:

- `THUMB_VARIANT`: 112 px.
- `THUMB_LARGE_VARIANT`: 256 px.
- `PREVIEW_VARIANT`: 1600 px.

Cache files are written through a temporary file and atomic rename. Thumbnail decoding first tries the embedded JPEG EXIF thumbnail, falling back to a full decode. Video posters only enter the image cache, not the metadata cache.

Bump the appropriate `CACHE_VERSION` when changing the cached JSON shape. Thumbnail cache entries have no JSON version field; the version is the FNV salt in `cache.rs`. Change it when the thumbnail encoding contract changes.

## 7. Thumbnails and previews

`ImageService` is a generic image worker pool:

```rust
ImageJob { path, max_size, orientation, cache_key }
ImageResult { path, max_size, cache_key, result: Result<ColorImage, String> }
```

The flow is:

1. Check the disk cache; a hit only needs a `ColorImage` conversion.
2. For JPEG at 256 px or less, try the embedded thumbnail.
3. Otherwise decode, apply EXIF orientation, resize and store in the disk cache.
4. Videos go through `video_poster::load_poster`.
5. The UI puts results in `thumb_ready` and uploads at most 6 textures per frame. The texture count is capped at 256 with LRU eviction.

Grid requests only cover rows visible to `ScrollArea::show_rows`. `MAX_THUMB_INFLIGHT` limits in-flight jobs so fast scrollbar dragging cannot flood the pool.

## 8. EXIF details and writing

Scanning only reads fields needed by the list. After a file is selected, `DetailsService` reads the full EXIF groups and editable fields. This avoids building full details for 39k files at once.

Read path:

- `kamadak-exif` reads EXIF from containers it supports.
- PNG/WebP/HEIF/AVIF/JXL fall back to `little_exif::Metadata` when `kamadak-exif` cannot read the container.
- Missing dimensions fall back to `image::image_dimensions`.
- Damaged or unsupported images produce a short message; the raw error is only shown in a hover tooltip.

Write path (`exif_io::save_edits`):

1. Read and parse existing EXIF. If existing metadata cannot be parsed safely, refuse the write.
2. Validate ISO, aperture, shutter, focal length, orientation and GPS.
3. Create a `.bak` copy according to settings.
4. Write the original file with `little_exif`.

This is not a database transaction; the backup is the recovery path. Any change to writing must preserve “parse, validate, optionally back up” and add tests.

Editable fields are defined in `exif_io::EditFields`. Adding one means updating reading, validation, writing, group display and the edit form. See [DEVELOPMENT.en.md](DEVELOPMENT.en.md).

## 9. Video

`video.rs` is a hand-written BMFF box parser; metadata does not depend on ffmpeg. It supports MP4, M4V, MOV, 3GP and 3G2 and reads:

- `mvhd`: creation time, timescale, duration.
- `tkhd`: track width and height.
- `stsd`/`stts`: codec FourCC and frame rate.
- `udta`/`meta`/`ilst`: camera, creation time, GPS and other tags.

All slices are bounds-checked before access. Video metadata is strictly read-only.

Posters are handled by `video_poster.rs`: Windows Shell first, then ffmpeg; macOS Quick Look first, then ffmpeg; Linux uses ffmpeg or ffmpegthumbnailer. A missing poster does not affect metadata display.

## 10. Duplicate detection

`vision.rs` computes:

- A 64-bit dHash from a 9×8 grayscale image.
- Brightness, contrast and Laplacian sharpness from a 64×64 grayscale image.
- Average RGB from an 8×8 image for color distance filtering.
- A quality score combining sharpness, contrast, exposure, resolution and clipping.

Clustering uses four 16-bit buckets and pigeonhole probing. Any pair within distance 6 must agree within one bit in at least one 16-bit block, so each block only probes 17 keys instead of comparing all 37k photos pairwise. Aspect ratio and average color thresholds filter false matches.

Inside a group, the preferred frame is scored with resolution 0.45, sharpness 0.33, contrast 0.12 and exposure 0.10. The display order (`duplicate_order`) is generated separately from the report according to duplicate count, time or resolution. `duplicates_revision` invalidates stale orders.

Visual analysis starts only after the Duplicates view opens, after the transition animation. If a cached report exists when scanning finishes, a provisional report is built first so auto-pick stars appear in Browse immediately. dHash and quality scoring are CPU bit operations; the GPU is not involved.

## 11. Statistics, ratings, watermark and API

- `stats.rs` aggregates all `entries` once when `stats_dirty` is set and stores the result in `FolderTab::stats`.
- `catalog.rs` saves manual stars and “cancel auto pick” decisions in `ratings.json`; it never modifies photo files.
- `watermark.rs` writes text into the LSB of a PNG copy. The payload is a 9-byte header (`EXSW`, version, length) plus text. The passphrase scrambles bit positions; it is not encryption. The visual index stores dHash, pHash and average color for local matching after screenshots or recompression. It is capped at 2,000 records / 16 MB with a 2.2-second search budget.
- `api.rs` publishes an `ApiRegistry` snapshot from the UI thread at most once per second. The HTTP thread only reads that snapshot and never holds `FolderTab`, so API access does not contend with scanning state. With large folders, generating the snapshot itself walks every entry; this is a known cost.

## 12. Performance-sensitive points

When changing browsing or duplicate UI, check these first:

1. Do not iterate all `entries` per frame. Counts, folder groups, favorite counts and the path index are cached or dirty-flagged.
2. Keep grid row virtualization with `ScrollArea::show_rows` or an equivalent; do not add all thumbnails to the UI at once.
3. Keep the per-frame texture upload cap. On a network share, decoded results can arrive in bursts; uploading dozens at once causes visible drops.
4. Keep filtering/sorting throttled during scanning. Otherwise every 32-file batch would re-sort 39k entries.
5. When a list grows incrementally, be careful with `stick_to_bottom` plus `vertical_scroll_offset`. Disable sticking on the frame that explicitly returns to the top, then re-enable it one frame later; otherwise an old stuck state pulls the view back down.
6. The scrollbar minimum handle length is set in `widgets::app_theme` via `handle_min_length`. Do not revert it to the default; the handle becomes hard to grab in very long lists.
7. Measure network-share performance with a release build. Debug builds distort image decoding and egui layout costs.

The repository has a baseline on one Windows machine over SMB: 39,496 files scanned in about 37 seconds, discovery progress visible within one second, and UI-thread message response in the millisecond range after repeated window resizes. Exact numbers depend on the network, disk and CPU, but the method is reproducible.

## 13. Where to make common changes

| Need | Main files |
| --- | --- |
| New EXIF field | `model.rs`, `exif_io.rs`, `app.rs` (edit form) |
| New image format | `model.rs`, `Cargo.toml`, `thumbs.rs`, `vision.rs` |
| New video container | `model.rs`, `video.rs`, `video_poster.rs` |
| New view/panel | `app.rs` (`ViewTab`, `view_switch`, `central_*`) |
| New sort mode | `settings.rs`, `refresh_filter` / duplicate order in `app.rs` |
| New setting | `settings.rs`, settings window and `apply_settings` in `app.rs` |
| New API field/endpoint | `api.rs`, `docs/API.en.md` |
| New cache format | the owning module and its `CACHE_VERSION` |

Detailed steps and test requirements are in [DEVELOPMENT.en.md](DEVELOPMENT.en.md).
