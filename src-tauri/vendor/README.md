# Patched rqbit components

`librqbit`, `librqbit-tracker-comms`, `librqbit-bencode` and `librqbit-peer-protocol` are vendored from the published 9.0.1 crates,
upstream [ikatson/rqbit](https://github.com/ikatson/rqbit) commit
`a499d2f243d124e144aef137afe7cb304a6e3f36`.
All retain their Apache-2.0 license; see `LICENSE-APACHE` in each directory.
The application uses these copies through `[patch.crates-io]`.

TorrentCat modifications:

- Confine file creation, reuse and deletion to an open directory capability using
  `cap-std`. On macOS reject a symbolic-link download root. Keep the root folder
  on removal so a replaced path cannot cause deletion of an unrelated directory.
- Bind UDP responses to the pending request endpoint and action, preserving the
  transaction reservation through delivery and cancellation. Use BEP 15 retry
  delays, renew expired connection IDs, try alternate DNS addresses and preserve
  path/query through bounded BEP 41 URLData options.
- Bound HTTP response bodies, local metadata reads, tracker peer lists and IDs,
  bencode depth/value/byte budgets and numeric conversion. Reject unrepresentable
  announce deadlines without terminating the monitor.
- Verify every disk piece on restart and explicit paused rechecks. Persist optional
  filesystem checkpoints only after payload sync, using atomic replacement and
  directory sync; never flush unsynced bitmaps on drop.
- Bound upload admission across peer/session queues, honor cancellation and interest,
  schedule ready peers round robin and bound writer/metadata request queues.
- Persist pause/removal intent before tracker cleanup, retain failed removals for
  retry and restart, and atomically sync JSON/metainfo plus unlink directories.
  Serialize operations per torrent, deduplicate concurrent magnet resolution and
  preserve monotonic persisted IDs. Join owned tasks and bound total final announces.
- Count upload payload at each successful transport write, including interrupted
  frames and explicit retransfers, excluding protocol headers/control messages.
  Preserve session totals across pauses, independently of verified disk data.
- Calculate tracker `left` across all files, including deselected files.
- Send HTTP/UDP lifecycle events and await bounded stopped requests on pause,
  removal and shutdown. Respect both tracker interval and minimum interval;
  retain and safely encode HTTP tracker IDs.
- Retain missing/invalid saved metainfo as visible paused recovery entries, with
  bounded regular-file reads and no implicit magnet discovery. Repair only from
  matching metainfo, preserving IDs, selection, output paths and pending removal.
- Preserve announce tiers, shuffle and promote successful URLs within each tier.
  Private failover waits for old peers to disconnect and discards their queued
  connections/retries; no global/custom trackers, initial peers, DHT, LSD, PEX
  or metadata sharing for private torrents.
- Resolve unknown magnets through an explicit tracker only; require the original
  `.torrent` file for private torrents. Unknown magnets cannot switch trackers
  before peer isolation is available.
- Apply additive extension updates and zero-ID disablement. Keep one PEX worker
  per peer, avoid timer catch-up bursts and filter local contacts before encoding.
- Avoid logging tracker URLs, passkeys or tracker failure text in announce errors.

Changes include the component `Cargo.toml` files, `librqbit/src/session.rs`,
`librqbit/src/torrent_state/{mod.rs,live/mod.rs}`,
`librqbit/src/peer_info_reader/mod.rs`,
`librqbit/src/storage/filesystem/fs.rs`, and
`librqbit-tracker-comms/src/{tracker_comms.rs,tracker_comms_http.rs,tracker_comms_udp.rs}`.
Bencode changes are in `librqbit-bencode/src/{deserialize.rs,lib.rs}`; the bounded
HTTP reader is `librqbit-tracker-comms/src/http_body.rs`. Checkpoint/recheck changes also touch `bitv.rs`, `bitv_factory.rs`,
`session_persistence/{json.rs,postgres.rs}`, `storage/mod.rs`,
`storage/filesystem/opened_file.rs` and `torrent_state/initializing.rs`.
Upload changes also touch `upload_queue.rs`, `peer_connection.rs`, `limits.rs` and
`torrent_state/live/peer/mod.rs`.
Extension parser/serializer changes are in `librqbit-peer-protocol/src/extended/`.
That crate omits the upstream benchmark target and Criterion dev dependency.
Other vendored sources retain their upstream implementation.

Regression tests live in `../src/tracker_tests.rs` and
`../tests/storage_confinement.rs`, `../tests/lifecycle.rs` and use only temporary data,
loopback HTTP/UDP trackers and loopback peers:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo test --manifest-path src-tauri/Cargo.toml --locked -p librqbit-bencode -p librqbit-tracker-comms -p librqbit-peer-protocol --lib
cargo test --manifest-path src-tauri/Cargo.toml --locked -p librqbit --lib --no-default-features --features rust-tls -- --skip tests::e2e
```

When upgrading rqbit, port these changes and run the local tracker tests before
removing or replacing a patch. Local correctness does not imply approval
by any private tracker.
