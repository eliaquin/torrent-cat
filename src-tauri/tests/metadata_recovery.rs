//! Broken saved metadata is retained without tracker discovery or payload changes.
use librqbit::{
    AddTorrent, AddTorrentOptions, Api, Session, SessionOptions, SessionPersistenceConfig,
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};
async fn open(root: &Path) -> Arc<Session> {
    timeout(
        Duration::from_secs(2),
        Session::new_with_opts(
            root.join("data"),
            SessionOptions {
                dht: None,
                disable_local_service_discovery: true,
                persistence: Some(SessionPersistenceConfig::Json {
                    folder: Some(root.join("profile")),
                }),
                ..Default::default()
            },
        ),
    )
    .await
    .expect("startup must not wait for metadata discovery")
    .unwrap()
}
async fn fixture(
    s: &Arc<Session>,
    root: &Path,
    name: &str,
    tracker: &str,
) -> (Arc<librqbit::ManagedTorrent>, Vec<u8>) {
    std::fs::create_dir_all(root.join("data")).unwrap();
    let file = root.join("data").join(name);
    std::fs::write(&file, vec![42; 16384]).unwrap();
    let mut bytes = Vec::new();
    lava_torrent::torrent::v1::TorrentBuilder::new(&file, 16384)
        .set_num_threads(1)
        .set_privacy(true)
        .set_announce(Some(tracker.to_owned()))
        .build()
        .unwrap()
        .write_into(&mut bytes)
        .unwrap();
    let h = s
        .add_torrent(
            AddTorrent::TorrentFileBytes(bytes.clone().into()),
            Some(AddTorrentOptions {
                overwrite: true,
                paused: true,
                only_files: Some(vec![0]),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    h.wait_until_initialized().await.unwrap();
    (h, bytes)
}
fn record_path(root: &Path, h: &librqbit::ManagedTorrent) -> std::path::PathBuf {
    root.join("profile")
        .join(format!("{}.torrent", h.info_hash().as_string()))
}
fn set_record(root: &Path, id: usize, edit: impl FnOnce(&mut serde_json::Value)) {
    let path = root.join("profile/session.json");
    let mut db: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut db["torrents"][id.to_string()]);
    std::fs::write(path, serde_json::to_vec(&db).unwrap()).unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_empty_invalid_mismatched_oversized_and_nonregular_metadata_remain_visible_and_repairable()
 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/announce", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            seen.fetch_add(1, Ordering::Relaxed);
            let mut buf = [0; 4096];
            let _ = stream.read(&mut buf).await;
            let _=stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 24\r\nConnection: close\r\n\r\nd8:intervali1e5:peers0:e").await;
        }
    });
    for damage in [
        "missing",
        "empty",
        "invalid",
        "mismatch",
        "oversize",
        "directory",
        "symlink",
        "fifo",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let s = open(root).await;
        let (h, bytes) = fixture(&s, root, "payload.bin", &url).await;
        let (healthy, other) = fixture(&s, root, "healthy.bin", &url).await;
        let path = record_path(root, &h);
        s.stop().await;
        set_record(root, h.id(), |v| {
            v["is_paused"] = false.into();
            if damage == "missing" {
                v.as_object_mut().unwrap().remove("name");
            }
        });
        std::fs::remove_file(&path).unwrap();
        match damage {
            "missing" => {}
            "empty" => std::fs::write(&path, b"").unwrap(),
            "invalid" => std::fs::write(&path, b"not a torrent").unwrap(),
            "mismatch" => std::fs::write(&path, &other).unwrap(),
            "oversize" => std::fs::File::create(&path)
                .unwrap()
                .set_len(32 * 1024 * 1024 + 1)
                .unwrap(),
            "directory" => std::fs::create_dir(&path).unwrap(),
            #[cfg(unix)]
            "symlink" => std::os::unix::fs::symlink(root.join("data/payload.bin"), &path).unwrap(),
            #[cfg(unix)]
            "fifo" => {
                let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            }
            _ => continue,
        }
        let restored = open(root).await;
        assert_eq!(restored.with_torrents(|t| t.count()), 2, "{damage}");
        let recovery = restored.get(h.id().into()).unwrap();
        assert!(recovery.stats().metadata_missing, "{damage}");
        assert!(recovery.stats().error.is_some());
        assert!(!recovery.name().unwrap().is_empty());
        assert!(recovery.is_paused());
        assert_eq!(recovery.output_folder(), root.join("data"));
        assert_eq!(recovery.only_files(), Some(vec![0]));
        assert!(
            !restored
                .get(healthy.id().into())
                .unwrap()
                .stats()
                .metadata_missing
        );
        let list = Api::new(restored.clone(), None)
            .api_torrent_list_ext(librqbit::api::ApiTorrentListOpts { with_stats: true });
        assert_eq!(list.torrents.len(), 2);
        assert!(
            list.torrents
                .iter()
                .find(|t| t.id == Some(h.id()))
                .unwrap()
                .stats
                .as_ref()
                .unwrap()
                .metadata_missing
        );
        assert!(restored.unpause(&recovery).await.is_err());
        assert!(
            restored
                .add_torrent(
                    AddTorrent::from_url(format!(
                        "magnet:?xt=urn:btih:{}&tr={url}",
                        h.info_hash().as_string()
                    )),
                    None
                )
                .await
                .is_err()
        );
        assert!(
            restored
                .repair_metadata(h.id(), other.into())
                .await
                .is_err(),
            "wrong hash accepted"
        );
        assert_eq!(
            std::fs::read(root.join("data/payload.bin")).unwrap(),
            vec![42; 16384]
        );
        if damage == "directory" {
            std::fs::remove_dir(&path).unwrap();
        } else if matches!(damage, "fifo" | "symlink") {
            std::fs::remove_file(&path).unwrap();
        }
        restored
            .repair_metadata(h.id(), bytes.clone().into())
            .await
            .unwrap();
        let repaired = restored.get(h.id().into()).unwrap();
        repaired.wait_until_initialized().await.unwrap();
        assert!(repaired.is_paused());
        assert!(!repaired.stats().metadata_missing);
        assert_eq!(repaired.only_files(), Some(vec![0]));
        assert_eq!(repaired.stats().progress_bytes, 16384);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        restored.stop().await;
        let reopened = open(root).await;
        assert!(
            !reopened
                .get(h.id().into())
                .unwrap()
                .stats()
                .metadata_missing
        );
        reopened.stop().await;
        assert_eq!(
            requests.load(Ordering::Relaxed),
            0,
            "{damage} caused tracker traffic"
        );
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn repair_failure_stays_visible_and_pending_removals_converge() {
    for pending in [None, Some(false), Some(true)] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let s = open(root).await;
        let (h, bytes) = fixture(&s, root, "payload.bin", "http://127.0.0.1:1/unused").await;
        let metadata = record_path(root, &h);
        s.stop().await;
        std::fs::remove_file(&metadata).unwrap();
        set_record(root, h.id(), |v| {
            v["is_paused"] = false.into();
            if let Some(delete) = pending {
                v["pending_delete"] = delete.into();
            }
        });
        let restored = open(root).await;
        if pending == Some(false) {
            assert!(restored.get(h.id().into()).is_none());
            assert_eq!(
                std::fs::read(root.join("data/payload.bin")).unwrap(),
                vec![42; 16384]
            );
            restored.stop().await;
            continue;
        }
        // An actual metadata publish failure must propagate and preserve recovery.
        std::fs::create_dir(&metadata).unwrap();
        assert!(
            restored
                .repair_metadata(h.id(), bytes.clone().into())
                .await
                .is_err()
        );
        assert!(
            restored
                .get(h.id().into())
                .unwrap()
                .stats()
                .metadata_missing
        );
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("profile/session.json")).unwrap())
                .unwrap();
        assert_eq!(
            saved["torrents"][h.id().to_string()]["is_paused"],
            true,
            "pin paused before metadata commit"
        );
        assert_eq!(
            std::fs::read(root.join("data/payload.bin")).unwrap(),
            vec![42; 16384]
        );
        std::fs::remove_dir(&metadata).unwrap();
        restored
            .repair_metadata(h.id(), bytes.into())
            .await
            .unwrap();
        if pending == Some(true) {
            assert!(restored.get(h.id().into()).is_none());
            assert!(!root.join("data/payload.bin").exists());
        } else {
            let repaired = restored.get(h.id().into()).unwrap();
            assert!(repaired.is_paused());
            assert_eq!(repaired.output_folder(), root.join("data"));
        }
        restored.stop().await;
        let reopened = open(root).await;
        assert_eq!(
            reopened.with_torrents(|t| t.count()),
            usize::from(pending.is_none())
        );
        reopened.stop().await;
    }
}
