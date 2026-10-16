//! A raw loopback peer exercises interest/cancel against the real socket writer.
use librqbit::{AddTorrent, AddTorrentOptions, ListenerOptions, Session, SessionOptions};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{Duration, timeout},
};
async fn message(socket: &mut TcpStream) -> Vec<u8> {
    let n = timeout(Duration::from_secs(3), socket.read_u32())
        .await
        .unwrap()
        .unwrap();
    assert!(n <= 20_000);
    let mut bytes = vec![0; n as usize];
    socket.read_exact(&mut bytes).await.unwrap();
    bytes
}
fn request(id: u8, index: u32) -> Vec<u8> {
    [
        13u32.to_be_bytes().as_slice(),
        &[id],
        &index.to_be_bytes(),
        &0u32.to_be_bytes(),
        &16384u32.to_be_bytes(),
    ]
    .concat()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interest_cancel_and_not_interested_control_queued_uploads() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("payload.bin");
    std::fs::write(&file, vec![42; 32768]).unwrap();
    let mut bytes = Vec::new();
    lava_torrent::torrent::v1::TorrentBuilder::new(&file, 16384)
        .set_num_threads(1)
        .build()
        .unwrap()
        .write_into(&mut bytes)
        .unwrap();
    let session = Session::new_with_opts(
        tmp.path().into(),
        SessionOptions {
            dht: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            listen: Some(ListenerOptions {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let h = session
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
    h.wait_until_initialized().await.unwrap();
    session.unpause(&h).await.unwrap();
    session
        .ratelimits
        .set_upload_bps(std::num::NonZeroU32::new(16384));
    let mut peer = TcpStream::connect(session.listen_addr().unwrap())
        .await
        .unwrap();
    peer.write_all(
        &[
            &[19][..],
            b"BitTorrent protocol",
            &[0; 8],
            &h.info_hash().0,
            &[77; 20],
        ]
        .concat(),
    )
    .await
    .unwrap();
    let mut handshake = [0; 68];
    peer.read_exact(&mut handshake).await.unwrap();
    peer.write_all(&[0, 0, 0, 2, 5, 0]).await.unwrap(); // empty peer bitfield
    assert_eq!(message(&mut peer).await[0], 5); // seed bitfield
    assert!(
        timeout(Duration::from_millis(100), peer.read_u32())
            .await
            .is_err(),
        "no unconditional unchoke"
    );
    peer.write_all(&[0, 0, 0, 1, 2]).await.unwrap(); // Interested
    assert_eq!(message(&mut peer).await, [1]);
    peer.write_all(&request(6, 0)).await.unwrap();
    let piece = message(&mut peer).await;
    assert_eq!(piece[0], 7);
    assert_eq!(piece.len(), 16393);
    assert!(piece[9..].iter().all(|b| *b == 42));
    // The initial rate-limit burst is consumed. This request is queued while
    // cancel + NotInterested arrive, before another block can be sent.
    peer.write_all(&[request(6, 1), request(8, 1), vec![0, 0, 0, 1, 3]].concat())
        .await
        .unwrap();
    assert_eq!(message(&mut peer).await, [0]); // Choke
    assert!(
        timeout(Duration::from_millis(1100), peer.read_u32())
            .await
            .is_err(),
        "canceled block must not be sent after tokens refill"
    );
    peer.write_all(&[0, 0, 0, 1, 2]).await.unwrap();
    assert_eq!(message(&mut peer).await, [1]);
    peer.write_all(&request(6, 1)).await.unwrap();
    let piece = message(&mut peer).await;
    assert_eq!(piece[0], 7);
    assert_eq!(&piece[1..5], &1u32.to_be_bytes());
    drop(peer);
    session.stop().await;
}
