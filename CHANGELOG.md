# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Map Local: custom response headers (one `Name: value` per line), e.g. CORS or `Cache-Control`.
- "Save Response & Map Local…" in the traffic context menu: saves the decoded response body under
  `~/Library/Application Support/crabtrap/map_local/` and opens a prefilled Map Local rule.
- Map Local recognises more file types: avif, mp4, webm, mp3, ttf, otf, woff, csv, yaml, md, source maps.
- [User Guide](docs/guide.md) for all tools, linked from **Help → User Guide**.
- Rule matching: **Regex** mode over the full URL, **Ignore query string** (on for new rules),
  and **GraphQL operation** conditions for Map Local, Map Remote, Block, Breakpoints, Rewrite and
  Scripting. Every rule dialog has a live **Test URL** check.
- **Copy as cURL** and **Export as HAR…** in the traffic context menu; **File → Export Session as
  HAR…** (with or without bodies).

## [0.1.0] - 2026-10-18

First public release.

### Added
- HTTP/HTTPS proxy with MITM, a local root CA, and one-step device setup (QR code, iOS profile).
- Traffic table with filters, search, domain/app sidebar, and request/response inspectors.
- HTTP/2 and WebSockets.
- Tools: SSL Proxying, Block List, Map Local, Map Remote, Throttle, Rewrite, Breakpoints, Scripting (JavaScript).
- Repeat, Edit & Resend, Compose.
- macOS system proxy toggle, menu bar icon, and access from devices on the local network.
- Status bar warning when the macOS proxy points at crabtrap while the proxy is stopped.
- Check for Updates (GitHub Releases).
