use anyhow::Result;
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode},
    response::Response,
    routing::get,
};
use librqbit::{Api, api::TorrentIdOrHash};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt},
    sync::Mutex,
};

#[derive(Clone)]
pub enum Source {
    Torrent(usize, usize),
    Local(PathBuf, String),
    // A snapshot of verified bytes. Never use api_stream for preview requests:
    // registering a streaming reader would change download priorities.
    Preview { id: usize, path: PathBuf, length: u64, ranges: Vec<[u64; 2]> },
}
#[derive(Clone)]
pub struct Media {
    pub port: u16,
    grants: Arc<Mutex<HashMap<String, Source>>>,
    shutdown: tokio::sync::watch::Sender<bool>,
    task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}
#[derive(Clone)]
struct Server {
    api: Api,
    port: u16,
    grants: Arc<Mutex<HashMap<String, Source>>>,
    shutdown: tokio::sync::watch::Receiver<bool>,
}
impl Media {
    pub async fn start(api: Api) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let (shutdown, mut shutdown_rx) = tokio::sync::watch::channel(false);
        let media = Self {
            port: listener.local_addr()?.port(),
            grants: Default::default(),
            shutdown,
            task: Default::default(),
        };
        let router = Router::new()
            .route("/media/{token}", get(serve))
            .with_state(Server {
                api,
                port: media.port,
                grants: media.grants.clone(),
                shutdown: shutdown_rx.clone(),
            });
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = shutdown_rx.wait_for(|stop| *stop) => break,
                    _ = connections.join_next(), if !connections.is_empty() => {},
                    accepted = listener.accept() => {
                        let (socket, _) = match accepted {
                            Ok(accepted) => accepted,
                            Err(error) => { eprintln!("Media listener: {error}"); break; },
                        };
                        let service = hyper_util::service::TowerToHyperService::new(router.clone());
                        connections.spawn(async move {
                            let _ = hyper::server::conn::http1::Builder::new()
                                .serve_connection(hyper_util::rt::TokioIo::new(socket), service).await;
                        });
                    }
                }
            }
            // Own each connection so stalled readers cannot survive shutdown.
            drop(listener);
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        *media.task.lock().await = Some(task);
        Ok(media)
    }
    pub async fn shutdown(&self) {
        self.shutdown.send_replace(true);
        self.revoke_all().await;
        // Serialize concurrent shutdown calls until the listener has stopped.
        let mut guard = self.task.lock().await;
        if let Some(task) = guard.take() {
            let _ = task.await;
        }
    }

    pub async fn grant(&self, source: Source) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let mut grants = self.grants.lock().await;
        if !*self.shutdown.borrow() {
            grants.insert(token.clone(), source);
        }
        format!("http://127.0.0.1:{}/media/{token}", self.port)
    }
    pub async fn revoke(&self, url: &str) {
        if let Some(token) = url.rsplit('/').next() {
            self.grants.lock().await.remove(token);
        }
    }
    pub async fn revoke_all(&self) {
        self.grants.lock().await.clear();
    }
    pub async fn revoke_torrent(&self, id: usize) {
        self.grants
            .lock()
            .await
            .retain(|_, s| !matches!(s,Source::Torrent(t,_) | Source::Preview { id: t, .. } if *t==id));
    }
    pub async fn authorized(&self, url: &str) -> bool {
        let prefix = format!("http://127.0.0.1:{}/media/", self.port);
        match url.strip_prefix(&prefix) {
            Some(t) => self.grants.lock().await.contains_key(t),
            None => false,
        }
    }
}
type HttpError = (StatusCode, String);
fn fail(e: impl std::fmt::Display) -> HttpError {
    (StatusCode::BAD_REQUEST, e.to_string())
}
async fn serve(
    State(server): State<Server>,
    Path(token): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    if headers.get("host").and_then(|v| v.to_str().ok())
        != Some(format!("127.0.0.1:{}", server.port).as_str())
    {
        return Err((StatusCode::FORBIDDEN, "Invalid host".into()));
    }
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    if origin.is_some_and(|o| {
        !matches!(
            o,
            "tauri://localhost" | "http://tauri.localhost" | "http://127.0.0.1:1420"
        )
    }) {
        return Err((StatusCode::FORBIDDEN, "Invalid origin".into()));
    }
    let source = server
        .grants
        .lock()
        .await
        .get(&token)
        .cloned()
        .ok_or((StatusCode::FORBIDDEN, "Expired media session".into()))?;
    let (len, mime): (u64, String) = match &source {
        Source::Torrent(id, index) => {
            let details = server
                .api
                .api_torrent_details(TorrentIdOrHash::Id(*id))
                .map_err(fail)?;
            let files = details.files.ok_or_else(|| fail("Metadata unavailable"))?;
            let f = files.get(*index).ok_or_else(|| fail("File unavailable"))?;
            (f.length, mime(&f.name).into())
        }
        Source::Preview { length, .. } => (*length, "application/octet-stream".into()),
        Source::Local(path, kind) => (
            tokio::fs::metadata(path).await.map_err(fail)?.len(),
            kind.clone(),
        ),
    };
    let range = headers
        .get("range")
        .map(|v| v.to_str().map_err(|_| ()))
        .transpose()
        .map_err(|_| fail("Invalid range"))?;
    let (start, end) = match byte_range(range, len) {
        Ok(r) => r,
        Err(_) => {
            return Ok(Response::builder()
                .status(416)
                .header("Content-Range", format!("bytes */{len}"))
                .body(Body::empty())
                .unwrap());
        }
    };
    let size = if len == 0 { 0 } else { end - start + 1 };
    let mut builder = Response::builder()
        .status(if range.is_some() { 206 } else { 200 })
        .header("Content-Type", mime)
        .header("Accept-Ranges", "bytes")
        .header("Content-Length", size.to_string())
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff");
    if range.is_some() {
        builder = builder.header("Content-Range", format!("bytes {start}-{end}/{len}"));
    }
    if let Some(origin) = origin {
        builder = builder
            .header("Access-Control-Allow-Origin", origin)
            .header("Vary", "Origin");
    }
    if method == Method::HEAD || size == 0 {
        return Ok(builder.body(Body::empty()).unwrap());
    }
    let verified = match &source {
        Source::Preview { ranges, .. } => Some(ranges.clone()),
        _ => None,
    };
    if verified.as_ref().is_some_and(|ranges| verified_read_length(ranges, start, 1) == 0) {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "Preview bytes unavailable".into()));
    }
    let reader: Box<dyn AsyncRead + Unpin + Send> = match source {
        Source::Torrent(id, index) => {
            let mut r = server
                .api
                .api_stream(TorrentIdOrHash::Id(id), index)
                .await
                .map_err(fail)?;
            r.seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(fail)?;
            Box::new(r.take(size))
        }
        Source::Local(path, _) | Source::Preview { path, .. } => {
            let mut r = tokio::fs::File::open(path).await.map_err(fail)?;
            r.seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(fail)?;
            Box::new(r.take(size))
        }
    };
    let grants = server.grants.clone();
    let mut shutdown = server.shutdown.clone();
    let stream = async_stream::try_stream! {
        let mut reader = reader;
        let mut offset = start;
        loop {
            if *shutdown.borrow() || !grants.lock().await.contains_key(&token) { break; }
            if offset > end { break; }
            let wanted = (end - offset + 1).min(65536) as usize;
            let count = verified.as_ref().map_or(wanted, |ranges| verified_read_length(ranges, offset, wanted));
            if count == 0 {
                Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Preview bytes unavailable"))?;
            }
            let mut buf = vec![0u8; count];
            let n = tokio::select! {
                _ = shutdown.wait_for(|stop| *stop) => break,
                result = reader.read(&mut buf) => result,
            }?;
            if n == 0 { break; }
            offset += n as u64;
            buf.truncate(n);
            yield Bytes::from(buf);
        }
    };
    let stream: std::pin::Pin<Box<dyn futures_core::Stream<Item = std::io::Result<Bytes>> + Send>> =
        Box::pin(stream);
    Ok(builder.body(Body::from_stream(stream)).unwrap())
}
// Limit each disk read to the current verified island, never across a hole.
fn verified_read_length(ranges: &[[u64; 2]], offset: u64, wanted: usize) -> usize {
    ranges.iter().find(|[start, end]| *start <= offset && offset < *end)
        .map_or(0, |[_, end]| (end - offset).min(wanted as u64) as usize)
}

pub fn mime(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "vtt" => "text/vtt; charset=utf-8",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        _ => "application/octet-stream",
    }
}
pub fn byte_range(header: Option<&str>, len: u64) -> Result<(u64, u64), ()> {
    let Some(header) = header else {
        return Ok((0, len.saturating_sub(1)));
    };
    if len == 0 {
        return Err(());
    }
    let text = header.strip_prefix("bytes=").ok_or(())?;
    if text.contains(',') {
        return Err(());
    }
    let (a, b) = text.split_once('-').ok_or(())?;
    if a.is_empty() {
        let suffix = b.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        return Ok((len.saturating_sub(suffix), len - 1));
    }
    let start = a.parse::<u64>().map_err(|_| ())?;
    let end = if b.is_empty() {
        len - 1
    } else {
        b.parse::<u64>().map_err(|_| ())?.min(len - 1)
    };
    if start >= len || end < start {
        return Err(());
    }
    Ok((start, end))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_closes_listener_and_rejects_late_grants() {
        let temp = tempfile::tempdir().unwrap();
        let session = librqbit::Session::new_with_opts(
            temp.path().into(),
            librqbit::SessionOptions {
                dht: None,
                disable_trackers: true,
                disable_local_service_discovery: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let media = Media::start(Api::new(session.clone(), None)).await.unwrap();
        let file = temp.path().join("video.bin");
        std::fs::write(&file, vec![42; 8 * 1024 * 1024]).unwrap();
        let url = media
            .grant(Source::Local(file.clone(), "video/mp4".into()))
            .await;
        let response = reqwest::get(&url).await.unwrap();
        assert!(response.status().is_success());
        let mut idle = tokio::net::TcpStream::connect(("127.0.0.1", media.port))
            .await
            .unwrap();
        let shutdown = media.shutdown();
        let repeated = media.shutdown();
        tokio::join!(shutdown, repeated);
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", media.port))
                .await
                .is_err()
        );
        let mut byte = [0];
        let closed = tokio::time::timeout(std::time::Duration::from_secs(3), idle.read(&mut byte))
            .await
            .unwrap();
        assert!(matches!(closed, Ok(0) | Err(_)));
        assert!(!media.authorized(&url).await);
        let late = media.grant(Source::Local(file, "video/mp4".into())).await;
        assert!(!media.authorized(&late).await);
        // A response already in progress must also finish or report disconnect;
        // closing only the listener would leave this request alive indefinitely.
        tokio::time::timeout(std::time::Duration::from_secs(3), response.bytes())
            .await
            .unwrap()
            .ok();
        session.stop().await;
    }
    #[tokio::test]
    async fn preview_reads_verified_islands_and_refuses_holes() {
        let temp = tempfile::tempdir().unwrap();
        let session = librqbit::Session::new_with_opts(temp.path().into(), librqbit::SessionOptions {
            dht: None, disable_trackers: true, disable_local_service_discovery: true,
            ..Default::default()
        }).await.unwrap();
        let media = Media::start(Api::new(session.clone(), None)).await.unwrap();
        let path = temp.path().join("partial.bin");
        // Even existing bytes on disk must not be exposed without verification.
        std::fs::write(&path, b"GOODsecretSAFE").unwrap();
        let url = media.grant(Source::Preview {
            id: 99, path, length: 14, ranges: vec![[0, 4], [10, 14]],
        }).await;
        let client = reqwest::Client::new();
        for (range, expected) in [("bytes=0-3", "GOOD"), ("bytes=10-13", "SAFE")] {
            let response = client.get(&url).header("Range", range).send().await.unwrap();
            assert_eq!(response.status(), 206);
            assert_eq!(response.text().await.unwrap(), expected);
        }
        let missing = client.get(&url).header("Range", "bytes=4-9").send().await.unwrap();
        assert_eq!(missing.status(), 503);
        // Open-ended reads cannot silently turn missing bytes into black frames.
        let crossing = client.get(&url).header("Range", "bytes=0-").send().await;
        if let Ok(response) = crossing { assert!(response.bytes().await.is_err()); }
        media.revoke_torrent(99).await;
        assert!(!media.authorized(&url).await);
        media.shutdown().await;
        session.stop().await;
    }

    #[test]
    fn preview_reads_stop_at_verified_boundaries() {
        let ranges = [[0, 4], [10, 14]];
        assert_eq!(verified_read_length(&ranges, 2, 65536), 2);
        assert_eq!(verified_read_length(&ranges, 4, 1), 0);
        assert_eq!(verified_read_length(&ranges, 9, 2), 0);
        assert_eq!(verified_read_length(&ranges, 10, 2), 2);
        assert_eq!(verified_read_length(&ranges, 14, 1), 0);
    }

    #[test]
    fn ranges() {
        assert_eq!(byte_range(Some("bytes=5-9"), 100), Ok((5, 9)));
        assert_eq!(byte_range(Some("bytes=-10"), 100), Ok((90, 99)));
        assert_eq!(byte_range(Some("bytes=90-"), 100), Ok((90, 99)));
        assert_eq!(byte_range(Some("bytes=0-999"), 100), Ok((0, 99)));
        for r in [
            "bytes=100-",
            "bytes=10-1",
            "bytes=-0",
            "bytes=1-2,4-5",
            "nope",
        ] {
            assert!(byte_range(Some(r), 100).is_err());
        }
    }
}
