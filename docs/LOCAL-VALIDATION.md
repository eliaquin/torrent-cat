# Local validation — October 15, 2026

Target: Apple Silicon macOS; Rust 1.99.0; Node 26.3.0. Built application version 0.1.0, identifier `com.eliaquin.torrentcat.next`.

## Automated checks

- TypeScript typecheck and Vite production build: passed.
- Rust debug compilation and optimized release compilation: passed.
- `npm test`: **12 passed, 0 failed** (local suite); the public-network smoke test is separate and ignored by default. FFmpeg was available, so the optional media integration test ran rather than skipping.
- Strict ad-hoc signature verification of `/Applications/TorrentCat Next.app`: passed.
- Release bundle reported by Tauri: **17.52 MiB**.

The Rust tests cover:

1. Input/address normalization and rejection of unsupported protocols.
2. Atomic JSON profile writes and previous-version backup.
3. HTTP range boundaries, suffix ranges and malformed/multiple-range rejection.
4. SRT timestamp conversion.
5. Two local torrent sessions: creation, seeding, direct-peer transfer, exact late-range bytes, HEAD, foreign-origin rejection, invalid-range response, selection, pause/revocation, resume, restart recovery and remove-without-delete.
6. Duplicate addition and multi-file root-folder preservation, including creation without trackers.
7. Synthetic legacy import: custom metadata filename, paused initial state, original paths, playback positions, repeated-import idempotency and unchanged legacy config/source files.
8. Generated MKV/AC3 media: FFprobe, JPEG poster, MP4 playback copy, unchanged original bytes and UTF-16 SRT decoding/serving.

9. Playback priority: complete playback leaves other transfers running; incomplete playback pauses competitors; close restores only automatic pauses; manual pause overrides restoration; restart recovers journaled pauses.
10. Real macOS watched-folder events: existing files are ignored, incomplete copies are retried without premature errors, and a subsequent completed file is added. Includes canonical `/var`/`/private/var` path handling.
11. Completion notifications: first observation establishes a baseline; only new incomplete-to-complete transitions notify; duplicate polls and re-added completed seeds do not.
12. Launch inbox: queued cold-start inputs, file-URL normalization, frontend readiness and duplicate native-event suppression.

## Public-network smoke check

Ran the opt-in `public_swarm_download_smoke` test against Debian's official `debian-13.7.0-amd64-netinst.iso.torrent`. The Rust engine received **2,097,152 verified bytes** out of 792,723,456 total in a **3.61-second** test run, then stopped the session and removed its temporary files. The test passed. This proves one conventional public swarm path, not every tracker or NAT configuration. Source: [Debian torrent downloads](https://www.debian.org/CD/torrent-cd/).

## Installed app checks

Used the actual native Tauri window through macOS accessibility and screenshots:

- Launched the debug build, then the installed optimized app from `/Applications/TorrentCat Next.app`.
- Created a private torrent from a generated MP4 through the native file/save dialogs, with a loopback-only test tracker.
- Observed seeding at 100%, file progress, pause/resume controls, and a persisted library after restart.
- Played the generated MP4 in the built-in WKWebView player; observed decoded video and advancing time.
- Verified saved playback position and restart-from-beginning behavior for a completed video.
- Switched to embedded VLC in the installed release; observed decoded video, advancing time and audio-track controls. Paused and entered/exited fullscreen.
- Returned to the library and left the test torrent paused.
- Inspected preferences and detected installed VLC/FFmpeg/FFprobe. After updating the release, confirmed that Prioritize streaming and Notification sounds default on, and optional completion notifications default off.

The original fork's Git working tree remained clean. Its profile was not imported or edited. Only synthetic data was used for import testing.

## Limits of these checks

This does not certify every public swarm/tracker/NAT configuration, arbitrary codecs, external player automation, autostart, macOS notification delivery under all system settings, Mac system-default associations, Intel builds, older macOS versions, or Linux/Windows. WebRTC/browser peers are unsupported. Full parity and distribution work is listed in the README and rebuild plan.


## Original request audit

| Requirement | Current evidence |
| --- | --- |
| Use the local fork as the reference | Plan identifies the inspected local path, commit and dependency inventory; original working tree remains clean. |
| Research Rust equivalents and assess Tauri | Rebuild plan includes framework comparison, runtime-library replacement table, primary sources, compatibility gaps and implementation phases. |
| Build a fresh app with less runtime baggage | New Rust/Tauri project, new TypeScript UI, own profile and app identifier. Installed bundle contains the executable, icon, plist and signature; no Node or Electron runtime. Installed folder is about 17 MiB versus 350 MiB for the existing TorrentCat installation (disk usage, not a memory/performance benchmark). |
| Preserve the interface | Existing fork's dark shelf and preference styling/assets retained; rendered native-window checks show library, search/status filters, cards, file list, preferences and integrated playback. |
| Preserve functionality as closely as possible | Implemented torrent and playback flows are listed above, with local and public-network evidence. Remaining differences are explicitly listed in README: browser/WebRTC peers lack a verified equivalent in the chosen Rust engine, optional media tools remain external, and some desktop/distribution conveniences are follow-up work. No claim of exact WebTorrent wire compatibility. |
| Start with this Mac; other platforms later | Optimized Apple Silicon macOS bundle installed and running from `/Applications/TorrentCat Next.app`; strict ad-hoc signature verification passed. Intel/older OS, Windows and Linux are not claimed tested. |
| Go from the beginning to running locally | Build commands and source are present; installed binary SHA-256 matches the built binary (`52de9c507154aca0e5e4f1d0a060e88039a4c2d366804c0d5cd48420b9e12980`); updated app visibly launches with the preserved test library. |

This audit establishes the requested local Mac app and researched implementation, with the documented approximations allowed by the original “same or as close as possible” requirement. It does not treat later public distribution or exact protocol parity as completed.
## Desktop preferences and library — 2026-10-16

- `npm run check`, 20 Rust tests and 16 player tests pass; the public-network smoke test remains skipped. The release bundle builds and passes ad-hoc signature verification.
- The installed app is named `TorrentCat.app`. Its existing bundle identifier and profile location are retained; all nine library items, preferences and playback positions survived the update.
- Preferences displays macOS's current `.torrent` and magnet handlers. The “Make default” action uses NSWorkspace and reports native errors; changing the user's associations was not exercised during validation.
- Integration tests cover newest-first ordering, duplicate additions, removal and re-addition, restart persistence and migration of profiles without ordering history.
- A long library title was checked in the installed app: its hover tooltip wraps and exposes the full name outside the card boundary. File names and player titles use the same component.

## Largest-video preview — 2026-10-16

- Library previews select the largest video by file size and wait until that file is complete. Completed samples do not override it. Existing poster caches are keyed by source file, so another video's cached image is not reused.
- Four library regression tests cover file ordering, larger non-video files, an incomplete main video with a complete sample, missing metadata/progress, audio-only torrents, single videos and equal-size ties.


## Private tracker accounting and privacy — 2026-10-16

- `cargo test --manifest-path src-tauri/Cargo.toml`: **29 passed, 0 failed, 1 ignored**. The ignored test accesses a public swarm and was not run. TypeScript and the Vite production build also pass.
- Nine regressions in `src-tauri/src/tracker_tests.rs` use temporary synthetic files, HTTP/UDP trackers and peers on loopback. No TD account, passkey or public tracker is used.
- For a 32 KiB private torrent with 16 KiB already on disk and only that file selected, the tracker receives `downloaded=0, left=16384`. After enabling the other file and downloading from a local seed, it receives `downloaded=16384, left=0`; the seed reports `uploaded=16384`.
- HTTP and UDP produce one `started` per run, periodic updates without repeated lifecycle events, one `completed` for the full download, and `stopped` before pause/removal/shutdown return. Pause/resume preserves transfer counters and the announce key. Existing complete data does not generate a completion event.
- HTTP tests verify both interval/minimum-interval orderings, preservation of the fixture passkey and round-tripping a tracker ID containing reserved URL characters. Duplicate tracker sources produce a single announce stream. An unresponsive tracker cannot indefinitely block pause; shutdown also cancels unresolved magnet discovery and sends `stopped`.
- Private torrents ignore global/custom trackers and manually supplied peers, even with only one tracker in the metadata. Tracker failures back off without trying another tracker. Private peer handshakes disable PEX and metadata sharing. A local DHT positive control observes a public hash and never the private hash. LSD remains excluded by the private discovery branch; the tests disable multicast to avoid LAN traffic.
- Playback priority continues to pause other incomplete downloads, but completed seeds remain active. Explicit user pauses and recovery after restart remain covered.

Private torrents require the tracker-issued `.torrent` file. Unknown magnets use an explicit tracker only, without DHT/LSD/PEX; magnets that resolve to private metadata are rejected. Private tracker failover is intentionally disabled until old-peer isolation is implemented. Final announces are bounded, best effort when the tracker is unavailable. Local tests do not establish approval by TD.

## Window zoom after playback — 2026-10-17

- Reproduced the reported AppKit abort with an isolated window: restoring `contentAspectRatio = (0, 0)` after a video ratio makes title-bar zoom calculate a frame with `NaN` height. The zero getter means unconstrained; passing it back to the aspect setter is not a safe reset.
- Playback cleanup now restores the saved content resize increments, and restores an aspect ratio only when it was positive. The original minimum size is preserved.
- Added `cargo test --manifest-path src-tauri/Cargo.toml --test window_resize`. It runs the actual Rust window-fitting code on AppKit's main thread using an undisplayed window. It aborted before the fix and passes afterward, covering repeated playback/cleanup/zoom cycles, portrait video, duplicate cleanup, and existing increment/aspect constraints.
- All three existing window-fitting unit tests pass. The production frontend build and locally signed app bundle succeed.
- Installed-app check: started VLC playback, returned to the library, then double-clicked the title bar to maximize and restore. Both resize animations completed without a crash. Startup briefly stalled opening an existing torrent file, then recovered without changing the profile.

## Desktop library — 2026-10-17

- Library now matches Preferences with a fixed category sidebar, compact toolbar, smaller card controls and a bottom status bar. Transfer filters show counts; search/filter results show a matching count. Thumbnail, streaming and transfer actions remain available.
- TypeScript/Vite production build and all four library tests pass. Locally signed macOS bundle builds successfully.
- Browser validation using the production assets and synthetic snapshot data covers sidebar filtering, search with no results, clearing filters, file expansion, and layouts at 1280×720 and the app minimum of 660×480.
- Initial native validation was blocked by a Downloads folder permission prompt: macOS TCC logs confirmed that the rebuilt app’s signature did not match the previous grant and triggered a new request. Torrent session restoration waits in filesystem `open` before the UI starts, making the app appear stalled until permission is answered. No download or profile data was modified to work around it. The user subsequently reviewed and approved the installed library layout.
