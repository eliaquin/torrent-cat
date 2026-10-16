use std::{io::IoSliceMut, time::Duration};

use crate::{Error, Result};
use crate::{
    peer_connection::with_timeout, type_aliases::BoxAsyncReadVectored,
    vectored_traits::AsyncReadVectoredExt,
};
use futures::TryFutureExt;
use peer_binary_protocol::{Handshake, Message, MessageDeserializeError};
use tokio::io::AsyncReadExt;

const BUFLEN: usize = 0x8000; // baseline for blocks and extensions
// A v1 metainfo of at most 32 MiB cannot contain more SHA-1 piece hashes.
pub(crate) const MAX_BITFIELD_BYTES: usize = (32usize * 1024 * 1024 / 20).div_ceil(8);

/// A ringbuffer for reading bittorrent messages from socket.
/// Messages may thus span 2 slices (notably, Piece message and UtMetadata messages), which is reflected in their contents.
pub struct ReadBuf {
    buf: Vec<u8>,
    bitfield_bytes: Option<usize>,
    start: usize,
    len: usize,
}

/// Advance by N bytes. A macro so that existing field-level
/// borrows are understood by the borrow checker.
macro_rules! advance {
    ($self:expr, $len:expr) => {
        $self.len = $self.len.saturating_sub($len);
        $self.start = ($self.start + $len) % $self.buf.len();
    };
}

/// Convert readbuf into 2 slices. A macro so that field-level
/// borrows are understood by the borrow checker.
macro_rules! as_slices {
    ($self:expr) => {{
        let (first, second) = $self.as_slice_ranges();
        (&$self.buf[first], &$self.buf[second])
    }};
}

impl ReadBuf {
    pub fn new() -> Self {
        Self {
            buf: vec![0u8; BUFLEN],
            bitfield_bytes: None,
            start: 0,
            len: 0,
        }
    }

    pub(crate) fn set_bitfield_bytes(&mut self, bytes: Option<usize>) -> Result<()> {
        if bytes.is_some_and(|n| n > MAX_BITFIELD_BYTES) {
            return Err(Error::Anyhow(anyhow::anyhow!(
                "bitfield exceeds metainfo limit"
            )));
        }
        self.bitfield_bytes = bytes;
        Ok(())
    }

    // Inspect only five bytes before deciding whether a frame may grow the
    // buffer. A huge declared length for a control/extension never allocates it.
    fn prepare_frame(&mut self) -> Result<()> {
        if self.len < 5 {
            return Ok(());
        }
        let byte = |n| self.buf[(self.start + n) % self.buf.len()];
        let prefix = u32::from_be_bytes([byte(0), byte(1), byte(2), byte(3)]) as usize;
        if prefix == 0 {
            return Ok(());
        }
        let id = byte(4);
        let total = prefix
            .checked_add(4)
            .ok_or_else(|| Error::Anyhow(anyhow::anyhow!("frame size overflow")))?;
        let limit = match id {
            5 => {
                if let Some(expected) = self.bitfield_bytes
                    && prefix != expected + 1
                {
                    return Err(Error::Anyhow(anyhow::anyhow!(
                        "bitfield length differs from torrent"
                    )));
                }
                self.bitfield_bytes.unwrap_or(MAX_BITFIELD_BYTES) + 5
            }
            7 => 16384 + 13,
            20 => BUFLEN,
            _ => 17,
        };
        if total > limit {
            return Err(Error::Anyhow(anyhow::anyhow!(
                "peer frame length {total} exceeds limit {limit}"
            )));
        }
        if total > self.buf.len() {
            let mut grown = vec![0; total];
            let (first, second) = as_slices!(self);
            grown[..first.len()].copy_from_slice(first);
            grown[first.len()..self.len].copy_from_slice(second);
            self.buf = grown;
            self.start = 0;
        }
        Ok(())
    }

    // Read the BT handshake.
    // This MUST be run as the first operation on the buffer.
    pub async fn read_handshake(
        &mut self,
        conn: &mut BoxAsyncReadVectored,
        timeout: Duration,
    ) -> Result<Handshake> {
        self.len = with_timeout(
            "reading",
            timeout,
            conn.read(&mut self.buf).map_err(Error::ReadHandshake),
        )
        .await?;
        if self.len == 0 {
            return Err(Error::PeerDisconnectedReadingHandshake);
        }
        let (h, size) =
            Handshake::deserialize(&self.buf[..self.len]).map_err(Error::DeserializeHandshake)?;
        self.advance(size);
        Ok(h)
    }

    fn is_contiguous(&self) -> bool {
        self.start + self.len == (self.start + self.len) % self.buf.len()
    }

    // In rare cases, we might need to make the buffer contiguous, as the message
    // parsing code won't work with a split ringbuffer. Only "bitfield" and "extended" messages
    // need contiguous buffers.
    // "Bitfield" however is always sent early, so in practice it won't ever happen. For "extended"
    // in practice, it would rarely happen with UtMetadata::Data messages if the split happens to land
    // in the middle of bencoded data (as we only can deserialize bencode from a single slice).
    fn make_contiguous(&mut self) -> Result<()> {
        if self.is_contiguous() {
            return Err(Error::BugReadBufMakeContiguous {
                start: self.start,
                len: self.len,
            });
        }

        // This should be so rare that it's not worth writing complex in-place code.
        // See the source in VecDequeue::make_contiguous to see how involved it would be
        // otherwise.
        let mut new = vec![0u8; self.buf.len()];
        let (first, second) = as_slices!(self);
        new[..first.len()].copy_from_slice(first);
        new[first.len()..first.len() + second.len()].copy_from_slice(second);
        self.buf = new;
        self.start = 0;
        Ok(())
    }

    /// Convert into 2 slices (as ranges).
    fn as_slice_ranges(&self) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
        // These .min() calls are for asm to be branchless and the code panicless.
        let len = self.len.min(self.buf.len());
        let start = self.start.min(self.buf.len());

        let first_len = len.min(self.buf.len() - start);
        let first = start..start + first_len;
        let second = 0..len.saturating_sub(first_len);
        (first, second)
    }

    fn advance(&mut self, len: usize) {
        advance!(self, len);
    }

    fn is_full(&self) -> bool {
        self.len == self.buf.len()
    }

    /// Get a part of the buffer for reading into.
    fn unfilled_ioslices(&mut self) -> [IoSliceMut<'_>; 2] {
        let write_start = (self.start.saturating_add(self.len)) % self.buf.len();
        let available_len = self.buf.len().saturating_sub(self.len);
        let first_len = (self.buf.len().saturating_sub(write_start)).min(available_len);
        let second_len = available_len.saturating_sub(first_len);

        let (second, first) = self.buf.split_at_mut(write_start);

        let first_len = first_len.min(first.len());
        let second_len = second_len.min(second.len());
        [
            IoSliceMut::new(&mut first[..first_len]),
            IoSliceMut::new(&mut second[..second_len]),
        ]
    }

    /// Read a message into the buffer, try to deserialize it and call the callback on it.
    pub async fn read_message(
        &mut self,
        conn: &mut BoxAsyncReadVectored,
        timeout: Duration,
    ) -> Result<Message<'_>> {
        loop {
            self.prepare_frame()?;
            let err = {
                // A workaround for borrow-checker not understanding early returns.
                //
                // A stacked reborrow of self. After this block we either return from
                // the function, or the block returns an owned error.
                //
                // Safety: there's nothing inherently unsafe here, this is just a
                // borrow checker workaround. However to ensure we respect aliasing
                // rules (&mut references are exclusive), there's a miri test below.
                //
                // In this case, 2 &mut references DO exist lexically, but in reality the
                // latter one is a reborrow that ends with the block.
                //
                // Run with `cargo +nightly miri test -p librqbit --features miri test_read_buf_miri -- --nocapture --ignore`
                let this = unsafe { &mut *(self as *mut Self) };
                let (first, second) = as_slices!(this);
                match Message::deserialize(first, second) {
                    Ok((msg, size)) => {
                        advance!(this, size);
                        return Ok(msg);
                    }
                    Err(e) => e,
                }
            };

            match err {
                MessageDeserializeError::NotEnoughData(need_additional_bytes, ..) => {
                    if self.is_full() {
                        return Err(Error::ReadBufFull {
                            need_additional_bytes,
                        });
                    }
                    let size = with_timeout(
                        "reading",
                        timeout,
                        conn.read_vectored(&mut self.unfilled_ioslices())
                            .map_err(Error::Read),
                    )
                    .await?;
                    if size == 0 {
                        return Err(Error::PeerDisconnected);
                    }
                    self.len += size;
                    // Revalidate after every read, including fragmented headers.
                }
                MessageDeserializeError::NeedContiguous => {
                    self.make_contiguous()?;
                    continue;
                }
                e => return Err(Error::Deserialize(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use librqbit_core::constants::CHUNK_SIZE;
    use peer_binary_protocol::{
        MAX_MSG_LEN, Message, Piece,
        extended::{
            ExtendedMessage, PeerExtendedMessageIds,
            ut_metadata::{UtMetadata, UtMetadataData},
        },
    };
    use std::{net::Ipv4Addr, task::Poll, time::Duration};
    use tokio::io::{AsyncRead, AsyncWriteExt};

    use crate::{
        tests::test_util::setup_test_logging, type_aliases::BoxAsyncReadVectored,
        vectored_traits::AsyncReadVectored,
    };

    use super::{BUFLEN, ReadBuf};

    #[tokio::test]
    async fn fragmented_large_frame_and_following_control_survive_ring_wrap() {
        use crate::vectored_traits::AsyncReadVectoredIntoCompat;
        struct BytesOneAtATime(std::vec::IntoIter<u8>);
        impl AsyncRead for BytesOneAtATime {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                out: &mut tokio::io::ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                if out.remaining() > 0 {
                    if let Some(byte) = self.0.next() {
                        out.put_slice(&[byte]);
                    }
                }
                Poll::Ready(Ok(()))
            }
        }
        let size = 32769usize;
        let mut source = ((size + 1) as u32).to_be_bytes().to_vec();
        source.push(5);
        source.extend(vec![0x55; size]);
        source.extend([0, 0, 0, 1, 1]);
        let mut source: BoxAsyncReadVectored =
            Box::new(BytesOneAtATime(source.into_iter()).into_vectored_compat());
        let mut rb = ReadBuf::new();
        rb.start = BUFLEN - 2;
        rb.set_bitfield_bytes(Some(size)).unwrap();
        match rb
            .read_message(&mut source, Duration::from_secs(1))
            .await
            .unwrap()
        {
            Message::Bitfield(bytes) => assert_eq!(bytes.as_ref(), vec![0x55; size]),
            _ => panic!("expected bitfield"),
        }
        assert!(matches!(
            rb.read_message(&mut source, Duration::from_secs(1))
                .await
                .unwrap(),
            Message::Unchoke
        ));
        for id in [0u8, 4, 5, 7, 20, 255] {
            let bytes = [u32::MAX.to_be_bytes().as_slice(), &[id]].concat();
            let mut source: BoxAsyncReadVectored =
                Box::new(BytesOneAtATime(bytes.into_iter()).into_vectored_compat());
            let mut rb = ReadBuf::new();
            let error = rb
                .read_message(&mut source, Duration::from_secs(1))
                .await
                .unwrap_err();
            assert!(error.to_string().contains("exceeds limit"), "{error}");
            assert_eq!(rb.buf.len(), BUFLEN);
        }
    }

    #[test]
    fn frame_limits_precede_allocation_and_growth_preserves_wrapped_data() {
        for size in [16495, 16500, 32763, 32768, 32769, super::MAX_BITFIELD_BYTES] {
            let mut rb = ReadBuf::new();
            rb.set_bitfield_bytes(Some(size)).unwrap();
            rb.start = BUFLEN - 2;
            let header = [&((size + 1) as u32).to_be_bytes()[..], &[5]].concat();
            for (i, byte) in header.iter().enumerate() {
                rb.buf[(rb.start + i) % BUFLEN] = *byte;
            }
            rb.len = 5;
            rb.prepare_frame().unwrap();
            assert_eq!(rb.buf.len(), BUFLEN.max(size + 5));
            let (a, b) = rb.as_slice_ranges();
            assert_eq!([&rb.buf[a], &rb.buf[b]].concat(), header);
        }
        for id in [0u8, 4, 5, 6, 7, 8, 20, 255] {
            let mut rb = ReadBuf::new();
            rb.buf[..4].copy_from_slice(&u32::MAX.to_be_bytes());
            rb.buf[4] = id;
            rb.len = 5;
            assert!(rb.prepare_frame().is_err());
            assert_eq!(rb.buf.len(), BUFLEN, "untrusted size must not allocate");
        }
        let mut rb = ReadBuf::new();
        rb.set_bitfield_bytes(Some(4)).unwrap();
        rb.buf[..5].copy_from_slice(&[0, 0, 0, 6, 5]);
        rb.len = 5;
        assert!(rb.prepare_frame().is_err());
        assert!(
            rb.set_bitfield_bytes(Some(super::MAX_BITFIELD_BYTES + 1))
                .is_err()
        );
    }

    #[test]
    fn test_ringbuf_ranges() {
        let mut b = ReadBuf::new();
        assert_eq!(b.as_slice_ranges(), (0..0, 0..0));

        b.start = 10;
        b.len = 10;
        assert_eq!(b.as_slice_ranges(), (10..20, 0..0));

        b.start = BUFLEN - 100;
        b.len = 100;
        assert_eq!(b.as_slice_ranges(), (BUFLEN - 100..BUFLEN, 0..0));

        b.start = BUFLEN - 100;
        b.len = 120;
        assert_eq!(b.as_slice_ranges(), (BUFLEN - 100..BUFLEN, 0..20));

        b.start = BUFLEN - 100;
        b.len = BUFLEN;
        assert_eq!(b.as_slice_ranges(), (BUFLEN - 100..BUFLEN, 0..BUFLEN - 100));
    }

    #[test]
    fn test_ringbuf_advance() {
        let mut b = ReadBuf::new();

        b.start = 10;
        b.len = 10;
        assert_eq!(b.as_slice_ranges(), (10..20, 0..0));
        b.advance(5);
        assert_eq!(b.as_slice_ranges(), (15..20, 0..0));

        b.start = BUFLEN - 5;
        b.len = 10;
        assert_eq!(b.as_slice_ranges(), (BUFLEN - 5..BUFLEN, 0..5));
        b.advance(5);
        assert_eq!(b.as_slice_ranges(), (0..5, 0..0));
    }

    #[test]
    fn test_ringbuf_make_contiguous() {
        let mut b = ReadBuf::new();
        assert!(b.is_contiguous());
        assert!(b.make_contiguous().is_err());

        fn fill(buf: &mut [u8], value: u8) {
            for b in buf.iter_mut() {
                *b = value;
            }
        }

        fill(&mut b.buf[BUFLEN - 100..], 42);
        fill(&mut b.buf[..200], 43);
        b.start = BUFLEN - 100;
        b.len = 300;
        assert!(!b.is_contiguous());
        assert!(b.make_contiguous().is_ok());
        assert_eq!(b.len, 300);
        assert_eq!(b.start, 0);
        assert_eq!(&b.buf[..100], &[42u8; 100]);
        assert_eq!(&b.buf[100..300], &[43u8; 200]);
        assert_eq!(&b.buf[300..], &[0u8; BUFLEN - 300]);
    }

    #[test]
    fn test_unfilled_ioslices() {
        ReadBuf::new();

        fn offset(buf: &ReadBuf, s: *const u8, len: usize) -> std::ops::Range<usize> {
            if len == 0 {
                return 0..0;
            }
            let offset = unsafe { s.byte_offset_from(buf.buf.as_ptr()) } as usize;
            offset..offset + len
        }

        fn offsets(
            buf: &ReadBuf,
            ioslices: [(*const u8, usize); 2],
        ) -> [std::ops::Range<usize>; 2] {
            [
                offset(buf, ioslices[0].0, ioslices[0].1),
                offset(buf, ioslices[1].0, ioslices[1].1),
            ]
        }

        #[track_caller]
        fn assert_one(start: usize, len: usize, ranges: [std::ops::Range<usize>; 2]) {
            let mut b = ReadBuf::new();
            b.start = start;
            b.len = len;
            let [f, s] = b.unfilled_ioslices();
            let slices = [(f.as_ptr(), f.len()), (s.as_ptr(), s.len())];
            assert_eq!(offsets(&b, slices), ranges)
        }

        // full
        assert_one(0, BUFLEN, [0..0, 0..0]);
        assert_one(100, BUFLEN, [0..0, 0..0]);
        assert_one(BUFLEN, BUFLEN, [0..0, 0..0]);

        // start=0
        assert_one(0, 100, [100..BUFLEN, 0..0]);
        assert_one(0, BUFLEN - 100, [BUFLEN - 100..BUFLEN, 0..0]);
        assert_one(0, BUFLEN - 1, [BUFLEN - 1..BUFLEN, 0..0]);

        // start=N
        assert_one(100, 100, [200..BUFLEN, 0..100]);
        assert_one(100, BUFLEN - 100, [0..100, 0..0]);
        assert_one(100, BUFLEN - 101, [BUFLEN - 1..BUFLEN, 0..100]);

        // start=BUFLEN-1
        assert_one(BUFLEN - 1, 100, [99..BUFLEN - 1, 0..0]);
    }

    #[tokio::test]
    async fn can_read_long_metainfo_correctly() {
        setup_test_logging();
        let reader = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = reader.local_addr().unwrap().port();
        let mut writer = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap()
            .into_split()
            .1;

        const ITERATIONS: u32 = 4096;

        let reader = async {
            let reader = reader.accept().await.unwrap().0.into_split().0;
            let mut reader: BoxAsyncReadVectored = Box::new(reader);
            let mut rb = ReadBuf::new();

            for piece in 0..ITERATIONS {
                let msg = rb
                    .read_message(&mut reader, Duration::from_millis(1000))
                    .await
                    .unwrap();
                let utdata = match msg {
                    Message::Extended(ExtendedMessage::UtMetadata(UtMetadata::Data(utdata))) => {
                        utdata
                    }
                    other => panic!("expected utdata, got {other:?}"),
                };
                assert_eq!(utdata.len(), CHUNK_SIZE as usize);
                assert_eq!(utdata.piece(), piece);
                #[allow(clippy::cast_possible_truncation)]
                let expected_byte = { piece as u8 };
                let all_good = utdata
                    .as_double_buf()
                    .get()
                    .into_iter()
                    .flatten()
                    .all(|b| *b == expected_byte);
                if !all_good {
                    panic!("broken data");
                }
            }
        };

        let pext = PeerExtendedMessageIds::my();
        let writer = async {
            let mut sbuf = [0u8; MAX_MSG_LEN];
            let mut pbuf = [0u8; CHUNK_SIZE as usize];
            for piece in 0..ITERATIONS {
                #[allow(clippy::cast_possible_truncation)]
                for b in pbuf.iter_mut() {
                    *b = piece as u8;
                }
                let len = Message::Extended(ExtendedMessage::UtMetadata(UtMetadata::Data(
                    UtMetadataData::from_bytes(piece, CHUNK_SIZE * ITERATIONS, pbuf[..].into()),
                )))
                .serialize(&mut sbuf, &|| pext)
                .unwrap();
                writer.write_all(&sbuf[..len]).await.unwrap();
            }
        };

        tokio::join!(reader, writer);
    }

    /// A test to prove that unsafe usage in read_buf is not UB.
    #[test]
    #[ignore = "run with --features=miri only, doesn't work with tokio"]
    fn test_read_buf_miri() {
        struct BufWrap(std::vec::IntoIter<u8>);
        impl AsyncRead for BufWrap {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &mut tokio::io::ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                unimplemented!("don't need this")
            }
        }

        impl AsyncReadVectored for BufWrap {
            fn poll_read_vectored(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                vec: &mut [std::io::IoSliceMut<'_>],
            ) -> Poll<std::io::Result<usize>> {
                // Yield one byte at a time to ensure that all code paths are executed
                // (both NotEnoughData and Ok).
                let this = self.get_mut();
                let byte = match this.0.next() {
                    Some(byte) => byte,
                    None => return Poll::Ready(Ok(0)),
                };
                let target = vec.iter_mut().find(|s| !s.is_empty()).unwrap();
                target[0] = byte;
                Poll::Ready(Ok(1))
            }
        }

        let mut rb = ReadBuf::new();
        rb.start = BUFLEN - 15;

        let mut src = {
            let mut buf = vec![0u8; 64];
            let piece = Message::Piece(Piece::from_data(42, 43, b"hello"));
            let len = piece.serialize(&mut buf, &|| Default::default()).unwrap();
            buf.truncate(len);
            Box::new(BufWrap(buf.into_iter())) as BoxAsyncReadVectored
        };

        pollster::block_on(async {
            let msg = rb.read_message(&mut src, Duration::ZERO).await.unwrap();
            match msg {
                Message::Piece(p) => {
                    assert_eq!(p.index, 42);
                    assert_eq!(p.begin, 43);
                    assert_eq!(p.data(), (b"he".as_slice(), b"llo".as_slice()));
                }
                _ => unreachable!(),
            }
            assert!(matches!(
                rb.read_message(&mut src, Duration::ZERO)
                    .await
                    .expect_err("expected error"),
                crate::Error::PeerDisconnected,
            ));
        })
    }
}
