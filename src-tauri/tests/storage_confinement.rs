//! F1 regressions: only temporary files, no discovery or external network.
#![cfg(unix)]
use librqbit::{
    AddTorrent, AddTorrentOptions, ManagedTorrent, Session, SessionOptions,
    SessionPersistenceConfig,
    storage::{StorageFactory, TorrentStorage, filesystem::FilesystemStorageFactory},
};
use std::{os::unix::fs::symlink, path::Path, sync::Arc, time::Duration};
use tokio::time::timeout;

async fn session(root: &Path, profile: Option<&Path>) -> Arc<Session> {
    Session::new_with_opts(
        root.into(),
        SessionOptions {
            dht: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            persistence: profile.map(|p| SessionPersistenceConfig::Json {
                folder: Some(p.into()),
            }),
            fastresume: false,
            ..Default::default()
        },
    )
    .await
    .unwrap()
}
fn torrent(root: &Path) -> Vec<u8> {
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join("nested/payload.bin"), vec![42; 16384]).unwrap();
    let torrent = lava_torrent::torrent::v1::TorrentBuilder::new(root, 16384)
        .set_num_threads(1)
        .build()
        .unwrap();
    let mut bytes = Vec::new();
    torrent.write_into(&mut bytes).unwrap();
    bytes
}
async fn add(
    s: &Arc<Session>,
    bytes: &[u8],
    overwrite: bool,
) -> anyhow::Result<Arc<ManagedTorrent>> {
    let h = s
        .add_torrent(
            AddTorrent::TorrentFileBytes(bytes.to_vec().into()),
            Some(AddTorrentOptions {
                paused: true,
                overwrite,
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .unwrap();
    timeout(Duration::from_secs(5), h.wait_until_initialized()).await??;
    Ok(h)
}

#[tokio::test(flavor = "multi_thread")]
async fn file_and_directory_links_cannot_escape_on_create_or_reuse() {
    for directory_link in [false, true] {
        for absolute in [false, true] {
            for overwrite in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let bytes = torrent(&temp.path().join("source"));
                let output = temp.path().join("download");
                let outside = temp.path().join("outside");
                std::fs::create_dir_all(output.join("nested")).unwrap();
                std::fs::create_dir(&outside).unwrap();
                let original = b"unrelated external data";
                std::fs::write(outside.join("payload.bin"), original).unwrap();
                if directory_link {
                    std::fs::remove_dir(output.join("nested")).unwrap();
                    let target = if absolute {
                        outside.clone()
                    } else {
                        "../outside".into()
                    };
                    symlink(target, output.join("nested")).unwrap();
                } else {
                    let target = if absolute {
                        outside.join("payload.bin")
                    } else {
                        "../../outside/payload.bin".into()
                    };
                    symlink(target, output.join("nested/payload.bin")).unwrap();
                }
                let s = session(&output, None).await;
                assert!(add(&s, &bytes, overwrite).await.is_err());
                s.stop().await;
                assert_eq!(
                    std::fs::read(outside.join("payload.bin")).unwrap(),
                    original
                );
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn regular_files_support_create_reuse_read_write_and_remove() {
    let temp = tempfile::tempdir().unwrap();
    let bytes = torrent(&temp.path().join("source"));
    let output = temp.path().join("download");
    let s = session(&output, None).await;
    let h = add(&s, &bytes, false).await.unwrap();
    s.stop().await;
    drop(h);
    drop(s);
    let s = session(&output, None).await;
    assert!(add(&s, &bytes, false).await.is_err());
    let h = add(&s, &bytes, true).await.unwrap();
    let metadata = h.with_metadata(Arc::clone).unwrap();
    let storage = FilesystemStorageFactory::default()
        .create_and_init(h.shared(), &metadata)
        .unwrap();
    storage.pwrite_all(0, 0, b"safe").unwrap();
    let mut buffer = [0; 4];
    storage.pread_exact(0, 0, &mut buffer).unwrap();
    assert_eq!(&buffer, b"safe");
    storage
        .remove_file(0, Path::new("nested/payload.bin"))
        .unwrap();
    storage
        .remove_directory_if_empty(Path::new("nested"))
        .unwrap();
    assert!(!output.join("nested").exists());
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_after_root_or_child_replacement_preserves_external_files() {
    for replace_root in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let bytes = torrent(&temp.path().join("source"));
        let output = temp.path().join("download");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(outside.join("nested")).unwrap();
        std::fs::write(outside.join("nested/payload.bin"), b"keep").unwrap();
        let s = session(&output, None).await;
        let h = add(&s, &bytes, true).await.unwrap();
        if replace_root {
            std::fs::rename(&output, temp.path().join("original")).unwrap();
            symlink(&outside, &output).unwrap();
        } else {
            std::fs::rename(output.join("nested"), output.join("original")).unwrap();
            symlink(outside.join("nested"), output.join("nested")).unwrap();
        }
        let removed = s.delete(h.id().into(), true).await;
        if replace_root {
            removed.unwrap();
        } else {
            assert!(
                removed.is_err(),
                "escaping child path must report incomplete deletion"
            );
            assert!(
                s.get(h.id().into()).is_some(),
                "failed deletion must remain retryable"
            );
        }
        s.stop().await;
        assert_eq!(
            std::fs::read(outside.join("nested/payload.bin")).unwrap(),
            b"keep"
        );
        if replace_root {
            assert!(!temp.path().join("original/nested/payload.bin").exists());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn restored_session_cannot_follow_replaced_file() {
    let temp = tempfile::tempdir().unwrap();
    let bytes = torrent(&temp.path().join("source"));
    let output = temp.path().join("download");
    let profile = temp.path().join("profile");
    let s = session(&output, Some(&profile)).await;
    let h = add(&s, &bytes, true).await.unwrap();
    s.stop().await;
    drop(h);
    drop(s);
    let outside = temp.path().join("outside.bin");
    std::fs::write(&outside, b"preserve").unwrap();
    std::fs::remove_file(output.join("nested/payload.bin")).unwrap();
    symlink(&outside, output.join("nested/payload.bin")).unwrap();
    let restored = session(&output, Some(&profile)).await;
    restored.stop().await;
    assert_eq!(std::fs::read(&outside).unwrap(), b"preserve");
}

#[tokio::test(flavor = "multi_thread")]
async fn symlink_as_download_root_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let bytes = torrent(&temp.path().join("source"));
    let outside = temp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let output = temp.path().join("download");
    symlink(&outside, &output).unwrap();
    let s = session(&output, None).await;
    assert!(add(&s, &bytes, true).await.is_err());
    s.stop().await;
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_directory_link_swaps_cannot_modify_external_data() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let temp = tempfile::tempdir().unwrap();
    let bytes = torrent(&temp.path().join("source"));
    let output = temp.path().join("download");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let original = vec![77; 257];
    std::fs::write(outside.join("payload.bin"), &original).unwrap();
    let s = session(&output, None).await;
    let h = add(&s, &bytes, true).await.unwrap();
    let metadata = h.with_metadata(Arc::clone).unwrap();
    // Rename a directory and replace it with an escaping symlink while storage
    // repeatedly creates, opens, resizes, writes and removes its children.
    let running = Arc::new(AtomicBool::new(true));
    let flag = running.clone();
    let swap_root = output.clone();
    let swap_outside = outside.clone();
    let swapper = std::thread::spawn(move || {
        while flag.load(Ordering::Relaxed) {
            if std::fs::rename(swap_root.join("nested"), swap_root.join("held")).is_ok() {
                if symlink(&swap_outside, swap_root.join("nested")).is_ok() {
                    std::thread::yield_now();
                    std::fs::remove_file(swap_root.join("nested")).unwrap();
                }
                // A concurrent create_dir_all may have recreated nested.
                let _ = std::fs::remove_dir(swap_root.join("nested"));
                let _ = std::fs::rename(swap_root.join("held"), swap_root.join("nested"));
            }
        }
    });
    let mut successes = 0;
    for _ in 0..500 {
        if let Ok(storage) =
            FilesystemStorageFactory::default().create_and_init(h.shared(), &metadata)
        {
            storage.ensure_file_length(0, 16384).unwrap();
            storage.pwrite_all(0, 0, b"confined").unwrap();
            let _ = storage.remove_file(0, Path::new("nested/payload.bin"));
            successes += 1;
        }
    }
    running.store(false, Ordering::Relaxed);
    swapper.join().unwrap();
    assert!(
        successes > 0,
        "exercise successful operations as well as denials"
    );
    assert_eq!(
        std::fs::read(outside.join("payload.bin")).unwrap(),
        original
    );
    s.stop().await;
}
