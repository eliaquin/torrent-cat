use crate::{core::Core, media::Source};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub fn tool(name: &str) -> Option<PathBuf> {
    [
        format!("/opt/homebrew/bin/{name}"),
        format!("/usr/local/bin/{name}"),
        format!("/usr/bin/{name}"),
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}
pub fn tools() -> Value {
    json!({"ffmpeg":tool("ffmpeg").is_some(),"ffprobe":tool("ffprobe").is_some(),"vlc":Path::new("/Applications/VLC.app/Contents/MacOS/lib/libvlc.dylib").is_file(),"legacyProfile":crate::core::home().join("Library/Application Support/WebTorrent").to_string_lossy()})
}
async fn run(name: &str, args: Vec<String>, seconds: u64) -> Result<Vec<u8>> {
    let exe =
        tool(name).with_context(|| format!("{name} is unavailable. Install FFmpeg or use VLC."))?;
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(seconds),
        Command::new(exe).args(args).kill_on_drop(true).output(),
    )
    .await
    .context("Media operation timed out")??;
    if !output.status.success() {
        bail!(
            "{}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(1200)
                .collect::<String>()
        );
    }
    Ok(output.stdout)
}
fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|v| v.to_string()).collect()
}
pub async fn probe(core: &Core, id: usize, index: usize) -> Result<Value> {
    let path = core.file_path(id, index, true)?;
    let _permit = core.jobs.acquire().await?;
    let mut a = args(&[
        "-v",
        "error",
        "-protocol_whitelist",
        "file,pipe",
        "-show_streams",
        "-show_format",
        "-of",
        "json",
    ]);
    a.push(path.to_string_lossy().into());
    Ok(serde_json::from_slice(&run("ffprobe", a, 30).await?)?)
}
pub async fn poster(core: &Core, id: usize, index: usize) -> Result<String> {
    let path = core.file_path(id, index, true)?;
    let _permit = core.jobs.acquire().await?;
    let dir = core.directory.join("posters");
    tokio::fs::create_dir_all(&dir).await?;
    let out = dir.join(format!("{}.jpg", cache_key(&path, "poster").await?));
    if !out.is_file() {
        let mut a = args(&[
            "-nostdin",
            "-v",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-ss",
            "2",
            "-i",
        ]);
        a.push(path.to_string_lossy().into());
        a.extend(args(&["-frames:v", "1", "-vf", "scale=640:-1", "-y"]));
        a.push(out.to_string_lossy().into());
        run("ffmpeg", a, 45).await?;
    }
    Ok(core
        .media
        .grant(Source::Local(out, "image/jpeg".into()))
        .await)
}
// Bound preview work independently of long conversions; never queue hover jobs.
pub async fn preview(core: &Core, id: usize, index: usize, time: f64) -> Result<Option<Vec<u8>>> {
    if !time.is_finite() || time < 0.0 {
        bail!("Invalid preview time");
    }
    if tool("ffmpeg").is_none() { return Ok(None); }
    let Ok(_permit) = core.preview_jobs.try_acquire() else { return Ok(None); };
    let Some(pieces) = core.file_pieces(id, index)? else { return Ok(None); };
    if pieces.ranges.is_empty() { return Ok(None); }
    let path = core.file_path(id, index, false)?;
    let url = core.media.grant(Source::Preview {
        id, path, length: pieces.file_length, ranges: pieces.ranges,
    }).await;
    let mut a = args(&[
        "-nostdin", "-v", "error", "-xerror", "-err_detect", "explode",
        "-threads", "1", "-protocol_whitelist", "http,tcp",
        "-rw_timeout", "1500000", "-ss",
    ]);
    a.push(time.to_string());
    a.push("-i".into());
    a.push(url.clone());
    a.extend(args(&[
        "-map", "0:v:0", "-an", "-sn", "-dn", "-frames:v", "1",
        "-vf", "scale=240:135:force_original_aspect_ratio=decrease",
        "-threads", "1", "-c:v", "mjpeg", "-q:v", "5", "-f", "image2pipe", "pipe:1",
    ]));
    // Missing headers, index or reference frames are normal: show just the time.
    let result = run("ffmpeg", a, 3).await;
    core.media.revoke(&url).await;
    Ok(result.ok().filter(|bytes| bytes.starts_with(&[0xff, 0xd8]) && bytes.len() <= 256 * 1024))
}

async fn cache_key(path: &Path, kind: &str) -> Result<String> {
    let m = tokio::fs::metadata(path).await?;
    let mut h = Sha256::new();
    h.update(format!(
        "{}:{}:{:?}:{kind}",
        path.display(),
        m.len(),
        m.modified()?
    ));
    Ok(format!("{:x}", h.finalize()))
}
pub async fn convert(core: &Core, id: usize, index: usize, audio: Option<usize>) -> Result<String> {
    let path = core.file_path(id, index, true)?;
    let _permit = core.jobs.acquire().await?;
    let dir = core.directory.join("media-cache");
    tokio::fs::create_dir_all(&dir).await?;
    let key = cache_key(&path, &format!("mp4-aac-v1-{audio:?}")).await?;
    let out = dir.join(format!("{key}.mp4"));
    if !out.exists() {
        let temp = dir.join(format!("{key}-{}.partial.mp4", uuid::Uuid::new_v4()));
        let mut a = args(&[
            "-nostdin",
            "-v",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-i",
        ]);
        a.push(path.to_string_lossy().into());
        a.extend(args(&["-map", "0:v:0?", "-map"]));
        a.push(audio.map(|i| format!("0:{i}")).unwrap_or("0:a:0?".into()));
        a.extend(args(&[
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-sn",
            "-movflags",
            "+faststart",
            "-y",
        ]));
        a.push(temp.to_string_lossy().into());
        let result = run("ffmpeg", a, 1800).await;
        if let Err(e) = result {
            let _ = tokio::fs::remove_file(temp).await;
            return Err(e.context("Unable to prepare an MP4 copy; try Native VLC"));
        }
        if key != cache_key(&path, &format!("mp4-aac-v1-{audio:?}")).await? {
            let _ = tokio::fs::remove_file(temp).await;
            bail!("Source changed during conversion");
        }
        tokio::fs::rename(temp, &out).await?;
    }
    Ok(core
        .media
        .grant(Source::Local(out, "video/mp4".into()))
        .await)
}
pub async fn subtitle(core: &Core, path: String) -> Result<String> {
    let p = PathBuf::from(path);
    let ext = p
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "srt" | "vtt") {
        bail!("Choose an SRT or WebVTT subtitle file");
    }
    if tokio::fs::metadata(&p).await?.len() > 16 * 1024 * 1024 {
        bail!("Subtitle file is too large");
    }
    let bytes = tokio::fs::read(p).await?;
    let text = if bytes.starts_with(&[255, 254]) {
        encoding_rs::UTF_16LE.decode(&bytes[2..]).0.into_owned()
    } else if bytes.starts_with(&[254, 255]) {
        encoding_rs::UTF_16BE.decode(&bytes[2..]).0.into_owned()
    } else {
        String::from_utf8(bytes.clone())
            .unwrap_or_else(|_| encoding_rs::WINDOWS_1252.decode(&bytes).0.into_owned())
    };
    let text = if ext == "srt" {
        srt_to_vtt(&text)
    } else {
        text
    };
    let dir = core.directory.join("subtitles");
    tokio::fs::create_dir_all(&dir).await?;
    let out = dir.join(format!("{}.vtt", uuid::Uuid::new_v4()));
    tokio::fs::write(&out, text).await?;
    Ok(core
        .media
        .grant(Source::Local(out, "text/vtt; charset=utf-8".into()))
        .await)
}
pub fn srt_to_vtt(text: &str) -> String {
    let body = text
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(|s| {
            if s.contains("-->") {
                s.replace(',', ".")
            } else {
                s.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("WEBVTT\n\n{body}\n")
}
pub async fn embedded(core: &Core, id: usize, index: usize, stream: usize) -> Result<String> {
    let path = core.file_path(id, index, true)?;
    let _permit = core.jobs.acquire().await?;
    let dir = core.directory.join("subtitles");
    tokio::fs::create_dir_all(&dir).await?;
    let out = dir.join(format!("{}.vtt", uuid::Uuid::new_v4()));
    let mut a = args(&[
        "-nostdin",
        "-v",
        "error",
        "-protocol_whitelist",
        "file,pipe",
        "-i",
    ]);
    a.push(path.to_string_lossy().into());
    a.extend(args(&["-map"]));
    a.push(format!("0:{stream}"));
    a.extend(args(&["-c:s", "webvtt", "-y"]));
    a.push(out.to_string_lossy().into());
    run("ffmpeg", a, 60).await?;
    Ok(core
        .media
        .grant(Source::Local(out, "text/vtt; charset=utf-8".into()))
        .await)
}
#[cfg(test)]
mod tests {
    #[test]
    fn convert_timing() {
        let s = super::srt_to_vtt("1\r\n00:00:01,200 --> 00:00:02,400\r\nHello, world\r\n");
        assert!(s.starts_with("WEBVTT"));
        assert!(s.contains("00:00:01.200 --> 00:00:02.400"));
        assert!(s.contains("Hello, world"));
    }
}
