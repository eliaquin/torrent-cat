use crate::{core::Core, media::Source};
use librqbit::{AddTorrent, AddTorrentOptions, api::TorrentIdOrHash};
use std::time::Duration;
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn newest_torrents_stay_first_across_duplicates_removal_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("profile");
    let core = Core::new(directory.clone(), true).await.unwrap();
    let mut added = Vec::new();
    for name in ["first", "second", "third"] {
        let source = root.path().join(format!("{name}.txt"));
        std::fs::write(&source, name.repeat(1000)).unwrap();
        let metadata = root.path().join(format!("{name}.torrent"));
        let result = core
            .create(
                source.to_string_lossy().into(),
                metadata.to_string_lossy().into(),
                false,
                String::new(),
                vec![],
            )
            .await
            .unwrap();
        added.push(result["details"]["info_hash"].as_str().unwrap().to_owned());
    }
    async fn order(core: &Core) -> Vec<String> {
        core.snapshot().await.unwrap()["torrents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["info_hash"].as_str().unwrap().to_owned())
            .collect()
    }
    assert_eq!(
        order(&core).await,
        vec![added[2].clone(), added[1].clone(), added[0].clone()]
    );
    let metadata = root
        .path()
        .join("first.torrent")
        .to_string_lossy()
        .into_owned();
    let existing = core
        .add(
            metadata.clone(),
            Some(root.path().to_string_lossy().into()),
            true,
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        order(&core).await,
        vec![added[2].clone(), added[1].clone(), added[0].clone()]
    );
    core.action(existing["id"].as_u64().unwrap() as usize, "remove")
        .await
        .unwrap();
    core.add(
        metadata,
        Some(root.path().to_string_lossy().into()),
        true,
        true,
    )
    .await
    .unwrap();
    let expected = vec![added[0].clone(), added[2].clone(), added[1].clone()];
    assert_eq!(order(&core).await, expected);
    for torrent in core.api.api_torrent_list().torrents {
        core.action(torrent.id.unwrap(), "pause").await.unwrap();
    }
    core.api.session().stop().await;
    drop(core);
    let restored = Core::new(directory.clone(), true).await.unwrap();
    assert_eq!(order(&restored).await, expected);
    assert_eq!(order(&restored).await, expected);
    restored.api.session().stop().await;
    drop(restored);

    // Old profiles acquire a stable persisted order without losing settings.
    let path = directory.join("profile.json");
    let mut profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    profile.as_object_mut().unwrap().remove("torrentOrder");
    std::fs::write(&path, serde_json::to_vec(&profile).unwrap()).unwrap();
    let migrated = Core::new(directory.clone(), true).await.unwrap();
    let stable = order(&migrated).await;
    assert_eq!(stable.len(), 3);
    assert_eq!(migrated.profile.lock().await.torrent_order, stable);
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(saved["prefs"], profile["prefs"]);
    migrated.api.session().stop().await;
    drop(migrated);
    let restarted = Core::new(directory, true).await.unwrap();
    assert_eq!(order(&restarted).await, stable);
    restarted.api.session().stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piece_map_reports_verified_islands_while_paused_and_live() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let downloads = root.path().join("downloads");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&downloads).unwrap();
    let piece = 16_384;
    let payload: Vec<u8> = (0..piece * 5 + 123).map(|i| (i % 251) as u8).collect();
    let path = source.join("movie.mp4");
    std::fs::write(&path, &payload).unwrap();
    let torrent = lava_torrent::torrent::v1::TorrentBuilder::new(path, piece as i64)
        .set_num_threads(2)
        .build()
        .unwrap();
    let mut metadata = Vec::new();
    torrent.write_into(&mut metadata).unwrap();
    let mut partial = payload.clone();
    // These pieces fail hash verification, leaving islands at the beginning,
    // middle and end, including the final short piece.
    partial[piece..piece * 2].fill(0);
    partial[piece * 3..piece * 4].fill(0);
    std::fs::write(downloads.join("movie.mp4"), partial).unwrap();
    let core = Core::new(root.path().join("profile"), true).await.unwrap();
    let handle = core
        .api
        .session()
        .add_torrent(
            AddTorrent::TorrentFileBytes(metadata.into()),
            Some(AddTorrentOptions {
                output_folder: Some(downloads.to_string_lossy().into()),
                paused: true,
                overwrite: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    timeout(Duration::from_secs(20), handle.wait_until_initialized())
        .await
        .unwrap()
        .unwrap();
    let expected = vec![
        [0, piece as u64],
        [(piece * 2) as u64, (piece * 3) as u64],
        [(piece * 4) as u64, payload.len() as u64],
    ];
    for live in [false, true] {
        if live {
            core.action(handle.id(), "resume").await.unwrap();
        }
        let map = core.file_pieces(handle.id(), 0).unwrap().unwrap();
        assert_eq!(map.ranges, expected);
        assert_eq!(map.downloaded_bytes, (payload.len() - piece * 2) as u64);
        assert_eq!(map.file_length, payload.len() as u64);
        let json = serde_json::to_value(&map).unwrap();
        assert_eq!(json["fileLength"], payload.len());
        assert_eq!(json["downloadedBytes"], payload.len() - piece * 2);
    }
    assert!(core.file_pieces(handle.id(), 1).is_err());
    assert!(core.file_pieces(usize::MAX, 0).is_err());
    core.api.session().stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_seed_download_stream_select_resume_and_remove() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    // Distinct pieces make a wrong seek or offset observable.
    let payload: Vec<u8> = (0..2_600_000)
        .map(|i| ((i * 31 + i / 65536) % 251) as u8)
        .collect();
    std::fs::write(source.join("movie.mp4"), &payload).unwrap();
    std::fs::write(source.join("notes.txt"), b"TorrentCat integration fixture").unwrap();
    let seed = Core::new(root.path().join("seed-profile"), true)
        .await
        .unwrap();
    let metadata = root.path().join("fixture.torrent");
    let result = seed
        .create(
            source.to_string_lossy().into(),
            metadata.to_string_lossy().into(),
            true,
            "Integration fixture".into(),
            vec!["http://127.0.0.1:1/announce".into()],
        )
        .await
        .unwrap();
    let sid = result["id"].as_u64().unwrap() as usize;
    let sh = seed.api.mgr_handle(TorrentIdOrHash::Id(sid)).unwrap();
    timeout(Duration::from_secs(20), sh.wait_until_completed())
        .await
        .unwrap()
        .unwrap();
    let torrent = lava_torrent::torrent::v1::Torrent::read_from_file(&metadata).unwrap();
    assert!(torrent.is_private());
    assert!(format!("{:?}", torrent.extra_fields).contains("Integration fixture"));
    let download_dir = root.path().join("downloads");
    std::fs::create_dir(&download_dir).unwrap();
    let client_profile = root.path().join("client-profile");
    let client = Core::new(client_profile.clone(), true).await.unwrap();
    let added = client
        .api
        .session()
        .add_torrent(
            AddTorrent::TorrentFileBytes(tokio::fs::read(&metadata).await.unwrap().into()),
            Some(AddTorrentOptions {
                output_folder: Some(download_dir.to_string_lossy().into()),
                initial_peers: Some(vec![seed.api.session().listen_addr().unwrap()]),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    timeout(Duration::from_secs(30), added.wait_until_initialized())
        .await
        .unwrap()
        .unwrap();
    let details = client
        .api
        .api_torrent_details(TorrentIdOrHash::Id(added.id()))
        .unwrap();
    let movie = details
        .files
        .as_ref()
        .unwrap()
        .iter()
        .position(|f| f.name == "movie.mp4")
        .unwrap();
    // Request a late range while the torrent may still be incomplete.
    let url = client.media.grant(Source::Torrent(added.id(), movie)).await;
    let http = reqwest::Client::new();
    let r = timeout(
        Duration::from_secs(40),
        http.get(&url)
            .header("Range", "bytes=2100000-2100999")
            .send(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(
        r.headers()["content-range"],
        "bytes 2100000-2100999/2600000"
    );
    assert_eq!(
        &r.bytes().await.unwrap()[..],
        &payload[2_100_000..2_101_000]
    );
    let head = http.head(&url).send().await.unwrap();
    assert_eq!(head.status(), 200);
    assert_eq!(head.headers()["content-length"], "2600000");
    assert_eq!(
        http.get(&url)
            .header("Origin", "https://untrusted.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        http.get(&url)
            .header("Range", "bytes=99999999-")
            .send()
            .await
            .unwrap()
            .status(),
        416
    );
    timeout(Duration::from_secs(30), added.wait_until_completed())
        .await
        .unwrap()
        .unwrap();
    let pieces = client.file_pieces(added.id(), movie).unwrap().unwrap();
    assert_eq!(pieces.file_length, payload.len() as u64);
    assert_eq!(pieces.ranges, vec![[0, payload.len() as u64]]);
    assert_eq!(
        tokio::fs::read(download_dir.join("movie.mp4"))
            .await
            .unwrap(),
        payload
    );
    let only = std::collections::HashSet::from([movie]);
    client
        .api
        .api_torrent_action_update_only_files(TorrentIdOrHash::Id(added.id()), &only)
        .await
        .unwrap();
    client.action(added.id(), "pause").await.unwrap();
    assert_eq!(http.get(&url).send().await.unwrap().status(), 403);
    client.action(added.id(), "resume").await.unwrap();
    client.action(added.id(), "pause").await.unwrap();
    let id = added.id();
    client.api.session().stop().await;
    drop(added);
    drop(client);
    let restored = Core::new(client_profile, true).await.unwrap();
    let handle = restored.api.mgr_handle(TorrentIdOrHash::Id(id)).unwrap();
    timeout(Duration::from_secs(20), handle.wait_until_initialized())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        handle.stats().state,
        librqbit::TorrentStatsState::Paused
    ));
    restored.action(id, "remove").await.unwrap();
    assert!(download_dir.join("movie.mp4").exists());
    assert!(restored.api.api_torrent_list().torrents.is_empty());
    restored.api.session().stop().await;
    seed.api.session().stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_add_and_folder_layout() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("collection");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("one.txt"), "one").unwrap();
    std::fs::write(source.join("two.txt"), "two").unwrap();
    let creator = Core::new(root.path().join("creator"), true).await.unwrap();
    let metadata = root.path().join("collection.torrent");
    creator
        .create(
            source.to_string_lossy().into(),
            metadata.to_string_lossy().into(),
            false,
            String::new(),
            vec![],
        )
        .await
        .unwrap();
    let dl = root.path().join("dl");
    std::fs::create_dir(&dl).unwrap();
    let core = Core::new(root.path().join("app"), true).await.unwrap();
    let add = || {
        core.add(
            metadata.to_string_lossy().into(),
            Some(dl.to_string_lossy().into()),
            true,
            false,
        )
    };
    let a = add().await.unwrap();
    let b = add().await.unwrap();
    assert_eq!(a["id"], b["id"]);
    assert_eq!(core.api.api_torrent_list().torrents.len(), 1);
    assert_eq!(
        a["output_folder"],
        dl.join("collection").to_string_lossy().as_ref()
    );
    core.api.session().stop().await;
    creator.api.session().stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_import_is_paused_idempotent_and_preserves_source() {
    let root = tempfile::tempdir().unwrap();
    let downloads = root.path().join("downloads");
    let source = downloads.join("collection");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("movie.mp4"), b"untouched fixture").unwrap();
    std::fs::write(source.join("notes.txt"), b"notes").unwrap();
    let legacy = root.path().join("legacy");
    std::fs::create_dir_all(legacy.join("Torrents")).unwrap();
    let metadata = legacy.join("Torrents/custom-name.torrent");
    let creator = Core::new(root.path().join("creator"), true).await.unwrap();
    let made = creator
        .create(
            source.to_string_lossy().into(),
            metadata.to_string_lossy().into(),
            true,
            "".into(),
            vec!["http://127.0.0.1:1/announce".into()],
        )
        .await
        .unwrap();
    let hash = made["details"]["info_hash"].as_str().unwrap();
    creator.api.session().stop().await;
    let config = serde_json::to_vec(&serde_json::json!({"torrents":[{"infoHash":hash,"name":"collection","path":downloads,"torrentFileName":"custom-name.torrent","selections":[true,false],"files":[{"currentTime":42.0},{}]}]})).unwrap();
    std::fs::write(legacy.join("config.json"), &config).unwrap();
    let core = Core::new(root.path().join("new"), true).await.unwrap();
    let imported = core
        .import_legacy(legacy.to_string_lossy().into())
        .await
        .unwrap();
    assert_eq!(imported["imported"], 1);
    assert_eq!(imported["errors"], serde_json::json!([]));
    let list = core.api.api_torrent_list().torrents;
    let id = list[0].id.unwrap();
    let handle = core.api.mgr_handle(TorrentIdOrHash::Id(id)).unwrap();
    timeout(Duration::from_secs(10), handle.wait_until_initialized())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        handle.stats().state,
        librqbit::TorrentStatsState::Paused
    ));
    assert_eq!(list[0].output_folder, source.to_string_lossy());
    assert_eq!(
        core.profile.lock().await.positions[&format!("{hash}:0")],
        42.0
    );
    assert_eq!(
        core.import_legacy(legacy.to_string_lossy().into())
            .await
            .unwrap()["imported"],
        0
    );
    assert_eq!(std::fs::read(legacy.join("config.json")).unwrap(), config);
    assert_eq!(
        std::fs::read(source.join("movie.mp4")).unwrap(),
        b"untouched fixture"
    );
    core.api.session().stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn media_tools_preserve_original_and_serve_compatible_outputs() {
    let Some(ffmpeg) = crate::jobs::tool("ffmpeg") else {
        eprintln!("FFmpeg unavailable: media integration skipped");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("video.mkv");
    let result = tokio::process::Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=160x90:r=10",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "3",
            "-c:v",
            "libx264",
            "-c:a",
            "ac3",
        ])
        .arg(&source)
        .status()
        .await
        .unwrap();
    assert!(result.success());
    let original = std::fs::read(&source).unwrap();
    let core = Core::new(root.path().join("app"), true).await.unwrap();
    let made = core
        .create(
            source.to_string_lossy().into(),
            root.path().join("video.torrent").to_string_lossy().into(),
            true,
            "".into(),
            vec!["http://127.0.0.1:1/announce".into()],
        )
        .await
        .unwrap();
    let id = made["id"].as_u64().unwrap() as usize;
    timeout(
        Duration::from_secs(10),
        core.api
            .mgr_handle(TorrentIdOrHash::Id(id))
            .unwrap()
            .wait_until_completed(),
    )
    .await
    .unwrap()
    .unwrap();
    let probe = crate::jobs::probe(&core, id, 0).await.unwrap();
    assert!(
        probe["streams"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["codec_name"] == "ac3")
    );
    let http = reqwest::Client::new();
    let poster = crate::jobs::poster(&core, id, 0).await.unwrap();
    let jpg = http
        .get(poster)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(&jpg[..2], &[0xff, 0xd8]);
    let converted = crate::jobs::convert(&core, id, 0, None).await.unwrap();
    let mp4 = http
        .get(converted)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(&mp4[4..8], b"ftyp");
    assert_eq!(std::fs::read(&source).unwrap(), original);
    let subtitle = root.path().join("subtitles.srt");
    let utf16: Vec<u8> = [0xff, 0xfe]
        .into_iter()
        .chain(
            "1\n00:00:00,000 --> 00:00:02,000\nHello\n"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        )
        .collect();
    std::fs::write(&subtitle, utf16).unwrap();
    let url = crate::jobs::subtitle(&core, subtitle.to_string_lossy().into())
        .await
        .unwrap();
    let vtt = http.get(url).send().await.unwrap().text().await.unwrap();
    assert!(vtt.starts_with("WEBVTT"));
    assert!(vtt.contains("00:00:00.000 --> 00:00:02.000"));
    core.api.session().stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playback_priority_restores_only_automatic_pauses_and_recovers_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let core_path = root.path().join("app");
    let core = Core::new(core_path.clone(), true).await.unwrap();
    let mut ids = Vec::new();
    let mut metadata = Vec::new();
    for n in 0..3 {
        let source = root.path().join(format!("source-{n}.mp4"));
        std::fs::write(&source, vec![n as u8 + 1; 8192]).unwrap();
        let torrent = root.path().join(format!("source-{n}.torrent"));
        let made = core
            .create(
                source.to_string_lossy().into(),
                torrent.to_string_lossy().into(),
                true,
                "".into(),
                vec!["http://127.0.0.1:1/announce".into()],
            )
            .await
            .unwrap();
        let id = made["id"].as_u64().unwrap() as usize;
        timeout(
            Duration::from_secs(10),
            core.api
                .mgr_handle(TorrentIdOrHash::Id(id))
                .unwrap()
                .wait_until_completed(),
        )
        .await
        .unwrap()
        .unwrap();
        ids.push(id);
        metadata.push(torrent);
    }
    core.start_playback(ids[0], 0).await.unwrap();
    assert!(
        matches!(
            core.api
                .api_stats_v1(TorrentIdOrHash::Id(ids[1]))
                .unwrap()
                .state,
            librqbit::TorrentStatsState::Live
        ),
        "Complete playback must not pause other transfers"
    );
    // Re-add one torrent without its original bytes, with no peers, so the
    // incomplete-stream scenario is deterministic.
    core.action(ids[0], "remove").await.unwrap();
    let dl = root.path().join("downloads");
    std::fs::create_dir(&dl).unwrap();
    let playing = core
        .add(
            metadata[0].to_string_lossy().into(),
            Some(dl.to_string_lossy().into()),
            true,
            false,
        )
        .await
        .unwrap()["id"]
        .as_u64()
        .unwrap() as usize;
    core.action(ids[2], "pause").await.unwrap();
    core.start_playback(playing, 0).await.unwrap();
    assert!(matches!(
        core.api
            .api_stats_v1(TorrentIdOrHash::Id(ids[1]))
            .unwrap()
            .state,
        librqbit::TorrentStatsState::Paused
    ));
    assert_eq!(core.profile.lock().await.suspended_for_playback.len(), 1);
    core.restore_playback_transfers().await.unwrap();
    assert!(matches!(
        core.api
            .api_stats_v1(TorrentIdOrHash::Id(ids[1]))
            .unwrap()
            .state,
        librqbit::TorrentStatsState::Live
    ));
    assert!(matches!(
        core.api
            .api_stats_v1(TorrentIdOrHash::Id(ids[2]))
            .unwrap()
            .state,
        librqbit::TorrentStatsState::Paused
    ));
    core.start_playback(playing, 0).await.unwrap();
    core.action(ids[1], "pause").await.unwrap();
    core.restore_playback_transfers().await.unwrap();
    assert!(
        matches!(
            core.api
                .api_stats_v1(TorrentIdOrHash::Id(ids[1]))
                .unwrap()
                .state,
            librqbit::TorrentStatsState::Paused
        ),
        "Explicit user pause must override automatic restoration"
    );
    core.action(ids[1], "resume").await.unwrap();
    core.start_playback(playing, 0).await.unwrap();
    core.api.session().stop().await;
    drop(core);
    let restored = Core::new(core_path, true).await.unwrap();
    assert!(
        restored
            .profile
            .lock()
            .await
            .suspended_for_playback
            .is_empty()
    );
    assert!(matches!(
        restored
            .api
            .api_stats_v1(TorrentIdOrHash::Id(ids[1]))
            .unwrap()
            .state,
        librqbit::TorrentStatsState::Live
    ));
    assert!(matches!(
        restored
            .api
            .api_stats_v1(TorrentIdOrHash::Id(ids[2]))
            .unwrap()
            .state,
        librqbit::TorrentStatsState::Paused
    ));
    restored.api.session().stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watched_folder_retries_partial_files_and_ignores_old_files() {
    let root = tempfile::tempdir().unwrap();
    let creator = Core::new(root.path().join("creator"), true).await.unwrap();
    let source = root.path().join("source.txt");
    std::fs::write(&source, b"watch fixture").unwrap();
    let torrent = root.path().join("test.torrent");
    creator
        .create(
            source.to_string_lossy().into(),
            torrent.to_string_lossy().into(),
            true,
            "".into(),
            vec!["http://127.0.0.1:1/announce".into()],
        )
        .await
        .unwrap();
    let watch = root.path().join("watch");
    let download = root.path().join("downloads");
    std::fs::create_dir(&watch).unwrap();
    std::fs::create_dir(&download).unwrap();
    std::fs::copy(&torrent, watch.join("existing.torrent")).unwrap();
    let core = Core::new(root.path().join("app"), true).await.unwrap();
    {
        let mut p = core.profile.lock().await;
        p.prefs.watch_folder = watch.to_string_lossy().into();
        p.prefs.auto_add_torrents = true;
        p.prefs.download_path = download.to_string_lossy().into();
    }
    let notices = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let n = notices.clone();
    let task = tokio::spawn(crate::watcher::run(core.clone(), move |message| {
        n.lock().unwrap().push(message)
    }));
    tokio::time::sleep(Duration::from_millis(750)).await;
    assert!(core.api.api_torrent_list().torrents.is_empty());
    let target = watch.join("copied.torrent");
    std::fs::write(&target, b"d4:info").unwrap();
    // Stay incomplete beyond the stability interval to exercise parse retry.
    tokio::time::sleep(Duration::from_millis(3250)).await;
    assert!(core.api.api_torrent_list().torrents.is_empty());
    std::fs::copy(&torrent, &target).unwrap();
    timeout(Duration::from_secs(15), async {
        loop {
            if core.api.api_torrent_list().torrents.len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        notices.lock().unwrap().is_empty(),
        "Incomplete copies must not report a premature error"
    );
    core.profile.lock().await.prefs.auto_add_torrents = false;
    tokio::time::sleep(Duration::from_millis(500)).await;
    task.abort();
    let _ = task.await;
    core.api.session().stop().await;
    creator.api.session().stop().await;
}

/// Opt-in network smoke test. Normal tests remain completely local.
/// Set TORRENTCAT_SMOKE_TORRENT_URL to an official freely redistributable
/// torrent URL. Stops after 2 MiB of verified data or a 60-second deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit public-network smoke test"]
async fn public_swarm_download_smoke() {
    let url =
        std::env::var("TORRENTCAT_SMOKE_TORRENT_URL").expect("Set an official public torrent URL");
    let root = tempfile::tempdir().unwrap();
    let download = root.path().join("downloads");
    std::fs::create_dir(&download).unwrap();
    let core = Core::new(root.path().join("profile"), false).await.unwrap();
    let outcome = timeout(Duration::from_secs(60), async {
        let made = core
            .add(url, Some(download.to_string_lossy().into()), false, false)
            .await?;
        let id = made["id"].as_u64().unwrap() as usize;
        let handle = core.api.mgr_handle(TorrentIdOrHash::Id(id))?;
        loop {
            let stats = handle.stats();
            if stats.progress_bytes >= 2 * 1024 * 1024 {
                println!(
                    "Public swarm: {} verified bytes of {} total bytes",
                    stats.progress_bytes, stats.total_bytes
                );
                return Ok::<_, anyhow::Error>(stats.progress_bytes);
            }
            if let Some(error) = stats.error {
                anyhow::bail!("{error}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    core.api.session().stop().await;
    assert!(outcome.unwrap().unwrap() >= 2 * 1024 * 1024);
}
