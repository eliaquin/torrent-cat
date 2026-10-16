use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

use anyhow::Context;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use size_format::SizeFormatterBinary as SF;
use tracing::{info, trace, warn};

use crate::{
    api::TorrentIdOrHash,
    chunk_tracker::{ChunkTracker, compute_selected_pieces},
    file_ops::FileOps,
    type_aliases::{FileStorage, PeerStream},
};

use super::{ManagedTorrentShared, TorrentMetadata, paused::TorrentStatePaused};

pub struct TorrentStateInitializing {
    pub(crate) files: FileStorage,
    pub(crate) shared: Arc<ManagedTorrentShared>,
    pub(crate) metadata: Arc<TorrentMetadata>,
    pub(crate) only_files: Option<Vec<usize>>,
    pub(crate) checked_bytes: AtomicU64,
    pause_requested: AtomicBool,
    check_state: Mutex<CheckState>,
}

#[derive(Default)]
struct CheckState {
    running: bool,
    cancel: CancellationToken,
    peers: Option<PeerStream>,
}

// Reset ownership even if session shutdown drops the initialization future.
pub(crate) struct CheckGuard {
    init: Arc<TorrentStateInitializing>,
    pub(crate) cancel: CancellationToken,
}
impl Drop for CheckGuard {
    fn drop(&mut self) {
        let mut state = self.init.check_state.lock();
        state.peers.take();
        state.running = false;
    }
}

impl TorrentStateInitializing {
    pub fn new(
        shared: Arc<ManagedTorrentShared>,
        metadata: Arc<TorrentMetadata>,
        only_files: Option<Vec<usize>>,
        files: FileStorage,
    ) -> Self {
        shared.files_initialized.store(true, Ordering::Release);
        Self {
            shared,
            metadata,
            only_files,
            files,
            checked_bytes: AtomicU64::new(0),
            pause_requested: AtomicBool::new(false),
            check_state: Mutex::new(CheckState::default()),
        }
    }

    pub fn get_checked_bytes(&self) -> u64 {
        self.checked_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn request_pause(&self) {
        self.pause_requested.store(true, Ordering::Release);
        let mut state = self.check_state.lock();
        state.cancel.cancel();
        state.peers.take();
    }

    pub(crate) fn is_pause_requested(&self) -> bool {
        self.pause_requested.load(Ordering::Acquire)
    }

    // Called under the managed-state lock, like request_pause. A second start
    // updates discovery intent without clearing an in-flight cancellation.
    pub(crate) fn start_check(self: &Arc<Self>, peers: Option<PeerStream>) -> Option<CheckGuard> {
        let mut state = self.check_state.lock();
        if peers.is_some() {
            state.peers = peers;
        }
        if state.running {
            return None;
        }
        state.running = true;
        state.cancel = CancellationToken::new();
        self.pause_requested.store(false, Ordering::Release);
        Some(CheckGuard {
            init: self.clone(),
            cancel: state.cancel.clone(),
        })
    }

    pub(crate) fn take_peers(&self) -> Option<PeerStream> {
        self.check_state.lock().peers.take()
    }

    pub async fn check(&self) -> anyhow::Result<TorrentStatePaused> {
        let id: TorrentIdOrHash = self.shared.info_hash.into();
        let bitv_factory = self
            .shared
            .session
            .upgrade()
            .context("session is dead")?
            .bitv_factory
            .clone();
        // A persisted bitfield is a hint, never proof of current disk contents.
        // Full hashing detects same-size edits, replaced files, torn writes and
        // corruption outside a sample; it also recovers uncheckpointed pieces.
        info!("Checking all torrent pieces before resuming...");
        let have_pieces = self
            .shared
            .spawner
            .block_in_place_with_semaphore(|| {
                let have =
                    FileOps::new(&self.metadata.info, &self.files, &self.metadata.file_infos)
                        .initial_check(&self.checked_bytes, &self.pause_requested)?;
                if bitv_factory.requires_durable_storage() {
                    self.files
                        .sync_all()
                        .context("syncing payload before checkpoint")?;
                }
                Ok::<_, anyhow::Error>(have)
            })
            .await?;
        let have_pieces = bitv_factory
            .store_initial_check(id, have_pieces)
            .await
            .context("error storing verified piece checkpoint")?;

        let selected_pieces = compute_selected_pieces(
            self.metadata.lengths(),
            |idx| {
                self.only_files
                    .as_ref()
                    .map(|o| o.contains(&idx))
                    .unwrap_or(true)
            },
            &self.metadata.file_infos,
        );

        let chunk_tracker = ChunkTracker::new(
            have_pieces,
            selected_pieces,
            *self.metadata.lengths(),
            &self.metadata.file_infos,
        )
        .context("error creating chunk tracker")?;

        let hns = chunk_tracker.get_hns();

        info!(
            torrent=?self.shared.id,
            "Initial check results: have {}, needed {}, total selected {}",
            SF::new(hns.have_bytes),
            SF::new(hns.needed_bytes),
            SF::new(hns.selected_bytes)
        );

        // Ensure file lengths are correct, and reopen read-only.
        self.shared
            .spawner
            .block_in_place_with_semaphore(|| {
                for (idx, fi) in self.metadata.file_infos.iter().enumerate() {
                    if self
                        .only_files
                        .as_ref()
                        .map(|v| v.contains(&idx))
                        .unwrap_or(true)
                    {
                        let now = Instant::now();
                        if fi.attrs.padding {
                            continue;
                        }
                        if let Err(err) = self.files.ensure_file_length(idx, fi.len) {
                            warn!(
                                id=?self.shared.id, info_hash = ?self.shared.info_hash,
                                "Error setting length for file {:?} to {}: {:#?}",
                                fi.relative_filename, fi.len, err
                            );
                        } else {
                            trace!(
                                "Set length for file {:?} to {} in {:?}",
                                fi.relative_filename,
                                SF::new(fi.len),
                                now.elapsed()
                            );
                        }
                    }
                }
                Ok::<_, anyhow::Error>(())
            })
            .await?;

        let paused = TorrentStatePaused {
            shared: self.shared.clone(),
            metadata: self.metadata.clone(),
            files: self.files.take()?,
            chunk_tracker,
            streams: Arc::new(Default::default()),
        };
        Ok(paused)
    }
}
