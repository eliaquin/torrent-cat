//! Entirely local tracker/peer regressions. No TD credentials or public tracker requests.
use axum::{
    Router,
    extract::{RawQuery, State},
    routing::get,
};
use librqbit::{
    AddTorrent, AddTorrentOptions, ListenerOptions, Session, SessionOptions, api::TorrentIdOrHash,
};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    time::{sleep, timeout},
};

#[derive(Clone, Debug)]
struct Announce {
    at: Instant,
    query: HashMap<String, String>,
}
type ManagedTorrentHandle = Arc<librqbit::ManagedTorrent>;
impl Announce {
    fn event(&self) -> &str {
        self.query.get("event").map(String::as_str).unwrap_or("")
    }
    fn number(&self, field: &str) -> u64 {
        self.query[field].parse().unwrap()
    }
}
#[derive(Default)]
struct TrackerState {
    requests: Vec<Announce>,
    peers: Vec<SocketAddr>,
    interval: u64,
    minimum: u64,
    reject: bool,
}
struct Tracker {
    url: String,
    state: Arc<Mutex<TrackerState>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Tracker {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Tracker {
    async fn http(interval: u64, minimum: u64) -> Self {
        async fn announce(
            State(state): State<Arc<Mutex<TrackerState>>>,
            RawQuery(query): RawQuery,
        ) -> (axum::http::StatusCode, Vec<u8>) {
            let mut state = state.lock().unwrap();
            state.requests.push(Announce {
                at: Instant::now(),
                query: url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
                    .into_owned()
                    .collect(),
            });
            if state.reject {
                return (axum::http::StatusCode::SERVICE_UNAVAILABLE, vec![]);
            }
            let mut peers = Vec::new();
            for peer in &state.peers {
                if let SocketAddr::V4(peer) = peer {
                    peers.extend_from_slice(&peer.ip().octets());
                    peers.extend_from_slice(&peer.port().to_be_bytes());
                }
            }
            let mut body = format!(
                "d8:intervali{}e12:min intervali{}e5:peers{}:",
                state.interval,
                state.minimum,
                peers.len()
            )
            .into_bytes();
            body.extend(peers);
            body.extend_from_slice(b"10:tracker id13:local id/+?&=e");
            (axum::http::StatusCode::OK, body)
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/announce?passkey=local-fixture-only",
            listener.local_addr().unwrap()
        );
        let state = Arc::new(Mutex::new(TrackerState {
            interval,
            minimum,
            ..Default::default()
        }));
        let app = Router::new()
            .route("/announce", get(announce))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { url, state, task }
    }
    async fn udp() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let url = format!("udp://{}/announce", socket.local_addr().unwrap());
        let state = Arc::new(Mutex::new(TrackerState::default()));
        let saved = state.clone();
        let task = tokio::spawn(async move {
            let mut buffer = [0; 2048];
            loop {
                let (len, addr) = socket.recv_from(&mut buffer).await.unwrap();
                if len < 16 {
                    continue;
                }
                let action = u32::from_be_bytes(buffer[8..12].try_into().unwrap());
                let mut reply = buffer[8..16].to_vec();
                if action == 0 {
                    reply.extend_from_slice(&12345u64.to_be_bytes());
                } else if action == 1 && len >= 98 {
                    let event = u32::from_be_bytes(buffer[80..84].try_into().unwrap());
                    let mut query = HashMap::new();
                    query.insert(
                        "event".into(),
                        match event {
                            1 => "completed",
                            2 => "started",
                            3 => "stopped",
                            _ => "",
                        }
                        .into(),
                    );
                    for (field, offset) in [("downloaded", 56), ("left", 64), ("uploaded", 72)] {
                        query.insert(
                            field.into(),
                            u64::from_be_bytes(buffer[offset..offset + 8].try_into().unwrap())
                                .to_string(),
                        );
                    }
                    query.insert(
                        "key".into(),
                        u32::from_be_bytes(buffer[88..92].try_into().unwrap()).to_string(),
                    );
                    let mut saved = saved.lock().unwrap();
                    saved.requests.push(Announce {
                        at: Instant::now(),
                        query,
                    });
                    reply.extend_from_slice(&5u32.to_be_bytes());
                    reply.extend_from_slice(&0u32.to_be_bytes());
                    reply.extend_from_slice(&1u32.to_be_bytes());
                    for peer in &saved.peers {
                        if let SocketAddr::V4(peer) = peer {
                            reply.extend_from_slice(&peer.ip().octets());
                            reply.extend_from_slice(&peer.port().to_be_bytes());
                        }
                    }
                } else {
                    continue;
                }
                socket.send_to(&reply, addr).await.unwrap();
            }
        });
        Self { url, state, task }
    }
    fn records(&self) -> Vec<Announce> {
        self.state.lock().unwrap().requests.clone()
    }
    async fn wait(&self, condition: impl Fn(&[Announce]) -> bool) {
        timeout(Duration::from_secs(25), async {
            while !condition(&self.records()) {
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            panic!(
                "local tracker condition timed out: {error}; records: {:?}",
                self.records()
            )
        });
    }
}
async fn session(path: &Path, extra_tracker: Option<&str>) -> Arc<Session> {
    let allowlist = path.parent().unwrap().join("loopback-allowlist.txt");
    std::fs::write(&allowlist, "loopback:127.0.0.1-127.0.0.2\n").unwrap();
    Session::new_with_opts(
        path.into(),
        SessionOptions {
            dht: None,
            disable_local_service_discovery: true,
            allowlist_url: Some(url::Url::from_file_path(allowlist).unwrap().into()),
            listen: Some(ListenerOptions {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                ..Default::default()
            }),
            trackers: extra_tracker
                .into_iter()
                .map(|t| t.parse().unwrap())
                .collect(),
            client_name_and_version: Some("TorrentCat/0.1.0".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}
fn fixture(path: &Path, trackers: Vec<String>) -> Vec<u8> {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("a.bin"), vec![42; 16_384]).unwrap();
    std::fs::write(path.join("b.bin"), vec![99; 16_384]).unwrap();
    let torrent = lava_torrent::torrent::v1::TorrentBuilder::new(path, 16_384)
        .set_privacy(true)
        .set_announce_list(trackers.into_iter().map(|u| vec![u]).collect())
        .set_num_threads(2)
        .build()
        .unwrap();
    let mut bytes = Vec::new();
    torrent.write_into(&mut bytes).unwrap();
    bytes
}
async fn add(
    session: &Arc<Session>,
    bytes: &[u8],
    path: &Path,
    only: Option<Vec<usize>>,
    initial: Vec<SocketAddr>,
    custom: Vec<String>,
) -> ManagedTorrentHandle {
    let handle = session
        .add_torrent(
            AddTorrent::TorrentFileBytes(bytes.to_vec().into()),
            Some(AddTorrentOptions {
                output_folder: Some(path.to_string_lossy().into()),
                overwrite: true,
                paused: true,
                only_files: only,
                initial_peers: Some(initial),
                trackers: Some(custom),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    timeout(Duration::from_secs(15), handle.wait_until_initialized())
        .await
        .unwrap()
        .unwrap();
    session.unpause(&handle).await.unwrap();
    handle
}

async fn transfer_accounting(udp: bool) {
    let tracker = if udp {
        Tracker::udp().await
    } else {
        Tracker::http(1, 1).await
    };
    let forbidden = Tracker::http(1, 1).await;
    let unsolicited = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let output = root.path().join("output");
    let bytes = fixture(&source, vec![tracker.url.clone()]);
    std::fs::create_dir(&output).unwrap();
    std::fs::copy(source.join("a.bin"), output.join("a.bin")).unwrap();
    let seed = session(&source, Some(&forbidden.url)).await;
    let seeder = add(
        &seed,
        &bytes,
        &source,
        None,
        vec![],
        vec![forbidden.url.clone()],
    )
    .await;
    tracker
        .wait(|r| r.iter().any(|r| r.event() == "started"))
        .await;
    let client = session(&output, Some(&forbidden.url)).await;
    let leecher = add(
        &client,
        &bytes,
        &output,
        Some(vec![0]),
        vec![unsolicited.local_addr().unwrap()],
        vec![forbidden.url.clone()],
    )
    .await;
    tracker
        .wait(|r| r.iter().filter(|r| r.event() == "started").count() == 2)
        .await;
    let starts: Vec<_> = tracker
        .records()
        .into_iter()
        .filter(|r| r.event() == "started")
        .collect();
    assert_eq!(starts[0].number("downloaded"), 0);
    assert_eq!(starts[0].number("left"), 0);
    assert_eq!(
        starts[1].number("downloaded"),
        0,
        "existing disk data is not network traffic"
    );
    assert_eq!(
        starts[1].number("left"),
        16_384,
        "unselected file must remain in left"
    );
    tracker
        .wait(|r| r.iter().any(|r| r.event().is_empty()))
        .await;
    assert_eq!(
        tracker
            .records()
            .iter()
            .filter(|r| r.event() == "started")
            .count(),
        2
    );
    assert!(tracker.records().iter().all(|r| r.event() != "completed"));
    client
        .update_only_files(&leecher, &HashSet::from([0, 1]))
        .await
        .unwrap();
    tracker.state.lock().unwrap().peers = vec![seed.listen_addr().unwrap()];
    tracker
        .wait(|r| r.iter().any(|r| r.event() == "completed"))
        .await;
    assert_eq!(
        std::fs::read(output.join("b.bin")).unwrap(),
        vec![99; 16_384]
    );
    let completion = tracker
        .records()
        .into_iter()
        .find(|r| r.event() == "completed")
        .unwrap();
    assert_eq!(completion.number("downloaded"), 16_384);
    assert_eq!(completion.number("left"), 0);
    assert_eq!(completion.number("uploaded"), 0);
    let key = completion.query["key"].clone();
    client.pause(&leecher).await.unwrap();
    let stopped = tracker
        .records()
        .into_iter()
        .filter(|r| r.event() == "stopped")
        .collect::<Vec<_>>();
    assert_eq!(stopped.len(), 1, "pause must await stopped");
    assert_eq!(stopped[0].number("downloaded"), 16_384);
    client.unpause(&leecher).await.unwrap();
    tracker
        .wait(|r| r.iter().filter(|r| r.event() == "started").count() == 3)
        .await;
    let resumed = tracker
        .records()
        .into_iter()
        .filter(|r| r.event() == "started")
        .last()
        .unwrap();
    assert_eq!(
        resumed.number("downloaded"),
        16_384,
        "pause/resume must not reset counters"
    );
    assert_eq!(resumed.query["key"], key);
    client
        .delete(TorrentIdOrHash::Id(leecher.id()), false)
        .await
        .unwrap();
    assert_eq!(
        tracker
            .records()
            .iter()
            .filter(|r| r.event() == "stopped")
            .count(),
        2
    );
    assert_eq!(
        tracker
            .records()
            .iter()
            .filter(|r| r.event() == "completed")
            .count(),
        1
    );
    assert!(
        forbidden.records().is_empty(),
        "private torrent contacted an additional tracker"
    );
    assert!(
        timeout(Duration::from_millis(100), unsolicited.accept())
            .await
            .is_err(),
        "private torrent contacted an initial peer"
    );
    if !udp {
        assert_private_extensions(&seed, &seeder).await;
    }
    seed.stop().await;
    let records = tracker.records();
    let last = records.last().unwrap();
    assert_eq!(
        last.event(),
        "stopped",
        "shutdown must await stopped before network teardown"
    );
    assert_eq!(last.number("uploaded"), 16_384);
    assert_eq!(last.number("downloaded"), 0);
    if !udp {
        assert!(
            records.iter().filter(|r| r.event() != "started").all(|r| r
                .query
                .get("trackerid")
                .map(String::as_str)
                == Some("local id/+?&="))
        );
        assert!(
            records
                .iter()
                .all(|r| r.query["passkey"] == "local-fixture-only")
        );
    }
    client.stop().await;
}
async fn assert_private_extensions(seed: &Arc<Session>, handle: &ManagedTorrentHandle) {
    let mut peer = TcpStream::connect(seed.listen_addr().unwrap())
        .await
        .unwrap();
    let hash = format!("{:?}", handle.info_hash());
    let mut handshake = b"\x13BitTorrent protocol\0\0\0\0\0\x10\0\0".to_vec();
    for i in (0..40).step_by(2) {
        handshake.push(u8::from_str_radix(&hash[i..i + 2], 16).unwrap());
    }
    handshake.extend_from_slice(b"-TC0001-local-fixture");
    peer.write_all(&handshake).await.unwrap();
    timeout(Duration::from_secs(5), async {
        let mut response = [0; 68];
        peer.read_exact(&mut response).await.unwrap();
        loop {
            let size = peer.read_u32().await.unwrap();
            assert!(size < 65536);
            let mut message = vec![0; size as usize];
            peer.read_exact(&mut message).await.unwrap();
            if message.starts_with(&[20, 0]) {
                let text = String::from_utf8_lossy(&message[2..]);
                assert!(text.contains("ut_pexi0e"));
                assert!(text.contains("ut_metadatai0e"));
                assert!(!text.contains("metadata_size"));
                break;
            }
        }
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_http_accounting_lifecycle_and_peer_isolation() {
    transfer_accounting(false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_udp_accounting_lifecycle_and_peer_isolation() {
    transfer_accounting(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_respects_interval_and_minimum_without_repeated_started() {
    for (interval, minimum) in [(3, 1), (1, 3)] {
        let tracker = Tracker::http(interval, minimum).await;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let bytes = fixture(&source, vec![tracker.url.clone()]);
        let client = session(&source, None).await;
        let _ = add(&client, &bytes, &source, None, vec![], vec![]).await;
        tracker.wait(|r| r.len() >= 2).await;
        let records = tracker.records();
        assert_eq!(records[0].event(), "started");
        assert_eq!(records[1].event(), "");
        assert!(records[1].at.duration_since(records[0].at) >= Duration::from_millis(2900));
        client.stop().await;
        assert_eq!(tracker.records().last().unwrap().event(), "stopped");
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_failover_disconnects_old_peers_before_announcing_and_does_not_fail_back() {
    let primary = Tracker::http(1, 1).await;
    let secondary = Tracker::http(1, 1).await;
    let forbidden = Tracker::http(1, 1).await;
    let old_peer = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stale_peer = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fresh_peer = TcpListener::bind("127.0.0.1:0").await.unwrap();
    primary.state.lock().unwrap().peers = vec![
        old_peer.local_addr().unwrap(),
        stale_peer.local_addr().unwrap(),
    ];
    secondary.state.lock().unwrap().peers = vec![fresh_peer.local_addr().unwrap()];
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let bytes = fixture(&source, vec![primary.url.clone(), secondary.url.clone()]);
    let client = Session::new_with_opts(
        source.clone(),
        SessionOptions {
            dht: None,
            disable_local_service_discovery: true,
            peer_limit: Some(1),
            trackers: HashSet::from([forbidden.url.parse().unwrap()]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let handle = add(
        &client,
        &bytes,
        &source,
        None,
        vec![],
        vec![forbidden.url.clone()],
    )
    .await;
    let (mut peer, _) = timeout(Duration::from_secs(5), old_peer.accept())
        .await
        .unwrap()
        .unwrap();
    let mut h = [0; 68];
    peer.read_exact(&mut h).await.unwrap();
    h[48..].fill(77);
    h[20..28].fill(0);
    peer.write_all(&h).await.unwrap();
    peer.write_all(&[0, 0, 0, 2, 5, 0, 0, 0, 0, 1, 2])
        .await
        .unwrap();
    // Drain initial bitfield + unchoke so EOF establishes actual socket closure.
    for _ in 0..2 {
        let n = peer.read_u32().await.unwrap();
        let mut body = vec![0; n as usize];
        peer.read_exact(&mut body).await.unwrap();
    }
    primary.state.lock().unwrap().reject = true;
    let mut byte = [0];
    assert_eq!(
        timeout(Duration::from_secs(8), peer.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let closed = Instant::now();
    secondary
        .wait(|r| r.iter().any(|r| r.event() == "started"))
        .await;
    // The tracker request may race observation of EOF, so the wire ordering is
    // additionally checked by keeping the backup handler gated below in unit tests.
    assert!(secondary.records()[0].at + Duration::from_millis(50) >= closed);
    let (_fresh, _) = timeout(Duration::from_secs(3), fresh_peer.accept())
        .await
        .unwrap()
        .unwrap();
    assert!(
        timeout(Duration::from_millis(200), stale_peer.accept())
            .await
            .is_err(),
        "queued peer from old tracker must never connect"
    );
    primary.state.lock().unwrap().reject = false;
    let primary_count = primary.records().len();
    secondary.wait(|r| r.len() >= 2).await;
    assert_eq!(
        primary.records().len(),
        primary_count,
        "private tracker must not fail back while active tracker works"
    );
    assert!(
        timeout(Duration::from_millis(1200), old_peer.accept())
            .await
            .is_err(),
        "old peers must not reconnect"
    );
    assert!(forbidden.records().is_empty());
    client.pause(&handle).await.unwrap();
    assert_eq!(secondary.records().last().unwrap().event(), "stopped");
    client.stop().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_tiers_retry_primary_before_backup_and_stop_at_success() {
    let primary = Tracker::http(1, 1).await;
    let secondary = Tracker::http(1, 1).await;
    let third = Tracker::http(1, 1).await;
    primary.state.lock().unwrap().reject = true;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fixture(&source, vec![primary.url.clone()]);
    let torrent = lava_torrent::torrent::v1::TorrentBuilder::new(&source, 16384)
        .set_announce_list(vec![
            vec![primary.url.clone()],
            vec![secondary.url.clone()],
            vec![third.url.clone()],
        ])
        .set_num_threads(1)
        .build()
        .unwrap();
    let mut bytes = Vec::new();
    torrent.write_into(&mut bytes).unwrap();
    let client = session(&source, None).await;
    add(&client, &bytes, &source, None, vec![], vec![]).await;
    secondary.wait(|r| r.len() >= 2).await;
    assert!(primary.records().len() >= 2);
    assert!(third.records().is_empty());
    let p = primary.records();
    let b = secondary.records();
    assert!(p[0].at < b[0].at && p[1].at < b[1].at);
    client.stop().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_hash_requires_explicit_tracker_and_ignores_initial_peers() {
    let tracker = Tracker::http(1, 1).await;
    let peer = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let client = session(root.path(), Some(&tracker.url)).await;
    let result = client
        .add_torrent(
            AddTorrent::from_url("magnet:?xt=urn:btih:0123456789012345678901234567890123456789"),
            Some(AddTorrentOptions {
                initial_peers: Some(vec![peer.local_addr().unwrap()]),
                ..Default::default()
            }),
        )
        .await;
    assert!(format!("{}", result.err().unwrap()).contains("explicit tracker"));
    assert!(tracker.records().is_empty());
    assert!(
        timeout(Duration::from_millis(100), peer.accept())
            .await
            .is_err()
    );
    client.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unresponsive_tracker_does_not_block_pause_or_shutdown() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/announce", listener.local_addr().unwrap());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        let mut connections = Vec::new();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            let n = stream.read(&mut buffer).await.unwrap();
            tx.send(String::from_utf8_lossy(&buffer[..n]).into_owned())
                .unwrap();
            connections.push(stream); // Deliberately never return an HTTP response.
        }
    });
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let bytes = fixture(&source, vec![url]);
    let client = session(&source, None).await;
    let handle = add(&client, &bytes, &source, None, vec![], vec![]).await;
    assert!(
        timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap()
            .contains("event=started")
    );
    let before = Instant::now();
    timeout(Duration::from_secs(5), client.pause(&handle))
        .await
        .unwrap()
        .unwrap();
    assert!(before.elapsed() >= Duration::from_secs(2));
    assert!(
        timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap()
            .contains("event=stopped")
    );
    timeout(Duration::from_secs(3), client.stop())
        .await
        .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_hash_is_not_sent_to_enabled_loopback_dht() {
    use lava_torrent::bencode::BencodeElem as B;
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let dht_addr = socket.local_addr().unwrap();
    let bootstrap = dht_addr.to_string();
    let queries = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let saved = queries.clone();
    let server = tokio::spawn(async move {
        let mut buffer = [0; 8192];
        loop {
            let (n, addr) = socket.recv_from(&mut buffer).await.unwrap();
            saved.lock().unwrap().push(buffer[..n].to_vec());
            let Ok(values) = B::from_bytes(&buffer[..n]) else {
                continue;
            };
            let Some(B::Dictionary(message)) = values.first() else {
                continue;
            };
            let Some(transaction) = message.get("t") else {
                continue;
            };
            let mut response =
                b"d1:rd2:id20:local-dht-fixture-015:nodes26:local-dht-fixture-01".to_vec();
            response.extend_from_slice(&[127, 0, 0, 1]);
            response.extend_from_slice(&dht_addr.port().to_be_bytes());
            response.extend_from_slice(b"5:token4:teste1:t");
            transaction.write_into(&mut response).unwrap();
            response.extend_from_slice(b"1:y1:re");
            socket.send_to(&response, addr).await.unwrap();
        }
    });
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let tracker = Tracker::http(1, 1).await;
    let private = fixture(&source, vec![tracker.url.clone()]);
    let client = Session::new_with_opts(
        source.clone(),
        SessionOptions {
            dht: Some(librqbit::DhtSessionConfig {
                bootstrap_addrs: Some(vec![bootstrap]),
                port: Some(0),
                persistence: None,
            }),
            disable_local_service_discovery: true,
            ipv4_only: true,
            listen: Some(ListenerOptions {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let handle = add(&client, &private, &source, None, vec![], vec![]).await;
    tracker.wait(|r| r.len() >= 2).await;
    let unknown = timeout(
        Duration::from_secs(2),
        client.add_torrent(
            AddTorrent::from_url("magnet:?xt=urn:btih:0123456789012345678901234567890123456789"),
            None,
        ),
    )
    .await
    .unwrap();
    assert!(
        unknown.is_err(),
        "unknown magnets must not start DHT discovery"
    );
    // Positive control: the same DHT must observe a public torrent's hash.
    let torrent = lava_torrent::torrent::v1::TorrentBuilder::new(&source, 16_384)
        .set_privacy(false)
        .set_num_threads(2)
        .build()
        .unwrap();
    let mut public = Vec::new();
    torrent.write_into(&mut public).unwrap();
    let public_handle = add(&client, &public, &source, None, vec![], vec![]).await;
    let public_hash = public_handle.info_hash().0;
    timeout(Duration::from_secs(15), async {
        while !queries
            .lock()
            .unwrap()
            .iter()
            .any(|q| q.windows(20).any(|h| h == public_hash))
        {
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "DHT positive control: {error}; stats={:?}; requests={:?}",
            client.get_dht().unwrap().stats(),
            queries
                .lock()
                .unwrap()
                .iter()
                .map(|q| String::from_utf8_lossy(q).into_owned())
                .collect::<Vec<_>>()
        )
    });
    let private_hash = handle.info_hash().0;
    assert!(
        !queries
            .lock()
            .unwrap()
            .iter()
            .any(|q| q.windows(20).any(|h| h == private_hash)),
        "private hash leaked to DHT"
    );
    client.stop().await;
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_cancels_unresolved_magnet_and_waits_for_stopped() {
    let tracker = Tracker::http(1, 1).await;
    let root = tempfile::tempdir().unwrap();
    let client = session(root.path(), None).await;
    let magnet = format!(
        "magnet:?xt=urn:btih:0123456789012345678901234567890123456789&tr={}",
        url::form_urlencoded::byte_serialize(tracker.url.as_bytes()).collect::<String>()
    );
    let resolving = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .add_torrent(AddTorrent::from_url(&magnet), None)
                .await
        })
    };
    tracker.wait(|r| !r.is_empty()).await;
    assert_eq!(
        tracker.records()[0].number("left"),
        1,
        "metadata lookup must not claim to seed"
    );
    timeout(Duration::from_secs(5), client.stop())
        .await
        .unwrap();
    assert_eq!(tracker.records().last().unwrap().event(), "stopped");
    assert!(resolving.await.unwrap().is_err());
    assert!(
        client
            .add_torrent(
                AddTorrent::from_url("0123456789012345678901234567890123456789"),
                None
            )
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_public_tracker_sources_do_not_duplicate_announces() {
    let tracker = Tracker::http(1, 1).await;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let _ = fixture(&source, vec![tracker.url.clone()]);
    let torrent = lava_torrent::torrent::v1::TorrentBuilder::new(&source, 16_384)
        .set_privacy(false)
        .set_announce_list(vec![vec![tracker.url.clone()]])
        .set_num_threads(2)
        .build()
        .unwrap();
    let mut metadata = Vec::new();
    torrent.write_into(&mut metadata).unwrap();
    let client = session(&source, Some(&tracker.url)).await;
    let handle = add(
        &client,
        &metadata,
        &source,
        None,
        vec![],
        vec![tracker.url.clone()],
    )
    .await;
    tracker.wait(|r| r.len() >= 2).await;
    client.pause(&handle).await.unwrap();
    let records = tracker.records();
    assert_eq!(records.iter().filter(|r| r.event() == "started").count(), 1);
    assert_eq!(records.iter().filter(|r| r.event() == "stopped").count(), 1);
    assert!(records.iter().any(|r| r.event().is_empty()));
    client.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extreme_http_interval_recovers_without_panic_or_immediate_retry() {
    let tracker = Tracker::http(u64::MAX, 0).await;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let bytes = fixture(&source, vec![tracker.url.clone()]);
    let client = session(&source, None).await;
    let _ = add(&client, &bytes, &source, None, vec![], vec![]).await;
    tracker.wait(|r| !r.is_empty()).await;
    // The next response is valid. The monitor must recover after its normal
    // error backoff instead of dying or retrying in a tight loop.
    {
        let mut state = tracker.state.lock().unwrap();
        state.interval = 1;
    }
    tracker.wait(|r| r.len() >= 3).await;
    let records = tracker.records();
    assert!(records[1].at.duration_since(records[0].at) >= Duration::from_secs(9));
    assert_eq!(records[1].event(), "started");
    assert_eq!(records[2].event(), "");
    client.stop().await;
    assert_eq!(tracker.records().last().unwrap().event(), "stopped");
}
