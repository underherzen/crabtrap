# crabtrap User Guide

How to inspect and change traffic with crabtrap. For installation and first launch, see the
[README](../README.md#getting-started).

- [Matching rules](#matching-rules)
- [SSL Proxying](#ssl-proxying)
- [Allow List](#allow-list)
- [Map Local](#map-local)
- [Map Remote](#map-remote)
- [Block List](#block-list)
- [Rewrite](#rewrite)
- [Breakpoints](#breakpoints)
- [Scripting](#scripting)
- [Throttle](#throttle)
- [Repeat, Edit & Resend, Compose](#repeat-edit--resend-compose)
- [Export: cURL and HAR](#export-curl-and-har)
- [Filters and search](#filters-and-search)
- [Order of tools](#order-of-tools)
- [Keyboard shortcuts](#keyboard-shortcuts)
- [Where rules are stored](#where-rules-are-stored)

All tools live in the **Tools** window (menu **Tools**, or right-click a request in the traffic
table). Every tool has its own **Enable** switch. Turning a tool off keeps its rules, and every rule
also has its own checkbox. Changes apply immediately, without restarting the proxy or dropping
connections.

Rules only see traffic crabtrap can read: plain HTTP, and HTTPS for hosts with
[SSL Proxying](#ssl-proxying) enabled.

## Matching rules

Every rule has a condition: which requests it applies to. The **Match** switch picks one of two
modes.

**Wildcard** (default)

| Field  | Syntax | Example |
|--------|--------|---------|
| Host   | `*` matches any run of characters, `?` matches one character | `api.example.com`, `*.example.com` |
| Path   | the same wildcards | `/api/v1/*` |
| Ignore query string | on: Path is compared without `?…`; off: the query is part of the path | — |

- **Ignore query string** is on for new rules, so `/api/users` also matches `/api/users?page=2`.
  Turn it off to match exact queries, for example `/search?q=*`.
- Rules created before this option existed keep comparing path and query together. With the
  option off, `/api/users` does **not** match `/api/users?page=2`; use `/api/users*` instead.
- An empty Path matches any path.
- You can paste a full URL into the Host field and it is split into host and path.
- Port and scheme are not part of a wildcard match.

**Regex (full URL)**

One regular expression over the whole URL, including scheme, port and query, for example
`https://api\.example\.com/v[12]/users(\?.*)?`.
- The regex must match the **entire** URL. A partial match counts as no match, as in Proxyman.
- The regex is checked against the **normalized** URL, which can differ from what you typed:
  - the host is lowercased;
  - default ports are dropped (`:443` for https, `:80` for http);
  - an empty path becomes `/`;
  - IPv6 hosts keep their brackets (`[::1]`).

  For example, `https://api\.example\.com` never matches, because the URL is
  `https://api.example.com/`. Write `https://api\.example\.com/.*` instead. **Test URL** shows
  the normalized form ("checked as …") whenever it differs from your input.
- Regex mode uses [Rust regex syntax](https://docs.rs/regex/latest/regex/#syntax).

**In both modes**

| Field | Meaning |
|-------|---------|
| Method | exact, or *Any method* |
| GraphQL op | Name of the GraphQL operation. Wildcards are allowed: `GetUser`, `Get*`. Empty means any. |

How GraphQL operations are detected:
- The name comes from `operationName` in the JSON body or the query string. Otherwise it is taken
  from the document (`query GetUser { … }`).
- In a batch, the first operation counts. Anonymous operations have no name and never match.
- Only JSON bodies (up to 1 MiB) and the `?operationName=` / `?query=` parameters are recognized.
  Compressed request bodies, `application/graphql` bodies, multipart uploads and persisted
  queries (hash only) give no name, so such requests don't match a GraphQL condition.
- crabtrap starts loading request bodies for this only while an enabled rule has a GraphQL
  condition. Scripts, Rewrite body edits and Breakpoints load bodies anyway.
- Throttle does not support GraphQL conditions.

Matching is case-insensitive. A rule saved in `rules.json` with `"case_sensitive": true` makes
wildcards, the regex and the method case-sensitive.

The dialog shows a live **Matches:** preview. **Test URL** checks any URL against the rule as you
type. The GraphQL operation is not part of that check, because it lives in the request body.

Within one tool, the first enabled matching rule wins. The exception is Rewrite, where every
matching rule applies, in list order.

## SSL Proxying

crabtrap decrypts HTTPS only for hosts on the SSL Proxying list. All other HTTPS traffic passes
through as an opaque tunnel, so you see the host but not the contents.

1. Install and trust the root CA: **Settings → Certificates** (QR code and iOS profile for phones).
2. Add hosts in **Tools → SSL Proxying**, or right-click a request → **Enable SSL Proxying for
   <host>**.

How entries match:
- `*.example.com` also matches `example.com` itself.
- An **exclude** entry always beats an include entry.
- Apple hosts that use certificate pinning (`*.apple.com`, `*.icloud.com` and others) are excluded
  by default, so system services keep working.

## Allow List

Records **only** the hosts on the list. Everything else still goes through crabtrap, but it is not
recorded and nothing is applied to it: no rules, no decryption, no throttling. It works like
Proxyman's Allow List and keeps the traffic table focused on the app you are debugging.

- Add hosts in **Tools → Allow List** (⌥⌘A), or right-click a request → **Allow Only <host>**,
  which also turns the list on.
- Patterns work like in SSL Proxying: `api.example.com`, or `*.example.com`, which also matches
  `example.com` itself.
- The list is in effect only while **Enable Allow List** is on **and** at least one host is
  enabled. An empty list never hides your traffic.
- While it is in effect, the status bar shows **Allow List: recording N hosts only**.
- **Block List** still applies to every host. A blocked host stays blocked even if it is not on
  the Allow List. Blocked HTTPS connections still show up as a `403` row.
- New requests you send with **Compose** are always recorded, whatever the host.
- Changes apply to **new connections**. A tunnel or decrypted connection that is already open
  keeps its state until the client reconnects.
- The status bar's connection count includes connections that are not recorded.

## DNS Spoofing

Connects chosen hosts to an IP address you set, instead of the one from DNS, like Proxyman's DNS
Spoofing. The request is unchanged: the same URL, `Host` header, SNI and certificate checks. Use it
to point the production name at a staging server or a backend on your Mac without editing
`/etc/hosts`.

- Add hosts in **Tools → DNS Spoofing**, or right-click a request → **DNS Spoofing…** (the host is
  filled in). Enter an IPv4 or IPv6 address, for example `127.0.0.1` or `::1`.
- Patterns work like in SSL Proxying: `api.example.com`, or `*.example.com`, which also matches
  `example.com` itself. When several entries match, the first one in the list wins. An entry
  applies to every port of the host; the port of the request is kept.
- It applies to everything crabtrap connects to: plain HTTP, decrypted HTTPS, HTTPS tunnels,
  WebSockets, Repeat and Compose. After **Map Remote**, the host of the new target is the one
  looked up.
- Spoofed requests and tunnels are tagged `dns-spoofed` in the traffic table.
- Changes apply to the next request straight away; idle keep-alive connections to the old address
  are dropped. An HTTPS tunnel that is already open (not decrypted) stays on its address until the
  client reconnects.
- Only traffic that goes through crabtrap is affected. The system DNS and apps that bypass the
  proxy are not.

## Reverse Proxy

Opens a local port that forwards to a server, like Proxyman's Reverse Proxy. Use it for clients that
can't use an HTTP proxy: point them at `http://localhost:9090`, and their requests go to the target
and show up in the traffic table, tagged `reverse-proxy`.

- Add an entry in **Tools → Reverse Proxy**: a **Local port** and the server to **Forward to**
  (protocol, host, optional port). Requests keep their path and query: `http://localhost:9090/v1/x`
  goes to `https://api.example.com/v1/x`.
- By default `Host` is the target's. **Keep original Host header** sends the client's
  (`localhost:9090`) instead.
- The port listens on the same interface as the proxy (`127.0.0.1`, or all interfaces when the
  proxy accepts devices). It is plain HTTP; the target can be HTTPS.
- All tools apply as for proxied traffic: rules, scripts, breakpoints, throttle, DNS Spoofing. A
  WebSocket handshake is forwarded too.
- Changes apply at once: removed or disabled entries stop listening, new ones start, and editing
  an entry restarts its port (requests in flight on it are cut). An open WebSocket keeps running
  until either side closes it or the proxy stops.
- If a port is taken, a notification says so once and the entry stays saved; crabtrap tries the
  port again whenever the rules change.
- Two enabled entries can't share a port, and an entry can't forward to its own port. **Duplicate**
  creates a disabled copy.
- The port only forwards. It does not work as an HTTP proxy: `CONNECT` is refused with `405`.
- When the proxy accepts devices (listens on all interfaces), reverse ports are open to the network
  too, without authentication, just like the proxy port itself.

## Map Local

Answers matching requests with a file from your Mac. The request never reaches the server.

**Create a rule**
- **Tools → Map Local → +**, or
- right-click a request → **Map Local…** (host, path without the query, and method are filled
  in), or
- right-click a request → **Save Response & Map Local…**. This saves the response body to
  `~/Library/Application Support/crabtrap/map_local/<host>/<path>`, already decompressed. It then
  opens the Map Local dialog with that file, the response status, its Content-Type and its
  headers. Review the values and press **Save**.
  - The file extension comes from the Content-Type (`users.json`). An unknown type gets `.bin`.
  - An existing file is never overwritten. A second save becomes `users-2.json`, then
    `users-3.json`, and so on.
  - Headers that describe that particular response are left out: `Content-Encoding`, `Set-Cookie`,
    `Date`, `ETag`, `Alt-Svc`.
  - Saving is refused, with a message, in three cases:
    - the response is still loading or failed;
    - its body was not recorded;
    - it uses a compression crabtrap can't decode (gzip, br, deflate and zstd are supported).

**Fields**

| Field | Meaning |
|-------|---------|
| Local path | A file, or a **folder** (see below). `~` is expanded. |
| Status | Response code, 200–599 (default 200). |
| Content-Type | Empty means it is guessed from the file extension. |
| Headers | Extra response headers, one `Name: value` per line, for example `Access-Control-Allow-Origin: *` or `Cache-Control: no-store`. Repeated names are allowed (`Set-Cookie`). |

Some headers cannot be set here:
- `Content-Length`, `Transfer-Encoding`, `Connection`, `Keep-Alive`, `Proxy-Connection`, `TE`,
  `Trailer` and `Upgrade` are always set by crabtrap.
- `Content-Type` has its own field.
- `Content-Encoding` is allowed, for example to serve a pre-compressed `.gz` file.

**Folder mapping.** When Local path is a folder, the request path is appended to it. For example,
`/static/app.js` is served from `<folder>/static/app.js`.
- The query string is ignored.
- A path ending in `/` serves `index.html`.
- Hidden files (`.env`, `.git/…`) and anything outside the folder (`..`, symlinks that escape it)
  are never served.

**Behaviour**
- The file is read on every request, so edits show up immediately with no reload.
- If the file is missing, the response is `404` with a short text body. The rule's headers are
  not added to it.
- Content-Type by extension: json, map, html, js, css, txt, csv, md, xml, yaml, svg, png, jpg,
  gif, webp, avif, ico, pdf, wasm, woff, woff2, ttf, otf, mp4, webm, mp3. Anything else is
  `application/octet-stream`.
- Rewrite, Scripting and Breakpoints on the response phase still apply to the local response.
  Use them to change the body on the fly, pause it, and so on.

Matching traffic rows are tagged `map-local`.

## Map Remote

Sends matching requests to another server, for example production to staging, or to
`localhost:3000`.

- **Protocol, Host, Port**: the new origin.
- **Path**: replaces the whole original path. Leave it empty to keep the original path.
- **Replace only the fixed start of the matched path**: keeps the rest of the path. The fixed
  start is the leading path segments that match the segments of the rule's Path pattern without
  `*` (`?` stands for one character). A segment like `v1*` counts only when the request segment
  is exactly `v1`.
  - `/v1/*` → `/v2`: `/v1/users/7` goes to `/v2/users/7`.
  - `/api/*` → empty path: `/api/users` goes to `/users`, which strips the prefix.
  - `/v1*` → `/v2`: `/v1/users` goes to `/v2/users`, but `/v10/x` goes to `/v2/v10/x`.
  - No Path pattern, or no segment matches: the target path is put in front, `/users` →
    `/mock/users`.
- The original query string is kept, unless the target has its own.
- **Keep original Host header**: by default `Host` is rewritten to the new server. Turn this on
  for virtual hosts that expect the original name.
- WebSocket connections are mapped too: the handshake goes to the new server, then messages flow
  as usual. For a `wss://` connection, SSL Proxying must be on for the host. In `rules.json` a
  `ws://`/`wss://` target is the same as `http://`/`https://`.

Rows are tagged `map-remote`.

## Block List

For each rule, choose an action:
- **Respond 403**: an empty `403 Forbidden` response.
- **Drop connection**: the connection is closed.

A rule with only a Host (no Path) also blocks HTTPS tunnels to that host, even without SSL
Proxying. Rules with a Path need the traffic to be decrypted. Rows are tagged `blocked`.

## Rewrite

Declarative edits to requests or responses, without writing a script. A rule has a **location**
(Request or Response), an optional match (empty means all traffic) and an ordered list of
operations:

| Operation | Notes |
|-----------|-------|
| Add header | Adds the header even if one with that name exists. |
| Replace header | Case-insensitive name. All copies become one value, and the header is added if missing. |
| Remove header | |
| Body find/replace | Plain text or regex. The replacement can use `${1}`, and `$$` for a literal `$`. Text bodies only; compressed bodies are decompressed first. |
| URL rewrite | Plain text or regex over the full URL. Request rules only. Map Local and Map Remote then see the rewritten URL. |
| Set status | Response rules only. |

Rows changed by a rule are tagged `modified`.

## Breakpoints

Pauses a matching request, response, or both, and opens an editor window:

- **Request**: edit method, URL, headers and body. Changing the URL also changes where the request
  goes (Map Local and Map Remote still apply after the edit).
- **Response**: edit status, headers and body.
- **Execute** (⌘↩) sends the edited message. **Abort** closes the connection.

A paused exchange continues unchanged after 5 minutes, or if the client disconnects.

Quick ways to add one: right-click a request → **Breakpoint…**, or ⌃⌥B (**Breakpoint for Selected
Request**).

## Scripting

JavaScript that runs on requests and/or responses, with a Proxyman-compatible API. Open it from
**Tools → Scripting**, or right-click a request → **Script…**.

```js
async function onRequest(context, url, request) {
  request.headers["Authorization"] = "Bearer test-token";
  request.queries["page"] = "2";
  return request;
}

async function onResponse(context, url, request, response) {
  response.statusCode = 200;
  response.body = { ok: true };   // object for JSON, string for text
  return response;
}
```

- `request`: `method`, `url`, `scheme`, `host`, `port`, `path`, `queries`, `headers`, `body`.
- `response`: `statusCode`, `headers`, `body`.
- Body format:
  - JSON and form bodies arrive as objects, text as a string.
  - Binary bodies arrive as `bodyBase64`.
  - A body the script doesn't touch is passed through byte-for-byte.
- **Run as Mock API**: `onRequest` returns `{ statusCode, headers, body }` and the server is never
  contacted. This is the most flexible way to mock a response.
- Helpers: `console.log` (shown in the editor console), `$sharedState` (a key–value store shared
  by all scripts), `btoa` / `atob`, `$crypto.sha256`, `$crypto.sha512`, `$crypto.randomUUID`, and
  the **Snippets** menu.
- **Test** runs the draft against the last matching exchange and shows the result in the console.
- Limits:
  - 3 s per call, so an endless loop can't hang traffic.
  - No file or network access.
  - Source up to 512 KiB.

Rows are tagged `scripted` or `mocked`.

## Throttle

Simulates a slow network with one profile at a time: **Tools → Throttle**.
- Presets: 56 kbps Modem, 256 kbps ISDN/DSL, 3G, Lossy 3G, 4G, 100 Mbps Fibre, Very Bad Network,
  100% Loss. You can also enter your own download and upload speed (kbps), latency (ms) and
  **Drop %**.
- Optionally limit throttling to one location.
- Latency is added before each request is sent. Bandwidth limits request and response bodies.
- **Drop %** simulates losses. After the latency, that share of requests is closed without a
  response and never reaches the server; the row fails and is tagged `dropped`. crabtrap works
  with whole requests, not packets, so there are no partial responses or retransmits.
  - The client connection is closed, so with HTTP/2 the other requests on that connection fail
    too, as they would when a real connection drops.
  - Traffic outside the Allow List is never throttled or dropped.
  | Preset | Down / Up | Latency | Drop |
  |--------|-----------|---------|------|
  | Lossy 3G | 768 / 384 kbps | 300 ms | 5 % |
  | Very Bad Network | 1000 / 500 kbps | 500 ms | 10 % |
  | 100% Loss | — | 0 ms | 100 % |
- TLS handshakes and non-decrypted tunnels are not throttled.

Rows are tagged `throttled`.

## Repeat, Edit & Resend, Compose

Right-click a request:
- **Repeat** (⌃R) sends the request again **as the client sent it**: the original headers and
  body, even if Rewrite, a script or a breakpoint changed them. Rules then apply once, not on top
  of their earlier result.
  - It refuses if the request body was not fully captured.
  - It also refuses if a rule changed the body and the client's original was not kept: it was no
    longer stored, body recording is off, or the rule replaced a body crabtrap never loaded.
    Edit & Resend still works and starts from the body that was sent.
- **Edit & Resend…** opens the client's request in **Compose**, where you can change method, URL,
  headers and body, then press ⌘↩. If the original body is gone, the form starts from the body
  that was sent.

Repeated and composed requests show up in the traffic table, tagged `repeated` or `composed`.

## Export: cURL, fetch and HAR

Right-click a request → **Copy** holds everything that goes to the clipboard, as in Chrome
DevTools: **Copy URL**, **Copy as cURL**, **Copy as fetch** and **Copy as fetch (Node.js)**.

**Copy as cURL** copies a `curl` command for the request **as crabtrap
sent it to the server**: after Rewrite, Scripting and Breakpoint edits, with the recorded body. If
a rule changed the request, the command repeats the changed version.
- Curl sets `Content-Length`, `Transfer-Encoding` and `Connection` itself. `Host` is kept only when
  it differs from the URL, for example a virtual host on an IP address.
- A text body is sent with `--data-raw`, so a body that starts with `@` is never read as a file.
- A binary body is piped in through `printf` with octal escapes. This reproduces every byte,
  including NUL, in zsh and bash without extra files.
- URLs that contain `[ ] { }` get `--globoff`.

**Copy as fetch** and **Copy as fetch (Node.js)** copy a `fetch(…)` call in the same layout as
Chrome, built from the same sent request.
- Headers that `fetch` can't set or the runtime adds itself (`Cookie`, `Referer`, `Host`,
  `User-Agent`, `Content-Length`, `Accept-Encoding`, …) are left out.
- The browser variant passes `Referer` as `referrer` and adds `mode: "cors"`; `credentials` is
  `"include"` when the request had `Cookie` or `Authorization`, otherwise `"omit"`.
- The Node.js variant has no `mode` or `credentials`, and puts `cookie` and `Referer` back into the
  headers, because Node.js has no browser cookies to send.
- Repeated headers are joined into one value (`Cookie` with `; `, others with `, `). A text body is
  a string; a binary body is a `new Uint8Array([…])`.

**HAR** (HTTP Archive 1.2) is the standard format that Chrome DevTools, Charles, Proxyman and
Postman import.
- Right-click a request → **Export as HAR…** saves that one exchange, with bodies. The item is
  disabled until the response arrives.
- **File → Export Session as HAR…** saves every recorded exchange, in the order they started.
- **File → Export Session as HAR (Headers Only)…** leaves bodies out. Use it for smaller files, or
  when bodies may contain secrets.

What the HAR contains:
- Response bodies are decompressed. Text is stored as is, binary as base64.
- Request bodies are included only when they are text, because HAR has no binary form for them.
- Exchanges that have no response yet are skipped. WebSocket messages are not exported.
- Rule tags (`map-local`, `modified`, …) go into each entry's `comment`.
- Timings that were not measured (DNS, connect and TLS on a reused connection) are `-1`.
- The file is written as it goes, so a large session is never held in memory twice.
- Only your user account can read the file. An interrupted export leaves the previous file in
  place.

> [!WARNING]
> A HAR file contains everything that was captured: cookies, `Authorization` headers, tokens and
> bodies. Check it before you share it.

## Filters and search

Filter bar tokens, combined with AND:

| Token | Example |
|-------|---------|
| `host:` | `host:*.example.com` |
| `status:` | `status:404`, `status:5xx`, `status:500-599`, `status:200,201` |
| `method:` | `method:POST` |
| `type:` | `type:json` (Content-Type substring) |
| `re:` | `re:/v[12]/users` (regex over the full URL) |
| text | substring of host or path |

**Search** (⌘F) looks inside headers and/or bodies, as plain text or regex. Compressed bodies are
decompressed, and bodies larger than 10 MiB are skipped.

## Order of tools

For every exchange the tools run in this order:

1. **Request**: Block → Scripting `onRequest` → Rewrite → Breakpoint → Map Local / Map Remote →
   Throttle → server.
2. **Response**: Scripting `onResponse` → Rewrite → Breakpoint → client.

So a breakpoint shows the message after scripts and rewrites, and Map Local sees the URL after
they have changed it.

## Keyboard shortcuts

| Shortcut | Action |
|----------|--------|
| ⌥⌘P / ⌥⌘B / ⌥⌘L / ⌥⌘R | SSL Proxying / Block List / Map Local / Map Remote |
| ⌥⌘T / ⌥⌘E / ⌥⌘K / ⌥⌘I | Throttle / Rewrite / Breakpoints / Scripting |
| ⌃R | Repeat selected request |
| ⌃⌥B | Breakpoint for selected request |
| ⇧⌘N | Compose |
| ⌘L / ⌘F | Focus filter / search |
| ⌘R / ⌘K | Start–stop recording / clear traffic |

## Where rules are stored

All rules are kept in `~/Library/Application Support/crabtrap/rules.json`. The app writes the
file atomically. If the file is damaged, it is renamed to `rules.json.corrupt-<time>` and crabtrap
starts with an empty rule set.

Saved responses are stored in `~/Library/Application Support/crabtrap/map_local/`.
