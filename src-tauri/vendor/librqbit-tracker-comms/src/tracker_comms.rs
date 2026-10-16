// Modified for TorrentCat: transfer accounting, lifecycle announces and private isolation.
use crate::{UdpTrackerClient, tracker_comms_http as http, tracker_comms_udp as udp};
use anyhow::{Context, bail};
use futures::{future::BoxFuture, stream::BoxStream};
use librqbit_core::hash_id::Id20;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::time::{Instant, timeout};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use url::Url;

#[derive(Default, Clone, Copy)]
pub enum TrackerCommsStatsState {
    #[default]
    None,
    Initializing,
    Paused,
    Live,
}
#[derive(Default, Clone, Copy)]
pub struct TrackerCommsStats {
    pub uploaded_bytes: u64,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    /// Verified bytes still missing from the ENTIRE torrent, including skipped files.
    pub left_bytes: u64,
    pub torrent_state: TrackerCommsStatsState,
}
impl TrackerCommsStats {
    pub fn get_left_to_download_bytes(&self) -> u64 {
        self.left_bytes
    }
    pub fn is_completed(&self) -> bool {
        self.total_bytes > 0 && self.left_bytes == 0
    }
}
pub trait TorrentStatsProvider: Send + Sync {
    fn get(&self) -> TrackerCommsStats;
    fn is_private(&self) -> bool {
        false
    }
    fn is_metadata_discovery(&self) -> bool {
        false
    }
    fn disconnect_for_tracker_switch(&self) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async { anyhow::bail!("private tracker switching requires peer isolation") })
    }
    fn wait_for_transfers(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}
impl TorrentStatsProvider for () {
    fn get(&self) -> TrackerCommsStats {
        Default::default()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Event {
    Started,
    Completed,
    Stopped,
    Update,
}
struct Reply {
    interval: Duration,
    peers: Vec<SocketAddr>,
    tracker_id: Option<Vec<u8>>,
}
enum PeerEvent {
    Peer(SocketAddr),
    Switch(tokio::sync::oneshot::Sender<anyhow::Result<()>>),
}
#[derive(Default)]
struct AnnounceState {
    tracker_id: Option<Vec<u8>>,
    started: bool,
    attempted: bool,
    completed: bool,
    failures: u32,
    retry_after: Option<Instant>,
}

pub struct TrackerComms {
    info_hash: Id20,
    peer_id: Id20,
    stats: Arc<dyn TorrentStatsProvider>,
    force_tracker_interval: Option<Duration>,
    announce_port: u16,
    reqwest_client: reqwest::Client,
    udp_client: UdpTrackerClient,
    key: u32,
    cancel: CancellationToken,
    tx: tokio::sync::mpsc::Sender<PeerEvent>,
}
impl TrackerComms {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        info_hash: Id20,
        peer_id: Id20,
        trackers: Vec<Vec<Url>>,
        stats: Box<dyn TorrentStatsProvider>,
        force_interval: Option<Duration>,
        announce_port: u16,
        reqwest_client: reqwest::Client,
        udp_client: UdpTrackerClient,
        tasks: TaskTracker,
    ) -> Option<BoxStream<'static, SocketAddr>> {
        use rand::seq::SliceRandom;
        let mut seen = std::collections::HashSet::new();
        let mut trackers: Vec<Vec<Url>> = trackers
            .into_iter()
            .map(|tier| {
                tier.into_iter()
                    .filter(|u| {
                        matches!(u.scheme(), "http" | "https" | "udp") && seen.insert(u.clone())
                    })
                    .collect()
            })
            .filter(|tier: &Vec<_>| !tier.is_empty())
            .collect();
        if !stats.is_metadata_discovery() {
            for tier in &mut trackers {
                tier.shuffle(&mut rand::rng());
            }
        }
        if trackers.is_empty() {
            return None;
        }
        let cancel = CancellationToken::new();
        let guard = cancel.clone().drop_guard();
        let (tx, mut rx) = tokio::sync::mpsc::channel(256);
        let comms = Arc::new(Self {
            info_hash,
            peer_id,
            stats: stats.into(),
            force_tracker_interval: force_interval,
            announce_port,
            reqwest_client,
            udp_client,
            // Stable throughout a process, including pause/resume; never impersonate another client.
            key: u32::from_be_bytes(peer_id.0[16..20].try_into().unwrap()),
            cancel,
            tx,
        });
        // Register before polling the stream, so shutdown cannot miss a queued worker.
        let consumer = comms.stats.clone();
        let worker = tasks.track_future(async move {
            comms.monitor(trackers).await;
        });
        Some(Box::pin(async_stream::stream! {
            let _guard = guard;
            tokio::spawn(worker);
            while let Some(event) = rx.recv().await {
                match event {
                    PeerEvent::Peer(peer) => yield peer,
                    PeerEvent::Switch(reply) => {
                        let result = consumer.disconnect_for_tracker_switch().await;
                        let _ = reply.send(result);
                    }
                }
            }
        }))
    }
    async fn switch_private_peers(&self) -> anyhow::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(PeerEvent::Switch(tx))
            .await
            .context("peer stream closed")?;
        rx.await.context("peer reset canceled")?
    }
    async fn finish_tracker(&self, url: &Url, state: &mut AnnounceState) {
        if state.started && !state.completed && self.stats.get().is_completed() {
            let _ = timeout(
                Duration::from_secs(3),
                self.request(url, Event::Completed, state.tracker_id.as_deref()),
            )
            .await;
        }
        if state.attempted {
            let _ = timeout(
                Duration::from_secs(3),
                self.request(url, Event::Stopped, state.tracker_id.as_deref()),
            )
            .await;
        }
        state.started = false;
        state.attempted = false;
    }
    async fn monitor(&self, mut tiers: Vec<Vec<Url>>) {
        let mut states = std::collections::HashMap::<Url, AnnounceState>::new();
        let mut active: Option<Url> = None;
        let mut peers_published = false;
        let initially_complete = self.stats.get().is_completed();
        let has_alternatives = tiers.iter().map(Vec::len).sum::<usize>() > 1;
        let mut next = Instant::now();
        let mut retry = Duration::from_secs(10);
        let mut retrying = false;
        'monitor: loop {
            if self.cancel.is_cancelled() {
                break;
            }
            let completion_due = active
                .as_ref()
                .and_then(|u| states.get(u))
                .is_some_and(|s| s.started && !s.completed && self.stats.get().is_completed());
            if Instant::now() < next && (!completion_due || retrying) {
                tokio::select! {
                    _ = self.cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(200).min(next.saturating_duration_since(Instant::now()))) => continue,
                }
            }
            // Public: start at the first tier on every announce. Private: remain
            // with the current tracker until it fails, then try alternatives.
            let mut candidates: Vec<Url> = tiers.iter().flatten().cloned().collect();
            if self.stats.is_private()
                && let Some(url) = active.as_ref()
            {
                candidates.retain(|u| u != url);
                candidates.insert(0, url.clone());
            }
            let mut success = false;
            for url in candidates {
                if self.cancel.is_cancelled() {
                    break 'monitor;
                }
                if states
                    .get(&url)
                    .and_then(|s| s.retry_after)
                    .is_some_and(|at| Instant::now() < at)
                {
                    continue;
                }
                if self.stats.is_private() && active.as_ref().is_some_and(|old| old != &url) {
                    if !self.stats.is_metadata_discovery() || peers_published {
                        let result = tokio::select! {
                            _ = self.cancel.cancelled() => break 'monitor,
                            r = self.switch_private_peers() => r,
                        };
                        if result.is_err() {
                            break 'monitor;
                        } // fail closed
                    }
                    if let Some(old) = active.as_ref()
                        && let Some(state) = states.get_mut(old)
                    {
                        tokio::select! {
                            _ = self.cancel.cancelled() => break 'monitor,
                            _ = self.finish_tracker(old, state) => {},
                        }
                    }
                }
                active = Some(url.clone());
                let state = states.entry(url.clone()).or_insert_with(|| AnnounceState {
                    completed: initially_complete,
                    ..Default::default()
                });
                let event = if !state.started {
                    Event::Started
                } else if !state.completed && self.stats.get().is_completed() {
                    Event::Completed
                } else {
                    Event::Update
                };
                state.attempted = true;
                // A silent UDP endpoint must not monopolize a multi-tracker
                // torrent. Single-endpoint sessions retain the full BEP 15 loop.
                let budget = Duration::from_secs(match (url.scheme(), has_alternatives) {
                    ("udp", true) => 45,
                    ("udp", false) => 16000,
                    _ => 10,
                });
                let result = tokio::select! {
                    _ = self.cancel.cancelled() => break 'monitor,
                    r = timeout(budget, self.request(&url, event, state.tracker_id.as_deref())) => r.unwrap_or_else(|_| Err(anyhow::anyhow!("tracker request timed out"))),
                }.and_then(|reply| {
                    let deadline = checked_deadline(reply.interval, self.force_tracker_interval)?;
                    Ok((reply, deadline))
                });
                match result {
                    Ok((reply, deadline)) => {
                        state.failures = 0;
                        state.retry_after = None;
                        if reply.tracker_id.is_some() {
                            state.tracker_id = reply.tracker_id;
                        }
                        state.started = true;
                        if event == Event::Completed {
                            state.completed = true;
                        }
                        for tier in &mut tiers {
                            if let Some(index) = tier.iter().position(|u| u == &url) {
                                let preferred = tier.remove(index);
                                tier.insert(0, preferred);
                                break;
                            }
                        }
                        retrying = false;
                        retry = Duration::from_secs(10);
                        next = deadline;
                        for peer in reply.peers {
                            peers_published = true;
                            tokio::select! {
                                _ = self.cancel.cancelled() => break 'monitor,
                                r = self.tx.send(PeerEvent::Peer(peer)) => if r.is_err() { break 'monitor; },
                            }
                        }
                        success = true;
                        break;
                    }
                    Err(error) => {
                        if url.scheme() == "udp" && has_alternatives {
                            state.failures = state.failures.saturating_add(1).min(7);
                            let cooldown =
                                Duration::from_secs((60u64 << (state.failures - 1)).min(3600));
                            state.retry_after = Some(Instant::now() + cooldown);
                        }
                        tracing::debug!(%error, "tracker announce failed");
                    }
                }
            }
            if !success {
                retrying = true;
                next = Instant::now() + retry;
                retry = (retry * 2).min(Duration::from_secs(600));
            }
        }
        let _ = timeout(Duration::from_secs(2), self.stats.wait_for_transfers()).await;
        // Private previous trackers were already stopped before crossing the barrier.
        // Bound the whole finalization, not each URL multiplied by an arbitrary
        // tracker list. No worker may outlive the session's shutdown allowance.
        if timeout(Duration::from_secs(6), async {
            for (url, mut state) in states {
                self.finish_tracker(&url, &mut state).await;
            }
        })
        .await
        .is_err()
        {
            tracing::warn!("final tracker announces exceeded shutdown budget");
        }
    }
    async fn request(
        &self,
        url: &Url,
        event: Event,
        tracker_id: Option<&[u8]>,
    ) -> anyhow::Result<Reply> {
        let stats = self.stats.get();
        if url.scheme() == "udp" {
            let addresses = resolve_udp_addresses(url).await?;
            let mut url_data = url.path().as_bytes().to_vec();
            if let Some(query) = url.query() {
                url_data.push(b'?');
                url_data.extend_from_slice(query.as_bytes());
            }
            let response = self
                .udp_client
                .announce_to_any(
                    &addresses,
                    udp::AnnounceFields {
                        info_hash: self.info_hash,
                        peer_id: self.peer_id,
                        downloaded: stats.downloaded_bytes,
                        uploaded: stats.uploaded_bytes,
                        left: stats.left_bytes,
                        key: self.key,
                        port: self.announce_port,
                        event: match event {
                            Event::Started => udp::EVENT_STARTED,
                            Event::Completed => udp::EVENT_COMPLETED,
                            Event::Stopped => udp::EVENT_STOPPED,
                            Event::Update => udp::EVENT_NONE,
                        },
                    },
                    &url_data,
                )
                .await
                .map_err(|_| anyhow::anyhow!("UDP tracker request failed"))?;
            return Ok(Reply {
                interval: Duration::from_secs(response.interval.max(5) as u64),
                peers: response.addrs,
                tracker_id: None,
            });
        }
        let request = http::TrackerRequest {
            info_hash: &self.info_hash,
            peer_id: &self.peer_id,
            port: self.announce_port,
            downloaded: stats.downloaded_bytes,
            uploaded: stats.uploaded_bytes,
            left: stats.left_bytes,
            compact: true,
            no_peer_id: false,
            event: match event {
                Event::Started => Some(http::TrackerRequestEvent::Started),
                Event::Completed => Some(http::TrackerRequestEvent::Completed),
                Event::Stopped => Some(http::TrackerRequestEvent::Stopped),
                Event::Update => None,
            },
            ip: None,
            numwant: None,
            key: Some(self.key),
            trackerid: tracker_id,
        };
        let mut url = url.clone();
        let mut query = request.as_querystring();
        if let Some(existing) = url.query() {
            query.push('&');
            query.push_str(existing);
        }
        url.set_query(Some(&query));
        let response = self
            .reqwest_client
            .get(url)
            .send()
            .await
            .map_err(|e| e.without_url())?;
        if !response.status().is_success() {
            bail!("tracker HTTP status {}", response.status());
        }
        let bytes = crate::read_bounded_http_body(response, 1024 * 1024).await?;
        if let Ok(error) = bencode::from_bytes::<http::TrackerError>(&bytes) {
            let _ = error.failure_reason; // Do not echo tracker text containing credentials.
            bail!("tracker rejected announce");
        }
        let response = bencode::from_bytes::<http::TrackerResponse>(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid tracker response"))?;
        anyhow::ensure!(
            response.iter_peers().count() <= http::MAX_PEERS,
            "too many tracker peers"
        );
        anyhow::ensure!(
            response
                .tracker_id
                .as_ref()
                .is_none_or(|id| id.as_ref().len() <= 4096),
            "tracker ID too large"
        );
        Ok(Reply {
            interval: Duration::from_secs(
                response
                    .interval
                    .max(response.min_interval.unwrap_or(0))
                    .max(1),
            ),
            peers: response.iter_peers().collect(),
            tracker_id: response.tracker_id.map(|id| id.as_ref().to_vec()),
        })
    }
}

async fn resolve_udp_addresses(url: &Url) -> anyhow::Result<Vec<SocketAddr>> {
    let port = url.port().context("UDP tracker has no port")?;
    Ok(match url.host().context("UDP tracker has no host")? {
        url::Host::Ipv4(ip) => vec![SocketAddr::new(ip.into(), port)],
        url::Host::Ipv6(ip) => vec![SocketAddr::new(ip.into(), port)],
        url::Host::Domain(host) => {
            let mut seen = std::collections::HashSet::new();
            timeout(
                Duration::from_secs(5),
                tokio::net::lookup_host((host, port)),
            )
            .await
            .context("UDP tracker DNS timeout")??
            .filter(|addr| seen.insert(*addr))
            .take(8)
            .collect()
        }
    })
}

fn checked_deadline(interval: Duration, forced: Option<Duration>) -> anyhow::Result<Instant> {
    Instant::now()
        .checked_add(forced.unwrap_or(interval).max(interval))
        .context("tracker interval cannot be represented")
}

#[cfg(test)]
mod interval_tests {
    use super::*;
    #[tokio::test]
    async fn ipv6_url_literals_do_not_go_through_bracketed_dns_lookup() {
        for (url, expected) in [
            ("udp://[::1]:1234/announce", "[::1]:1234"),
            ("udp://127.0.0.1:1234/announce", "127.0.0.1:1234"),
        ] {
            assert_eq!(
                resolve_udp_addresses(&Url::parse(url).unwrap())
                    .await
                    .unwrap(),
                vec![expected.parse::<SocketAddr>().unwrap()]
            );
        }
    }
    #[test]
    fn extreme_intervals_are_recoverable_and_minimum_is_preserved() {
        assert!(checked_deadline(Duration::from_secs(u64::MAX), None).is_err());
        assert!(checked_deadline(Duration::from_secs(1), Some(Duration::MAX)).is_err());
        let before = Instant::now();
        assert!(
            checked_deadline(Duration::from_secs(60), Some(Duration::from_secs(1))).unwrap()
                >= before + Duration::from_secs(60)
        );
        assert!(checked_deadline(Duration::ZERO, None).is_ok());
    }
}

#[cfg(test)]
mod failover_tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::Notify,
    };

    struct HttpTracker {
        url: Url,
        reject: Arc<AtomicBool>,
        requests: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for HttpTracker {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    impl HttpTracker {
        async fn new() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = Url::parse(&format!(
                "http://{}/announce",
                listener.local_addr().unwrap()
            ))
            .unwrap();
            let reject = Arc::new(AtomicBool::new(false));
            let requests = Arc::new(AtomicUsize::new(0));
            let r = reject.clone();
            let n = requests.clone();
            let task = tokio::spawn(async move {
                loop {
                    let (mut peer, _) = listener.accept().await.unwrap();
                    let mut buf = [0; 8192];
                    let _ = peer.read(&mut buf).await;
                    n.fetch_add(1, Ordering::SeqCst);
                    let status = if r.load(Ordering::SeqCst) {
                        "503 Unavailable"
                    } else {
                        "200 OK"
                    };
                    let body = "d8:intervali1e5:peers0:e";
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = peer.write_all(response.as_bytes()).await;
                }
            });
            Self {
                url,
                reject,
                requests,
                task,
            }
        }
    }
    struct PrivateStats {
        entered: Arc<Notify>,
        release: Arc<Notify>,
        fail: bool,
    }
    impl TorrentStatsProvider for PrivateStats {
        fn get(&self) -> TrackerCommsStats {
            TrackerCommsStats {
                total_bytes: 1,
                left_bytes: 1,
                ..Default::default()
            }
        }
        fn is_private(&self) -> bool {
            true
        }
        fn disconnect_for_tracker_switch(&self) -> BoxFuture<'_, anyhow::Result<()>> {
            Box::pin(async {
                self.entered.notify_one();
                self.release.notified().await;
                anyhow::ensure!(!self.fail, "injected disconnect failure");
                Ok(())
            })
        }
    }
    #[tokio::test(start_paused = true)]
    async fn silent_udp_yields_to_http_tier_and_is_not_retried_each_interval() {
        let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let good = HttpTracker::new().await;
        let cancel = CancellationToken::new();
        let tasks = TaskTracker::new();
        let udp = UdpTrackerClient::new(cancel.clone(), None).await.unwrap();
        let mut stream = TrackerComms::start(
            Id20::default(),
            Id20::default(),
            vec![
                vec![
                    Url::parse(&format!("udp://{}/announce", silent.local_addr().unwrap()))
                        .unwrap(),
                ],
                vec![good.url.clone()],
            ],
            Box::new(()),
            None,
            1234,
            reqwest::Client::new(),
            udp,
            tasks.clone(),
        )
        .unwrap();
        let consumer = tokio::spawn(async move { while stream.next().await.is_some() {} });
        let mut packet = [0; 1500];
        silent.recv_from(&mut packet).await.unwrap();
        tokio::time::advance(Duration::from_secs(15)).await;
        silent.recv_from(&mut packet).await.unwrap();
        tokio::time::advance(Duration::from_secs(30)).await;
        for _ in 0..2000 {
            if good.requests.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            good.requests.load(Ordering::SeqCst) > 0,
            "backup must be reached after the 45-second candidate budget"
        );
        // Drain any packet at the exact timeout boundary, then check cooldown.
        while silent.try_recv_from(&mut packet).is_ok() {}
        let initial = good.requests.load(Ordering::SeqCst);
        for _ in 0..10 {
            for _ in 0..100 {
                tokio::task::yield_now().await;
            }
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        assert!(good.requests.load(Ordering::SeqCst) > initial);
        assert!(
            silent.try_recv_from(&mut packet).is_err(),
            "failed primary must cool down while backup remains useful"
        );
        consumer.abort();
        let _ = consumer.await;
        tasks.close();
        tasks.wait().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn successful_tracker_is_promoted_within_its_tier() {
        let bad = HttpTracker::new().await;
        let good = HttpTracker::new().await;
        bad.reject.store(true, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let tasks = TaskTracker::new();
        let udp = UdpTrackerClient::new(cancel.clone(), None).await.unwrap();
        let mut stream = TrackerComms::start(
            Id20::default(),
            Id20::default(),
            vec![vec![bad.url.clone(), good.url.clone()]],
            Box::new(()),
            None,
            1234,
            reqwest::Client::new(),
            udp,
            tasks.clone(),
        )
        .unwrap();
        let consumer = tokio::spawn(async move { while stream.next().await.is_some() {} });
        timeout(Duration::from_secs(3), async {
            while good.requests.load(Ordering::SeqCst) < 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let initial_bad = bad.requests.load(Ordering::SeqCst);
        assert!(initial_bad <= 1);
        bad.reject.store(false, Ordering::SeqCst);
        timeout(Duration::from_secs(3), async {
            while good.requests.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            bad.requests.load(Ordering::SeqCst),
            initial_bad,
            "successful tracker should stay first within tier"
        );
        consumer.abort();
        let _ = consumer.await;
        tasks.close();
        timeout(Duration::from_secs(4), tasks.wait()).await.unwrap();
        cancel.cancel();
    }

    #[tokio::test]
    async fn backup_waits_for_disconnect_and_disconnect_failure_is_closed() {
        for fail in [false, true] {
            let a = HttpTracker::new().await;
            let b = HttpTracker::new().await;
            a.reject.store(true, Ordering::SeqCst);
            let entered = Arc::new(Notify::new());
            let release = Arc::new(Notify::new());
            let cancel = CancellationToken::new();
            let tasks = TaskTracker::new();
            let udp = UdpTrackerClient::new(cancel.clone(), None).await.unwrap();
            let mut stream = TrackerComms::start(
                Id20::default(),
                Id20::default(),
                vec![vec![a.url.clone()], vec![b.url.clone()]],
                Box::new(PrivateStats {
                    entered: entered.clone(),
                    release: release.clone(),
                    fail,
                }),
                None,
                1234,
                reqwest::Client::new(),
                udp,
                tasks.clone(),
            )
            .unwrap();
            let consumer = tokio::spawn(async move { while stream.next().await.is_some() {} });
            timeout(Duration::from_secs(3), entered.notified())
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(
                b.requests.load(Ordering::SeqCst),
                0,
                "announce crossed an unfinished disconnect barrier"
            );
            release.notify_one();
            if fail {
                timeout(Duration::from_secs(3), consumer)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(b.requests.load(Ordering::SeqCst), 0);
            } else {
                timeout(Duration::from_secs(3), async {
                    while b.requests.load(Ordering::SeqCst) == 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                consumer.abort();
                let _ = consumer.await;
            }
            tasks.close();
            timeout(Duration::from_secs(4), tasks.wait()).await.unwrap();
            cancel.cancel();
        }
    }
}
