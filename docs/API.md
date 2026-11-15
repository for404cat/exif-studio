# 本地 API

简体中文 | [English](API.en.md)

应用内置一个可选的、仅监听回环地址的只读 HTTP API，用于本机脚本、插件或 AI Agent 查询当前打开的文件夹和媒体元数据。默认关闭。

## 启用

在“设置 → 本地 API（开放端口）”中开启，设置端口（默认 7420），保存后应用会生成并持久化一个 64 字符的 Token。Token 可以在设置中复制或重新生成。

推荐通过环境变量传递：

```bash
EXIF_STUDIO_TOKEN="..."
curl -H "Authorization: Bearer $EXIF_STUDIO_TOKEN" \
  "http://127.0.0.1:7420/v1/ping"
```

## 安全边界

- 只绑定 `127.0.0.1`，不监听局域网地址。
- 每个请求必须带 `Authorization: Bearer <token>`；Token 比较使用常数时间实现。
- 只接受 `GET`，不提供写入、删除、缩略图、文件内容或命令执行。
- `/v1/file` 的 `path` 必须与当前已打开标签页中的文件路径完全一致，不能读取任意路径，也不存在目录遍历入口。
- 不启用 CORS；响应带 `Cache-Control: no-store` 和 `X-Content-Type-Options: nosniff`。
- Unix 下配置文件以 `0600` 权限写入。Windows 使用当前用户配置目录，未额外设置 ACL；与当前用户同等权限的本机程序仍可读取 Token。

这个 API 面向本机可信进程，不是公网服务。不要把端口反向代理、隧道或转发到外部网络。

## 端点

### `GET /v1/ping`

返回应用名称和版本，用于确认端口可用。

```json
{"ok":true,"name":"Exif Studio","version":"0.1.0"}
```

### `GET /v1/folders`

返回当前所有打开标签页的摘要，不包含文件数组。

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

`id` 是本次运行内的标签页编号，关闭标签页后会变化。`scanning` 为 `true` 时，`file_count` 只是当前已读取的部分。

### `GET /v1/files`

返回已索引媒体的元数据。查询参数：

| 参数 | 说明 |
| --- | --- |
| `folder` | 标签页 `id`。省略时查询所有已打开标签页 |
| `kind` | `image` 或 `video` |
| `q` | 不区分大小写的子串，匹配文件名、完整路径、相机和镜头 |
| `limit` | 返回数量，1–1000，默认 200 |
| `offset` | 跳过的匹配数量，默认 0 |

响应：

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

`total` 是过滤后的总匹配数，不是本页数量。文件的元数据来自扫描缓存；图片缩略图、完整 EXIF 分组和视频封面不通过 API 提供。

`read_error` 非空表示文件元数据读取失败。它通常是简短的技术错误文本，生产脚本应将其视为“元数据不可用”，不要依赖具体措辞。

### `GET /v1/file?path=...`

按完整路径查询单个文件。路径必须进行 URL 编码，并与 API 快照中的 `path` 完全一致。

```bash
curl -G -H "Authorization: Bearer $EXIF_STUDIO_TOKEN" \
  --data-urlencode "path=D:\\Photos\\P1000100.JPG" \
  "http://127.0.0.1:7420/v1/file"
```

成功：

```json
{"file": {"path": "...", "name": "...", "kind": "image", "...": "..."}}
```

失败：

```json
{"error":"文件不在当前打开的文件夹中"}
```

## 状态码

| 状态码 | 场景 |
| --- | --- |
| 200 | 成功 |
| 400 | `/v1/file` 缺少 `path` |
| 401 | 缺少或错误的 Bearer Token |
| 404 | 文件夹、文件或端点不存在 |
| 405 | 使用了非 GET 方法 |

## 数据一致性

UI 线程在扫描状态变化、条目追加或设置变化时标记 API 需要刷新，实际发布最多每秒一次。因此 API 读到的是一份只读快照：

- 扫描中可能看到部分文件；
- 星标、筛选和当前选中状态不包含在 API 中；
- 文件被外部修改后，下一次重新扫描才会更新元数据；
- 启用 API 时，每次发布都会为所有已打开条目生成 `ApiFile`。在数万文件规模下这是可见的 CPU 成本，不需要时请关闭。
