use std::{
    collections::{HashMap, hash_map::Entry},
    ffi::CStr,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use librqbit_core::{hash_id::Id20, spawn_utils::spawn_with_cancel};
use librqbit_dualstack_sockets::{BindDevice, UdpSocket};
use parking_lot::RwLock;
use rand::RngExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, debug_span, trace, warn};

const ACTION_CONNECT: u32 = 0;
const ACTION_ANNOUNCE: u32 = 1;
// const ACTION_SCRAPE: u32 = 2;
const ACTION_ERROR: u32 = 3;

pub const EVENT_NONE: u32 = 0;
pub const EVENT_COMPLETED: u32 = 1;
pub const EVENT_STARTED: u32 = 2;
pub const EVENT_STOPPED: u32 = 3;

pub type ConnectionId = u64;
const CONNECTION_ID_MAGIC: ConnectionId = 0x41727101980;

pub type TransactionId = u32;

pub fn new_transaction_id() -> TransactionId {
    rand::rng().random()
}

#[derive(Debug)]
pub struct AnnounceFields {
    pub info_hash: Id20,
    pub peer_id: Id20,
    pub downloaded: u64,
    pub left: u64,
    pub uploaded: u64,
    pub event: u32,
    pub key: u32,
    pub port: u16,
}

#[derive(Debug)]
pub enum Request {
    Connect,
    Announce(ConnectionId, AnnounceFields),
}

impl Request {
    fn action(&self) -> u32 {
        match self {
            Self::Connect => ACTION_CONNECT,
            Self::Announce(..) => ACTION_ANNOUNCE,
        }
    }

    pub fn serialize(
        &self,
        transaction_id: TransactionId,
        buf: &mut [u8],
    ) -> anyhow::Result<usize> {
        struct W<'a> {
            buf: &'a mut [u8],
            offset: usize,
        }
        impl W<'_> {
            fn extend_from_slice(&mut self, s: &[u8]) -> anyhow::Result<()> {
                if self.buf.len() < self.offset + s.len() {
                    bail!("not enough space in buffer")
                }
                self.buf[self.offset..self.offset + s.len()].copy_from_slice(s);
                self.offset += s.len();
                Ok(())
            }
        }

        let mut w = W { buf, offset: 0 };

        match self {
            Request::Connect => {
                w.extend_from_slice(&CONNECTION_ID_MAGIC.to_be_bytes())?;
                w.extend_from_slice(&ACTION_CONNECT.to_be_bytes())?;
                w.extend_from_slice(&transaction_id.to_be_bytes())?;
            }
            Request::Announce(connection_id, fields) => {
                w.extend_from_slice(&connection_id.to_be_bytes())?;
                w.extend_from_slice(&ACTION_ANNOUNCE.to_be_bytes())?;
                w.extend_from_slice(&transaction_id.to_be_bytes())?;
                w.extend_from_slice(&fields.info_hash.0)?;
                w.extend_from_slice(&fields.peer_id.0)?;
                w.extend_from_slice(&fields.downloaded.to_be_bytes())?;
                w.extend_from_slice(&fields.left.to_be_bytes())?;
                w.extend_from_slice(&fields.uploaded.to_be_bytes())?;
                w.extend_from_slice(&fields.event.to_be_bytes())?;
                w.extend_from_slice(&0u32.to_be_bytes())?; // ip address 0
                w.extend_from_slice(&fields.key.to_be_bytes())?;
                w.extend_from_slice(&(-1i32).to_be_bytes())?; // num want -1
                w.extend_from_slice(&fields.port.to_be_bytes())?;
            }
        }
        Ok(w.offset)
    }
}

#[derive(Debug)]
pub struct AnnounceResponse {
    pub interval: u32,
    #[allow(dead_code)]
    pub leechers: u32,
    #[allow(dead_code)]
    pub seeders: u32,
    pub addrs: Vec<SocketAddr>,
}

#[derive(Debug)]
pub enum Response {
    Connect(ConnectionId),
    Announce(AnnounceResponse),
    #[allow(dead_code)]
    Error(String),
    Unknown,
}

fn split_slice(s: &[u8], first_len: usize) -> Option<(&[u8], &[u8])> {
    if s.len() < first_len {
        return None;
    }
    Some(s.split_at(first_len))
}

fn s_to_arr<const T: usize>(buf: &[u8]) -> [u8; T] {
    let mut arr = [0u8; T];
    arr.copy_from_slice(buf);
    arr
}

trait ParseNum: Sized {
    fn parse_num(buf: &[u8]) -> anyhow::Result<(Self, &[u8])>;
}

macro_rules! parse_impl {
    ($ty:tt, $size:expr) => {
        impl ParseNum for $ty {
            fn parse_num(buf: &[u8]) -> anyhow::Result<($ty, &[u8])> {
                let (bytes, rest) =
                    split_slice(buf, $size).with_context(|| format!("expected {} bytes", $size))?;
                let num = $ty::from_be_bytes(s_to_arr(bytes));
                Ok((num, rest))
            }
        }
    };
}

parse_impl!(u32, 4);
parse_impl!(u64, 8);
parse_impl!(u128, 16);
parse_impl!(u16, 2);
parse_impl!(i32, 4);
parse_impl!(i64, 8);
parse_impl!(i16, 2);

impl Response {
    pub fn parse(buf: &[u8], is_ipv6: bool) -> anyhow::Result<(TransactionId, Self)> {
        let (action, buf) = u32::parse_num(buf).context("can't parse action")?;
        let (tid, buf) = u32::parse_num(buf).context("can't parse transaction id")?;

        let response = match Self::parse_response(action, is_ipv6, buf) {
            Ok(r) => r,
            Err(e) => {
                debug!("error parsing: {e:#}");
                Response::Unknown
            }
        };

        Ok((tid, response))
    }

    fn parse_response(action: u32, is_ipv6: bool, mut buf: &[u8]) -> anyhow::Result<Self> {
        let response = match action {
            ACTION_CONNECT => {
                let (connection_id, b) =
                    u64::parse_num(buf).context("can't parse connection id")?;
                buf = b;
                Response::Connect(connection_id)
            }
            ACTION_ANNOUNCE => {
                let (interval, b) = u32::parse_num(buf).context("can't parse interval")?;
                let (leechers, b) = u32::parse_num(b).context("can't parse leechers")?;
                let (seeders, mut b) = u32::parse_num(b).context("can't parse seeders")?;
                let mut addrs = Vec::new();
                while !b.is_empty() {
                    let (addr, b2) = if is_ipv6 {
                        let (ip, b2) = u128::parse_num(b)?;
                        (IpAddr::V6(Ipv6Addr::from(ip)), b2)
                    } else {
                        let (ip, b2) = u32::parse_num(b)?;
                        (IpAddr::V4(Ipv4Addr::from(ip)), b2)
                    };
                    b = b2;

                    let (port, b2) = u16::parse_num(b)?;
                    b = b2;
                    addrs.push(SocketAddr::new(addr, port));
                }
                buf = b;
                Response::Announce(AnnounceResponse {
                    interval,
                    leechers,
                    seeders,
                    addrs,
                })
            }
            ACTION_ERROR => {
                let msg = CStr::from_bytes_with_nul(buf)
                    .ok()
                    .and_then(|s| s.to_str().ok())
                    .or_else(|| std::str::from_utf8(buf).ok())
                    .unwrap_or("<invalid UTF-8>")
                    .to_owned();
                return Ok(Response::Error(msg));
            }
            _ => bail!("unsupported action {action}"),
        };

        if !buf.is_empty() {
            bail!(
                "parsed {response:?} so far, but got {} remaining bytes",
                buf.len()
            );
        }

        Ok(response)
    }
}

struct ConnectionIdMeta {
    id: ConnectionId,
    created: Instant,
}

#[derive(Default)]
struct ClientLocked {
    connections: HashMap<SocketAddr, ConnectionIdMeta>,
    transactions: HashMap<TransactionId, PendingTransaction>,
}

struct PendingTransaction {
    endpoint: SocketAddr,
    action: u32,
    // Keep the slot reserved until its guard drops, even after delivering a
    // response. Otherwise a new request could reuse the ID before the old guard
    // removes it and inadvertently cancel that new request.
    sender: Option<tokio::sync::oneshot::Sender<Response>>,
}

impl ClientLocked {
    fn deliver(&mut self, tid: TransactionId, addr: SocketAddr, response: Response) {
        let Some(pending) = self.transactions.get_mut(&tid) else {
            return;
        };
        if pending.endpoint != addr
            || !matches!(
                (&response, pending.action),
                (Response::Connect(_), ACTION_CONNECT)
                    | (Response::Announce(_), ACTION_ANNOUNCE)
                    | (Response::Error(_), _)
            )
        {
            return;
        }
        if let Some(sender) = pending.sender.take() {
            let _ = sender.send(response);
        }
    }
}

struct ClientShared {
    sock: UdpSocket,
    locked: RwLock<ClientLocked>,
}

#[derive(Clone)]
pub struct UdpTrackerClient {
    state: Arc<ClientShared>,
}

struct TransactionIdGuard<'a> {
    tid: TransactionId,
    state: &'a ClientShared,
}

impl Drop for TransactionIdGuard<'_> {
    fn drop(&mut self) {
        let mut g = self.state.locked.write();
        g.transactions.remove(&self.tid);
    }
}

impl UdpTrackerClient {
    pub async fn new(
        cancel_token: CancellationToken,
        bind_device: Option<&BindDevice>,
    ) -> anyhow::Result<Self> {
        let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);
        let sock = UdpSocket::bind_udp(
            addr,
            librqbit_dualstack_sockets::BindOpts {
                device: bind_device,
                ..Default::default()
            },
        )
        .with_context(|| format!("error creating UDP socket at {addr}"))?;

        let client = Self {
            state: Arc::new(ClientShared {
                sock,
                locked: RwLock::new(Default::default()),
            }),
        };

        spawn_with_cancel(debug_span!("udp_tracker"), "udp_tracker", cancel_token, {
            let client = client.clone();
            async move { client.run().await }
        });

        Ok(client)
    }

    async fn run(self) -> anyhow::Result<()> {
        let mut buf = [0u8; 16384];
        loop {
            let (len, addr) = match self.state.sock.recv_from(&mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    warn!("error in UdpSocket::recv_from: {e:#}");
                    continue;
                }
            };

            let (tid, response) = match Response::parse(&buf[..len], addr.is_ipv6()) {
                Ok(r) => r,
                Err(e) => {
                    debug!(?addr, "error parsing UDP response: {e:#}");
                    continue;
                }
            };

            trace!(?tid, ?addr, "received UDP tracker response");

            self.state.locked.write().deliver(tid, addr, response);
        }
    }

    async fn get_connection_id(&self, addr: SocketAddr) -> anyhow::Result<ConnectionId> {
        if let Some(m) = self.state.locked.read().connections.get(&addr)
            && m.created.elapsed() < Duration::from_secs(60)
        {
            return Ok(m.id);
        }

        let response = self.request(addr, Request::Connect).await?;
        match response {
            Response::Connect(connection_id) => {
                self.state.locked.write().connections.insert(
                    addr,
                    ConnectionIdMeta {
                        id: connection_id,
                        created: Instant::now(),
                    },
                );
                Ok(connection_id)
            }
            _ => anyhow::bail!("expected connect response"),
        }
    }

    async fn request(&self, addr: SocketAddr, request: Request) -> anyhow::Result<Response> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tid_g = self.reserve_transaction_id(addr, request.action(), tx)?;

        let mut write_buf = [0u8; 1024];
        let len = request.serialize(tid_g.tid, &mut write_buf)?;
        self.state
            .sock
            .send_to(&write_buf[..len], addr)
            .await
            .with_context(|| format!("error sending to {addr:?}"))?;

        let response = tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .context("timeout connecting")?
            .context("sender dead")?;
        match &response {
            Response::Error(e) => {
                anyhow::bail!("remote errored: {e}")
            }
            Response::Unknown => {
                anyhow::bail!("remote replied with something we could not parse")
            }
            _ => {}
        }
        Ok(response)
    }

    fn reserve_transaction_id(
        &self,
        endpoint: SocketAddr,
        action: u32,
        tx: tokio::sync::oneshot::Sender<Response>,
    ) -> anyhow::Result<TransactionIdGuard<'_>> {
        let mut g = self.state.locked.write();
        for _ in 0..10 {
            let t = new_transaction_id();
            match g.transactions.entry(t) {
                Entry::Occupied(_) => continue,
                Entry::Vacant(vac) => {
                    vac.insert(PendingTransaction {
                        endpoint: match endpoint {
                            SocketAddr::V6(v6) if v6.ip().to_ipv4_mapped().is_some() => {
                                SocketAddr::new(v6.ip().to_ipv4_mapped().unwrap().into(), v6.port())
                            }
                            other => other,
                        },
                        action,
                        sender: Some(tx),
                    });
                    return Ok(TransactionIdGuard {
                        tid: t,
                        state: &self.state,
                    });
                }
            }
        }
        bail!("cant generate transaction id")
    }

    pub async fn announce(
        &self,
        tracker: SocketAddr,
        fields: AnnounceFields,
    ) -> anyhow::Result<AnnounceResponse> {
        let connection_id = self.get_connection_id(tracker).await?;
        let request = Request::Announce(connection_id, fields);
        let response = self.request(tracker, request).await?;
        match response {
            Response::Announce(r) => Ok(r),
            other => bail!("unexpected response {other:?}, expected announce"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Write, str::FromStr};

    use librqbit_core::{hash_id::Id20, peer_id::generate_peer_id};

    use crate::tracker_comms_udp::{
        AnnounceFields, EVENT_NONE, Request, Response, new_transaction_id,
    };

    use super::{
        ACTION_ANNOUNCE, ACTION_CONNECT, ClientLocked, PendingTransaction, UdpTrackerClient,
    };
    use std::net::SocketAddr;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn invalid_datagrams_leave_transaction_available() {
        for endpoint in ["127.0.0.1:1000", "[::1]:1000"] {
            let addr: SocketAddr = endpoint.parse().unwrap();
            let (tx, mut rx) = tokio::sync::oneshot::channel();
            let mut state = ClientLocked::default();
            state.transactions.insert(
                7,
                PendingTransaction {
                    endpoint: addr,
                    action: ACTION_CONNECT,
                    sender: Some(tx),
                },
            );
            state.deliver(6, addr, Response::Connect(99)); // unknown ID
            let mut wrong = addr;
            wrong.set_port(1001);
            state.deliver(7, wrong, Response::Connect(99));
            wrong.set_ip(if addr.is_ipv4() {
                "127.0.0.2".parse().unwrap()
            } else {
                "::2".parse().unwrap()
            });
            wrong.set_port(1000);
            state.deliver(7, wrong, Response::Error("untrusted".into()));
            state.deliver(
                7,
                addr,
                Response::Announce(super::AnnounceResponse {
                    interval: 60,
                    leechers: 0,
                    seeders: 0,
                    addrs: vec![],
                }),
            );
            state.deliver(7, addr, Response::Unknown);
            assert!(matches!(
                rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            state.deliver(7, addr, Response::Connect(42));
            assert!(matches!(rx.await.unwrap(), Response::Connect(42)));
            assert!(
                state.transactions.contains_key(&7),
                "guard retains ID reservation"
            );
            state.deliver(7, addr, Response::Connect(99)); // duplicate is ignored
            state.transactions.remove(&7); // request/guard completion
            state.deliver(7, addr, Response::Connect(99)); // late response
            let (tx, mut rx) = tokio::sync::oneshot::channel();
            state.transactions.insert(
                7,
                PendingTransaction {
                    endpoint: wrong,
                    action: ACTION_ANNOUNCE,
                    sender: Some(tx),
                },
            );
            state.deliver(7, addr, Response::Error("old endpoint".into()));
            assert!(matches!(
                rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            state.deliver(7, wrong, Response::Error("expected tracker".into()));
            assert!(matches!(rx.await.unwrap(), Response::Error(_)));
        }
    }

    #[tokio::test]
    async fn loopback_ipv4_and_ipv6_reject_wrong_port_and_action() {
        for bind in ["127.0.0.1:0", "[::1]:0"] {
            let tracker = tokio::net::UdpSocket::bind(bind).await.unwrap();
            let impostor = tokio::net::UdpSocket::bind(bind).await.unwrap();
            let token = CancellationToken::new();
            let client = UdpTrackerClient::new(token.clone(), None).await.unwrap();
            let addr = tracker.local_addr().unwrap();
            let task = tokio::spawn(async move { client.request(addr, Request::Connect).await });
            let mut packet = [0; 1024];
            let (len, destination) = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                tracker.recv_from(&mut packet),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(len, 16);
            let tid = &packet[12..16];
            let response = |action: u32, payload: &[u8]| {
                [action.to_be_bytes().as_slice(), tid, payload].concat()
            };
            impostor
                .send_to(&response(ACTION_CONNECT, &99u64.to_be_bytes()), destination)
                .await
                .unwrap();
            tracker
                .send_to(&response(ACTION_ANNOUNCE, &[0; 12]), destination)
                .await
                .unwrap();
            tracker
                .send_to(&response(ACTION_CONNECT, &[0; 2]), destination)
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            assert!(
                !task.is_finished(),
                "unsolicited packets must not consume the request"
            );
            tracker
                .send_to(&response(ACTION_CONNECT, &42u64.to_be_bytes()), destination)
                .await
                .unwrap();
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(matches!(result, Response::Connect(42)));
            token.cancel();
        }
    }

    #[tokio::test]
    async fn reservation_survives_delivery_and_is_released_on_drop() {
        let token = CancellationToken::new();
        let client = UdpTrackerClient::new(token.clone(), None).await.unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let guard = client
            .reserve_transaction_id(
                "[::ffff:127.0.0.1]:1234".parse().unwrap(),
                ACTION_CONNECT,
                tx,
            )
            .unwrap();
        let tid = guard.tid;
        client.state.locked.write().deliver(
            tid,
            "127.0.0.1:1234".parse().unwrap(),
            Response::Connect(1),
        );
        assert!(matches!(rx.await.unwrap(), Response::Connect(1)));
        assert!(client.state.locked.read().transactions.contains_key(&tid));
        drop(guard);
        assert!(!client.state.locked.read().transactions.contains_key(&tid));
        let (tx, rx) = tokio::sync::oneshot::channel();
        let guard = client
            .reserve_transaction_id("127.0.0.1:1234".parse().unwrap(), ACTION_CONNECT, tx)
            .unwrap();
        drop(guard); // cancellation before any response
        assert!(rx.await.is_err());
        assert!(client.state.locked.read().transactions.is_empty());
        token.cancel();
    }

    #[test]
    fn test_parse_announce() {
        let b = include_bytes!("../resources/test/udp-tracker-announce-response.bin");
        let (tid, response) = Response::parse(b, false).unwrap();
        dbg!(tid, response);
    }

    #[ignore]
    #[tokio::test]
    async fn test_announce() {
        let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await.unwrap();
        sock.connect("opentor.net:6969").await.unwrap();

        let tid = new_transaction_id();
        let mut write_buf = [0u8; 16384];
        let mut read_buf = vec![0u8; 4096];

        let len = Request::Connect.serialize(tid, &mut write_buf).unwrap();

        sock.send(&write_buf[..len]).await.unwrap();

        let size = sock.recv(&mut read_buf).await.unwrap();

        let (rtid, response) = Response::parse(&read_buf[..size], false).unwrap();
        assert_eq!(tid, rtid);
        let connection_id = match response {
            Response::Connect(connection_id) => {
                dbg!(connection_id)
            }
            other => panic!("unexpected response {:?}", other),
        };

        let hash = Id20::from_str("775459190aa65566591634203f8d9f17d341f969").unwrap();

        let tid = new_transaction_id();
        let request = Request::Announce(
            connection_id,
            AnnounceFields {
                info_hash: hash,
                peer_id: generate_peer_id(b"-xx1234-"),
                downloaded: 0,
                left: 0,
                uploaded: 0,
                event: EVENT_NONE,
                key: 0, // whatever that is?
                port: 24563,
            },
        );
        let size = request.serialize(tid, &mut write_buf).unwrap();

        sock.send(&write_buf[..size]).await.unwrap();
        let size = sock.recv(&mut read_buf).await.unwrap();

        {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open("/tmp/proto.bin")
                .unwrap();
            f.write_all(&read_buf[..size]).unwrap();
        }

        dbg!(&read_buf[..size]);
        let (rtid, response) = Response::parse(&read_buf[..size], false).unwrap();
        assert_eq!(tid, rtid);
        match response {
            Response::Announce(r) => {
                dbg!(r);
            }
            other => panic!("unexpected response {:?}", other),
        }
    }
}
