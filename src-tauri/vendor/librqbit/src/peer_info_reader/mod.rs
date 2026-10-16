// Modified for TorrentCat: disable PEX while privacy is unknown.
use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;
use bencode::from_bytes;
use buffers::{ByteBuf, ByteBufOwned};
use bytes::Bytes;
use librqbit_core::{
    constants::CHUNK_SIZE,
    hash_id::Id20,
    lengths::{ChunkInfo, last_element_size},
    torrent_metainfo::TorrentMetaV1Info,
};
use parking_lot::{Mutex, RwLock};
use peer_binary_protocol::{
    Handshake, Message,
    extended::{
        ExtendedMessage,
        handshake::ExtendedHandshake,
        ut_metadata::{UtMetadata, UtMetadataData},
    },
};
use sha1w::{ISha1, Sha1};
use tracing::trace;

use crate::{
    peer_connection::{
        PeerConnection, PeerConnectionHandler, PeerConnectionOptions, WriterRequest,
    },
    spawn_utils::BlockingSpawner,
    stream_connect::{ConnectionKind, StreamConnector},
};

pub(crate) async fn read_metainfo_from_peer(
    addr: SocketAddr,
    peer_id: Id20,
    info_hash: Id20,
    peer_connection_options: Option<PeerConnectionOptions>,
    spawner: BlockingSpawner,
    connector: Arc<StreamConnector>,
    client_name_and_version: String,
) -> anyhow::Result<TorrentAndInfoBytes> {
    let (result_tx, result_rx) = tokio::sync::oneshot::channel::<
        Result<(TorrentMetaV1Info<ByteBufOwned>, ByteBufOwned), bencode::DeserializeError>,
    >();
    let (writer_tx, writer_rx) = crate::peer_connection::writer_channel();
    let handler = Handler {
        addr,
        info_hash,
        writer_tx,
        result_tx: Mutex::new(Some(result_tx)),
        locked: RwLock::new(HandlerState::default()),
        client_name_and_version,
    };
    let connection = PeerConnection::new(
        addr,
        info_hash,
        peer_id,
        handler,
        peer_connection_options,
        spawner,
        connector,
    );

    let result_reader = result_rx;
    let (_, brx) = tokio::sync::broadcast::channel(1);
    let connection_runner = async move { connection.manage_peer_outgoing(writer_rx, brx).await };

    tokio::select! {
        result = result_reader => Ok(result??),
        whatever = connection_runner => match whatever {
            Ok(_) => anyhow::bail!("connection runner completed first"),
            Err(e) => Err(e.into())
        }
    }
}

#[derive(Default)]
struct HandlerLocked {
    metadata_size: u32,
    total_pieces: usize,
    next_request: usize,
    buffer: Vec<u8>,
    received_pieces: Vec<bool>,
}

impl HandlerLocked {
    fn new(metadata_size: u32) -> anyhow::Result<Self> {
        if metadata_size == 0 || metadata_size > 32 * 1024 * 1024 {
            anyhow::bail!("metadata size {} is too big", metadata_size);
        }
        let buffer = vec![0u8; metadata_size as usize];
        let total_pieces: usize = (metadata_size as u64)
            .div_ceil(CHUNK_SIZE as u64)
            .try_into()?;
        let received_pieces = vec![false; total_pieces];
        Ok(Self {
            metadata_size,
            received_pieces,
            buffer,
            total_pieces,
            next_request: total_pieces.min(64),
        })
    }
    fn piece_size(&self, index: u32) -> usize {
        if index as usize == self.total_pieces - 1 {
            last_element_size(self.metadata_size as u64, CHUNK_SIZE as u64)
                .try_into()
                .unwrap()
        } else {
            CHUNK_SIZE as usize
        }
    }
    fn record_piece(
        &mut self,
        d: &UtMetadataData<ByteBuf>,
        info_hash: &Id20,
    ) -> anyhow::Result<bool> {
        let piece = d.piece();
        if d.total_size() != self.metadata_size {
            anyhow::bail!("metadata size changed during transfer");
        }
        if piece as usize >= self.next_request {
            anyhow::bail!("unsolicited metadata piece");
        }
        if piece as usize >= self.total_pieces {
            anyhow::bail!("wrong index");
        }
        let offset = (piece * CHUNK_SIZE) as usize;
        let size = self.piece_size(piece);
        if d.len() != size {
            anyhow::bail!(
                "expected length of piece {} to be {}, but got {}",
                piece,
                size,
                d.len()
            );
        }
        if self.received_pieces[piece as usize] {
            anyhow::bail!("already received piece {}", piece);
        }
        d.copy_to_slice(&mut self.buffer[offset..offset + d.len()]);
        self.received_pieces[piece as usize] = true;

        if self.received_pieces.iter().all(|p| *p) {
            // check metadata
            let mut hash = Sha1::new();
            hash.update(&self.buffer);
            if hash.finish() != info_hash.0 {
                anyhow::bail!("info checksum invalid");
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

pub type TorrentAndInfoBytes = (TorrentMetaV1Info<ByteBufOwned>, ByteBufOwned);

#[derive(Default)]
struct HandlerState {
    metadata_id: Option<u8>,
    metadata_size: Option<u32>,
    download: Option<HandlerLocked>,
    completed: bool,
}

struct Handler {
    addr: SocketAddr,
    info_hash: Id20,
    writer_tx: crate::peer_connection::WriterSender,
    result_tx: Mutex<
        Option<
            tokio::sync::oneshot::Sender<Result<TorrentAndInfoBytes, bencode::DeserializeError>>,
        >,
    >,
    locked: RwLock<HandlerState>,
    client_name_and_version: String,
}

impl PeerConnectionHandler for Handler {
    fn update_my_extended_handshake(
        &self,
        handshake: &mut ExtendedHandshake<ByteBuf>,
    ) -> anyhow::Result<()> {
        // Privacy is still unknown during metadata discovery; never offer peer exchange.
        handshake.m.ut_pex = Some(0);
        Ok(())
    }

    fn should_send_bitfield(&self) -> bool {
        false
    }

    fn serialize_bitfield_message_to_buf(&self, _buf: &mut [u8]) -> anyhow::Result<usize> {
        Ok(0)
    }

    fn on_handshake(&self, handshake: Handshake, _kind: ConnectionKind) -> anyhow::Result<()> {
        if !handshake.supports_extended() {
            anyhow::bail!(
                "this peer does not support extended handshaking, which is a prerequisite to download metadata"
            )
        }
        Ok(())
    }

    async fn on_received_message(&self, msg: Message<'_>) -> anyhow::Result<()> {
        trace!("{}: received message: {:?}", self.addr, msg);

        let Message::Extended(ExtendedMessage::UtMetadata(metadata)) = msg else {
            return Ok(());
        };
        let mut state = self.locked.write();
        match metadata {
            UtMetadata::Request(piece) => {
                self.writer_tx
                    .send(WriterRequest::UtMetadata(UtMetadata::Reject(piece)))?;
            }
            UtMetadata::Reject(_) => anyhow::bail!("peer rejected metadata request"),
            UtMetadata::Data(data) => {
                let inner = state
                    .download
                    .as_mut()
                    .context("metadata data before negotiation or after completion")?;
                let complete = inner.record_piece(&data, &self.info_hash)?;
                if !complete {
                    if inner.next_request < inner.total_pieces {
                        self.writer_tx
                            .send(WriterRequest::UtMetadata(UtMetadata::Request(
                                inner.next_request.try_into()?,
                            )))?;
                        inner.next_request += 1;
                    }
                    return Ok(());
                }
                let inner = state.download.take().context("metadata transfer missing")?;
                state.completed = true;
                let buf = Bytes::from(inner.buffer);
                let info = from_bytes::<TorrentMetaV1Info<ByteBuf>>(&buf)
                    .map(|i| {
                        use clone_to_owned::CloneToOwned;
                        i.clone_to_owned(Some(&buf))
                    })
                    .map_err(|e| e.into_kind())
                    .map(|i| (i, ByteBufOwned(buf)));
                self.result_tx
                    .lock()
                    .take()
                    .context("metadata result already delivered")?
                    .send(info)
                    .map_err(|_| anyhow::anyhow!("metadata consumer closed"))?;
            }
        }
        Ok(())
    }

    fn on_uploaded_bytes(&self, _bytes: u32) {}

    fn read_chunk(&self, _chunk: &ChunkInfo, _buf: &mut [u8]) -> anyhow::Result<()> {
        anyhow::bail!("the peer is not supposed to be requesting chunks")
    }

    fn on_extended_handshake(
        &self,
        extended_handshake: &ExtendedHandshake<ByteBuf>,
    ) -> anyhow::Result<()> {
        let mut state = self.locked.write();
        if let Some(id) = extended_handshake.m.ut_metadata {
            if id == 0 {
                anyhow::bail!("peer disabled metadata transfer");
            }
            state.metadata_id = Some(id);
        }
        if let Some(size) = extended_handshake.metadata_size {
            if size == 0 || size > 32 * 1024 * 1024 {
                anyhow::bail!("invalid metadata size");
            }
            if state.metadata_size.is_some_and(|previous| previous != size) {
                anyhow::bail!("peer changed metadata size");
            }
            state.metadata_size = Some(size);
        }
        // BEP 10 deltas may supply these fields separately. Do not restart a
        // transfer or enqueue its request window on repeated handshakes.
        if state.download.is_some() || state.completed || state.metadata_id.is_none() {
            return Ok(());
        }
        let Some(size) = state.metadata_size else {
            return Ok(());
        };
        let inner = HandlerLocked::new(size)?;
        for piece in 0..inner.next_request {
            self.writer_tx
                .send(WriterRequest::UtMetadata(UtMetadata::Request(
                    piece.try_into()?,
                )))?;
        }
        state.download = Some(inner);
        Ok(())
    }

    fn should_transmit_have(&self, _id: librqbit_core::lengths::ValidPieceIndex) -> bool {
        false
    }

    fn client_name_and_version(&self) -> &str {
        &self.client_name_and_version
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;
    use peer_binary_protocol::extended::ut_metadata::UtMetadataData;
    #[tokio::test]
    async fn large_metadata_uses_bounded_sliding_request_window() {
        let (tx, mut rx) = crate::peer_connection::writer_channel();
        let (result_tx, _result_rx) = tokio::sync::oneshot::channel();
        let handler = Handler {
            addr: "127.0.0.1:1".parse().unwrap(),
            info_hash: Id20::default(),
            writer_tx: tx,
            result_tx: Mutex::new(Some(result_tx)),
            locked: RwLock::new(HandlerState::default()),
            client_name_and_version: "test".into(),
        };
        let mut h = ExtendedHandshake::new();
        h.metadata_size = Some(16384 * 2048);
        handler.on_extended_handshake(&h).unwrap();
        assert_eq!(rx.len(), 64); // metadata does not require interest/unchoke
        while rx.try_recv().is_ok() {}
        handler
            .on_received_message(Message::Extended(ExtendedMessage::UtMetadata(
                UtMetadata::Data(UtMetadataData::from_bytes(
                    0,
                    16384 * 2048,
                    ByteBuf(&[0; 16384]),
                )),
            )))
            .await
            .unwrap();
        match rx.try_recv().unwrap() {
            WriterRequest::UtMetadata(UtMetadata::Request(64)) => {}
            other => panic!("unexpected refill: {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }
    #[tokio::test]
    async fn negotiation_is_incremental_and_invalid_sequences_are_errors() {
        let (tx, mut rx) = crate::peer_connection::writer_channel();
        let handler = Handler {
            addr: "127.0.0.1:1".parse().unwrap(),
            info_hash: Id20::default(),
            writer_tx: tx,
            result_tx: Mutex::new(None),
            locked: RwLock::new(HandlerState::default()),
            client_name_and_version: "test".into(),
        };
        let block = [0u8; 16384];
        let data = |piece, total| {
            Message::Extended(ExtendedMessage::UtMetadata(UtMetadata::Data(
                UtMetadataData::from_bytes(piece, total, ByteBuf(&block)),
            )))
        };
        assert!(handler.on_received_message(data(0, 32768)).await.is_err());
        let mut delta = ExtendedHandshake::new();
        delta.m.ut_metadata = None;
        delta.m.ut_pex = None;
        delta.metadata_size = Some(32768);
        handler.on_extended_handshake(&delta).unwrap();
        assert!(rx.try_recv().is_err());
        delta.metadata_size = None;
        delta.m.ut_metadata = Some(7);
        handler.on_extended_handshake(&delta).unwrap();
        assert_eq!(rx.len(), 2);
        while rx.try_recv().is_ok() {}
        handler.on_received_message(data(0, 32768)).await.unwrap();
        for _ in 0..100 {
            handler.on_extended_handshake(&delta).unwrap();
        }
        assert!(rx.try_recv().is_err());
        assert!(
            handler
                .locked
                .read()
                .download
                .as_ref()
                .unwrap()
                .received_pieces[0]
        );
        assert!(handler.on_received_message(data(1, 49152)).await.is_err());
        assert!(handler.on_received_message(data(0, 32768)).await.is_err());
        delta.metadata_size = Some(32769);
        assert!(handler.on_extended_handshake(&delta).is_err());
        delta.metadata_size = None;
        delta.m.ut_metadata = Some(0);
        assert!(handler.on_extended_handshake(&delta).is_err());
        assert!(HandlerLocked::new(0).is_err());
        assert!(HandlerLocked::new(32 * 1024 * 1024 + 1).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn premature_metadata_peer_cannot_abort_resolution_from_valid_peer() {
        use crate::dht_utils::{ReadMetainfoResult, read_metainfo_from_peer_receiver};
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let mut info = b"d6:lengthi1e4:name1:x12:piece lengthi16384e6:pieces20:".to_vec();
        info.extend_from_slice(&[0; 20]);
        info.extend_from_slice(b"e");
        let mut sha = Sha1::new();
        sha.update(&info);
        let hash = Id20::new(sha.finish());
        let bad = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let good = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addrs = vec![bad.local_addr().unwrap(), good.local_addr().unwrap()];
        let frame = |id: u8, data: &[u8]| {
            [
                &((data.len() + 2) as u32).to_be_bytes()[..],
                &[20, id],
                data,
            ]
            .concat()
        };
        let payload = [
            format!("d8:msg_typei1e5:piecei0e10:total_sizei{}ee", info.len()).as_bytes(),
            &info,
        ]
        .concat();
        let bad_frame = frame(3, &payload);
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let bad_task = tokio::spawn(async move {
            let (mut peer, _) = bad.accept().await.unwrap();
            let mut handshake = [0; 68];
            peer.read_exact(&mut handshake).await.unwrap();
            handshake[48..].fill(93);
            peer.write_all(&handshake).await.unwrap();
            peer.write_all(&bad_frame).await.unwrap();
            // Wait for rejection/EOF before letting the good peer complete.
            let mut ignored = Vec::new();
            peer.read_to_end(&mut ignored).await.unwrap();
            closed_tx.send(()).unwrap();
        });
        let len = info.len();
        let good_task = tokio::spawn(async move {
            let (mut peer, _) = good.accept().await.unwrap();
            let mut handshake = [0; 68];
            peer.read_exact(&mut handshake).await.unwrap();
            handshake[48..].fill(94);
            peer.write_all(&handshake).await.unwrap();
            closed_rx.await.unwrap();
            peer.write_all(&frame(
                0,
                format!("d1:md11:ut_metadatai7ee13:metadata_sizei{len}ee").as_bytes(),
            ))
            .await
            .unwrap();
            loop {
                let n = peer.read_u32().await.unwrap();
                assert!(n < 32768);
                let mut msg = vec![0; n as usize];
                peer.read_exact(&mut msg).await.unwrap();
                if msg.starts_with(&[20, 7]) {
                    break;
                }
            }
            peer.write_all(&frame(3, &payload)).await.unwrap();
            let mut ignored = Vec::new();
            let _ = peer.read_to_end(&mut ignored).await;
        });
        let connector = Arc::new(
            StreamConnector::new(crate::stream_connect::StreamConnectorArgs {
                enable_tcp: true,
                ..Default::default()
            })
            .await
            .unwrap(),
        );
        let found = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_metainfo_from_peer_receiver(
                Id20::new([1; 20]),
                hash,
                addrs,
                futures::stream::empty(),
                None,
                connector,
                "test".into(),
            ),
        )
        .await
        .unwrap();
        match found {
            ReadMetainfoResult::Found { info_bytes, .. } => assert_eq!(info_bytes.as_ref(), info),
            _ => panic!("valid peer must survive malformed peer"),
        }
        bad_task.await.unwrap();
        good_task.await.unwrap();
    }
}
