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
async fn seed_fixture() -> (
    tempfile::TempDir,
    std::sync::Arc<Session>,
    std::sync::Arc<librqbit::ManagedTorrent>,
) {
    seed_fixture_with_peers(vec![]).await
}
async fn seed_fixture_with_peers(
    peers: Vec<std::net::SocketAddr>,
) -> (
    tempfile::TempDir,
    std::sync::Arc<Session>,
    std::sync::Arc<librqbit::ManagedTorrent>,
) {
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
                initial_peers: Some(peers),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    h.wait_until_initialized().await.unwrap();
    session.unpause(&h).await.unwrap();
    (tmp, session, h)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interest_cancel_and_not_interested_control_queued_uploads() {
    let (_tmp, session, h) = seed_fixture().await;
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

fn extension(id: u8, body: &[u8]) -> Vec<u8> {
    let size = u32::try_from(body.len() + 2).unwrap();
    [&size.to_be_bytes()[..], &[20, id], body].concat()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn negotiated_ids_survive_deltas_and_disable_without_disconnect() {
    let (_tmp, session, h) = seed_fixture().await;
    let mut peer = TcpStream::connect(session.listen_addr().unwrap())
        .await
        .unwrap();
    peer.write_all(
        &[
            &[19][..],
            b"BitTorrent protocol",
            &[0, 0, 0, 0, 0, 16, 0, 0],
            &h.info_hash().0,
            &[78; 20],
        ]
        .concat(),
    )
    .await
    .unwrap();
    peer.read_exact(&mut [0; 68]).await.unwrap();
    assert_eq!(&message(&mut peer).await[..2], &[20, 0]);
    assert_eq!(message(&mut peer).await[0], 5);
    peer.write_all(&[0, 0, 0, 2, 5, 0]).await.unwrap();
    let request = extension(3, b"d8:msg_typei0e5:piecei0ee");
    for (update, id) in [
        (b"d1:md11:ut_metadatai7e6:ut_pexi8eee".as_slice(), 7),
        (b"d1:md6:ut_pexi9eee", 7),
        (b"d4:reqqi16ee", 7),
    ] {
        peer.write_all(&extension(0, update)).await.unwrap();
        peer.write_all(&request).await.unwrap();
        assert_eq!(&message(&mut peer).await[..2], &[20, id]);
    }
    peer.write_all(&extension(0, b"d1:md11:ut_metadatai0e6:ut_pexi0eee"))
        .await
        .unwrap();
    peer.write_all(&request).await.unwrap();
    assert!(
        timeout(Duration::from_millis(150), peer.read_u32())
            .await
            .is_err(),
        "disabled payload must not be encoded as a handshake"
    );
    peer.write_all(&extension(0, b"d1:md11:ut_metadatai12eee"))
        .await
        .unwrap();
    peer.write_all(&request).await.unwrap();
    assert_eq!(&message(&mut peer).await[..2], &[20, 12]);
    drop(peer);
    session.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_pex_handshakes_emit_a_single_update() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (_tmp, session, _h) = seed_fixture_with_peers(vec![listener.local_addr().unwrap()]).await;
    let (mut other, _) = timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut handshake = [0; 68];
    other.read_exact(&mut handshake).await.unwrap();
    handshake[20..28].fill(0);
    handshake[48..].fill(79);
    other.write_all(&handshake).await.unwrap();
    other
        .write_all(&[0, 0, 0, 2, 5, 0, 0, 0, 0, 1, 2])
        .await
        .unwrap();
    assert_eq!(message(&mut other).await[0], 5);
    assert_eq!(message(&mut other).await, [1]);
    let mut peer = TcpStream::connect(session.listen_addr().unwrap())
        .await
        .unwrap();
    handshake[25] = 16;
    handshake[48..].fill(80);
    peer.write_all(&handshake).await.unwrap();
    peer.read_exact(&mut [0; 68]).await.unwrap();
    assert_eq!(&message(&mut peer).await[..2], &[20, 0]);
    assert_eq!(message(&mut peer).await[0], 5);
    peer.write_all(&[0, 0, 0, 2, 5, 0]).await.unwrap();
    let update = extension(0, b"d1:md6:ut_pexi8eee");
    for _ in 0..100 {
        peer.write_all(&update).await.unwrap();
    }
    let len = timeout(Duration::from_secs(13), peer.read_u32())
        .await
        .unwrap()
        .unwrap();
    let mut packet = vec![0; len as usize];
    peer.read_exact(&mut packet).await.unwrap();
    assert_eq!(&packet[..2], &[20, 8]);
    let contact = match listener.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => [&a.ip().octets()[..], &a.port().to_be_bytes()].concat(),
        _ => unreachable!(),
    };
    assert!(packet.windows(contact.len()).any(|b| b == contact));
    assert!(
        timeout(Duration::from_millis(300), peer.read_u32())
            .await
            .is_err(),
        "repeated handshake created duplicate PEX workers"
    );
    drop(peer);
    drop(other);
    session.stop().await;
}
