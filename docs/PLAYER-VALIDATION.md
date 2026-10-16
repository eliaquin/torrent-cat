# Player validation — 2026-10-15

Tested on macOS with an isolated offline profile and generated H.264/AAC video. The MKV fixture contains English and Spanish audio tracks and text subtitles, with English marked as the default for both.

## Automated checks

- `npm run test:player`: 7 passing cases covering the idle timeout, renewed activity, windowed playback, pause, loading/error state, hover, keyboard focus and dragging.
- `npm test`: 12 passing tests; the explicit public-network test remains skipped.
- `npm run check`: TypeScript and Rust checks pass.
- `npm run bundle`: release bundle builds and passes local ad-hoc signature verification.

## Playback checks

- Native VLC shows the default English audio and subtitle selections on initial playback.
- Switching both tracks to Spanish updates the selectors.
- Fullscreen video retains its aspect ratio and fills the available viewport, with controls layered above VLC.
- Controls disappear during idle playback and reappear after pointer activity; paused playback keeps controls visible.
- Fullscreen button, Escape, double-click and F toggle fullscreen. Returning to the library exits fullscreen.
- Built-in playback works with an MP4 playback copy and the same fullscreen layout.

Test fixtures and profiles are local, ignored files and are not included in the repository.

## Sleep prevention — 2026-10-16

- Updated from `639da33`; `npm run check`, 19 Rust tests and 16 player tests pass (public-network test skipped).
- Three Rust regression tests check actual macOS assertions: release/reopen, shutdown rejecting late playback, and owner termination without cleanup.
- The release app was tested with an isolated offline profile. `pmset -g assertions` confirmed sleep prevention during playback and its removal after returning to the library, quitting with Cmd-Q, closing the window, and forcibly terminating the app with SIGKILL.
- `caffeinate` now watches the app PID; normal player/app closure also explicitly terminates and reaps it.

## Unified timeline — 2026-10-16

- Downloaded byte ranges and elapsed playback share one rail. Download gaps remain visible behind the elapsed fill; seeking still uses time.
- Browser checks covered partial, complete, empty and unavailable download maps, unknown duration, click/drag seeking, and Home/End/arrow keys.
- The installed macOS release was checked with native VLC in windowed and fullscreen playback, including seeking and accessible time/download descriptions.
- TypeScript/Vite build, 16 player tests and the signed release bundle pass.
