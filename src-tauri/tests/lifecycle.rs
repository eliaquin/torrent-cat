//! Durable operations use temporary profiles and no discovery or external trackers.
use librqbit::{AddTorrent, AddTorrentOptions, Session, SessionOptions, SessionPersistenceConfig};
use std::{path::Path, sync::Arc, time::Duration};

async fn open(root: &Path) -> Arc<Session> {
    Session::new_with_opts(
        root.join("data"),
        SessionOptions {
            dht: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            runtime_worker_threads: Some(1),
            persistence: Some(SessionPersistenceConfig::Json {
                folder: Some(root.join("profile")),
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}
async fn add(s: &Arc<Session>, root: &Path) -> Arc<librqbit::ManagedTorrent> {
    std::fs::create_dir_all(root.join("data")).unwrap();
    let file = root.join("data/payload.bin");
    std::fs::write(&file, vec![7; 16384]).unwrap();
    let mut bytes = Vec::new();
    lava_torrent::torrent::v1::TorrentBuilder::new(&file, 16384)
        .set_num_threads(1)
        .build()
        .unwrap()
        .write_into(&mut bytes)
        .unwrap();
    let h = s
        .add_torrent(
            AddTorrent::TorrentFileBytes(bytes.into()),
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
    tokio::time::timeout(Duration::from_secs(5), h.wait_until_initialized())
        .await
        .unwrap()
        .unwrap();
    h
}
fn db(root: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(root.join("profile/session.json")).unwrap()).unwrap()
}
fn block_database(root: &Path) {
    std::fs::rename(
        root.join("profile/session.json"),
        root.join("profile/saved.json"),
    )
    .unwrap();
    std::fs::create_dir(root.join("profile/session.json")).unwrap();
}
fn unblock_database(root: &Path) {
    std::fs::remove_dir(root.join("profile/session.json")).unwrap();
    std::fs::rename(
        root.join("profile/saved.json"),
        root.join("profile/session.json"),
    )
    .unwrap();
}
#[tokio::test(flavor = "multi_thread")]
async fn persistence_failure_is_visible_and_pause_resume_delete_can_be_retried() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let s = open(root).await;
    let h = add(&s, root).await;
    let id = h.id().to_string();
    s.unpause(&h).await.unwrap();
    block_database(root);
    assert!(s.pause(&h).await.is_err());
    assert!(h.is_paused());
    unblock_database(root);
    s.pause(&h).await.unwrap();
    assert_eq!(db(root)["torrents"][&id]["is_paused"], true);
    block_database(root);
    assert!(s.unpause(&h).await.is_err());
    assert!(
        h.is_paused(),
        "failed resume must not leave unpersisted activity running"
    );
    unblock_database(root);
    s.unpause(&h).await.unwrap();
    s.pause(&h).await.unwrap();
    block_database(root);
    assert!(s.delete(h.id().into(), true).await.is_err());
    assert!(s.get(h.id().into()).is_some());
    assert!(root.join("data/payload.bin").exists());
    assert!(
        s.unpause(&h).await.is_err(),
        "uncertain removal remains inert until retry"
    );
    unblock_database(root);
    s.stop().await;
    let restored = open(root).await;
    assert!(restored.get(h.id().into()).unwrap().is_paused());
    restored.delete(h.id().into(), true).await.unwrap();
    assert!(!root.join("data/payload.bin").exists());
    restored.delete(h.id().into(), true).await.unwrap();
    assert!(db(root)["torrents"].as_object().unwrap().is_empty());
    assert!(
        restored.unpause(&h).await.is_err(),
        "stale handles cannot restart removed torrents"
    );
    restored.stop().await;
    let next = open(root).await;
    let new = add(&next, root).await;
    assert!(
        new.id() > h.id(),
        "deleted IDs must not be reused after restart"
    );
    next.delete(h.id().into(), true).await.unwrap();
    assert!(next.get(new.id().into()).is_some());
    next.stop().await;
}

async fn fail_payload_removal(root: &Path) {
    let s = open(root).await;
    let h = add(&s, root).await;
    let file = root.join("data/payload.bin");
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    std::fs::write(file.join("keep"), b"unrelated").unwrap();
    assert!(s.delete(h.id().into(), true).await.is_err());
    assert!(s.get(h.id().into()).is_some());
    assert!(s.unpause(&h).await.is_err());
    assert!(
        s.delete(h.id().into(), false).await.is_err(),
        "retry cannot downgrade durable data deletion"
    );
    assert_eq!(
        db(root)["torrents"][h.id().to_string()]["pending_delete"],
        true
    );
    assert_eq!(std::fs::read(file.join("keep")).unwrap(), b"unrelated");
    // Deliberately no stop: parent simulates repair after a process crash.
}
#[tokio::test(flavor = "multi_thread")]
async fn pending_delete_survives_abrupt_exit_and_converges_on_restart() {
    const ENV: &str = "TORRENTCAT_LIFECYCLE_CHILD";
    if let Some(root) = std::env::var_os(ENV) {
        fail_payload_removal(Path::new(&root)).await;
        std::process::exit(0);
    }
    let temp = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "pending_delete_survives_abrupt_exit_and_converges_on_restart",
            "--nocapture",
        ])
        .env(ENV, temp.path())
        .status()
        .unwrap();
    assert!(status.success());
    let file = temp.path().join("data/payload.bin");
    std::fs::rename(&file, temp.path().join("unrelated-directory")).unwrap();
    let s = tokio::time::timeout(Duration::from_secs(5), open(temp.path()))
        .await
        .unwrap();
    assert_eq!(s.with_torrents(|t| t.count()), 0);
    assert!(db(temp.path())["torrents"].as_object().unwrap().is_empty());
    assert!(!file.exists());
    assert_eq!(
        std::fs::read(temp.path().join("unrelated-directory/keep")).unwrap(),
        b"unrelated"
    );
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_joins_owned_background_work_before_returning() {
    struct Finished(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Finished {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let s = open(temp.path()).await;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (finished_tx, mut finished_rx) = tokio::sync::oneshot::channel();
    s.spawn(
        tracing::Span::none(),
        "owned-background-regression",
        async move {
            let _finished = Finished(Some(finished_tx));
            ready_tx.send(()).unwrap();
            std::future::pending::<anyhow::Result<()>>().await
        },
    );
    ready_rx.await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), s.stop())
        .await
        .unwrap();
    finished_rx
        .try_recv()
        .expect("stop returned before dropping background work");
    s.stop().await;
}
