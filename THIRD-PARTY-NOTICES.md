# Third-party notices

## Notification sounds

The eight AAC files in `public/sound/` were converted from WebTorrent Desktop's notification WAV assets, carried over through the local fork at commit `86a5f8deef3dec27c78d8796e18ddf8231c26a05`. The fork's sound history traces these assets to upstream WebTorrent Desktop contributors.

Copyright (c) WebTorrent, LLC. The MIT notice for these sound assets is preserved in [licenses/WebTorrent-MIT.txt](licenses/WebTorrent-MIT.txt). This notice applies to the inherited sounds, not to Eliaquín Encarnación's TorrentCat UI design or the new application as a whole.

## TorrentCat application icon

The mint-teal cat and download-arrow icon in `src-tauri/icons/` was generated for TorrentCat using OpenAI image generation. It replaces the inherited application icon. The PNG is the 1024-pixel master; the ICNS contains macOS icon sizes from 16 to 1024 pixels.

## Material Icons

`public/MaterialIcons-Regular.woff2` is Google’s Material Icons font.

Copyright Google LLC. Licensed under the Apache License, Version 2.0.

Source: https://github.com/google/material-design-icons
License: [Apache 2.0](licenses/Apache-2.0.txt)

## Optional media tools

VLC and FFmpeg are discovered on the user's machine and are not redistributed in this app bundle. Their licenses and build configurations are provided by their respective installations.

## Patched torrent engine

`src-tauri/vendor/` contains modified copies of librqbit and librqbit-tracker-comms 9.0.1 by the rqbit contributors, licensed under Apache-2.0. Source provenance and changes are recorded in [the vendor notes](src-tauri/vendor/README.md); each crate includes its license.

## Dependency licenses

Exact Rust and JavaScript dependencies are recorded in `src-tauri/Cargo.lock` and `package-lock.json`. Before distributing a public release, generate a complete dependency license inventory for that release, include required notices, and review any distribution-specific obligations.
