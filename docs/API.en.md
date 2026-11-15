# Local API

[简体中文](API.md) | English

The app includes an optional, loopback-only, read-only HTTP API for local scripts, plugins or AI agents that need to query the folders currently open in the app. It is disabled by default.

## Enabling

Open Settings → Local API, choose a port (default 7420) and save. The app generates and persists a 64-character token. The token can be copied or regenerated in Settings.

Pass it through an environment variable:

```bash
EXIF_STUDIO_TOKEN="..."
curl -H "Authorization: Bearer $EXIF_STUDIO_TOKEN" \
  "http://127.0.0.1:7420/v1/ping"
```

## Security boundary

- Binds only to `127.0.0.1`; no LAN listener.
- Every request must carry `Authorization: Bearer <token>`. Token comparison is constant-time.
- Only `GET` is accepted. There is no writing, deletion, thumbnail, file-content or command execution endpoint.
- `/v1/file` requires an exact path match against files in an open tab. It cannot read arbitrary paths, and there is no directory traversal entry point.
- CORS is disabled. Responses include `Cache-Control: no-store` and `X-Content-Type-Options: nosniff`.
- On Unix, the config file is written with `0600`. Windows uses the user config directory without extra ACLs; local programs with the same user permissions can still read the token.

This API is for trusted local processes, not the public internet. Never reverse-proxy, tunnel or expose the port externally.

## Endpoints

### `GET /v1/ping`

Returns the app name and version.

```json
{"ok":true,"name":"Exif Studio","version":"0.1.0"}
```

### `GET /v1/folders`

Returns a summary of every open tab, without the file arrays.

```json
{
  "folders": [
    {
      "id": 1,
      "path": "D:\\Photos",
      "scanning": false,
      "file_count": 39496
    }
  ]
}
```

`id` is the runtime tab id and changes when tabs are closed. When `scanning` is `true`, `file_count` is only the portion read so far.

### `GET /v1/files`

Returns indexed media metadata. Query parameters:

| Parameter | Meaning |
| --- | --- |
| `folder` | Tab `id`; omit to query all open tabs |
| `kind` | `image` or `video` |
| `q` | Case-insensitive substring matching filename, full path, camera and lens |
| `limit` | Result count, 1–1000, default 200 |
| `offset` | Number of matches to skip, default 0 |

Response:

```json
{
  "total": 39496,
  "files": [
    {
      "path": "D:\\Photos\\P1000100.JPG",
      "name": "P1000100.JPG",
      "kind": "image",
      "format": "JPG",
      "size": 8388608,
      "modified": "2026-10-24T18:22:11+08:00",
      "resolution": "5776×4336",
      "width": 5776,
      "height": 4336,
      "camera": "Panasonic DC-S1",
      "lens": "LUMIX S 24-105/F4",
      "taken_at": "2026-10-24 18:22:11",
      "iso": 640,
      "aperture": 4.0,
      "exposure_time": 0.00625,
      "focal_length": 35.0,
      "gps": [31.2304, 121.4737],
      "codec": null,
      "container": null,
      "fps": null,
      "duration_secs": null,
      "read_error": null
    }
  ]
}
```

`total` is the number of matches after filtering, not the page size. Metadata comes from the scan cache; thumbnails, full EXIF groups and video posters are not exposed.

A non-null `read_error` means metadata reading failed. It is usually a short technical message; treat it as “metadata unavailable” and do not depend on exact wording.

### `GET /v1/file?path=...`

Looks up one file by full path. URL-encode the path; it must match the `path` in the API snapshot exactly.

```bash
curl -G -H "Authorization: Bearer $EXIF_STUDIO_TOKEN" \
  --data-urlencode "path=D:\\Photos\\P1000100.JPG" \
  "http://127.0.0.1:7420/v1/file"
```

Success:

```json
{"file": {"path": "...", "name": "...", "kind": "image", "...": "..."}}
```

Failure:

```json
{"error":"文件不在当前打开的文件夹中"}
```

## Status codes

| Code | Meaning |
| --- | --- |
| 200 | Success |
| 400 | `/v1/file` is missing `path` |
| 401 | Missing or wrong Bearer token |
| 404 | Unknown folder, file or endpoint |
| 405 | Method is not GET |

## Consistency

The UI marks the API dirty when scan state, entries or settings change, and publishes at most once per second. Consumers therefore read a read-only snapshot:

- During scanning it may contain a partial file set.
- Ratings, filters and the current selection are not part of the API.
- External file changes are reflected only after a rescan.
- When the API is enabled, every publish builds `ApiFile` for all open entries. At tens of thousands of files this is measurable CPU work; turn it off when not needed.
