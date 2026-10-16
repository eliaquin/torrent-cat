# Patched rqbit components

`librqbit`, `librqbit-tracker-comms` and `librqbit-bencode` are vendored from the published 9.0.1 crates,
upstream [ikatson/rqbit](https://github.com/ikatson/rqbit) commit
`a499d2f243d124e144aef137afe7cb304a6e3f36`.
All retain their Apache-2.0 license; see `LICENSE-APACHE` in each directory.
The application uses these copies through `[patch.crates-io]`.

TorrentCat modifications:

- Confine file creation, reuse and deletion to an open directory capability using
  `cap-std`. On macOS reject a symbolic-link download root. Keep the root folder
  on removal so a replaced path cannot cause deletion of an unrelated directory.
- Bind UDP responses to the pending request endpoint and action, preserving the
  transaction reservation through delivery and cancellation.
- Bound HTTP response bodies, local metadata reads, tracker peer lists and IDs,
  bencode depth/value/byte budgets and numeric conversion. Reject unrepresentable
  announce deadlines without terminating the monitor.
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

Changes include the component `Cargo.toml` files, `librqbit/src/session.rs`,
`librqbit/src/torrent_state/{mod.rs,live/mod.rs}`,
`librqbit/src/peer_info_reader/mod.rs`,
`librqbit/src/storage/filesystem/fs.rs`, and
`librqbit-tracker-comms/src/{tracker_comms.rs,tracker_comms_http.rs,tracker_comms_udp.rs}`.
Bencode changes are in `librqbit-bencode/src/{deserialize.rs,lib.rs}`; the bounded
HTTP reader is `librqbit-tracker-comms/src/http_body.rs`. Other vendored sources
retain their upstream implementation.

Regression tests live in `../src/tracker_tests.rs` and
`../tests/storage_confinement.rs` and use only temporary data,
loopback HTTP/UDP trackers and loopback peers:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo test --manifest-path src-tauri/Cargo.toml --locked -p librqbit-bencode -p librqbit-tracker-comms --lib
```

When upgrading rqbit, port these changes and run the local tracker tests before
removing or replacing either patch. Local correctness does not imply approval
by any private tracker.
