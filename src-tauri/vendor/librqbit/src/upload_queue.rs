//! Admission budgets cover queued, rate-limited and writing payload requests.
use librqbit_core::lengths::ChunkInfo;
use parking_lot::Mutex;
use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    sync::Arc,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

pub(crate) const PER_PEER_REQUESTS: usize = 32;
pub(crate) const SESSION_REQUESTS: usize = 1024;

#[derive(Default, Debug)]
struct Pending {
    interested: bool,
    requests: HashMap<ChunkInfo, PendingRequest>,
}

#[derive(Debug)]
struct PendingRequest {
    cancel: Arc<CancellationToken>,
    _peer_permit: OwnedSemaphorePermit,
}

#[derive(Debug)]
pub(crate) struct PeerUploads {
    pending: Mutex<Pending>,
    slots: Arc<Semaphore>,
    session_slots: Arc<Semaphore>,
}
impl PeerUploads {
    pub fn new(session_slots: Arc<Semaphore>) -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(Pending::default()),
            slots: Arc::new(Semaphore::new(PER_PEER_REQUESTS)),
            session_slots,
        })
    }
    pub fn set_interested(&self, interested: bool) -> bool {
        let mut pending = self.pending.lock();
        let changed = pending.interested != interested;
        pending.interested = interested;
        if !interested {
            for (_, request) in pending.requests.drain() {
                request.cancel.cancel();
            }
        }
        changed
    }
    pub fn cancel(&self, chunk: &ChunkInfo) {
        if let Some(request) = self.pending.lock().requests.remove(chunk) {
            request.cancel.cancel();
        }
    }
    pub fn admit(self: &Arc<Self>, chunk: ChunkInfo) -> anyhow::Result<Option<UploadRequest>> {
        let mut pending = self.pending.lock();
        if !pending.interested || pending.requests.contains_key(&chunk) {
            return Ok(None);
        }
        // Saturation is transient local backpressure, not peer misbehavior.
        // Unadmitted legacy requests may be retried; never close the connection.
        let Ok(peer_permit) = self.slots.clone().try_acquire_owned() else {
            return Ok(None);
        };
        let Ok(session_permit) = self.session_slots.clone().try_acquire_owned() else {
            return Ok(None);
        };
        let cancel = Arc::new(CancellationToken::new());
        pending.requests.insert(
            chunk,
            PendingRequest {
                cancel: cancel.clone(),
                _peer_permit: peer_permit,
            },
        );
        Ok(Some(UploadRequest {
            chunk,
            cancel,
            owner: self.clone(),
            _session_permit: session_permit,
        }))
    }
}

#[derive(Debug)]
pub(crate) struct UploadRequest {
    pub chunk: ChunkInfo,
    pub cancel: Arc<CancellationToken>,
    owner: Arc<PeerUploads>,
    _session_permit: OwnedSemaphorePermit,
}
impl Drop for UploadRequest {
    fn drop(&mut self) {
        let mut pending = self.owner.pending.lock();
        if pending
            .requests
            .get(&self.chunk)
            .is_some_and(|request| Arc::ptr_eq(&request.cancel, &self.cancel))
        {
            pending.requests.remove(&self.chunk);
        }
    }
}

/// One block per ready peer per round. Payload admission, rather than the number
/// of input messages, bounds every queue including jobs already sent to writers.
pub(crate) struct FairQueue<T> {
    ready: VecDeque<SocketAddr>,
    peers: HashMap<SocketAddr, VecDeque<T>>,
}
impl<T> Default for FairQueue<T> {
    fn default() -> Self {
        Self {
            ready: VecDeque::new(),
            peers: HashMap::new(),
        }
    }
}
impl<T> FairQueue<T> {
    pub fn push(&mut self, peer: SocketAddr, item: T) {
        let queue = self.peers.entry(peer).or_default();
        if queue.is_empty() {
            self.ready.push_back(peer);
        }
        queue.push_back(item);
    }
    pub fn pop(&mut self) -> Option<T> {
        let peer = self.ready.pop_front()?;
        let queue = self.peers.get_mut(&peer).expect("ready peer has a queue");
        let item = queue.pop_front();
        if queue.is_empty() {
            self.peers.remove(&peer);
        } else {
            self.ready.push_back(peer);
        }
        item
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use librqbit_core::lengths::Lengths;
    fn chunk(index: u32) -> ChunkInfo {
        let lengths = Lengths::new(16384 * 2048, 16384).unwrap();
        lengths
            .chunk_info_from_received_data(lengths.validate_piece_index(index).unwrap(), 0, 16384)
            .unwrap()
    }
    #[test]
    fn duplicate_flood_and_slow_consumer_remain_bounded() {
        let budget = Arc::new(Semaphore::new(SESSION_REQUESTS));
        let peer = PeerUploads::new(budget.clone());
        assert!(
            peer.admit(chunk(0)).unwrap().is_none(),
            "choked before interest"
        );
        peer.set_interested(true);
        let mut held = vec![peer.admit(chunk(0)).unwrap().unwrap()];
        for _ in 0..100_000 {
            assert!(peer.admit(chunk(0)).unwrap().is_none());
        }
        assert_eq!(peer.pending.lock().requests.len(), 1);
        for i in 1..PER_PEER_REQUESTS as u32 {
            held.push(peer.admit(chunk(i)).unwrap().unwrap());
        }
        assert!(peer.admit(chunk(32)).unwrap().is_none());
        assert_eq!(
            budget.available_permits(),
            SESSION_REQUESTS - PER_PEER_REQUESTS
        );
        peer.set_interested(false);
        assert!(held.iter().all(|r| r.cancel.is_cancelled()));
        // Global retained-job capacity stays bounded until the queue drains.
        assert_eq!(
            budget.available_permits(),
            SESSION_REQUESTS - PER_PEER_REQUESTS
        );
        drop(held);
        assert_eq!(budget.available_permits(), SESSION_REQUESTS);
    }
    #[test]
    fn global_budget_and_cancel_rerequest_preserve_new_reservation() {
        let budget = Arc::new(Semaphore::new(2));
        let a = PeerUploads::new(budget.clone());
        let b = PeerUploads::new(budget.clone());
        a.set_interested(true);
        b.set_interested(true);
        let old = a.admit(chunk(0)).unwrap().unwrap();
        a.cancel(&chunk(0));
        let new = a.admit(chunk(0)).unwrap().unwrap();
        assert!(!new.cancel.is_cancelled());
        assert!(b.admit(chunk(0)).unwrap().is_none());
        drop(old);
        assert!(
            a.admit(chunk(0)).unwrap().is_none(),
            "old drop must not remove new entry"
        );
        let other = b.admit(chunk(0)).unwrap().unwrap();
        drop(new);
        drop(other);
        assert_eq!(budget.available_permits(), 2);
    }
    #[test]
    fn cancellation_releases_peer_slots_without_unbounding_retained_jobs() {
        let budget = Arc::new(Semaphore::new(SESSION_REQUESTS));
        let peer = PeerUploads::new(budget.clone());
        peer.set_interested(true);
        let mut old = Vec::new();
        let mut new = Vec::new();
        for i in 0..32 {
            old.push(peer.admit(chunk(i)).unwrap().unwrap());
        }
        for i in 0..32 {
            peer.cancel(&chunk(i));
        }
        for i in 32..64 {
            new.push(peer.admit(chunk(i)).unwrap().unwrap());
        }
        assert_eq!(peer.pending.lock().requests.len(), 32);
        assert_eq!(budget.available_permits(), SESSION_REQUESTS - 64);
        assert!(old.iter().all(|r| r.cancel.is_cancelled()));
        drop(old);
        drop(new);
        assert_eq!(budget.available_permits(), SESSION_REQUESTS);
    }

    #[test]
    fn ready_peers_receive_round_robin_service() {
        let a = "127.0.0.1:1".parse().unwrap();
        let b = "127.0.0.1:2".parse().unwrap();
        let c = "[::1]:3".parse().unwrap();
        let mut q = FairQueue::default();
        for i in 0..10 {
            q.push(a, (a, i));
        }
        q.push(b, (b, 0));
        q.push(c, (c, 0));
        q.push(b, (b, 1));
        for expected in [a, b, c, a, b, a] {
            assert_eq!(q.pop().unwrap().0, expected);
        }
        while q.pop().is_some() {}
        assert!(q.peers.is_empty());
        assert!(q.ready.is_empty());
    }
    #[test]
    fn slow_writer_control_queue_is_bounded() {
        let (tx, mut rx) = crate::peer_connection::writer_channel();
        for _ in 0..256 {
            tx.send(crate::peer_connection::WriterRequest::Message(
                peer_binary_protocol::Message::KeepAlive,
            ))
            .unwrap();
        }
        assert!(
            tx.send(crate::peer_connection::WriterRequest::Message(
                peer_binary_protocol::Message::KeepAlive
            ))
            .is_err()
        );
        assert_eq!(rx.len(), 256);
        rx.try_recv().unwrap();
        tx.send(crate::peer_connection::WriterRequest::Message(
            peer_binary_protocol::Message::KeepAlive,
        ))
        .unwrap();
    }
}
