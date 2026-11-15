# Security

[简体中文](SECURITY.md) | English

## Reporting

Use the repository's Security → Report a vulnerability flow for private reports. Do not open a public issue first. Include affected versions, reproduction conditions and potential impact.

## Local API

The local API is off by default. When enabled it binds only to `127.0.0.1`, requires a Bearer token, and serves read-only metadata from already-opened folders. It is not a public service and must not be exposed through a reverse proxy, port forward or tunnel.

On Windows the token lives in `settings.json` under the current user's config directory, without extra ACLs. On Unix the file is written with mode `0600`. Local programs with the same user permissions can still read the token.

## File writes

- EXIF writes parse existing structures and validate fields first; unparseable structures are refused.
- A `.bak` backup is created by default. The write itself is performed by `little_exif` and is not transactional; the backup is the recovery path.
- Invisible watermarking only writes a new PNG copy; the original is untouched.
- Video metadata is always read-only.

## Watermark limits

LSB watermarking is for lossless copy tracking. The passphrase only scrambles bit positions; it is not encryption. Do not use it to protect secrets. Screenshot or recompression matching depends on the local `watermarks.json` visual index and is not guaranteed across devices or strong processing.

## Privacy

The app has no telemetry and does not initiate network connections. When the local API is disabled there is no network listener. Do not include real photos, share addresses or personal information in issues, tests or screenshots.
