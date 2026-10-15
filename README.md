# TorrentCat Next

A new macOS torrent-streaming app built with **Tauri 2, Rust, React and TypeScript**. The interface follows the local TorrentCat fork in `../webtorrent-desktop`: dark poster shelves, library filters, file lists, preferences and an integrated player. The new app has its own identity and profile.

Node is used to build the frontend; it is not shipped in the app. The installed app uses macOS WKWebView and a Rust torrent engine. VLC and FFmpeg are optional external native tools, not bundled runtimes.

## Run locally

Requirements: macOS 13+, Apple command line developer tools, a current stable Rust toolchain and Node 22+ (or Node 24/26). The first build was made on Apple Silicon using Rust 1.99 and Node 26.

```sh
npm ci
npm start
```

Build a standalone optimized application:

```sh
npm run bundle
```

The result is `src-tauri/target/release/bundle/macos/TorrentCat Next.app`. The bundle command applies and verifies an ad-hoc signature. It can be copied to Applications and opened normally. This local build is not a notarized distribution release.

On the development Mac, the release is installed at `/Applications/TorrentCat Next.app`. A generated 16-second test video remains in the new library, paused, for a quick playback check. The fixture is stored under `.local/smoke/` and is not part of the source repository.

Checks:

```sh
npm run check
npm test
npm run build
```

The default integration suite creates temporary files and uses loopback peers. It does not join public swarms or read the user's original profile. Its media-tools test uses installed FFmpeg and reports a skip if it is unavailable. A separate, ignored-by-default public-network smoke test downloads at least 2 MiB of verified data, stops within 60 seconds, and removes its temporary profile/downloads:

```sh
TORRENTCAT_SMOKE_TORRENT_URL='https://cdimage.debian.org/debian-cd/current/amd64/bt-cd/debian-13.7.0-amd64-netinst.iso.torrent' \
cargo test --manifest-path src-tauri/Cargo.toml public_swarm_download_smoke -- --ignored --nocapture
```

Use an official freely redistributable torrent URL; the example release URL may change.

## What works

- `.torrent` files, magnet links, info hashes and HTTP(S) torrent addresses; native file picker, drop, paste and launch handling.
- Downloading, seeding, pause/resume, individual file selection, progress, search, status filters and persistent engine state.
- Streaming before download completion, with seek-driven piece requests through librqbit and a loopback HTTP server supporting byte ranges.
- Built-in video/audio playback, seek, speed, volume, fullscreen, next/previous media file and saved playback positions.
- Optional embedded **VLC 3** from `/Applications/VLC.app`, with audio and subtitle track selection, and external player launch.
- FFmpeg posters, FFprobe metadata, SRT/VTT loading, embedded text-subtitle extraction and MP4/AAC playback copies. Original downloads are preserved.
- Creating and seeding a file or folder torrent, including private torrents, comments and tracker lists.
- Download/watch-folder settings, global trackers, launch at login and saved window geometry. The watcher waits for stable file size/modification time, retries incomplete copies and normalizes macOS path aliases.
- Streaming priority pauses competing active transfers for incomplete playback, restores them afterward, respects explicit manual pauses and recovers the restoration journal after restart.
- Notification sounds and optional macOS completion notifications. Desktop alerts are off by default and can be enabled in General preferences; macOS notification settings also apply.
- File/protocol launch inputs are queued until the frontend listeners are ready and deduplicated across native launch events.
- Export torrent metadata, copy magnet, reveal in Finder, remove from library, or explicitly delete downloaded data.
- Explicit import of the old fork's library and playback positions, starting paused, with duplicate-import protection.

For broader video-format support install VLC 3 matching the Mac's architecture. FFmpeg/FFprobe are discovered under `/opt/homebrew/bin`, `/usr/local/bin`, or `/usr/bin`. Both VLC and FFmpeg were already available on the development Mac.

## Library isolation and import

The application identifier is `com.eliaquin.torrentcat.next`. Its files live under:

```text
~/Library/Application Support/com.eliaquin.torrentcat.next/
  profile.json          preferences and playback positions
  profile.json.bak      preceding saved profile
  engine/               librqbit metadata and resume state
  posters/              generated thumbnails
  media-cache/          generated playback copies and subtitles
```

The original fork and `~/Library/Application Support/WebTorrent` are not modified. Import is an explicit action in Preferences. Quit the original app before importing because the new app will reuse its download locations. Imported torrents are paused; resume verifies their files. Missing locations or metadata are reported individually. Import does not copy old preferences, posters or engine resume bitfields.

Development tests can isolate state using `TORRENTCAT_PROFILE=/absolute/test/profile`. `TORRENTCAT_OFFLINE=1` disables DHT, trackers, UPnP and local peer discovery and binds the peer listener to loopback. The media server always binds to loopback and requires unpredictable, revocable URLs.

## Architecture and replacements

| Responsibility | Implementation |
| --- | --- |
| Desktop shell, menus, dialogs, clipboard, autostart, window state | Tauri 2 and narrowly scoped plugins |
| Torrent engine and persistence | `librqbit` 9.0.1, Tokio |
| Torrent creation with private/comment support | `lava_torrent` |
| Seekable local streaming | Axum, librqbit file streams, revocable media tokens |
| Folder watching | `notify` |
| Profile storage | Serde JSON with serialized atomic writes and backup |
| Media probing, posters, conversion | Rust-managed FFmpeg/FFprobe processes, two-job concurrency limit |
| Subtitle encoding | `encoding_rs` and SRT-to-VTT conversion |
| Embedded native playback | `libloading`, libVLC 3 and `objc2` NSView integration |
| UI | React 19 / TypeScript / CSS based on the fork's design |

`src/main.tsx` owns library navigation and state, `settings.tsx` owns preferences and torrent creation, `player.tsx` owns playback, and `ui.tsx` provides shared controls. Rust services are in `src-tauri/src/{core,media,jobs,native}.rs`; `main.rs` wires commands and desktop integration.

## Remaining parity and release work

This is a working local macOS preview, not a claim of complete WebTorrent compatibility:

- **WebRTC/browser-only peers are not supported.** The engine uses conventional BitTorrent peers. Web seeds, NAT-PMP and every tracker/network configuration have not been certified against the fork.
- The built-in player depends on WKWebView codec support. MP4 conversion copies the video track and converts audio; unsupported video codecs still require VLC. Embedded bitmap subtitles are not converted to text.
- Posters and metadata extraction wait for a complete file. Generated cache files currently require manual cleanup if disk space is needed.
- Torrent creation supports one file or one folder, rather than arbitrary files drawn from multiple folders.
- Global media-key integration, preferred-handler registration, update checking, and full legacy preference migration remain follow-up work. File/protocol associations are included in the bundle; changing the system default handler is left to macOS.
- Watch-folder ingestion retries stable but invalid metadata for approximately 30 seconds. Later file changes trigger a new attempt; permanently malformed metadata reports an error.
- Later work: Intel and oldest-supported-macOS testing, signed/notarized packaging, broader network/media regression tests, and Linux/Windows adapters. The current native player module is macOS-specific.

The original research, alternatives and phased rollout are recorded in [the rebuild plan](docs/TAURI-REBUILD-PLAN.md). That document predates this implementation; this README describes the local preview.

## Attribution

The visual styling, application icon and Material Icons font were adapted/copied from the user's MIT-licensed local WebTorrent Desktop fork. Its copyright notice is retained in [LICENSE](LICENSE). Material Icons are provided under Apache 2.0; see [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md). Rust and npm dependencies retain their respective licenses.
