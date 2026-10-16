// Modified for TorrentCat; see src-tauri/vendor/README.md.
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    io::Read,
    net::SocketAddr,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::{
    ApiError, CreateTorrentOptions, FileInfos, ManagedTorrent, ManagedTorrentShared,
    api::TorrentIdOrHash,
    api_error::WithStatus,
    bitv_factory::{BitVFactory, NonPersistentBitVFactory},
    create_torrent,
    create_torrent_file::CreateTorrentResult,
    dht_utils::{ReadMetainfoResult, read_metainfo_from_peer_receiver},
    ip_ranges::IpRanges,
    limits::{Limits, LimitsConfig},
    listen::{Accept, ListenerOptions},
    merge_streams::merge_streams,
    peer_connection::PeerConnectionOptions,
    read_buf::ReadBuf,
    session_persistence::{
        SerializedTorrent, SessionPersistenceStore, json::JsonSessionPersistenceStore,
    },
    session_stats::SessionStats,
    spawn_utils::BlockingSpawner,
    storage::{
        BoxStorageFactory, StorageFactoryExt, TorrentStorage, filesystem::FilesystemStorageFactory,
    },
    stream_connect::{
        ConnectionKind, ConnectionOptions, SocksProxyConfig, StreamConnector, StreamConnectorArgs,
    },
    torrent_state::{
        ManagedTorrentHandle, ManagedTorrentLocked, ManagedTorrentOptions, ManagedTorrentState,
        TorrentMetadata, TorrentStateLive, initializing::TorrentStateInitializing,
    },
    type_aliases::{BoxAsyncReadVectored, BoxAsyncWrite, PeerStream},
};
use anyhow::{Context, bail};
use arc_swap::ArcSwapOption;
use bencode::bencode_serialize_to_writer;
use buffers::ByteBufOwned;
use bytes::Bytes;
use clone_to_owned::CloneToOwned;
use dht::{Dht, DhtBuilder, DhtConfig, DhtPersistenceConfig, Id20, PersistentDht, dht_listen_addr};
use futures::{
    FutureExt, Stream, StreamExt, TryFutureExt,
    future::BoxFuture,
    stream::{BoxStream, FuturesUnordered},
};
use http::StatusCode;
use itertools::Itertools;
use librqbit_core::{
    crate_version,
    directories::get_configuration_directory,
    magnet::Magnet,
    peer_id::generate_azereus_style,
    spawn_utils::spawn_with_cancel,
    torrent_metainfo::{TorrentMetaV1Owned, ValidatedTorrentMetaV1Info},
};
use librqbit_lsd::{LocalServiceDiscovery, LocalServiceDiscoveryOptions};
use librqbit_utp::BindDevice;
use parking_lot::RwLock;
use peer_binary_protocol::Handshake;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio_util::{
    sync::{CancellationToken, DropGuard},
    task::TaskTracker,
};
use tracing::{Instrument, debug, debug_span, error, info, trace, warn};
use tracker_comms::{TrackerComms, UdpTrackerClient};

pub const SUPPORTED_SCHEMES: [&str; 3] = ["http:", "https:", "magnet:"];

pub type TorrentId = usize;

struct ParsedTorrentFile {
    meta: TorrentMetaV1Owned,
    torrent_bytes: Bytes,
}

fn torrent_from_bytes(bytes: Bytes) -> anyhow::Result<ParsedTorrentFile> {
    let parsed = librqbit_core::torrent_metainfo::torrent_from_bytes(&bytes)?;
    Ok(ParsedTorrentFile {
        meta: parsed.clone_to_owned(Some(&bytes)),
        torrent_bytes: bytes,
    })
}

#[derive(Default)]
pub struct SessionDatabase {
    torrents: HashMap<TorrentId, ManagedTorrentHandle>,
}

impl SessionDatabase {
    fn add_torrent(&mut self, torrent: ManagedTorrentHandle, id: TorrentId) {
        self.torrents.insert(id, torrent);
    }
}

pub struct Session {
    // Core state and services
    pub(crate) db: RwLock<SessionDatabase>,
    next_id: AtomicUsize,
    pub(crate) bitv_factory: Arc<dyn BitVFactory>,
    spawner: BlockingSpawner,

    // Network
    peer_id: Id20,
    announce_port: Option<u16>,
    listen_addr: Option<SocketAddr>,
    dht: Option<Dht>,
    pub(crate) connector: Arc<StreamConnector>,
    reqwest_client: reqwest::Client,
    udp_tracker_client: UdpTrackerClient,
    disable_trackers: bool,

    // Lifecycle management
    cancellation_token: CancellationToken,
    tracker_tasks: RwLock<HashMap<Id20, TaskTracker>>,
    closing: CancellationToken,
    lifecycle: tokio::sync::Mutex<()>,
    stopping: tokio::sync::Mutex<()>,
    background_tasks: TaskTracker,
    adding: RwLock<HashMap<Id20, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    _cancellation_token_drop_guard: DropGuard,

    // Runtime settings
    output_folder: PathBuf,
    peer_opts: PeerConnectionOptions,
    default_storage_factory: Option<BoxStorageFactory>,
    persistence: Option<Arc<dyn SessionPersistenceStore>>,
    trackers: HashSet<url::Url>,

    lsd: Option<LocalServiceDiscovery>,

    // Limits and throttling
    pub(crate) concurrent_initialize_semaphore: Arc<tokio::sync::Semaphore>,
    pub ratelimits: Limits,
    pub(crate) upload_slots: Arc<tokio::sync::Semaphore>,

    pub blocklist: IpRanges,
    pub allowlist: Option<IpRanges>,

    // Monitoring / tracing / logging
    pub(crate) stats: Arc<SessionStats>,
    root_span: Option<tracing::Span>,

    // Feature flags
    #[cfg(feature = "disable-upload")]
    _disable_upload: bool,
    pub ipv4_only: bool,
    pub peer_limit: Option<usize>,
    client_name_and_version: String,
}

async fn torrent_from_url(
    reqwest_client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<ParsedTorrentFile> {
    let b = tokio::time::timeout(Duration::from_secs(30), async {
        let response = reqwest_client
            .get(url)
            .send()
            .await
            .map_err(|e| e.without_url())
            .context("error downloading torrent metadata")?;
        if !response.status().is_success() {
            bail!("torrent metadata request returned {}", response.status());
        }
        tracker_comms::read_bounded_http_body(response, bencode::MAX_INPUT_BYTES).await
    })
    .await
    .context("torrent metadata download timed out")??;
    torrent_from_bytes(b.into()).context("error decoding torrent")
}

fn compute_only_files_regex<ByteBuf: AsRef<[u8]>>(
    torrent: &ValidatedTorrentMetaV1Info<ByteBuf>,
    filename_re: &str,
) -> anyhow::Result<Vec<usize>> {
    let filename_re = regex::Regex::new(filename_re).context("filename regex is incorrect")?;
    let mut only_files = Vec::new();
    for (idx, fd) in torrent.iter_file_details().enumerate() {
        let full_path = fd.filename.to_pathbuf();
        if filename_re.is_match(full_path.to_str().unwrap()) {
            only_files.push(idx);
        }
    }
    if only_files.is_empty() {
        bail!("none of the filenames match the given regex")
    }
    Ok(only_files)
}

fn compute_only_files(
    info: &ValidatedTorrentMetaV1Info<ByteBufOwned>,
    only_files: Option<Vec<usize>>,
    only_files_regex: Option<String>,
    list_only: bool,
) -> anyhow::Result<Option<Vec<usize>>> {
    match (only_files, only_files_regex) {
        (Some(_), Some(_)) => {
            bail!("only_files and only_files_regex are mutually exclusive");
        }
        (Some(only_files), None) => {
            let total_files = info.iter_file_lengths().count();
            for id in only_files.iter().copied() {
                if id >= total_files {
                    bail!("file id {} is out of range", id);
                }
            }
            Ok(Some(only_files))
        }
        (None, Some(filename_re)) => {
            let only_files = compute_only_files_regex(info, &filename_re)?;
            for (idx, fd) in info.iter_file_details().enumerate() {
                if !only_files.contains(&idx) {
                    continue;
                }
                if !list_only {
                    info!(filename=?fd.filename, "will download");
                }
            }
            Ok(Some(only_files))
        }
        (None, None) => Ok(None),
    }
}

fn merge_two_optional_streams<T>(
    s1: Option<impl Stream<Item = T> + Unpin + Send + 'static>,
    s2: Option<impl Stream<Item = T> + Unpin + Send + 'static>,
) -> Option<BoxStream<'static, T>> {
    match (s1, s2) {
        (Some(s1), None) => Some(Box::pin(s1)),
        (None, Some(s2)) => Some(Box::pin(s2)),
        (Some(s1), Some(s2)) => Some(Box::pin(merge_streams(s1, s2))),
        (None, None) => None,
    }
}

/// Options for adding new torrents to the session.
//
// Serialize/deserialize is for Tauri.
#[derive(Default, Serialize, Deserialize)]
pub struct AddTorrentOptions {
    /// Start in paused state.
    #[serde(default)]
    pub paused: bool,
    /// A regex to only download files matching it.
    pub only_files_regex: Option<String>,
    /// An explicit list of file IDs to download.
    /// To see the file indices, run with "list_only".
    pub only_files: Option<Vec<usize>>,
    /// Allow writing on top of existing files, including when resuming a torrent.
    /// You probably want to set it, however for safety it's not default.
    ///
    /// Even when all the torrent pieces have been written, `overwrite` needs to
    /// be enabled in order to resume/seed the torrent.
    #[serde(default)]
    pub overwrite: bool,
    /// Only list the files in the torrent without starting it.
    #[serde(default)]
    pub list_only: bool,
    /// The output folder for the torrent. If not set, the session's default one will be used.
    pub output_folder: Option<String>,
    /// Sub-folder within session's default output folder. Will error if "output_folder" if also set.
    /// By default, multi-torrent files are downloaded to a sub-folder.
    pub sub_folder: Option<String>,
    /// Peer connection options, timeouts etc. If not set, session's defaults will be used.
    pub peer_opts: Option<PeerConnectionOptions>,

    /// Force a refresh interval for polling trackers.
    pub force_tracker_interval: Option<Duration>,

    #[serde(default)]
    pub disable_trackers: bool,

    #[serde(default)]
    pub ratelimits: LimitsConfig,

    /// Initial peers to start of with.
    pub initial_peers: Option<Vec<SocketAddr>>,

    /// Max concurrent connected peers.
    pub peer_limit: Option<usize>,

    /// This is used to restore the session from serialized state.
    pub preferred_id: Option<usize>,

    #[serde(skip)]
    pub storage_factory: Option<BoxStorageFactory>,

    // Custom trackers
    pub trackers: Option<Vec<String>>,
}

pub struct ListOnlyResponse {
    pub info_hash: Id20,
    pub info: ValidatedTorrentMetaV1Info<ByteBufOwned>,
    pub only_files: Option<Vec<usize>>,
    pub output_folder: PathBuf,
    pub seen_peers: Vec<SocketAddr>,
    pub torrent_bytes: Bytes,
}

#[allow(clippy::large_enum_variant)]
pub enum AddTorrentResponse {
    AlreadyManaged(TorrentId, ManagedTorrentHandle),
    ListOnly(ListOnlyResponse),
    Added(TorrentId, ManagedTorrentHandle),
}

impl AddTorrentResponse {
    pub fn into_handle(self) -> Option<ManagedTorrentHandle> {
        match self {
            Self::AlreadyManaged(_, handle) => Some(handle),
            Self::ListOnly(_) => None,
            Self::Added(_, handle) => Some(handle),
        }
    }
}

pub fn read_local_file_including_stdin(filename: &str) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    if filename == "-" {
        std::io::stdin()
            .take(bencode::MAX_INPUT_BYTES as u64 + 1)
            .read_to_end(&mut buf)
            .context("error reading stdin")?;
    } else {
        std::fs::File::open(filename)
            .context("error opening")?
            .take(bencode::MAX_INPUT_BYTES as u64 + 1)
            .read_to_end(&mut buf)
            .context("error reading")?;
    }
    anyhow::ensure!(
        buf.len() <= bencode::MAX_INPUT_BYTES,
        "torrent metadata exceeds size limit"
    );
    Ok(buf)
}

pub enum AddTorrent<'a> {
    Url(Cow<'a, str>),
    TorrentFileBytes(Bytes),
}

impl<'a> AddTorrent<'a> {
    // Don't call this from HTTP API.
    #[inline(never)]
    pub fn from_cli_argument(path: &'a str) -> anyhow::Result<Self> {
        if SUPPORTED_SCHEMES.iter().any(|s| path.starts_with(s)) {
            return Ok(Self::Url(Cow::Borrowed(path)));
        }
        if path.len() == 40 && !Path::new(path).exists() && Magnet::parse(path).is_ok() {
            return Ok(Self::Url(Cow::Borrowed(path)));
        }
        Self::from_local_filename(path)
    }

    pub fn from_url(url: impl Into<Cow<'a, str>>) -> Self {
        Self::Url(url.into())
    }

    pub fn from_bytes(bytes: impl Into<Bytes>) -> Self {
        Self::TorrentFileBytes(bytes.into())
    }

    // Don't call this from HTTP API.
    #[inline(never)]
    pub fn from_local_filename(filename: &str) -> anyhow::Result<Self> {
        let file = read_local_file_including_stdin(filename)
            .with_context(|| format!("error reading local file {filename:?}"))?;
        Ok(Self::TorrentFileBytes(file.into()))
    }

    pub fn into_bytes(self) -> Bytes {
        match self {
            Self::Url(s) => s.into_owned().into_bytes().into(),
            Self::TorrentFileBytes(b) => b,
        }
    }
}

pub enum SessionPersistenceConfig {
    /// The filename for persistence. By default uses an OS-specific folder.
    Json { folder: Option<PathBuf> },
    #[cfg(feature = "postgres")]
    Postgres { connection_string: String },
}

impl SessionPersistenceConfig {
    pub fn default_json_persistence_folder() -> anyhow::Result<PathBuf> {
        let dir = get_configuration_directory("session")?;
        Ok(dir.data_dir().to_owned())
    }
}

/// Configuration for the DHT subsystem.
/// Set to `None` in `SessionOptions::dht` to disable DHT entirely.
pub struct DhtSessionConfig {
    /// Bootstrap nodes (host:port or ip:port). Uses built-in defaults if None.
    pub bootstrap_addrs: Option<Vec<String>>,
    /// The DHT listen port. Priority: this explicit port -> persisted port
    /// (when persistence is enabled) -> random. The bind IP is derived from
    /// `SessionOptions::ipv4_only` (`0.0.0.0` if true, `[::]` otherwise).
    /// Use `SessionOptions::bind_device_name` to scope the bind to a specific
    /// network interface.
    pub port: Option<u16>,
    /// Persistence behavior. If None, persistence is disabled.
    pub persistence: Option<DhtPersistenceConfig>,
}

impl Default for DhtSessionConfig {
    fn default() -> Self {
        Self {
            bootstrap_addrs: None,
            port: None,
            persistence: Some(DhtPersistenceConfig::default()),
        }
    }
}

pub struct SessionOptions {
    /// DHT configuration. Set to None to disable DHT entirely.
    /// Defaults to DHT enabled with persistence.
    pub dht: Option<DhtSessionConfig>,

    /// What network device to bind to for DHT, BT-UDP, BT-TCP, trackers and LSD.
    /// On OSX will use IP(V6)_BOUND_IF, on Linux will use SO_BINDTODEVICE.
    pub bind_device_name: Option<String>,

    /// Disable tracker communication
    pub disable_trackers: bool,

    /// Persist piece checkpoints. TorrentCat still verifies all disk pieces on restart.
    pub fastresume: bool,

    /// Turn on to dump session contents into a file periodically, so that on next start
    /// all remembered torrents will continue where they left off.
    pub persistence: Option<SessionPersistenceConfig>,

    /// The peer ID to use. If not specified, a random one will be generated.
    pub peer_id: Option<Id20>,

    /// Options for listening on TCP and/or uTP for incoming connections.
    pub listen: Option<ListenerOptions>,
    /// Options for connecting to peers (for outgiong connections).
    pub connect: Option<ConnectionOptions>,

    pub default_storage_factory: Option<BoxStorageFactory>,

    pub cancellation_token: Option<CancellationToken>,

    /// how many concurrent torrent initializations can happen
    pub concurrent_init_limit: Option<usize>,

    /// How many blocking threads does the tokio runtime have.
    /// Will limit blocking work to that number to avoid starving the runtime.
    pub runtime_worker_threads: Option<usize>,

    /// the root span to use. If not set will be None.
    pub root_span: Option<tracing::Span>,

    pub ratelimits: LimitsConfig,

    pub blocklist_url: Option<String>,
    pub allowlist_url: Option<String>,

    // The list of tracker URLs to always use for each torrent.
    pub trackers: HashSet<url::Url>,

    /// Default peer limit per torrent.
    pub peer_limit: Option<usize>,

    #[cfg(feature = "disable-upload")]
    pub disable_upload: bool,

    /// Disable LSD multicast
    pub disable_local_service_discovery: bool,

    /// Force IPv4 only.
    pub ipv4_only: bool,

    /// Override the client name and version used in User-Agent headers and
    /// peer extended handshakes. Defaults to "rqbit X.Y.Z".
    pub client_name_and_version: Option<String>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            dht: Some(DhtSessionConfig::default()),
            bind_device_name: None,
            disable_trackers: false,
            fastresume: false,
            persistence: None,
            peer_id: None,
            listen: None,
            connect: None,
            default_storage_factory: None,
            cancellation_token: None,
            concurrent_init_limit: None,
            runtime_worker_threads: None,
            root_span: None,
            ratelimits: LimitsConfig::default(),
            blocklist_url: None,
            allowlist_url: None,
            trackers: HashSet::new(),
            peer_limit: None,
            #[cfg(feature = "disable-upload")]
            disable_upload: false,
            disable_local_service_discovery: false,
            ipv4_only: false,
            client_name_and_version: None,
        }
    }
}

fn torrent_file_from_info_bytes(info_bytes: &[u8], trackers: &[url::Url]) -> anyhow::Result<Bytes> {
    #[derive(Serialize)]
    struct Tmp<'a> {
        announce: &'a str,
        #[serde(rename = "announce-list")]
        announce_list: &'a [&'a [url::Url]],
        info: bencode::raw_value::RawValue<&'a [u8]>,
    }

    let mut w = Vec::new();
    let v = Tmp {
        info: bencode::raw_value::RawValue(info_bytes),
        announce: trackers.first().map(|s| s.as_str()).unwrap_or(""),
        announce_list: &[trackers],
    };
    bencode_serialize_to_writer(&v, &mut w)?;
    Ok(w.into())
}

pub(crate) struct CheckedIncomingConnection {
    pub kind: ConnectionKind,
    pub addr: SocketAddr,
    pub reader: BoxAsyncReadVectored,
    pub writer: BoxAsyncWrite,
    pub read_buf: ReadBuf,
    pub handshake: Handshake,
}

struct InternalAddResult {
    info_hash: Id20,
    metadata: Option<TorrentMetadata>,
    trackers: Vec<url::Url>,
    name: Option<String>,
}

impl Session {
    /// Create a new session with default options.
    /// The passed in folder will be used as a default unless overridden per torrent.
    /// It will run a DHT server/client, a TCP listener and .
    #[inline(never)]
    pub fn new(default_output_folder: PathBuf) -> BoxFuture<'static, anyhow::Result<Arc<Self>>> {
        Self::new_with_opts(default_output_folder, SessionOptions::default())
    }

    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    pub fn client_name_and_version(&self) -> &str {
        &self.client_name_and_version
    }

    /// Create a new session with options.
    #[inline(never)]
    pub fn new_with_opts(
        default_output_folder: PathBuf,
        mut opts: SessionOptions,
    ) -> BoxFuture<'static, anyhow::Result<Arc<Self>>> {
        async move {
            let peer_id = opts
                .peer_id
                .unwrap_or_else(|| generate_azereus_style(*b"rQ", crate_version!()));
            let token = opts.cancellation_token.take().unwrap_or_default();

            #[cfg(feature = "disable-upload")]
            if opts.disable_upload {
                warn!("uploading disabled");
            }

            let bind_device = match opts.bind_device_name.as_ref() {
                Some(name) => Some(
                    BindDevice::new_from_name(name)
                        .with_context(|| format!("error creating bind device {name}"))?,
                ),
                None => None,
            };

            let listen_result = if let Some(listen_opts) = opts.listen.take() {
                Some(
                    listen_opts
                        .start(
                            opts.root_span.as_ref().and_then(|s| s.id()),
                            token.child_token(),
                            bind_device.as_ref(),
                        )
                        .await
                        .context("error starting listeners")?,
                )
            } else {
                None
            };

            let dht = if let Some(dht_config) = opts.dht.take() {
                let dht = if let Some(persistence_config) = dht_config.persistence {
                    PersistentDht::create(
                        persistence_config,
                        dht_config.port,
                        opts.ipv4_only,
                        dht_config.bootstrap_addrs,
                        Some(token.clone()),
                        bind_device.as_ref(),
                    )
                    .await
                    .context("error initializing persistent DHT")?
                } else {
                    let listen_addr = dht_listen_addr(dht_config.port, None, opts.ipv4_only);
                    DhtBuilder::with_config(DhtConfig {
                        bootstrap_addrs: dht_config.bootstrap_addrs,
                        cancellation_token: Some(token.child_token()),
                        bind_device: bind_device.as_ref(),
                        listen_addr: Some(listen_addr),
                        ..Default::default()
                    })
                    .await
                    .context("error initializing DHT")?
                };

                Some(dht)
            } else {
                None
            };
            let peer_opts = opts
                .connect
                .as_ref()
                .and_then(|p| p.peer_opts)
                .unwrap_or_default();

            async fn persistence_factory(
                opts: &SessionOptions,
                spawner: BlockingSpawner,
            ) -> anyhow::Result<(
                Option<Arc<dyn SessionPersistenceStore>>,
                Arc<dyn BitVFactory>,
            )> {
                macro_rules! make_result {
                    ($store:expr) => {
                        if opts.fastresume {
                            Ok((Some($store.clone()), $store))
                        } else {
                            Ok((Some($store), Arc::new(NonPersistentBitVFactory {})))
                        }
                    };
                }

                match &opts.persistence {
                    Some(SessionPersistenceConfig::Json { folder }) => {
                        let folder = match folder.as_ref() {
                            Some(f) => f.clone(),
                            None => SessionPersistenceConfig::default_json_persistence_folder()?,
                        };

                        let s = Arc::new(
                            JsonSessionPersistenceStore::new(folder, spawner)
                                .await
                                .context("error initializing JsonSessionPersistenceStore")?,
                        );

                        make_result!(s)
                    }
                    #[cfg(feature = "postgres")]
                    Some(SessionPersistenceConfig::Postgres { connection_string }) => {
                        use crate::session_persistence::postgres::PostgresSessionStorage;
                        let p = Arc::new(PostgresSessionStorage::new(connection_string).await?);
                        make_result!(p)
                    }
                    None => Ok((None, Arc::new(NonPersistentBitVFactory {}))),
                }
            }

            const DEFAULT_BLOCKING_THREADS_IF_NOT_SET: usize = 8;
            let spawner = BlockingSpawner::new(
                opts.runtime_worker_threads
                    .unwrap_or(DEFAULT_BLOCKING_THREADS_IF_NOT_SET),
            );

            let (persistence, bitv_factory) = persistence_factory(&opts, spawner.clone())
                .await
                .context("error initializing session persistence store")?;

            let proxy_url = opts.connect.as_ref().and_then(|s| s.proxy_url.as_ref());
            let proxy_config = match proxy_url {
                Some(pu) => Some(
                    SocksProxyConfig::parse(pu)
                        .with_context(|| format!("error parsing proxy url {pu}"))?,
                ),
                None => None,
            };

            let client_name_and_version = opts
                .client_name_and_version
                .unwrap_or_else(|| crate::client_name_and_version().to_owned());

            let reqwest_client = {
                let builder = if let Some(proxy_url) = proxy_url {
                    let proxy = reqwest::Proxy::all(proxy_url)
                        .context("error creating socks5 proxy for HTTP")?;
                    reqwest::Client::builder().proxy(proxy)
                } else {
                    #[allow(unused_mut)]
                    let mut b = reqwest::Client::builder();
                    #[cfg(not(windows))]
                    if let Some(bd) = opts.bind_device_name.as_ref() {
                        b = b.interface(bd);
                    }
                    b
                };

                builder
                    .user_agent(&client_name_and_version)
                    .build()
                    .context("error building HTTP(S) client")?
            };

            let stream_connector = Arc::new(
                StreamConnector::new(StreamConnectorArgs {
                    enable_tcp: opts.connect.as_ref().map(|c| c.enable_tcp).unwrap_or(true),
                    socks_proxy_config: proxy_config,
                    utp_socket: listen_result.as_ref().and_then(|l| l.utp_socket.clone()),
                    bind_device: bind_device.clone(),
                    ipv4_only: opts.ipv4_only,
                })
                .await
                .context("error creating stream connector")?,
            );

            let blocklist = if let Some(blocklist_url) = opts.blocklist_url {
                info!(url = blocklist_url, "loading p2p blocklist");
                let bl = IpRanges::load_from_url(&blocklist_url)
                    .await
                    .with_context(|| format!("error reading blocklist from {blocklist_url}"))?;
                info!(len = bl.len(), "loaded blocklist");
                bl
            } else {
                IpRanges::default()
            };

            let allowlist = if let Some(allowlist_url) = opts.allowlist_url {
                info!(url = allowlist_url, "loading p2p allowlist");
                let al = IpRanges::load_from_url(&allowlist_url)
                    .await
                    .with_context(|| format!("error reading allowlist from {allowlist_url}"))?;
                info!(len = al.len(), "loaded allowlist");
                Some(al)
            } else {
                None
            };

            let udp_tracker_client = UdpTrackerClient::new(token.clone(), bind_device.as_ref())
                .await
                .context("error creating UDP tracker client")?;

            let lsd = {
                if opts.disable_local_service_discovery {
                    None
                } else {
                    LocalServiceDiscovery::new(LocalServiceDiscoveryOptions {
                        cancel_token: token.clone(),
                        bind_device: bind_device.as_ref(),
                        ..Default::default()
                    })
                    .await
                    .inspect_err(|e| warn!("error starting local service discovery: {e:#}"))
                    .ok()
                }
            };

            let session = Arc::new(Self {
                persistence,
                bitv_factory,
                peer_id,
                dht,
                peer_opts,
                spawner: spawner.clone(),
                output_folder: default_output_folder,
                next_id: AtomicUsize::new(0),
                db: RwLock::new(Default::default()),
                _cancellation_token_drop_guard: token.clone().drop_guard(),
                cancellation_token: token,
                tracker_tasks: RwLock::new(HashMap::new()),
                closing: CancellationToken::new(),
                lifecycle: tokio::sync::Mutex::new(()),
                stopping: tokio::sync::Mutex::new(()),
                background_tasks: TaskTracker::new(),
                adding: Default::default(),
                announce_port: listen_result.as_ref().and_then(|l| l.announce_port),
                listen_addr: listen_result.as_ref().map(|l| l.addr),
                default_storage_factory: opts.default_storage_factory,
                reqwest_client,
                connector: stream_connector,
                root_span: opts.root_span,
                stats: Arc::new(SessionStats::new()),
                concurrent_initialize_semaphore: Arc::new(tokio::sync::Semaphore::new(
                    opts.concurrent_init_limit.unwrap_or(3),
                )),
                udp_tracker_client,
                ratelimits: Limits::new(opts.ratelimits),
                upload_slots: Arc::new(tokio::sync::Semaphore::new(
                    crate::upload_queue::SESSION_REQUESTS,
                )),
                ipv4_only: opts.ipv4_only,
                trackers: opts.trackers,
                disable_trackers: opts.disable_trackers,
                peer_limit: opts.peer_limit,
                client_name_and_version,

                #[cfg(feature = "disable-upload")]
                _disable_upload: opts.disable_upload,
                blocklist,
                allowlist,
                lsd,
            });

            if let Some(mut listen) = listen_result {
                if let Some(tcp) = listen.tcp_socket.take() {
                    let max_pending_incoming_handshake_checks =
                        listen.max_pending_incoming_handshake_checks;
                    session.spawn(
                        debug_span!(parent: session.rs(), "tcp_listen", addr = ?listen.addr),
                        "tcp_listen",
                        {
                            let this = session.clone();
                            async move {
                                this.task_listener(tcp, max_pending_incoming_handshake_checks)
                                    .await
                            }
                        },
                    );
                }
                if let Some(utp) = listen.utp_socket.take() {
                    let max_pending_incoming_handshake_checks =
                        listen.max_pending_incoming_handshake_checks;
                    session.spawn(
                        debug_span!(parent: session.rs(), "utp_listen", addr = ?listen.addr),
                        "utp_listen",
                        {
                            let this = session.clone();
                            async move {
                                this.task_listener(utp, max_pending_incoming_handshake_checks)
                                    .await
                            }
                        },
                    );
                }
                if listen.enable_upnp_port_forwarding
                    && let Some(announce_port) = listen.announce_port
                {
                    info!(port = announce_port, "starting UPnP port forwarder");
                    let bind_device = bind_device.clone();
                    session.spawn(
                        debug_span!(parent: session.rs(), "upnp_forward", port = announce_port),
                        "upnp_forward",
                        Self::task_upnp_port_forwarder(announce_port, bind_device),
                    );
                }
            }

            if let Some(persistence) = session.persistence.as_ref() {
                info!("will use {persistence:?} for session persistence");

                let mut ps = persistence.stream_all().await?;
                let mut added_all = false;
                let mut futs = FuturesUnordered::new();

                while !added_all || !futs.is_empty() {
                    // NOTE: this closure exists purely to workaround rustfmt screwing up when inlining it.
                    let add_torrent_span = |info_hash: &Id20| -> tracing::Span {
                        debug_span!(parent: session.rs(), "add_torrent", info_hash=?info_hash)
                    };
                    tokio::select! {
                        Some(res) = futs.next(), if !futs.is_empty() => {
                            if let Err(e) = res {
                                error!("error adding torrent to session: {e:#}");
                            }
                        }
                        st = ps.next(), if !added_all => {
                            match st {
                                Some(st) => {
                                    let (id, st) = st?;
                                    let span = add_torrent_span(st.info_hash());
                                    let session = session.clone();
                                    let fut = async move {
                                        let restore = async {
                                            if st.pending_delete == Some(false) {
                                                session.persistence.as_ref().context("missing persistence")?.delete(id).await?;
                                                return Ok::<_, anyhow::Error>(());
                                            }
                                            let (add_torrent, mut opts) = st.clone().into_add_torrent()?;
                                            opts.preferred_id = Some(id);
                                            if st.pending_delete.is_some() { opts.paused = true; }
                                            session.add_torrent(add_torrent, Some(opts)).await?;
                                            if let Some(delete_files) = st.pending_delete {
                                                session.delete(id.into(), delete_files).await?;
                                            }
                                            Ok::<_, anyhow::Error>(())
                                        }.await;
                                        match restore {
                                            Ok(()) => Ok::<_, anyhow::Error>(()),
                                            Err(error) => {
                                                if session.get(id.into()).is_none() {
                                                    session.register_recovery(id, &st, error)?;
                                                }
                                                Ok(())
                                            }
                                        }
                                    }.instrument(span);
                                    futs.push(fut);
                                },
                                None => added_all = true
                            };
                        }
                    };
                }
            }

            session.start_speed_estimator_updater();

            Ok(session)
        }
        .boxed()
    }

    async fn check_incoming_connection(
        self: Arc<Self>,
        addr: SocketAddr,
        kind: ConnectionKind,
        mut reader: BoxAsyncReadVectored,
        writer: BoxAsyncWrite,
    ) -> anyhow::Result<(Arc<TorrentStateLive>, CheckedIncomingConnection)> {
        let rwtimeout = self
            .peer_opts
            .read_write_timeout
            .unwrap_or_else(|| Duration::from_secs(10));

        let incoming_ip = addr.ip();
        if self.blocklist.has(incoming_ip) {
            self.stats
                .counters
                .blocked_incoming
                .fetch_add(1, Ordering::Relaxed);
            bail!("Incoming ip {incoming_ip} is in blocklist");
        }
        if self.allowlist.as_ref().is_some_and(|l| !l.has(incoming_ip)) {
            self.stats
                .counters
                .blocked_incoming
                .fetch_add(1, Ordering::Relaxed);
            bail!("Incoming ip {incoming_ip} is not in allowlist");
        }

        let mut read_buf = ReadBuf::new();
        let h = read_buf
            .read_handshake(&mut reader, rwtimeout)
            .await
            .context("error reading handshake")?;
        trace!("received handshake from {addr}: {:?}", h);

        if h.peer_id == self.peer_id {
            bail!("seems like we are connecting to ourselves, ignoring");
        }

        let (id, torrent) = self
            .db
            .read()
            .torrents
            .iter()
            .find(|(_, t)| t.info_hash() == h.info_hash)
            .map(|(id, t)| (*id, t.clone()))
            .with_context(|| format!("didn't find a matching torrent {:?}", h.info_hash))?;

        let live = torrent
            .live_wait_initializing(Duration::from_secs(5))
            .await
            .with_context(|| format!("torrent {id} is not live, ignoring connection"))?;

        Ok((
            live,
            CheckedIncomingConnection {
                addr,
                reader,
                writer,
                kind,
                handshake: h,
                read_buf,
            },
        ))
    }

    async fn task_listener<A: Accept>(
        self: Arc<Self>,
        l: A,
        max_pending_incoming_handshake_checks: usize,
    ) -> anyhow::Result<()> {
        let mut futs = FuturesUnordered::new();
        let session = Arc::downgrade(&self);
        drop(self);

        loop {
            tokio::select! {
                r = l.accept(), if futs.len() < max_pending_incoming_handshake_checks => {
                    match r {
                        Ok((addr, (read, write))) => {
                            trace!("accepted connection from {addr}");
                            let session = session.upgrade().context("session is dead")?;
                            let span = debug_span!(parent: session.rs(), "incoming", addr=%addr);
                            futs.push(
                                session.check_incoming_connection(addr, A::KIND, Box::new(read), Box::new(write))
                                    .map_err(|e| {
                                        debug!("error checking incoming connection: {e:#}");
                                        e
                                    })
                                    .instrument(span)
                            );
                        }
                        Err(e) => {
                            warn!("error accepting: {e:#}");
                            // Whatever is the reason, ensure we are not stuck trying to
                            // accept indefinitely.
                            tokio::time::sleep(Duration::from_secs(10)).await;
                            continue
                        }
                    }
                },
                Some(Ok((live, checked))) = futs.next(), if !futs.is_empty() => {
                    let (addr, kind) = (checked.addr, checked.kind);
                    if let Err(e) = live.add_incoming_peer(checked) {
                        warn!(?addr, ?kind, "error handing over incoming connection: {e:#}");
                    }
                },
            }
        }
    }

    async fn task_upnp_port_forwarder(
        port: u16,
        bind_device: Option<BindDevice>,
    ) -> anyhow::Result<()> {
        let pf = librqbit_upnp::UpnpPortForwarder::new(vec![port], None, bind_device)?;
        pf.run_forever().await
    }

    pub fn get_dht(&self) -> Option<&Dht> {
        self.dht.as_ref()
    }

    fn merge_peer_opts(&self, other: Option<PeerConnectionOptions>) -> PeerConnectionOptions {
        let other = match other {
            Some(o) => o,
            None => self.peer_opts,
        };
        PeerConnectionOptions {
            connect_timeout: other.connect_timeout.or(self.peer_opts.connect_timeout),
            read_write_timeout: other
                .read_write_timeout
                .or(self.peer_opts.read_write_timeout),
            keep_alive_interval: other
                .keep_alive_interval
                .or(self.peer_opts.keep_alive_interval),
        }
    }

    /// Spawn a task in the context of the session.
    #[track_caller]
    pub fn spawn(
        &self,
        span: tracing::Span,
        name: impl Into<Cow<'static, str>>,
        fut: impl std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    ) {
        let task = self.background_tasks.token();
        spawn_with_cancel(span, name, self.cancellation_token.clone(), async move {
            let _task = task;
            fut.await
        });
    }

    pub(crate) fn rs(&self) -> Option<tracing::Id> {
        self.root_span.as_ref().and_then(|s| s.id())
    }

    /// Stop the session and all managed tasks.
    pub async fn stop(&self) {
        let _stopping = self.stopping.lock().await;
        self.closing.cancel();
        let torrents = {
            let _lifecycle = self.lifecycle.lock().await;
            self.db
                .read()
                .torrents
                .values()
                .cloned()
                .collect::<Vec<_>>()
        };
        for torrent in &torrents {
            let _operation = torrent.shared.lifecycle.lock().await;
            if let Err(e) = torrent.pause() {
                debug!("error pausing torrent: {e:#}");
            }
        }
        let tasks: Vec<_> = self.tracker_tasks.read().values().cloned().collect();
        for task in &tasks {
            task.close();
        }
        if tokio::time::timeout(
            Duration::from_secs(9),
            futures::future::join_all(tasks.iter().map(|t| t.wait())),
        )
        .await
        .is_err()
        {
            warn!("tracker shutdown timed out; final announces could not be confirmed");
        }
        self.cancellation_token.cancel();
        self.udp_tracker_client.stop().await;
        self.background_tasks.close();
        self.background_tasks.wait().await;
        for torrent in torrents {
            torrent.shared.transfer_tasks.close();
            torrent.shared.transfer_tasks.wait().await;
        }
        futures::future::join_all(tasks.iter().map(|task| task.wait())).await;
    }

    /// Run a callback given the currently managed torrents.
    pub fn with_torrents<R>(
        &self,
        callback: impl Fn(&mut dyn Iterator<Item = (TorrentId, &ManagedTorrentHandle)>) -> R,
    ) -> R {
        callback(&mut self.db.read().torrents.iter().map(|(id, t)| (*id, t)))
    }

    /// Add a torrent to the session.
    #[inline(never)]
    pub fn add_torrent<'a>(
        self: &'a Arc<Self>,
        add: AddTorrent<'a>,
        opts: Option<AddTorrentOptions>,
    ) -> BoxFuture<'a, anyhow::Result<AddTorrentResponse>> {
        async move {
            if self.closing.is_cancelled() {
                bail!("session is stopping");
            }
            let mut opts = opts.unwrap_or_default();
            let add_res = match add {
                AddTorrent::Url(magnet) if magnet.starts_with("magnet:") || magnet.len() == 40 => {
                    let magnet = Magnet::parse(&magnet)
                        .context("provided path is not a valid magnet URL")?;
                    let info_hash = magnet
                        .as_id20()
                        .context("magnet link didn't contain a BTv1 infohash")?;
                    if let Some(so) = magnet.get_select_only() {
                        // Only overwrite opts.only_files if user didn't specify
                        if opts.only_files.is_none() {
                            opts.only_files = Some(so);
                        }
                    }

                    InternalAddResult {
                        info_hash,
                        trackers: magnet
                            .trackers
                            .into_iter()
                            .filter_map(|t| url::Url::parse(&t).ok())
                            .collect(),
                        metadata: None,
                        name: magnet.name,
                    }
                }
                other => {
                    let torrent = match other {
                        AddTorrent::Url(url)
                            if url.starts_with("http://") || url.starts_with("https://") =>
                        {
                            torrent_from_url(&self.reqwest_client, &url).await?
                        }
                        AddTorrent::Url(url) => {
                            bail!(
                                "unsupported URL {:?}. Supporting magnet:, http:, and https",
                                url
                            )
                        }
                        AddTorrent::TorrentFileBytes(bytes) => {
                            torrent_from_bytes(bytes).context("error decoding torrent")?
                        }
                    };

                    let mut trackers = tracker_tiers_from_bytes(&torrent.torrent_bytes, &[])?
                        .into_iter()
                        .flatten()
                        .map(|url| url.to_string())
                        .collect::<Vec<_>>();
                    if !torrent.meta.info.data.private
                        && let Some(custom_trackers) = opts.trackers.clone()
                    {
                        trackers.extend(custom_trackers);
                    }

                    InternalAddResult {
                        info_hash: torrent.meta.info_hash,
                        metadata: Some(TorrentMetadata::new(
                            torrent.meta.info.data.validate()?,
                            torrent.torrent_bytes,
                            torrent.meta.info.raw_bytes.0,
                        )?),
                        trackers: trackers
                            .iter()
                            .filter_map(|t| url::Url::parse(t).ok())
                            .collect(),
                        name: None,
                    }
                }
            };

            self.add_torrent_internal(add_res, opts).await
        }
        .instrument(debug_span!(parent: self.rs(), "add_torrent"))
        .boxed()
    }

    fn get_default_subfolder_for_torrent(
        &self,
        info: &ValidatedTorrentMetaV1Info<ByteBufOwned>,
        magnet_name: Option<&str>,
    ) -> anyhow::Result<Option<PathBuf>> {
        let files = info
            .iter_file_details()
            .map(|fd| Ok((fd.filename.to_pathbuf(), fd.len)))
            .collect::<anyhow::Result<Vec<(PathBuf, u64)>>>()?;
        if files.len() < 2 {
            return Ok(None);
        }

        fn check_valid(pb: &Path) -> anyhow::Result<()> {
            if pb.components().any(|x| !matches!(x, Component::Normal(_))) {
                bail!("path traversal in torrent name detected")
            }
            Ok(())
        }

        if let Some(name) = info.name()
            && !name.is_empty()
        {
            let pb = PathBuf::from(name.as_ref());
            check_valid(&pb)?;
            return Ok(Some(pb));
        };
        if let Some(name) = magnet_name {
            let pb = PathBuf::from(name);
            check_valid(&pb)?;
            return Ok(Some(pb));
        }
        // Let the subfolder name be the longest filename
        let longest = files
            .iter()
            .max_by_key(|(_, l)| l)
            .unwrap()
            .0
            .file_stem()
            .context("can't determine longest filename")?;
        Ok::<_, anyhow::Error>(Some(PathBuf::from(longest)))
    }

    async fn add_torrent_internal(
        self: &Arc<Self>,
        add_res: InternalAddResult,
        mut opts: AddTorrentOptions,
    ) -> anyhow::Result<AddTorrentResponse> {
        let InternalAddResult {
            info_hash,
            mut metadata,
            trackers,
            name,
        } = add_res;

        let add_lock = {
            let mut locks = self.adding.write();
            locks.retain(|_, lock| lock.strong_count() > 0);
            let lock = locks
                .get(&info_hash)
                .and_then(|lock| lock.upgrade())
                .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(())));
            locks.insert(info_hash, Arc::downgrade(&lock));
            lock
        };
        let _add = tokio::select! {
            guard = add_lock.lock() => guard,
            _ = self.closing.cancelled() => bail!("session is stopping"),
        };
        if !opts.list_only
            && let Some(existing) = self.get(info_hash.into())
        {
            let _operation = existing.shared.lifecycle.lock().await;
            if self.ensure_managed(&existing).is_ok() {
                if existing.metadata.load().is_none() {
                    bail!(
                        "Saved metadata needs repair. Use Repair torrent with the original .torrent file."
                    );
                }
                if existing.shared.deleting.load(Ordering::Acquire) {
                    bail!("torrent removal is pending");
                }
                return Ok(AddTorrentResponse::AlreadyManaged(
                    existing.id(),
                    existing.clone(),
                ));
            }
        }
        if metadata.is_none() {
            let existing = self
                .db
                .read()
                .torrents
                .values()
                .find(|t| t.info_hash() == info_hash)
                .cloned();
            if let Some(existing) = existing {
                if existing.metadata.load().is_none() {
                    bail!(
                        "Saved metadata needs repair. Use Repair torrent with the original .torrent file."
                    );
                }
                if !opts.list_only {
                    return Ok(AddTorrentResponse::AlreadyManaged(existing.id(), existing));
                }
                if let Some(m) = existing.metadata.load_full() {
                    metadata = Some(TorrentMetadata::new(
                        m.info.clone(),
                        m.torrent_bytes.clone(),
                        m.info_bytes.clone(),
                    )?);
                }
            }
        }

        // Unknown hashes must not be broadcast before their privacy flag is known.
        let private = metadata.as_ref().is_none_or(|m| m.info.info().private);

        let tracker_tiers = metadata
            .as_ref()
            .map(|m| tracker_tiers_from_bytes(&m.torrent_bytes, &trackers))
            .transpose()?
            .unwrap_or_else(|| vec![trackers.clone()]);

        let metadata_discovery = metadata.is_none();
        let make_peer_rx = || {
            self.make_peer_rx(
                info_hash,
                tracker_tiers.clone(),
                !opts.paused && !opts.list_only,
                opts.force_tracker_interval,
                opts.initial_peers.clone().unwrap_or_default(),
                private,
                metadata_discovery,
            )
        };

        let mut seen_peers = Vec::new();

        let (metadata, peer_rx) = {
            match metadata {
                Some(metadata) => {
                    let mut peer_rx = None;
                    if !opts.paused && !opts.list_only {
                        peer_rx = make_peer_rx();
                    }
                    (metadata, peer_rx)
                }
                None => {
                    let peer_rx = make_peer_rx().context(
                        "magnet discovery requires an explicit tracker; open the .torrent file for private torrents",
                    )?;
                    let resolved_magnet = tokio::select! {
                        _ = self.closing.cancelled() => bail!("session is stopping"),
                        result = self.resolve_magnet(info_hash, peer_rx, &trackers, opts.peer_opts) => result?,
                    };

                    if resolved_magnet.metadata.info.info().private {
                        // Private metadata must come from the tracker-issued .torrent file.
                        drop(resolved_magnet);
                        self.wait_for_tracker_stop(info_hash).await?;
                        bail!(
                            "private magnets are not supported; open the tracker-issued .torrent file"
                        );
                    }
                    seen_peers = resolved_magnet.seen_peers;
                    let metadata = resolved_magnet.metadata;
                    drop(resolved_magnet.peer_rx);
                    self.wait_for_tracker_stop(info_hash).await?;
                    let peer_rx = if !opts.paused && !opts.list_only {
                        self.make_peer_rx(
                            info_hash,
                            tracker_tiers.clone(),
                            true,
                            opts.force_tracker_interval,
                            seen_peers.clone(),
                            false,
                            false,
                        )
                    } else {
                        None
                    };
                    (metadata, peer_rx)
                }
            }
        };

        trace!("Torrent metadata: {:#?}", &metadata.info.info());

        let only_files = compute_only_files(
            &metadata.info,
            opts.only_files,
            opts.only_files_regex,
            opts.list_only,
        )?;

        let output_folder = match (opts.output_folder, opts.sub_folder) {
            (None, None) => self.output_folder.join(
                self.get_default_subfolder_for_torrent(&metadata.info, name.as_deref())?
                    .unwrap_or_default(),
            ),
            (Some(o), None) => PathBuf::from(o),
            (Some(_), Some(_)) => {
                bail!("you can't provide both output_folder and sub_folder")
            }
            (None, Some(s)) => self.output_folder.join(s),
        };

        if opts.list_only {
            return Ok(AddTorrentResponse::ListOnly(ListOnlyResponse {
                info_hash,
                info: metadata.info,
                only_files,
                output_folder,
                seen_peers,
                torrent_bytes: metadata.torrent_bytes,
            }));
        }

        let _lifecycle = self.lifecycle.lock().await;
        if self.closing.is_cancelled() {
            bail!("session is stopping");
        }
        let storage_factory = opts
            .storage_factory
            .take()
            .or_else(|| self.default_storage_factory.as_ref().map(|f| f.clone_box()))
            .unwrap_or_else(|| FilesystemStorageFactory::default().boxed());

        let id = if let Some(id) = opts.preferred_id {
            id
        } else if let Some(p) = self.persistence.as_ref() {
            p.next_id().await?
        } else {
            self.next_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        };

        let _permit = self.spawner.semaphore().acquire_owned().await?;

        let (managed_torrent, metadata, _operation) = {
            let mut g = self.db.write();
            if let Some((id, handle)) = g.torrents.iter().find_map(|(eid, t)| {
                if t.info_hash() == info_hash || *eid == id {
                    Some((*eid, t.clone()))
                } else {
                    None
                }
            }) {
                return Ok(AddTorrentResponse::AlreadyManaged(id, handle));
            }

            let span = debug_span!(parent: self.rs(), "torrent", id);
            let peer_opts = self.merge_peer_opts(opts.peer_opts);
            let metadata = Arc::new(metadata);
            let minfo = Arc::new(ManagedTorrentShared {
                id,
                span,
                info_hash,
                trackers: trackers.into_iter().unique().collect(),
                tracker_tiers,
                tracker_downloaded: AtomicU64::new(0),
                tracker_uploaded: AtomicU64::new(0),
                tracker_have: AtomicU64::new(0),
                transfer_tasks: TaskTracker::new(),
                lifecycle: Arc::new(tokio::sync::Mutex::new(())),
                deleting: std::sync::atomic::AtomicBool::new(false),
                delete_files: std::sync::atomic::AtomicBool::new(false),
                spawner: self.spawner.clone(),
                peer_id: self.peer_id,
                storage_factory,
                options: ManagedTorrentOptions {
                    force_tracker_interval: opts.force_tracker_interval,
                    peer_connect_timeout: peer_opts.connect_timeout,
                    peer_read_write_timeout: peer_opts.read_write_timeout,
                    allow_overwrite: opts.overwrite,
                    output_folder,
                    ratelimits: opts.ratelimits,
                    initial_peers: opts.initial_peers.clone().unwrap_or_default(),
                    peer_limit: opts.peer_limit.or(self.peer_limit),
                    #[cfg(feature = "disable-upload")]
                    _disable_upload: self._disable_upload,
                },
                connector: self.connector.clone(),
                session: Arc::downgrade(self),
                magnet_name: name,
                client_name_and_version: self.client_name_and_version.clone(),
            });

            let initializing = Arc::new(TorrentStateInitializing::new(
                minfo.clone(),
                metadata.clone(),
                only_files.clone(),
                self.spawner
                    .block_in_place(|| minfo.storage_factory.create_and_init(&minfo, &metadata))?,
            ));
            let handle = Arc::new(ManagedTorrent {
                locked: RwLock::new(ManagedTorrentLocked {
                    paused: opts.paused,
                    state: ManagedTorrentState::Initializing(initializing),
                    only_files,
                }),
                state_change_notify: Notify::new(),
                shared: minfo,
                metadata: ArcSwapOption::new(Some(metadata.clone())),
            });

            // Acquire before publishing the handle: pause/delete must not race
            // the initial durable store or be overwritten by start's defaults.
            let operation = handle
                .shared
                .lifecycle
                .clone()
                .try_lock_owned()
                .expect("new torrent lifecycle mutex is unlocked");
            g.add_torrent(handle.clone(), id);
            (handle, metadata, operation)
        };

        drop(_permit);
        if let Some(p) = self.persistence.as_ref()
            && let Err(e) = p.store(id, &managed_torrent).await
        {
            self.db.write().torrents.remove(&id);
            return Err(e);
        }

        let _e = managed_torrent.shared.span.clone().entered();

        managed_torrent
            .start(peer_rx, opts.paused)
            .context("error starting torrent")?;

        if let Some(name) = metadata.info.name() {
            info!(?name, "added torrent");
        }

        Ok(AddTorrentResponse::Added(id, managed_torrent))
    }

    fn restored_shared(
        self: &Arc<Self>,
        id: TorrentId,
        record: &SerializedTorrent,
        metadata: Option<&TorrentMetadata>,
    ) -> anyhow::Result<Arc<ManagedTorrentShared>> {
        let tiers = metadata
            .map(|m| tracker_tiers_from_bytes(&m.torrent_bytes, &[]))
            .transpose()?
            .unwrap_or_default();
        let trackers = if metadata.is_some() {
            tiers.iter().flatten().cloned().unique().collect()
        } else {
            record
                .trackers
                .iter()
                .filter_map(|u| url::Url::parse(u).ok())
                .collect()
        };
        Ok(Arc::new(ManagedTorrentShared {
            id,
            info_hash: record.info_hash,
            spawner: self.spawner.clone(),
            trackers,
            tracker_tiers: tiers,
            tracker_downloaded: AtomicU64::new(0),
            tracker_uploaded: AtomicU64::new(0),
            tracker_have: AtomicU64::new(0),
            transfer_tasks: TaskTracker::new(),
            lifecycle: Arc::new(tokio::sync::Mutex::new(())),
            deleting: std::sync::atomic::AtomicBool::new(record.pending_delete.is_some()),
            delete_files: std::sync::atomic::AtomicBool::new(
                record.pending_delete.unwrap_or(false),
            ),
            peer_id: self.peer_id,
            span: debug_span!(parent:self.rs(), "torrent", id),
            options: ManagedTorrentOptions {
                output_folder: record.output_folder.clone(),
                allow_overwrite: true,
                peer_connect_timeout: self.peer_opts.connect_timeout,
                peer_read_write_timeout: self.peer_opts.read_write_timeout,
                peer_limit: self.peer_limit,
                #[cfg(feature = "disable-upload")]
                _disable_upload: self._disable_upload,
                ..Default::default()
            },
            connector: self.connector.clone(),
            storage_factory: self
                .default_storage_factory
                .as_ref()
                .map(|factory| factory.clone_box())
                .unwrap_or_else(|| FilesystemStorageFactory::default().boxed()),
            session: Arc::downgrade(self),
            magnet_name: record
                .name
                .clone()
                .or_else(|| Some(record.info_hash.as_string())),
            client_name_and_version: self.client_name_and_version.clone(),
        }))
    }

    fn register_recovery(
        self: &Arc<Self>,
        id: TorrentId,
        record: &SerializedTorrent,
        error: anyhow::Error,
    ) -> anyhow::Result<()> {
        let shared = self.restored_shared(id, record, None)?;
        let handle = Arc::new(ManagedTorrent {
            shared,
            metadata: ArcSwapOption::new(None),
            state_change_notify: Notify::new(),
            locked: RwLock::new(ManagedTorrentLocked {
                paused: true,
                only_files: record.only_files.clone(),
                state: ManagedTorrentState::Error(anyhow::anyhow!(
                    "{}. Choose Repair torrent with the original .torrent file.",
                    error.to_string().trim_end_matches('.')
                )),
            }),
        });
        self.db.write().add_torrent(handle, id);
        Ok(())
    }

    /// Restore a retained recovery entry using matching metainfo. Never discovers
    /// metadata on the network or changes the saved output folder or selection.
    pub async fn repair_metadata(
        self: &Arc<Self>,
        id: TorrentId,
        bytes: Bytes,
    ) -> anyhow::Result<()> {
        let handle = self.get(id.into()).context("no such torrent")?;
        let _operation = handle.shared.lifecycle.lock().await;
        self.ensure_managed(&handle)?;
        anyhow::ensure!(
            handle.metadata.load().is_none(),
            "Torrent metadata is already available"
        );
        anyhow::ensure!(
            bytes.len() <= 32 * 1024 * 1024,
            "Torrent metadata is too large"
        );
        let parsed = torrent_from_bytes(bytes).context("Invalid torrent metadata")?;
        anyhow::ensure!(
            parsed.meta.info_hash == handle.info_hash(),
            "This .torrent file belongs to a different torrent"
        );
        let metadata = Arc::new(TorrentMetadata::new(
            parsed.meta.info.data.validate()?,
            parsed.torrent_bytes,
            parsed.meta.info.raw_bytes.0,
        )?);
        let record = self
            .persistence
            .as_ref()
            .context("Recovery entry has no persistence store")?
            .get(id)
            .await?;
        if let Some(selected) = &record.only_files {
            anyhow::ensure!(
                selected.iter().all(|i| *i < metadata.file_infos.len()),
                "Saved file selection is invalid"
            );
        }
        let lifecycle = self.lifecycle.lock().await;
        anyhow::ensure!(!self.closing.is_cancelled(), "session is stopping");
        // Pin recovery paused before publishing usable metadata. If the later
        // metadata/JSON commit fails, restart must not unexpectedly announce.
        self.persistence
            .as_ref()
            .context("missing persistence")?
            .update_metadata(id, &handle)
            .await?;
        let shared = self.restored_shared(id, &record, Some(&metadata))?;
        let storage = self
            .spawner
            .block_in_place_with_semaphore(|| {
                shared.storage_factory.create_and_init(&shared, &metadata)
            })
            .await?;
        let init = Arc::new(TorrentStateInitializing::new(
            shared.clone(),
            metadata.clone(),
            record.only_files.clone(),
            storage,
        ));
        let repaired = Arc::new(ManagedTorrent {
            shared,
            metadata: ArcSwapOption::new(Some(metadata)),
            state_change_notify: Notify::new(),
            locked: RwLock::new(ManagedTorrentLocked {
                paused: true,
                only_files: record.only_files,
                state: ManagedTorrentState::Initializing(init),
            }),
        });
        let new_operation = repaired.shared.lifecycle.clone().lock_owned().await;
        self.persistence
            .as_ref()
            .context("missing persistence")?
            .store(id, &repaired)
            .await?;
        self.db.write().add_torrent(repaired.clone(), id);
        repaired.start(None, true)?;
        drop(lifecycle);
        drop(new_operation);
        drop(_operation);
        if let Some(delete_files) = record.pending_delete {
            self.delete(id.into(), delete_files).await?;
        }
        Ok(())
    }

    pub fn get(&self, id: TorrentIdOrHash) -> Option<ManagedTorrentHandle> {
        match id {
            TorrentIdOrHash::Id(id) => self.db.read().torrents.get(&id).cloned(),
            TorrentIdOrHash::Hash(id) => self.db.read().torrents.iter().find_map(|(_, v)| {
                if v.info_hash() == id {
                    Some(v.clone())
                } else {
                    None
                }
            }),
        }
    }

    pub async fn delete(&self, id: TorrentIdOrHash, delete_files: bool) -> anyhow::Result<()> {
        let Some(handle) = self.get(id) else {
            return Ok(());
        };
        let _operation = handle.shared.lifecycle.lock().await;
        if self.ensure_managed(&handle).is_err() {
            return Ok(());
        }
        handle.pause()?;
        let delete_files = delete_files || handle.shared.delete_files.load(Ordering::Acquire);
        // Once removal is requested, keep the in-memory handle inert even if
        // persistence reports an uncertain commit (rename succeeded, fsync failed).
        handle
            .shared
            .delete_files
            .store(delete_files, Ordering::Release);
        handle.shared.deleting.store(true, Ordering::Release);
        if let Some(p) = &self.persistence {
            p.mark_delete_pending(handle.id(), delete_files).await?;
        }
        self.quiesce(&handle).await?;
        if delete_files {
            let metadata = handle
                .metadata
                .load_full()
                .context("metadata unavailable for removal")?;
            handle.with_state(|state| match state {
                ManagedTorrentState::Paused(p) => {
                    remove_files_and_dirs(&metadata.file_infos, &p.files)
                }
                ManagedTorrentState::Initializing(p) => {
                    remove_files_and_dirs(&metadata.file_infos, &p.files)
                }
                _ => {
                    let storage = handle
                        .shared
                        .storage_factory
                        .create(handle.shared(), &metadata)?;
                    remove_files_and_dirs(&metadata.file_infos, &storage)
                }
            })?;
        }
        if let Some(p) = &self.persistence {
            p.delete(handle.id()).await?;
        }
        self.db.write().torrents.remove(&handle.id());
        self.tracker_tasks.write().remove(&handle.info_hash());
        info!(id = handle.id(), "deleted torrent");
        Ok(())
    }

    pub fn make_peer_rx_managed_torrent(
        self: &Arc<Self>,
        t: &Arc<ManagedTorrent>,
        announce: bool,
    ) -> Option<PeerStream> {
        let is_private = t.with_metadata(|m| m.info.info().private).unwrap_or(true);
        self.make_peer_rx(
            t.info_hash(),
            t.shared().tracker_tiers.clone(),
            announce,
            t.shared().options.force_tracker_interval,
            t.shared().options.initial_peers.clone(),
            is_private,
            false,
        )
    }

    // Get a peer stream from both DHT and trackers.
    #[allow(clippy::too_many_arguments)]
    fn make_peer_rx(
        self: &Arc<Self>,
        info_hash: Id20,
        mut trackers: Vec<Vec<url::Url>>,
        announce: bool,
        force_tracker_interval: Option<Duration>,
        initial_peers: Vec<SocketAddr>,
        is_private: bool,
        metadata_discovery: bool,
    ) -> Option<PeerStream> {
        let dht_rx = if is_private {
            None
        } else {
            self.dht.as_ref().map(|dht| {
                dht.get_peers(info_hash, if announce { self.announce_port } else { None })
            })
        };

        let lsd_rx = if is_private {
            None
        } else {
            self.lsd.as_ref().map(|lsd| {
                lsd.announce(info_hash, if announce { self.announce_port } else { None })
            })
        };

        if self.disable_trackers {
            trackers.clear();
        }

        // Never append global trackers for private torrents or unknown magnets.
        if !is_private && !self.disable_trackers {
            trackers.extend(self.trackers.iter().cloned().map(|u| vec![u]));
        }
        let tasks = self
            .tracker_tasks
            .write()
            .entry(info_hash)
            .or_default()
            .clone();
        tasks.reopen();

        let tracker_rx_stats = PeerRxTorrentInfo {
            info_hash,
            session: self.clone(),
            cached: RwLock::new(None),
            private: is_private,
            metadata_discovery,
        };
        let tracker_rx = TrackerComms::start(
            info_hash,
            self.peer_id,
            trackers,
            Box::new(tracker_rx_stats),
            force_tracker_interval,
            self.announce_port().unwrap_or(4240),
            self.reqwest_client.clone(),
            self.udp_tracker_client.clone(),
            tasks,
        );

        let initial_peers_rx = if is_private || initial_peers.is_empty() {
            None
        } else {
            Some(futures::stream::iter(initial_peers))
        };
        merge_two_optional_streams(
            merge_two_optional_streams(
                merge_two_optional_streams(dht_rx, tracker_rx),
                initial_peers_rx,
            ),
            lsd_rx,
        )
    }

    fn ensure_managed(&self, handle: &ManagedTorrentHandle) -> anyhow::Result<()> {
        if !self
            .get(handle.id().into())
            .is_some_and(|current| Arc::ptr_eq(&current, handle))
        {
            bail!("torrent is no longer managed");
        }
        Ok(())
    }

    async fn try_update_persistence_metadata(
        &self,
        handle: &ManagedTorrentHandle,
    ) -> anyhow::Result<()> {
        if let Some(p) = &self.persistence {
            p.update_metadata(handle.id(), handle).await?;
        }
        Ok(())
    }

    async fn quiesce(&self, handle: &ManagedTorrentHandle) -> anyhow::Result<()> {
        handle.shared.transfer_tasks.close();
        tokio::time::timeout(Duration::from_secs(10), handle.shared.transfer_tasks.wait())
            .await
            .context("torrent transfers have not stopped; retry the operation")?;
        if let Err(error) = self.wait_for_tracker_stop(handle.info_hash()).await {
            warn!(%error, "local operation completed; final tracker announce unconfirmed");
        }
        Ok(())
    }

    async fn wait_for_tracker_stop(&self, hash: Id20) -> anyhow::Result<()> {
        let tasks = self.tracker_tasks.read().get(&hash).cloned();
        if let Some(tasks) = tasks {
            tasks.close();
            tokio::time::timeout(Duration::from_secs(9), tasks.wait())
                .await
                .context("timed out waiting for final tracker announces")?;
        }
        Ok(())
    }

    pub async fn pause(&self, handle: &ManagedTorrentHandle) -> anyhow::Result<()> {
        let _operation = handle.shared.lifecycle.lock().await;
        self.ensure_managed(handle)?;
        if handle.shared.deleting.load(Ordering::Acquire) {
            bail!("torrent removal is pending");
        }
        handle.pause()?;
        let persisted = self.try_update_persistence_metadata(handle).await;
        let stopped = self.quiesce(handle).await;
        persisted?;
        stopped
    }

    /// Verify all current disk files; remains paused and performs no discovery.
    pub async fn recheck(&self, handle: &ManagedTorrentHandle) -> anyhow::Result<()> {
        let _operation = handle.shared.lifecycle.lock().await;
        self.ensure_managed(handle)?;
        if handle.shared.deleting.load(Ordering::Acquire) {
            bail!("torrent removal is pending");
        }
        if handle.metadata.load().is_none() {
            bail!("Saved metadata needs repair before resuming");
        }
        if self.closing.is_cancelled() {
            bail!("session is stopping");
        }
        if !handle.with_state(|s| matches!(s, ManagedTorrentState::Paused(_))) {
            bail!("pause the torrent before verifying files");
        }
        self.wait_for_tracker_stop(handle.info_hash()).await?;
        handle.recheck()?;
        self.try_update_persistence_metadata(handle).await?;
        Ok(())
    }

    pub async fn unpause(self: &Arc<Self>, handle: &ManagedTorrentHandle) -> anyhow::Result<()> {
        let _operation = handle.shared.lifecycle.lock().await;
        self.ensure_managed(handle)?;
        if handle.shared.deleting.load(Ordering::Acquire) {
            bail!("torrent removal is pending");
        }
        if handle.metadata.load().is_none() {
            bail!("Saved metadata needs repair before resuming");
        }
        if self.closing.is_cancelled() {
            bail!("session is stopping");
        }
        if handle.with_state(|s| matches!(s, ManagedTorrentState::Live(_))) {
            bail!("torrent is already live");
        }
        // A pause may have timed out while synchronous disk work was still
        // running. Do not report a resume until that cancelled generation exits.
        let cancelled_check = handle.with_state(|state| {
            matches!(state, ManagedTorrentState::Initializing(init) if init.is_pause_requested())
        });
        if cancelled_check {
            self.quiesce(handle).await?;
            if self.closing.is_cancelled() {
                bail!("session is stopping");
            }
        }
        self.wait_for_tracker_stop(handle.info_hash()).await?;
        let peer_rx = self.make_peer_rx_managed_torrent(handle, true);
        handle.start(peer_rx, false)?;
        if let Err(error) = self.try_update_persistence_metadata(handle).await {
            handle.pause()?;
            let _ = self.quiesce(handle).await;
            return Err(error);
        }
        Ok(())
    }

    pub async fn update_only_files(
        self: &Arc<Self>,
        handle: &ManagedTorrentHandle,
        only_files: &HashSet<usize>,
    ) -> anyhow::Result<()> {
        let _operation = handle.shared.lifecycle.lock().await;
        self.ensure_managed(handle)?;
        if handle.shared.deleting.load(Ordering::Acquire) {
            bail!("torrent removal is pending");
        }
        handle.update_only_files(only_files)?;
        self.try_update_persistence_metadata(handle).await?;
        Ok(())
    }

    pub fn listen_addr(&self) -> Option<SocketAddr> {
        self.listen_addr
    }

    pub fn announce_port(&self) -> Option<u16> {
        self.announce_port
    }

    async fn resolve_magnet(
        self: &Arc<Self>,
        info_hash: Id20,
        peer_rx: PeerStream,
        trackers: &[url::Url],
        peer_opts: Option<PeerConnectionOptions>,
    ) -> anyhow::Result<ResolveMagnetResult> {
        match read_metainfo_from_peer_receiver(
            self.peer_id,
            info_hash,
            Default::default(),
            peer_rx,
            Some(self.merge_peer_opts(peer_opts)),
            self.connector.clone(),
            self.client_name_and_version.clone(),
        )
        .await
        {
            ReadMetainfoResult::Found {
                info,
                info_bytes,
                rx,
                seen,
            } => {
                trace!(?info, "received result from DHT");
                let info = info.validate()?;
                Ok(ResolveMagnetResult {
                    metadata: TorrentMetadata::new(
                        info,
                        torrent_file_from_info_bytes(info_bytes.as_ref(), trackers)?,
                        info_bytes.0,
                    )?,
                    peer_rx: rx,
                    seen_peers: {
                        let seen = seen.into_iter().collect_vec();
                        for peer in &seen {
                            trace!(?peer, "seen")
                        }
                        seen
                    },
                })
            }
            ReadMetainfoResult::ChannelClosed { .. } => {
                bail!("input address stream exhausted, no way to discover torrent metainfo")
            }
        }
    }

    pub async fn create_and_serve_torrent(
        self: &Arc<Self>,
        path: &Path,
        opts: CreateTorrentOptions<'_>,
    ) -> Result<(CreateTorrentResult, ManagedTorrentHandle), ApiError> {
        if !path.exists() {
            return Err(ApiError::from((
                StatusCode::BAD_REQUEST,
                "path doesn't exist",
            )));
        }

        let torrent = create_torrent(path, opts, &self.spawner)
            .await
            .with_status(StatusCode::BAD_REQUEST)?;

        let bytes = torrent.as_bytes()?;

        let handle = self
            .add_torrent(
                AddTorrent::TorrentFileBytes(bytes.clone()),
                Some(AddTorrentOptions {
                    paused: false,
                    overwrite: true,
                    output_folder: Some(
                        torrent
                            .output_folder
                            .to_str()
                            .context("invalid utf-8")?
                            .to_owned(),
                    ),
                    ..Default::default()
                }),
            )
            .await?
            .into_handle()
            .context("error adding to session")?;

        Ok((torrent, handle))
    }
}

pub(crate) struct ResolveMagnetResult {
    pub metadata: TorrentMetadata,
    pub peer_rx: PeerStream,
    pub seen_peers: Vec<SocketAddr>,
}

fn remove_files_and_dirs(infos: &FileInfos, files: &dyn TorrentStorage) -> anyhow::Result<()> {
    let mut all_dirs = HashSet::new();
    for (id, fi) in infos.iter().enumerate() {
        if fi.attrs.padding {
            continue;
        }
        let mut fname = &*fi.relative_filename;
        if let Err(e) = files.remove_file(id, fname) {
            if !e.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
            }) {
                return Err(e).context("could not delete torrent file; removal remains pending");
            }
        } else {
            debug!(?fi.relative_filename, "deleted the file")
        }
        while let Some(parent) = fname.parent() {
            if parent != Path::new("") {
                all_dirs.insert(parent);
            }
            fname = parent;
        }
    }

    let all_dirs = {
        let mut v = all_dirs.into_iter().collect::<Vec<_>>();
        v.sort_unstable_by_key(|p| std::cmp::Reverse(p.as_os_str().len()));
        v
    };
    for dir in all_dirs {
        if let Err(e) = files.remove_directory_if_empty(dir) {
            if !e.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
            }) {
                return Err(e)
                    .context("could not remove torrent directory; removal remains pending");
            }
        } else {
            debug!("removed {dir:?}")
        }
    }
    Ok(())
}

// Preserve BEP 12 tiers from original metainfo. Extra public trackers form
// fallback tiers; private torrents never receive them from callers.
fn tracker_tiers_from_bytes(
    bytes: &[u8],
    trackers: &[url::Url],
) -> anyhow::Result<Vec<Vec<url::Url>>> {
    let meta = librqbit_core::torrent_metainfo::torrent_from_bytes(bytes)?;
    #[derive(serde::Deserialize)]
    struct AnnounceMetadata {
        announce: Option<buffers::ByteBufOwned>,
        #[serde(rename = "announce-list")]
        announce_list: Option<Vec<Vec<buffers::ByteBufOwned>>>,
    }
    let raw: AnnounceMetadata = bencode::from_bytes(bytes).map_err(|e| e.into_kind())?;
    let parse = |raw: &buffers::ByteBufOwned| {
        std::str::from_utf8(raw.as_ref())
            .ok()
            .and_then(|s| url::Url::parse(s).ok())
    };
    let mut tiers: Vec<Vec<url::Url>> = match raw.announce_list {
        Some(list) => list
            .iter()
            .map(|tier| tier.iter().filter_map(parse).collect())
            .collect(),
        None => vec![raw.announce.iter().filter_map(parse).collect()],
    };
    if !meta.info.data.private {
        for tracker in trackers {
            if !tiers.iter().flatten().any(|u| u == tracker) {
                tiers.push(vec![tracker.clone()]);
            }
        }
    }
    Ok(tiers)
}

// Ad adapter for converting stats into the format that tracker_comms accepts.
struct PeerRxTorrentInfo {
    info_hash: Id20,
    session: Arc<Session>,
    // Retain final stats for the stopped announce even after removal from the database.
    cached: RwLock<Option<ManagedTorrentHandle>>,
    private: bool,
    metadata_discovery: bool,
}

impl tracker_comms::TorrentStatsProvider for PeerRxTorrentInfo {
    fn is_metadata_discovery(&self) -> bool {
        self.metadata_discovery
    }
    fn is_private(&self) -> bool {
        self.private
    }
    fn disconnect_for_tracker_switch(&self) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async move {
            let mt = self
                .session
                .with_torrents(|torrents| {
                    for (_, t) in torrents {
                        if t.info_hash() == self.info_hash {
                            return Some(t.clone());
                        }
                    }
                    None
                })
                .context("cannot switch trackers during metadata discovery")?;
            if let Some(live) = mt.live() {
                live.disconnect_for_tracker_switch().await?;
            }
            Ok(())
        })
    }
    fn wait_for_transfers(&self) -> BoxFuture<'_, ()> {
        let cached = self.cached.read().clone();
        Box::pin(async move {
            if let Some(mt) = cached {
                mt.shared.transfer_tasks.close();
                mt.shared.transfer_tasks.wait().await;
            }
        })
    }
    fn get(&self) -> tracker_comms::TrackerCommsStats {
        let cached = self.cached.read().clone();
        let mt = cached.or_else(|| {
            self.session.with_torrents(|torrents| {
                for (_, mt) in torrents {
                    if mt.info_hash() == self.info_hash {
                        return Some(mt.clone());
                    }
                }
                None
            })
        });
        if let Some(mt) = mt {
            *self.cached.write() = Some(mt.clone());
            mt.tracker_stats()
        } else {
            // Metadata discovery is not a completed download. Never advertise as a seed.
            tracker_comms::TrackerCommsStats {
                total_bytes: 1,
                left_bytes: 1,
                ..Default::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    async fn initialization_fixture() -> (tempfile::TempDir, std::sync::Arc<super::Session>) {
        let root = tempfile::tempdir().unwrap();
        let session = super::Session::new_with_opts(
            root.path().to_owned(),
            super::SessionOptions {
                dht: None,
                disable_trackers: true,
                disable_local_service_discovery: true,
                concurrent_init_limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        (root, session)
    }
    async fn waiting_torrent(s: &std::sync::Arc<super::Session>) -> super::ManagedTorrentHandle {
        let mut bytes =
            b"d4:infod6:lengthi16384e4:name7:payload12:piece lengthi16384e6:pieces20:".to_vec();
        bytes.extend_from_slice(&[0; 20]);
        bytes.extend_from_slice(b"ee");
        s.add_torrent(
            super::AddTorrent::TorrentFileBytes(bytes.into()),
            Some(super::AddTorrentOptions {
                paused: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn initialization_wait_is_cancelled_by_pause_delete_and_stop() {
        use std::time::Duration;
        use tokio::time::timeout;
        for operation in ["pause", "delete", "stop"] {
            let (_root, s) = initialization_fixture().await;
            let barrier = s.concurrent_initialize_semaphore.acquire().await.unwrap();
            let h = waiting_torrent(&s).await;
            assert!(
                h.with_state(|state| matches!(state, crate::ManagedTorrentState::Initializing(_)))
            );
            match operation {
                "pause" => timeout(Duration::from_secs(2), s.pause(&h))
                    .await
                    .unwrap()
                    .unwrap(),
                "delete" => timeout(Duration::from_secs(2), s.delete(h.id().into(), true))
                    .await
                    .unwrap()
                    .unwrap(),
                _ => timeout(Duration::from_secs(2), s.stop()).await.unwrap(),
            }
            assert!(h.is_paused());
            assert!(h.live().is_none());
            assert!(h.shared.transfer_tasks.is_empty());
            drop(barrier);
            if operation == "pause" {
                // The cancelled worker released its running flag. Resume can
                // create a new check and reach Live, rather than staying stuck.
                s.unpause(&h).await.unwrap();
                timeout(Duration::from_secs(2), h.wait_until_initialized())
                    .await
                    .unwrap()
                    .unwrap();
                assert!(h.live().is_some());
                s.pause(&h).await.unwrap();
            } else if operation == "delete" {
                assert!(s.get(h.id().into()).is_none());
                assert!(!h.shared.options.output_folder.join("payload").exists());
                assert!(s.unpause(&h).await.is_err());
            }
            timeout(Duration::from_secs(2), s.stop()).await.unwrap();
            assert!(h.live().is_none());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn resume_after_pause_timeout_waits_for_cancelled_check_ownership() {
        let (_root, s) = initialization_fixture().await;
        let barrier = s.concurrent_initialize_semaphore.acquire().await.unwrap();
        let h = waiting_torrent(&s).await;
        s.pause(&h).await.unwrap();
        let init = h.with_state(|state| match state {
            crate::ManagedTorrentState::Initializing(init) => init.clone(),
            _ => panic!("expected waiting initialization"),
        });
        // Model non-cancellable disk work holding both ownership and its task
        // registration beyond the pause deadline. No wall-clock sleep required.
        let guard = init.start_check(None).unwrap();
        let task = h.shared.transfer_tasks.token();
        assert!(s.pause(&h).await.is_err());
        assert!(h.is_paused());
        let session = s.clone();
        let handle = h.clone();
        let resume = tokio::spawn(async move { session.unpause(&handle).await });
        tokio::task::yield_now().await;
        assert!(!resume.is_finished());
        assert!(
            h.is_paused(),
            "resume must not publish intent before the old check exits"
        );
        drop(guard);
        drop(task);
        drop(barrier);
        resume.await.unwrap().unwrap();
        h.wait_until_initialized().await.unwrap();
        assert!(h.live().is_some());
        s.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_during_initialization_retains_new_peer_stream() {
        use futures::StreamExt;
        let (_root, s) = initialization_fixture().await;
        let barrier = s.concurrent_initialize_semaphore.acquire().await.unwrap();
        let h = waiting_torrent(&s).await;
        let polled = std::sync::Arc::new(tokio::sync::Notify::new());
        let signal = polled.clone();
        h.start(
            Some(
                futures::stream::poll_fn(move |_| {
                    signal.notify_one();
                    std::task::Poll::Pending
                })
                .boxed(),
            ),
            false,
        )
        .unwrap();
        drop(barrier);
        tokio::time::timeout(std::time::Duration::from_secs(2), polled.notified())
            .await
            .unwrap();
        assert!(h.live().is_some());
        s.stop().await;
        assert!(h.shared.transfer_tasks.is_empty());
    }

    use buffers::ByteBuf;
    use itertools::Itertools;
    use librqbit_core::torrent_metainfo::{TorrentMetaV1, torrent_from_bytes};

    use super::torrent_file_from_info_bytes;

    #[test]
    fn tiers_survive_original_metadata_and_ignore_announce_when_list_is_present() {
        let url = |s: &str| url::Url::parse(s).unwrap();
        for private in [false, true] {
            let source = include_bytes!("../resources/ubuntu-21.04-desktop-amd64.iso.torrent");
            let mut meta = torrent_from_bytes(source).unwrap();
            meta.info.data.private = private;
            meta.announce = Some(ByteBuf(b"http://127.0.0.1/ignored"));
            meta.announce_list = vec![
                vec![
                    ByteBuf(b"http://127.0.0.1/a"),
                    ByteBuf(b"http://127.0.0.1/b"),
                ],
                vec![ByteBuf(b"http://127.0.0.1/c")],
            ];
            let mut bytes = Vec::new();
            bencode::bencode_serialize_to_writer(&meta, &mut bytes).unwrap();
            let tiers =
                super::tracker_tiers_from_bytes(&bytes, &[url("http://127.0.0.1/custom")]).unwrap();
            assert_eq!(
                tiers[0],
                vec![url("http://127.0.0.1/a"), url("http://127.0.0.1/b")]
            );
            assert_eq!(tiers[1], vec![url("http://127.0.0.1/c")]);
            assert_eq!(tiers.len(), if private { 2 } else { 3 });
            assert!(!tiers.iter().flatten().any(|u| u.path() == "/ignored"));
        }
    }

    #[test]
    fn test_torrent_file_from_info_and_bytes() {
        fn get_trackers(info: &TorrentMetaV1<ByteBuf>) -> Vec<url::Url> {
            info.iter_announce()
                .filter_map(|t| std::str::from_utf8(t.as_ref()).ok().map(|t| t.to_owned()))
                .filter_map(|t| t.parse().ok())
                .collect_vec()
        }

        let orig_full_torrent =
            include_bytes!("../resources/ubuntu-21.04-desktop-amd64.iso.torrent");
        let parsed = torrent_from_bytes(&orig_full_torrent[..]).unwrap();
        let parsed_trackers = get_trackers(&parsed);

        let generated_torrent =
            torrent_file_from_info_bytes(parsed.info.raw_bytes.as_ref(), &parsed_trackers).unwrap();
        let generated_parsed = torrent_from_bytes(generated_torrent.as_ref()).unwrap();
        assert_eq!(parsed.info_hash, generated_parsed.info_hash);
        assert_eq!(parsed.info, generated_parsed.info);
        assert_eq!(parsed_trackers, get_trackers(&generated_parsed));
    }
}
