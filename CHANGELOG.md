# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.0] - 2026-10-21

### Added
- Map Local: custom response headers (one `Name: value` per line), e.g. CORS or `Cache-Control`.
- "Save Response & Map Local…" in the traffic context menu: saves the decoded response body under
  `~/Library/Application Support/crabtrap/map_local/` and opens a prefilled Map Local rule.
- Map Local recognises more file types: avif, mp4, webm, mp3, ttf, otf, woff, csv, yaml, md, source maps.
- [User Guide](docs/guide.md) for all tools, linked from **Help → User Guide**.
- Rule matching: **Regex** mode over the full URL, **Ignore query string** (on for new rules),
  and **GraphQL operation** conditions for Map Local, Map Remote, Block, Breakpoints, Rewrite and
  Scripting. Every rule dialog has a live **Test URL** check.
- **Allow List** (Tools, ⌥⌘A, or right-click → Allow Only <host>): record only chosen hosts;
  everything else is proxied unrecorded and untouched. A status bar note shows when it is on.
- **Copy as cURL** and **Export as HAR…** in the traffic context menu; **File → Export Session as
  HAR…** (with or without bodies).
- **Mirror** (Tools): save decompressed successful (200) GET responses of chosen hosts to a folder as
  `<host>/<path>`.
- **File → Save Session… / Open Session…** (`.crabtrap`: requests, bodies, timings, tags and
  WebSocket messages) and **File → Import HAR…** (Chrome, Charles, Proxyman, crabtrap; Chrome's
  WebSocket messages included). Opened requests are added to the list, tagged `imported`.
- The body inspector decides decompression from the stored body, not the `Content-Encoding`
  header, so decompressed bodies from HAR are shown as is.
- WebSocket **Resend**, **Edit & Send…** and **Send Message…** in the Messages tab: send a text or
  hex message to the server or to the client over the open connection; such messages are marked
  "sent from crabtrap".
- **Reverse Proxy** (Tools): a local port that forwards to a server, for clients that can't use a
  proxy; requests are recorded (tagged `reverse-proxy`) and all rules apply. Changes apply at once;
  a busy port is reported.
- Map Remote: **Replace only the fixed start of the matched path** (`/v1/*` → `/v2` maps
  `/v1/users/7` to `/v2/users/7`); WebSocket handshakes are mapped, `ws://`/`wss://` targets are
  accepted.
- Throttle: **Drop %** (a share of requests is closed without a response, tagged `dropped`) and
  presets Lossy 3G, Very Bad Network, 100% Loss.
- **DNS Spoofing** (Tools, or right-click → DNS Spoofing…): connect chosen hosts (`*.example.com`
  patterns too) to a given IPv4/IPv6 address while keeping Host, SNI and certificate checks.
  Spoofed traffic is tagged `dns-spoofed`; changes apply to the next request.
- **Copy as fetch** and **Copy as fetch (Node.js)**, in the Chrome DevTools format. All copy actions
  (URL, cURL, fetch) are grouped in a **Copy** submenu of the traffic context menu.

### Fixed
- Repeat and Edit & Resend sent the client's original headers together with a body that a rule had
  already changed. The client's body is now kept when a rule edits it, so Repeat replays the
  original request.

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
