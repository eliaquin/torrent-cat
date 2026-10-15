# Third-party notices

## WebTorrent Desktop / local TorrentCat fork

The library and preference styling, notification sounds and visual design originate in the user's local fork of WebTorrent Desktop (`../webtorrent-desktop`), baseline commit `86a5f8deef3dec27c78d8796e18ddf8231c26a05`. Its MIT notice is preserved in `LICENSE`. Notification WAV assets were converted to smaller AAC files in `public/sound/`.

## TorrentCat application icon

The mint-teal cat and download-arrow icon in `src-tauri/icons/` was generated for TorrentCat using OpenAI image generation. It replaces the inherited application icon. The PNG is the 1024-pixel master; the ICNS contains macOS icon sizes from 16 to 1024 pixels.

## Material Icons

`public/MaterialIcons-Regular.woff2` is the Material Icons font from that fork.

Copyright Google LLC. Licensed under the Apache License, Version 2.0.

Source: https://github.com/google/material-design-icons
License: [Apache 2.0](licenses/Apache-2.0.txt)

## Optional media tools

VLC and FFmpeg are discovered on the user's machine and are not redistributed in this app bundle. Their licenses and build configurations are provided by their respective installations.

## Dependency licenses

Exact Rust and JavaScript dependencies are recorded in `src-tauri/Cargo.lock` and `package-lock.json`. Before distributing a public release, generate a complete dependency license inventory for that release, include required notices, and review any distribution-specific obligations.
