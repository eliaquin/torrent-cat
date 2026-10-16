# TorrentCat Next

A macOS app for downloading and streaming torrents, built with Tauri, Rust, React and TypeScript. Powered by librqbit.

## Features

- Open torrent files, magnet links, info hashes and torrent URLs.
- Download, seed, pause, resume and select individual files.
- Play media while it downloads, with seeking, subtitles and saved playback positions.
- Use embedded VLC 3 by default, with built-in playback when VLC is unavailable. Choose a different player in Preferences.
- Create torrents from a file or folder, including private torrents.
- Configure watch folders, streaming priority, launch at login and completion notifications.

## Build and run

Requires macOS 13+, Apple command line developer tools, stable Rust and Node.js 22.12+.

```sh
git clone https://github.com/eliaquin/torrent-cat.git
cd torrent-cat
npm ci
npm start
```

Build the macOS application:

```sh
npm run bundle
```

Copy `src-tauri/target/release/bundle/macos/TorrentCat Next.app` to Applications. The build is ad-hoc signed, not notarized.

Currently tested on Apple Silicon. No prebuilt releases are available. Linux and Windows are not supported yet.

### Optional media tools

- **VLC 3:** install at `/Applications/VLC.app`, matching your Mac's architecture, for broader codec support.
- **FFmpeg and FFprobe:** enable thumbnails, media metadata, audio conversion and embedded text-subtitle extraction. Executables are detected in `/opt/homebrew/bin`, `/usr/local/bin` and `/usr/bin`.

## App data

Preferences, playback positions, torrent state and media caches are stored in:

```text
~/Library/Application Support/com.eliaquin.torrentcat.next/
```

Downloads use the folder selected in Preferences. Generated media caches currently require manual cleanup.

## Limitations

- WebRTC/browser-only peers are unsupported. Web seeds and NAT-PMP need further validation.
- Built-in playback depends on WKWebView codec support. Audio conversion preserves the video codec; unsupported video formats require VLC or another player.
- Thumbnails and metadata require a completed file. Bitmap subtitles cannot be converted to text.
- Torrent creation accepts one file or one folder at a time.
- Automatic updates and global media-key controls are not implemented.

## Development

```sh
npm run check   # TypeScript and Rust checks
npm test        # Rust unit and integration tests
npm run test:player # Player control visibility tests
npm run build  # Frontend production build
```

The default tests use temporary files and loopback peers. Media conversion tests require FFmpeg. See [validation notes](docs/LOCAL-VALIDATION.md) for recorded results.

Set `TORRENTCAT_PROFILE` to a separate profile directory for development. `TORRENTCAT_OFFLINE=1` disables peer discovery and trackers and restricts peer connections to loopback.

## License and contributions

[MIT](LICENSE). [Third-party notices](THIRD-PARTY-NOTICES.md).

External contributions are not accepted; pull requests are disabled. Issues are available for bug reports. See [CONTRIBUTING.md](CONTRIBUTING.md).

Thanks to WebTorrent Desktop for inspiring this project.
