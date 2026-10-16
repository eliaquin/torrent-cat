//! Exercise both bitfield directions and a verified download/upload over TCP.
use librqbit::{AddTorrent, AddTorrentOptions, ListenerOptions, Session, SessionOptions};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{Duration, timeout},
};
async fn message(peer: &mut TcpStream) -> Vec<u8> {
    let n = timeout(Duration::from_secs(5), peer.read_u32())
        .await
        .unwrap()
        .unwrap();
    assert!(n <= 210000);
    let mut body = vec![0; n as usize];
    timeout(Duration::from_secs(5), peer.read_exact(&mut body))
        .await
        .unwrap()
        .unwrap();
    body
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_bitfields_allow_download_then_seed_on_same_connection() {
    for count in [131960usize, 132000, 262104, 262144, 262152] {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("payload");
        let mut contents = vec![42; count];
        contents[count - 1] = 0;
        std::fs::write(&file, &contents).unwrap();
        // SHA-1 of the one-byte fixture '*'. Small pieces keep disk use small.
        let hash = [
            223, 88, 36, 140, 65, 79, 52, 44, 129, 224, 86, 180, 11, 238, 18, 209, 122, 8, 191, 97,
        ];
        let mut meta = format!(
            "d4:infod6:lengthi{count}e4:name7:payload12:piece lengthi1e6:pieces{}:",
            count * 20
        )
        .into_bytes();
        for _ in 0..count {
            meta.extend_from_slice(&hash);
        }
        meta.extend_from_slice(b"ee");
        let session = Session::new_with_opts(
            root.path().into(),
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
                AddTorrent::TorrentFileBytes(meta.into()),
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
        timeout(Duration::from_secs(20), h.wait_until_initialized())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(h.stats().progress_bytes, count as u64 - 1);
        session.unpause(&h).await.unwrap();
        let mut peer = TcpStream::connect(session.listen_addr().unwrap())
            .await
            .unwrap();
        peer.write_all(
            &[
                &[19][..],
                b"BitTorrent protocol",
                &[0; 8],
                &h.info_hash().0,
                &[91; 20],
            ]
            .concat(),
        )
        .await
        .unwrap();
        peer.read_exact(&mut [0; 68]).await.unwrap();
        let bitfield = message(&mut peer).await;
        assert_eq!(bitfield.len(), count.div_ceil(8) + 1);
        assert_eq!(bitfield[0], 5);
        assert!(bitfield[1..bitfield.len() - 1].iter().all(|b| *b == 255));
        assert_eq!(*bitfield.last().unwrap(), 254);
        let mut advertised = vec![0; count.div_ceil(8) + 5];
        advertised[..4].copy_from_slice(&((count.div_ceil(8) + 1) as u32).to_be_bytes());
        advertised[4] = 5;
        *advertised.last_mut().unwrap() = 1;
        for bytes in advertised.chunks(997) {
            peer.write_all(bytes).await.unwrap();
        }
        peer.write_all(&[0, 0, 0, 1, 1]).await.unwrap(); // unchoke
        loop {
            let m = message(&mut peer).await;
            if m.first() != Some(&6) {
                continue;
            }
            assert_eq!(&m[1..5], &((count - 1) as u32).to_be_bytes());
            assert_eq!(&m[9..13], &1u32.to_be_bytes());
            let piece = [&10u32.to_be_bytes()[..], &[7], &m[1..9], &[42]].concat();
            peer.write_all(&piece).await.unwrap();
            break;
        }
        timeout(Duration::from_secs(5), h.wait_until_completed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), vec![42; count]);
        // Upload from the now-verified piece; ensure the connection is usable.
        peer.write_all(&[0, 0, 0, 1, 2]).await.unwrap();
        loop {
            if message(&mut peer).await == [1] {
                break;
            }
        }
        peer.write_all(
            &[
                &13u32.to_be_bytes()[..],
                &[6],
                &((count - 1) as u32).to_be_bytes(),
                &0u32.to_be_bytes(),
                &1u32.to_be_bytes(),
            ]
            .concat(),
        )
        .await
        .unwrap();
        loop {
            let m = message(&mut peer).await;
            if m.first() == Some(&7) {
                assert_eq!(&m[9..], &[42]);
                break;
            }
        }
        drop(peer);
        session.stop().await;
        assert_eq!(h.stats().uploaded_bytes, 1);
    }
}
