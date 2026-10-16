# Patched rqbit components

`librqbit` and `librqbit-tracker-comms` are vendored from the published 9.0.1 crates,
upstream [ikatson/rqbit](https://github.com/ikatson/rqbit) commit
`a499d2f243d124e144aef137afe7cb304a6e3f36`.
Both retain their Apache-2.0 license; see `LICENSE-APACHE` in each directory.
The application uses these copies through `[patch.crates-io]`.

TorrentCat modifications:

- Confine file creation, reuse and deletion to an open directory capability using
  `cap-std`. On macOS reject a symbolic-link download root. Keep the root folder
  on removal so a replaced path cannot cause deletion of an unrelated directory.
- Track actual payload bytes across pauses, independently of verified disk data.
- Calculate tracker `left` across all files, including deselected files.
- Send HTTP/UDP lifecycle events and await bounded stopped requests on pause,
  removal and shutdown. Respect both tracker interval and minimum interval;
  retain and safely encode HTTP tracker IDs.
- Preserve private tracker ordering; use only the first eligible tracker, without
  global/custom trackers, initial peers, DHT, LSD, PEX or metadata sharing.
- Resolve unknown magnets through an explicit tracker only; require the original
  `.torrent` file for private torrents. No automatic private tracker failover.
- Avoid logging tracker URLs, passkeys or tracker failure text in announce errors.

Changes are in the two `Cargo.toml` files, `librqbit/src/session.rs`,
`librqbit/src/torrent_state/{mod.rs,live/mod.rs}`,
`librqbit/src/peer_info_reader/mod.rs`,
`librqbit/src/storage/filesystem/fs.rs`, and
`librqbit-tracker-comms/src/{tracker_comms.rs,tracker_comms_http.rs}`.
Upstream code elsewhere is unchanged.

Regression tests live in `../src/tracker_tests.rs` and
`../tests/storage_confinement.rs` and use only temporary data,
loopback HTTP/UDP trackers and loopback peers:

```sh
cargo test --manifest-path src-tauri/Cargo.toml tracker_tests
```

When upgrading rqbit, port these changes and run the local tracker tests before
removing or replacing either patch. Local correctness does not imply approval
by any private tracker.
