use anyhow::{Context, Result, bail};
use librqbit::{
    AddTorrent, AddTorrentOptions, Api, ListenerOptions, Session, SessionOptions,
    SessionPersistenceConfig,
    api::{ApiTorrentListOpts, TorrentIdOrHash},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Preferences {
    pub download_path: String,
    pub video_engine: String,
    pub external_player_path: String,
    pub sound_notifications: bool,
    pub completion_notifications: bool,
    pub highest_playback_priority: bool,
    pub watch_folder: String,
    pub auto_add_torrents: bool,
    pub global_trackers: Vec<String>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            download_path: home().join("Downloads").to_string_lossy().into(),
            video_engine: "builtin".into(),
            external_player_path: "/Applications/VLC.app".into(),
            sound_notifications: true,
            completion_notifications: false,
            highest_playback_priority: true,
            watch_folder: String::new(),
            auto_add_torrents: false,
            global_trackers: vec![],
        }
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Profile {
    pub version: u32,
    pub prefs: Preferences,
    pub positions: HashMap<String, f64>,
    pub imported: Vec<String>,
    pub suspended_for_playback: Vec<String>,
}
pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

pub struct Core {
    pub api: Api,
    pub profile: Mutex<Profile>,
    pub directory: PathBuf,
    pub media: crate::media::Media,
    pub jobs: tokio::sync::Semaphore,
    pub(crate) playback_transition: Mutex<()>,
}
impl Core {
    pub async fn new(directory: PathBuf, offline: bool) -> Result<Arc<Self>> {
        tokio::fs::create_dir_all(&directory).await?;
        let profile: Profile = match tokio::fs::read(directory.join("profile.json")).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .context("Profile is damaged; restore profile.json.bak before starting")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Profile {
                version: 1,
                ..Default::default()
            },
            Err(e) => return Err(e.into()),
        };
        let mut opts = SessionOptions {
            persistence: Some(SessionPersistenceConfig::Json {
                folder: Some(directory.join("engine")),
            }),
            fastresume: true,
            listen: Some(ListenerOptions {
                listen_addr: if offline {
                    "127.0.0.1:0".parse()?
                } else {
                    "[::]:0".parse()?
                },
                enable_upnp_port_forwarding: !offline,
                ..Default::default()
            }),
            client_name_and_version: Some("TorrentCat/0.1.0".into()),
            ..Default::default()
        };
        if offline {
            opts.dht = None;
            opts.disable_trackers = true;
            opts.disable_local_service_discovery = true;
        }
        // Keep DHT persistence in this application's profile as well.
        if let Some(dht) = opts.dht.as_mut() {
            dht.persistence = None;
        }
        let session =
            Session::new_with_opts(PathBuf::from(&profile.prefs.download_path), opts).await?;
        let api = Api::new(session, None);
        let media = crate::media::Media::start(api.clone()).await?;
        let core = Arc::new(Self {
            api,
            profile: Mutex::new(profile),
            directory,
            media,
            jobs: tokio::sync::Semaphore::new(2),
            playback_transition: Mutex::new(()),
        });
        if let Err(error) = core.restore_playback_transfers().await {
            eprintln!("Unable to restore transfers after playback: {error}");
        }
        Ok(core)
    }
    pub async fn save(&self, profile: &Profile) -> Result<()> {
        atomic_json(&self.directory.join("profile.json"), profile).await
    }
    pub async fn snapshot(&self) -> Result<Value> {
        let mut list = self
            .api
            .api_torrent_list_ext(ApiTorrentListOpts { with_stats: true });
        for torrent in &mut list.torrents {
            if let Some(id) = torrent.id {
                if let Ok(details) = self.api.api_torrent_details(TorrentIdOrHash::Id(id)) {
                    torrent.files = details.files;
                }
            }
        }
        let profile = self.profile.lock().await.clone();
        Ok(
            json!({"torrents":list.torrents,"profile":profile,"tools":crate::jobs::tools(),"downloadPathExists":Path::new(&profile.prefs.download_path).is_dir(),"version":"0.1.0"}),
        )
    }
    pub async fn add(
        &self,
        address: String,
        output: Option<String>,
        paused: bool,
        reuse: bool,
    ) -> Result<Value> {
        let prefs = self.profile.lock().await.prefs.clone();
        let address = normalize_address(&address)?;
        let source = if Path::new(&address).is_absolute() {
            let meta = tokio::fs::metadata(&address).await?;
            if meta.len() > 32 * 1024 * 1024 {
                bail!("Torrent metadata is too large");
            }
            AddTorrent::TorrentFileBytes(tokio::fs::read(&address).await?.into())
        } else {
            AddTorrent::Url(address.into())
        };
        let target = output.unwrap_or(prefs.download_path);
        if !Path::new(&target).is_dir() {
            bail!("Download folder is unavailable. Choose an existing folder in Preferences.");
        }
        let preview = tokio::time::timeout(
            std::time::Duration::from_secs(90),
            self.api.session().add_torrent(
                source,
                Some(AddTorrentOptions {
                    list_only: true,
                    trackers: Some(prefs.global_trackers.clone()),
                    ..Default::default()
                }),
            ),
        )
        .await
        .context(
            "No torrent metadata received within 90 seconds. Check the link and available peers.",
        )??;
        let preview = match preview {
            librqbit::AddTorrentResponse::ListOnly(p) => p,
            librqbit::AddTorrentResponse::AlreadyManaged(id, _)
            | librqbit::AddTorrentResponse::Added(id, _) => {
                return Ok(
                    json!({"id":id,"details":self.api.api_torrent_details(TorrentIdOrHash::Id(id))?}),
                );
            }
        };
        let mut target = PathBuf::from(target);
        if preview.info.info().files.is_some() {
            let name = preview
                .info
                .name()
                .context("Multi-file torrent has no name")?;
            let name_path = Path::new(name.as_ref());
            if name_path.components().count() != 1
                || !matches!(
                    name_path.components().next(),
                    Some(std::path::Component::Normal(_))
                )
            {
                bail!("Invalid torrent folder name");
            }
            target.push(name_path);
        }
        let response = self
            .api
            .api_add_torrent(
                AddTorrent::TorrentFileBytes(preview.torrent_bytes),
                Some(AddTorrentOptions {
                    output_folder: Some(target.to_string_lossy().into()),
                    paused,
                    overwrite: reuse,
                    trackers: Some(prefs.global_trackers),
                    initial_peers: Some(preview.seen_peers),
                    ..Default::default()
                }),
            )
            .await?;
        Ok(serde_json::to_value(response)?)
    }
    pub async fn action(&self, id: usize, action: &str) -> Result<()> {
        let _transition = self.playback_transition.lock().await;
        let key = TorrentIdOrHash::Id(id);
        let hash = self.api.api_torrent_details(key)?.info_hash;
        // A user's explicit transfer action overrides automatic restoration.
        let mut profile = self.profile.lock().await;
        if profile.suspended_for_playback.iter().any(|h| h == &hash) {
            let mut next = profile.clone();
            next.suspended_for_playback.retain(|h| h != &hash);
            self.save(&next).await?;
            *profile = next;
        }
        drop(profile);
        if matches!(action, "remove" | "delete" | "pause") {
            self.media.revoke_torrent(id).await;
        }
        match action {
            "pause" => {
                if !matches!(
                    self.api.api_stats_v1(key)?.state,
                    librqbit::TorrentStatsState::Paused
                ) {
                    self.api.api_torrent_action_pause(key).await?;
                }
            }
            "resume" => {
                if !matches!(
                    self.api.api_stats_v1(key)?.state,
                    librqbit::TorrentStatsState::Live
                ) {
                    self.api.api_torrent_action_start(key).await?;
                }
            }
            "remove" => {
                self.api.api_torrent_action_forget(key).await?;
            }
            "delete" => {
                self.api.api_torrent_action_delete(key).await?;
            }
            _ => bail!("Unknown torrent action"),
        }
        Ok(())
    }
    pub fn file_path(&self, id: usize, index: usize, complete: bool) -> Result<PathBuf> {
        let details = self.api.api_torrent_details(TorrentIdOrHash::Id(id))?;
        let files = details.files.context("Torrent metadata not ready")?;
        let file = files.get(index).context("Unknown file")?;
        if complete {
            let stats = self.api.api_stats_v1(TorrentIdOrHash::Id(id))?;
            if stats.file_progress.get(index).copied().unwrap_or(0) < file.length {
                bail!("This operation is available when the file finishes downloading.");
            }
        }
        let root = PathBuf::from(details.output_folder).canonicalize()?;
        let path = root.join(&file.name).canonicalize()?;
        if !path.starts_with(&root) {
            bail!("File is outside the torrent folder");
        }
        Ok(path)
    }
    pub async fn create(
        &self,
        path: String,
        destination: String,
        private: bool,
        comment: String,
        trackers: Vec<String>,
    ) -> Result<Value> {
        let source = PathBuf::from(path).canonicalize()?;
        let output = if source.is_dir() {
            source.clone()
        } else {
            source.parent().context("Missing parent folder")?.to_owned()
        };
        let task_source = source.clone();
        let _permit = self.jobs.acquire().await?;
        let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            use lava_torrent::{bencode::BencodeElem, torrent::v1::TorrentBuilder};
            let mut builder = TorrentBuilder::new(task_source, 1_048_576)
                .set_privacy(private)
                .set_num_threads(2);
            if !trackers.is_empty() {
                builder = builder.set_announce_list(vec![trackers]);
            }
            let mut torrent = builder.build()?;
            if !comment.is_empty() {
                torrent
                    .extra_fields
                    .get_or_insert_with(Default::default)
                    .insert("comment".into(), BencodeElem::String(comment));
            }
            let mut bytes = Vec::new();
            torrent.write_into(&mut bytes)?;
            Ok(bytes)
        })
        .await??;
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(destination)
            .await
            .context("Choose a new .torrent filename (existing files are not overwritten)")?;
        use tokio::io::AsyncWriteExt;
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        let response = self
            .api
            .api_add_torrent(
                AddTorrent::TorrentFileBytes(bytes.into()),
                Some(AddTorrentOptions {
                    output_folder: Some(output.to_string_lossy().into()),
                    overwrite: true,
                    ..Default::default()
                }),
            )
            .await?;
        Ok(serde_json::to_value(response)?)
    }
    pub async fn import_legacy(&self, path: String) -> Result<Value> {
        let base = PathBuf::from(path).canonicalize()?;
        let saved: Value =
            serde_json::from_slice(&tokio::fs::read(base.join("config.json")).await?)?;
        let torrents = saved["torrents"]
            .as_array()
            .context("No torrent library found in this profile")?;
        let mut imported = 0;
        let mut errors = Vec::new();
        for torrent in torrents {
            let Some(hash) = torrent["infoHash"]
                .as_str()
                .filter(|h| h.len() == 40 && h.chars().all(|c| c.is_ascii_hexdigit()))
            else {
                continue;
            };
            if self.profile.lock().await.imported.iter().any(|h| h == hash) {
                continue;
            }
            let path = torrent["path"].as_str().unwrap_or("");
            if !Path::new(path).is_dir() {
                errors.push(format!(
                    "{}: download location unavailable",
                    torrent["name"].as_str().unwrap_or(hash)
                ));
                continue;
            }
            let filename = torrent["torrentFileName"]
                .as_str()
                .filter(|name| {
                    let p = Path::new(name);
                    p.components().count() == 1
                        && matches!(p.components().next(), Some(std::path::Component::Normal(_)))
                })
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{hash}.torrent"));
            let metadata = base.join("Torrents").join(&filename);
            let metadata = if metadata.exists() {
                metadata
            } else {
                base.join("torrents").join(&filename)
            };
            let address = if metadata.exists() {
                metadata.to_string_lossy().into_owned()
            } else {
                torrent["magnetURI"].as_str().unwrap_or("").to_owned()
            };
            match self.add(address, Some(path.into()), true, true).await {
                Ok(response) => {
                    if let (Some(id), Some(selections)) =
                        (response["id"].as_u64(), torrent["selections"].as_array())
                    {
                        let selected: std::collections::HashSet<usize> = selections
                            .iter()
                            .enumerate()
                            .filter_map(|(i, v)| v.as_bool().filter(|v| *v).map(|_| i))
                            .collect();
                        let _ = self
                            .api
                            .api_torrent_action_update_only_files(
                                TorrentIdOrHash::Id(id as usize),
                                &selected,
                            )
                            .await;
                    }
                    let mut profile = self.profile.lock().await;
                    profile.imported.push(hash.into());
                    if let Some(files) = torrent["files"].as_array() {
                        for (i, file) in files.iter().enumerate() {
                            if let Some(t) = file["currentTime"].as_f64() {
                                profile.positions.insert(format!("{hash}:{i}"), t);
                            }
                        }
                    }
                    self.save(&profile).await?;
                    imported += 1;
                }
                Err(e) => errors.push(format!("{}: {e}", torrent["name"].as_str().unwrap_or(hash))),
            }
        }
        Ok(json!({"imported":imported,"errors":errors}))
    }
}

pub async fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let bytes = serde_json::to_vec_pretty(value)?;
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut f = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .await?;
    f.write_all(&bytes).await?;
    f.sync_all().await?;
    drop(f);
    if path.exists() {
        tokio::fs::copy(path, path.with_extension("json.bak")).await?;
    }
    tokio::fs::rename(temp, path).await?;
    Ok(())
}
pub fn normalize_address(value: &str) -> Result<String> {
    let mut s = value.trim().to_owned();
    if s.starts_with("https://instant.io/#")
        || s.starts_with("http://instant.io/#")
        || s.starts_with("instant.io/#")
    {
        s = s.split_once('#').unwrap().1.to_owned();
    }
    if s.starts_with("stream-magnet:") {
        s = s.replacen("stream-magnet:", "magnet:", 1);
    }
    if (s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()))
        || (s.len() == 32
            && s.chars()
                .all(|c| c.is_ascii_alphabetic() || ('2'..='7').contains(&c)))
    {
        s = format!("magnet:?xt=urn:btih:{s}");
    }
    if Path::new(&s).is_absolute() && s.to_lowercase().ends_with(".torrent") {
        return Ok(s);
    }
    let url = url::Url::parse(&s).context("Enter a magnet link, torrent URL or .torrent file")?;
    match url.scheme() {
        "http" | "https" if url.host_str().is_some() => Ok(s),
        "magnet"
            if url
                .query_pairs()
                .any(|(k, v)| k == "xt" && v.starts_with("urn:btih:")) =>
        {
            Ok(s)
        }
        _ => bail!("Enter a valid magnet link, torrent URL or info hash"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inputs() {
        let hash = "0123456789012345678901234567890123456789";
        assert!(normalize_address(hash).unwrap().starts_with("magnet:"));
        assert_eq!(
            normalize_address(&format!("https://instant.io/#{hash}")).unwrap(),
            format!("magnet:?xt=urn:btih:{hash}")
        );
        for s in [
            "",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "magnet:?dn=x",
        ] {
            assert!(normalize_address(s).is_err());
        }
    }
    #[tokio::test]
    async fn atomic_profile_backup() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("profile.json");
        atomic_json(&p, &json!({"v":1})).await.unwrap();
        atomic_json(&p, &json!({"v":2})).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(
                &tokio::fs::read(p.with_extension("json.bak")).await.unwrap()
            )
            .unwrap()["v"],
            1
        );
    }
}
