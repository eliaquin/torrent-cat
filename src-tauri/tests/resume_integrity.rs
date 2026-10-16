//! Restart and manual verification use every piece, with loopback discovery disabled.
use librqbit::{AddTorrent, AddTorrentOptions, Session, SessionOptions, SessionPersistenceConfig};
use std::{path::Path, sync::Arc, time::Duration};

async fn open(output: &Path, profile: &Path, checkpoints: bool) -> Arc<Session> {
    Session::new_with_opts(
        output.into(),
        SessionOptions {
            dht: None,
            disable_local_service_discovery: true,
            disable_trackers: true,
            fastresume: checkpoints,
            persistence: Some(SessionPersistenceConfig::Json {
                folder: Some(profile.into()),
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}
async fn initialized(h: &librqbit::ManagedTorrent) {
    tokio::time::timeout(Duration::from_secs(5), h.wait_until_initialized())
        .await
        .unwrap()
        .unwrap();
}
fn metadata(file: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    lava_torrent::torrent::v1::TorrentBuilder::new(file, 16384)
        .set_num_threads(1)
        .build()
        .unwrap()
        .write_into(&mut bytes)
        .unwrap();
    bytes
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_checks_same_size_replacement_truncation_and_late_piece_corruption() {
    for checkpoints in [false, true] {
        for mutation in [
            "same-size",
            "replacement",
            "truncation",
            "late-piece",
            "checkpoint",
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let output = tmp.path().join("data");
            std::fs::create_dir(&output).unwrap();
            let profile = tmp.path().join("profile");
            let file = output.join("data.bin");
            let data = vec![42; 16384 * 100];
            std::fs::write(&file, &data).unwrap();
            let s = open(&output, &profile, checkpoints).await;
            let h = s
                .add_torrent(
                    AddTorrent::TorrentFileBytes(metadata(&file).into()),
                    Some(AddTorrentOptions {
                        paused: true,
                        overwrite: true,
                        ..Default::default()
                    }),
                )
                .await
                .unwrap()
                .into_handle()
                .unwrap();
            initialized(&h).await;
            assert_eq!(h.stats().progress_bytes, data.len() as u64);
            s.stop().await;
            drop(h);
            drop(s);
            let expected = match mutation {
                "same-size" => {
                    std::fs::write(&file, vec![0; data.len()]).unwrap();
                    0
                }
                "replacement" => {
                    std::fs::rename(&file, output.join("old.bin")).unwrap();
                    std::fs::write(&file, vec![0; data.len()]).unwrap();
                    0
                }
                "truncation" => {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(&file)
                        .unwrap()
                        .set_len(16384)
                        .unwrap();
                    16384
                }
                "late-piece" => {
                    let mut changed = data.clone();
                    changed[99 * 16384..].fill(0);
                    std::fs::write(&file, changed).unwrap();
                    99 * 16384
                }
                _ => {
                    // A corrupt/truncated checkpoint must not prevent recovery of good disk data.
                    let mut changed_checkpoints = 0;
                    for entry in std::fs::read_dir(&profile).unwrap() {
                        let path = entry.unwrap().path();
                        if path.extension().is_some_and(|e| e == "bitv") {
                            std::fs::write(path, b"broken").unwrap();
                            changed_checkpoints += 1;
                        }
                    }
                    assert_eq!(changed_checkpoints, usize::from(checkpoints));
                    data.len() as u64
                }
            };
            let restored = open(&output, &profile, checkpoints).await;
            let handles = restored.with_torrents(|t| t.map(|(_, h)| h.clone()).collect::<Vec<_>>());
            assert_eq!(handles.len(), 1);
            initialized(&handles[0]).await;
            assert_eq!(
                handles[0].stats().progress_bytes,
                expected,
                "{mutation}, checkpoints={checkpoints}"
            );
            assert!(handles[0].live().is_none());
            restored.stop().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_recheck_reopens_replaced_files_and_stays_paused() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("data");
    std::fs::create_dir(&output).unwrap();
    let file = output.join("data.bin");
    std::fs::write(&file, vec![42; 16384]).unwrap();
    let s = open(&output, &tmp.path().join("profile"), true).await;
    let h = s
        .add_torrent(
            AddTorrent::TorrentFileBytes(metadata(&file).into()),
            Some(AddTorrentOptions {
                paused: true,
                overwrite: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    initialized(&h).await;
    let id = h.id();
    std::fs::rename(&file, output.join("original")).unwrap();
    std::fs::write(&file, vec![0; 16384]).unwrap();
    s.recheck(&h).await.unwrap();
    initialized(&h).await;
    assert_eq!(h.stats().progress_bytes, 0);
    assert_eq!(h.id(), id);
    assert!(h.live().is_none());
    std::fs::write(&file, vec![42; 16384]).unwrap();
    s.recheck(&h).await.unwrap();
    initialized(&h).await;
    assert_eq!(h.stats().progress_bytes, 16384);
    assert!(h.live().is_none());
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn abrupt_checkpoint_writer() {
    let Ok(root) = std::env::var("TORRENTCAT_RESUME_CRASH_FIXTURE") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let output = root.join("data");
    let s = open(&output, &root.join("profile"), true).await;
    let file = output.join("data.bin");
    let h = s
        .add_torrent(
            AddTorrent::TorrentFileBytes(metadata(&file).into()),
            Some(AddTorrentOptions {
                paused: true,
                overwrite: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    initialized(&h).await;
    // No Session::stop and no destructors: emulate abrupt process termination.
    std::process::exit(0);
}

#[tokio::test(flavor = "multi_thread")]
async fn abrupt_exit_checkpoint_does_not_hide_subsequent_corruption() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("data");
    std::fs::create_dir(&output).unwrap();
    let file = output.join("data.bin");
    std::fs::write(&file, vec![42; 32768]).unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "abrupt_checkpoint_writer", "--nocapture"])
        .env("TORRENTCAT_RESUME_CRASH_FIXTURE", tmp.path())
        .status()
        .unwrap();
    assert!(status.success());
    let mut changed = vec![42; 32768];
    changed[16384..].fill(0);
    std::fs::write(&file, changed).unwrap();
    let s = open(&output, &tmp.path().join("profile"), true).await;
    let h = s.with_torrents(|t| t.next().unwrap().1.clone());
    initialized(&h).await;
    assert_eq!(h.stats().progress_bytes, 16384);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn payload_sync_failure_prevents_checkpoint_publication() {
    use librqbit::storage::{
        StorageFactory, TorrentStorage,
        filesystem::{FilesystemStorage, FilesystemStorageFactory},
    };
    use librqbit::{ManagedTorrentShared, TorrentMetadata};
    struct FailingSync(FilesystemStorage);
    impl TorrentStorage for FailingSync {
        fn init(&mut self, s: &ManagedTorrentShared, m: &TorrentMetadata) -> anyhow::Result<()> {
            self.0.init(s, m)
        }
        fn pread_exact(&self, id: usize, o: u64, b: &mut [u8]) -> anyhow::Result<()> {
            self.0.pread_exact(id, o, b)
        }
        fn pwrite_all(&self, id: usize, o: u64, b: &[u8]) -> anyhow::Result<()> {
            self.0.pwrite_all(id, o, b)
        }
        fn remove_file(&self, id: usize, p: &Path) -> anyhow::Result<()> {
            self.0.remove_file(id, p)
        }
        fn remove_directory_if_empty(&self, p: &Path) -> anyhow::Result<()> {
            self.0.remove_directory_if_empty(p)
        }
        fn ensure_file_length(&self, id: usize, n: u64) -> anyhow::Result<()> {
            self.0.ensure_file_length(id, n)
        }
        fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
            anyhow::bail!("sync failure must precede take")
        }
        fn sync_all(&self) -> anyhow::Result<()> {
            anyhow::bail!("injected payload sync failure")
        }
    }
    #[derive(Clone, Copy)]
    struct Factory;
    impl StorageFactory for Factory {
        type Storage = Box<dyn TorrentStorage>;
        // Instrument the supported filesystem backend without changing the
        // persistent store's backend-compatibility gate.
        fn is_type_id(&self, id: std::any::TypeId) -> bool {
            FilesystemStorageFactory::default().is_type_id(id)
        }
        fn create(
            &self,
            s: &ManagedTorrentShared,
            m: &TorrentMetadata,
        ) -> anyhow::Result<Box<dyn TorrentStorage>> {
            Ok(Box::new(FailingSync(
                FilesystemStorageFactory::default().create(s, m)?,
            )))
        }
        fn clone_box(&self) -> librqbit::storage::BoxStorageFactory {
            Box::new(*self)
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("data");
    std::fs::create_dir(&output).unwrap();
    let profile = tmp.path().join("profile");
    let file = output.join("data.bin");
    std::fs::write(&file, vec![42; 16384]).unwrap();
    let s = Session::new_with_opts(
        output,
        SessionOptions {
            dht: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            fastresume: true,
            persistence: Some(SessionPersistenceConfig::Json {
                folder: Some(profile.clone()),
            }),
            default_storage_factory: Some(Box::new(Factory)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let h = s
        .add_torrent(
            AddTorrent::TorrentFileBytes(metadata(&file).into()),
            Some(AddTorrentOptions {
                paused: true,
                overwrite: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(5), h.wait_until_initialized())
        .await
        .unwrap()
        .unwrap_err();
    assert!(format!("{error:#}").contains("sync"));
    assert!(
        !std::fs::read_dir(&profile).unwrap().any(|e| e
            .unwrap()
            .path()
            .extension()
            .is_some_and(|e| e == "bitv"))
    );
    s.stop().await;
}
