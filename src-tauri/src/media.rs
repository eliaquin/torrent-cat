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
}
#[derive(Clone)]
pub struct Media {
    pub port: u16,
    grants: Arc<Mutex<HashMap<String, Source>>>,
}
#[derive(Clone)]
struct Server {
    api: Api,
    media: Media,
}
impl Media {
    pub async fn start(api: Api) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let media = Self {
            port: listener.local_addr()?.port(),
            grants: Default::default(),
        };
        let router = Router::new()
            .route("/media/{token}", get(serve))
            .with_state(Server {
                api,
                media: media.clone(),
            });
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                eprintln!("Media server: {e}");
            }
        });
        Ok(media)
    }
    pub async fn grant(&self, source: Source) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.grants.lock().await.insert(token.clone(), source);
        format!("http://127.0.0.1:{}/media/{token}", self.port)
    }
    pub async fn revoke_all(&self) {
        self.grants.lock().await.clear();
    }
    pub async fn revoke_torrent(&self, id: usize) {
        self.grants
            .lock()
            .await
            .retain(|_, s| !matches!(s,Source::Torrent(t,_) if *t==id));
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
        != Some(format!("127.0.0.1:{}", server.media.port).as_str())
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
        .media
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
        Source::Local(path, _) => {
            let mut r = tokio::fs::File::open(path).await.map_err(fail)?;
            r.seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(fail)?;
            Box::new(r.take(size))
        }
    };
    let grants = server.media.grants.clone();
    let stream = async_stream::try_stream! { let mut reader=reader; loop {if !grants.lock().await.contains_key(&token){break;} let mut buf=vec![0u8;65536];let n=reader.read(&mut buf).await?;if n==0{break;}buf.truncate(n);yield Bytes::from(buf);} };
    let stream: std::pin::Pin<Box<dyn futures_core::Stream<Item = std::io::Result<Bytes>> + Send>> =
        Box::pin(stream);
    Ok(builder.body(Body::from_stream(stream)).unwrap())
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
