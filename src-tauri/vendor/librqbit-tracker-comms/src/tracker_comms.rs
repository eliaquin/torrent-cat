// Modified for TorrentCat: transfer accounting, lifecycle announces and private isolation.
use crate::{UdpTrackerClient, tracker_comms_http as http, tracker_comms_udp as udp};
use anyhow::{Context, bail};
use futures::{
    StreamExt,
    future::BoxFuture,
    stream::{BoxStream, FuturesUnordered},
};
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
pub struct TrackerComms {
    info_hash: Id20,
    peer_id: Id20,
    stats: Box<dyn TorrentStatsProvider>,
    force_tracker_interval: Option<Duration>,
    announce_port: u16,
    reqwest_client: reqwest::Client,
    udp_client: UdpTrackerClient,
    key: u32,
    cancel: CancellationToken,
    tx: tokio::sync::mpsc::Sender<SocketAddr>,
}
impl TrackerComms {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        info_hash: Id20,
        peer_id: Id20,
        trackers: Vec<Url>,
        stats: Box<dyn TorrentStatsProvider>,
        force_interval: Option<Duration>,
        announce_port: u16,
        reqwest_client: reqwest::Client,
        udp_client: UdpTrackerClient,
        tasks: TaskTracker,
    ) -> Option<BoxStream<'static, SocketAddr>> {
        let mut trackers: Vec<_> = trackers
            .into_iter()
            .filter(|u| matches!(u.scheme(), "http" | "https" | "udp"))
            .collect();
        // Fail closed: no private failover until peer isolation is implemented.
        if stats.is_private() {
            trackers.truncate(1);
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
            stats,
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
        let worker = tasks.track_future(async move {
            let mut monitors = FuturesUnordered::new();
            for url in trackers {
                monitors.push(comms.monitor(url));
            }
            while monitors.next().await.is_some() {}
        });
        Some(Box::pin(async_stream::stream! {
            let _guard = guard;
            tokio::spawn(worker);
            while let Some(peer) = rx.recv().await { yield peer; }
        }))
    }
    async fn monitor(&self, url: Url) {
        let mut tracker_id: Option<Vec<u8>> = None;
        let mut started = false;
        let mut attempted = false;
        let mut retrying = false;
        let initially_complete = self.stats.get().is_completed();
        let mut completed = initially_complete;
        let mut next = Instant::now();
        let mut retry = Duration::from_secs(10);
        loop {
            if self.cancel.is_cancelled() {
                break;
            }
            let event = if !started {
                Event::Started
            } else if !completed && self.stats.get().is_completed() {
                Event::Completed
            } else {
                Event::Update
            };
            if Instant::now() < next && (event != Event::Completed || retrying) {
                tokio::select! {
                    _ = self.cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(200).min(next.saturating_duration_since(Instant::now()))) => continue,
                }
            }
            attempted = true;
            let result = tokio::select! {
                _ = self.cancel.cancelled() => break,
                result = timeout(Duration::from_secs(10), self.request(&url, event, tracker_id.as_deref())) => result.unwrap_or_else(|_| Err(anyhow::anyhow!("tracker request timed out"))),
            };
            match result {
                Ok(reply) => {
                    if reply.tracker_id.is_some() {
                        tracker_id = reply.tracker_id;
                    }
                    started = true;
                    retrying = false;
                    if event == Event::Completed {
                        completed = true;
                    }
                    retry = Duration::from_secs(10);
                    next = Instant::now()
                        + self
                            .force_tracker_interval
                            .unwrap_or(reply.interval)
                            .max(reply.interval);
                    for peer in reply.peers {
                        tokio::select! {
                            _ = self.cancel.cancelled() => break,
                            result = self.tx.send(peer) => if result.is_err() { break; },
                        }
                    }
                }
                Err(error) => {
                    // Do not log URLs/passkeys. Rejection is not an invitation to spoof identity.
                    tracing::debug!(%error, "tracker announce failed");
                    retrying = true;
                    next = Instant::now() + retry;
                    retry = (retry * 2).min(Duration::from_secs(600));
                }
            }
        }
        // Finish canceled peer writers before taking the final accounting snapshot.
        let _ = timeout(Duration::from_secs(2), self.stats.wait_for_transfers()).await;
        // A pause can race the final verified piece; do not lose its completion event.
        if started && !completed && self.stats.get().is_completed() {
            let _ = timeout(
                Duration::from_secs(3),
                self.request(&url, Event::Completed, tracker_id.as_deref()),
            )
            .await;
        }
        // Best effort and bounded, but awaited by pause/remove/application shutdown.
        if attempted {
            let _ = timeout(
                Duration::from_secs(3),
                self.request(&url, Event::Stopped, tracker_id.as_deref()),
            )
            .await;
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
            let host = url.host_str().context("tracker has no host")?;
            let port = url.port().context("UDP tracker has no port")?;
            let addr = tokio::net::lookup_host((host, port))
                .await?
                .next()
                .context("tracker has no addresses")?;
            let response = self
                .udp_client
                .announce(
                    addr,
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
        let bytes = response.bytes().await.map_err(|e| e.without_url())?;
        if let Ok((error, _)) = bencode::from_bytes_with_rest::<http::TrackerError>(&bytes) {
            let _ = error.failure_reason; // Do not echo tracker text containing credentials.
            bail!("tracker rejected announce");
        }
        let response = bencode::from_bytes_with_rest::<http::TrackerResponse>(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid tracker response"))?
            .0;
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
