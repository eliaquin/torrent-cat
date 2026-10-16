//! Network input limits with synthetic metadata and loopback HTTP only.
use librqbit::{AddTorrent, AddTorrentOptions, Session, SessionOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn oversized_metadata_url_fails_and_a_valid_url_still_loads() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("payload");
    std::fs::write(&source, b"test payload").unwrap();
    let torrent = lava_torrent::torrent::v1::TorrentBuilder::new(&source, 16384)
        .set_num_threads(1)
        .build()
        .unwrap();
    let mut metadata = Vec::new();
    torrent.write_into(&mut metadata).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/metadata", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for valid in [false, true] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut req = [0; 4096];
            socket.read(&mut req).await.unwrap();
            let body = if valid { metadata.as_slice() } else { &[] };
            let len = if valid {
                metadata.len()
            } else {
                32 * 1024 * 1024 + 1
            };
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {len}\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(body).await.unwrap();
        }
    });
    let session = Session::new_with_opts(
        root.path().join("downloads"),
        SessionOptions {
            dht: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    for valid in [false, true] {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            session.add_torrent(
                AddTorrent::from_url(&url),
                Some(AddTorrentOptions {
                    paused: true,
                    list_only: true,
                    ..Default::default()
                }),
            ),
        )
        .await
        .unwrap();
        assert_eq!(result.is_ok(), valid);
        if let Err(error) = result {
            assert!(format!("{error:#}").contains("size limit"));
        }
    }
    server.await.unwrap();
    session.stop().await;
}
