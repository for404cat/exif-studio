# Development and maintenance

[简体中文](DEVELOPMENT.md) | English

Read [ARCHITECTURE.en.md](ARCHITECTURE.en.md) first for the module map and state invariants. This document covers day-to-day changes and release work.

## 1. Toolchain

- Rust stable, edition 2024, minimum 1.85.
- Windows is the primary development and test platform.
- macOS/Linux share the same Rust code; platform differences live in `platform.rs` and `video_poster.rs`.

Common commands:

```bash
cargo check
cargo fmt
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Release builds use the Windows GUI subsystem, so there is no console. Use a debug build when you need panic output or a backtrace:

```bash
RUST_BACKTRACE=1 cargo run -- "D:\\Photos"
```

## 2. Local test media

The repository does not commit binary test media. `samples/` and `_shots/` are ignored by git.

```bash
# Generate JPEG, PNG, TIFF, MP4 and MOV examples
cargo run --example make_samples
# Writes to ./samples by default
```

These samples cover basic paths only. Test thumbnails, EXIF writes, video parsing and SMB performance with real camera files, different extensions and damaged files.

## 3. Change recipes

### 3.1 Add an EXIF field

For a read-only field:

1. Add the field to `model.rs::ImageInfo` and its `Default` value.
2. Parse it in `exif_io::read_image_info`. Common helpers are `find_ascii`, `find_uint` and `find_rational`.
3. Add it to the relevant group in `groups_from_exif` if it should be displayed. Collapsed-summary fields are controlled by `app.rs::collapsed_summary`.
4. If the value comes from the `little_exif` fallback, add it to `EditFields`, `read_editable`, and `apply_editable_info`.
5. For an editable field, also update:
   - `EditFields`;
   - `read_editable`;
   - `save_edits` and `validate_fields`;
   - `groups_from_editable`;
   - the form and save flow in `app.rs::show_edit_tab`;
   - round-trip tests in `exif_io`.
6. If the API should expose it, update `api.rs::ApiFile` and `from_entry`, then `docs/API.en.md`.

Non-negotiable EXIF write rules: refuse to write when existing metadata cannot be parsed safely; validate before writing; create a backup by default. Never write files directly from the UI thread.

### 3.2 Add an image format

Format lists are spread across several modules. Missing one causes “scanned but no thumbnail” or “previewable but not hashed”:

1. `model.rs::image_extensions`.
2. `image` crate features in `Cargo.toml` (if built-in decoding is needed).
3. `app.rs::is_previewable`.
4. `model.rs::is_writable_extension` (if writing is supported).
5. `vision.rs::is_visual_extension` (if it should participate in duplicate detection).
6. `exif_io::supports_editable_fallback` (if `little_exif` can read the container).
7. The format tables in README and `docs/API.en.md`.

RAW is not supported. Adding RAW means handling embedded previews, camera-specific EXIF dialects and write safety, not just adding extensions.

### 3.3 Add a video container

Video metadata is parsed by `video.rs` without ffmpeg:

1. `model.rs::video_extensions`.
2. Box/track parsing and tag mapping in `video.rs`.
3. Poster fallback behavior in `video_poster.rs`.
4. Parser tests covering truncation, oversized length fields and missing boxes.

Video is read-only. Do not add metadata writing on this path.

### 3.4 Add a view or page

1. Add a variant to `ViewTab` in `app.rs`.
2. Update the `ITEMS` array in `view_switch`.
3. Add the shortcut to the `views` array in `handle_shortcuts`.
4. Add a `central_*` call to the `match` in `show_workspace_for`.
5. Add any view-specific state to `FolderTab` and reset it in `FolderTab::new` and `scan_tab`.
6. Make sure `sync_tab_content` does not perform heavy IO on every repaint.

View changes use an animated pill. If a view has a first-load cost, follow the Duplicates view: delay the work briefly and show progress instead of starting everything in the transition frame.

### 3.5 Add a sort mode

1. Add an enum and default to `settings.rs`, then add a field to `Settings`; `#[serde(default)]` keeps old config readable.
2. Implement the sort key and `filtered_time_range` maintenance in `app.rs::refresh_filter`.
3. Add a dropdown to the browse sidebar or Duplicates header.
4. Reset scroll position when the order changes; invalidate timeline data at the same time.
5. Add settings missing-field tests and sorting/timeline tests in `app.rs`.

Browse sorting applies to `filtered`; do not reorder `entries`. Duplicate sorting applies to `duplicate_order`; do not mutate `VisionReport::groups`, or preferred stars and original groups will be damaged.

### 3.6 Add a setting

1. Add the field, default and `normalize` validation in `settings.rs`.
2. Add a control to the settings window and keep `settings_draft` in sync.
3. Handle immediate changes and service rebuilds in `apply_settings`.
4. If it affects cache directories, thumbnail services or scanning, check for active scans/vision jobs first.
5. Add a test for old configs with the field missing.

Settings live in the config directory; never commit them. Bump the relevant `CACHE_VERSION` when cache semantics change.

### 3.7 Add a local API endpoint

1. Add a route in `api.rs::handle_request` and keep it read-only (`GET`).
2. Query the `ApiRegistry` snapshot; do not hold UI state.
3. If new fields are needed, extend `ApiFile`/`ApiFolder` and the publish path.
4. Update `docs/API.en.md` with parameters, fields and error codes.
5. Never return file contents, thumbnails or arbitrary directory listings from an unauthenticated path.

The API is optional and off by default. Do not make it non-loopback or writable without a separate authorization design.

### 3.8 UI text and localization

UI strings currently live directly in Rust and are mostly Chinese. For localization:

1. Add a central text table (for example `src/text.rs`) with stable keys.
2. Migrate high-traffic UI first: top bar, Browse, Settings, error messages.
3. Keep raw technical context in debug/error details; user-facing messages should stay short.
4. Keep exported JSON/CSV field names stable even if the UI language changes.

## 4. Performance rules

These rules come from testing 39k files over SMB. Keep them when touching browse paths:

- **Do not iterate all entries per frame.** Use `image_count`, `total_bytes`, `favorite_count`, `folder_group_cache` and `entry_index`; set dirty flags when data changes.
- **Do not rebuild the path index per frame.** Leave it dirty during scanning and build it once with `rebuild_entry_index`.
- **Render only visible grid rows.** Use `ScrollArea::show_rows`.
- **Cap texture uploads.** `MAX_TEXTURE_UPLOADS_PER_FRAME` is 6. Do not raise it to dozens just because thumbnails feel slow.
- **Throttle filtering during scans.** See `SCAN_FILTER_INTERVAL`, `SCAN_CHIP_INTERVAL` and `FAVORITE_REFRESH_INTERVAL`.
- **Do not let incrementally loaded lists jump.** `stick_to_bottom` only applies when the user is at the bottom; after an explicit jump to the top, wait a frame before restoring stickiness (`duplicates_stick_ready`).
- **Measure release builds.** Debug `image` decoding and egui layout are not representative.
- **Record SMB test conditions:** file count, image/video ratio, cold or warm cache, resize smoothness, time to first progress. Do not test only a local SSD.

Do not commit real photos, share addresses or screenshots containing personal information.

## 5. Tests and checks

Run before every commit:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

Focus areas:

- `exif_io`: EXIF round-trip, PNG/WebP fallback, GPS, clearing fields, backup.
- `video`: truncated boxes, invalid lengths, valid MP4/MOV.
- `vision`: identical images, similar grouping, cache round-trip.
- `watermark`: lossless round-trip, wrong passphrase, recompression, original unchanged.
- `cache`/`metacache`: TTL, LRU, over-limit, atomic writes.
- `api`: token comparison, URL decoding.

UI changes have no automated visual regression. At minimum, check Browse, Stats, Duplicates, Settings, editing and dialogs in a release build. Test scrolling and resizing on a real large folder, not only `samples`.

## 6. Release checklist

1. Bump the version in `Cargo.toml`.
2. Run formatting, clippy and all tests.
3. `cargo build --release`.
4. Launch the Windows release build and check:
   - opening folders, scan progress and window resizing;
   - thumbnails, previews and EXIF details;
   - video posters and the external-player confirmation;
   - duplicate paging/waterfall, sorting and timeline;
   - Settings, cache directory, ratings and watermark;
   - local API is off by default.
5. Copy the binary to `dist/ExifStudio.exe` (`dist/` is not committed).
6. Before tagging, confirm the repository has no `samples/`, `_shots/`, caches, settings, backups or personal paths.

## 7. Common pitfalls

- **Borrow conflicts**: when you need `&mut ExifStudioApp` and `&mut FolderTab`, use the `with_tab_mut` temporary-replacement pattern.
- **Forgotten dirty flags**: appending/removing/re-reading entries, changing ratings or sorting can affect filters, stats and duplicate order. Set the dependent flags.
- **Time semantics**: EXIF times have no zone; file modification times are local wall time. Do not mix UTC timestamps and local wall time in sorting or timeline labels.
- **Scroll rebound**: `vertical_scroll_offset(0.0)` and `stick_to_bottom(true)` in the same frame can pull the view back on the next layout. Disable stickiness on the reset frame and re-enable it one frame later.
- **Windows release has no console**: release uses the GUI subsystem, so panics and logs are invisible. Use a debug build or redirect output.
- **Cache versions**: bump `CACHE_VERSION` when changing `MetadataCache`, `VisionCache` or other cached JSON; otherwise stale data is reused.
- **EXIF writes**: do not bypass parse/validate/backup in `save_edits`.
- **Privacy**: issues, screenshots and PRs must not contain real photos, share addresses, usernames, API tokens or personal information. `_shots/` is ignored; do not force-add it.
