use std::{any::TypeId, collections::HashMap, path::PathBuf};

use crate::{
    api::TorrentIdOrHash,
    bitv::{BitV, DiskBackedBitV},
    bitv_factory::BitVFactory,
    session::TorrentId,
    spawn_utils::BlockingSpawner,
    storage::filesystem::FilesystemStorageFactory,
    torrent_state::ManagedTorrentHandle,
    type_aliases::BF,
};
use anyhow::{Context, bail};
use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use itertools::Itertools;
use librqbit_core::Id20;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tracing::{trace, warn};

use super::{SerializedTorrent, SessionPersistenceStore};

#[derive(Serialize, Deserialize, Default, Clone)]
struct SerializedSessionDatabase {
    torrents: HashMap<usize, SerializedTorrent>,
    #[serde(default)]
    next_id: usize,
}

pub struct JsonSessionPersistenceStore {
    output_folder: PathBuf,
    db_filename: PathBuf,
    db_content: tokio::sync::RwLock<SerializedSessionDatabase>,
    spawner: BlockingSpawner,
}

impl std::fmt::Debug for JsonSessionPersistenceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON database: {:?}", self.output_folder)
    }
}

impl JsonSessionPersistenceStore {
    pub async fn new(output_folder: PathBuf, spawner: BlockingSpawner) -> anyhow::Result<Self> {
        let db_filename = output_folder.join("session.json");
        tokio::fs::create_dir_all(&output_folder)
            .await
            .with_context(|| {
                format!("couldn't create directory {output_folder:?} for session storage")
            })?;

        let db = match tokio::fs::File::open(&db_filename).await {
            Ok(f) => {
                let mut buf = Vec::new();
                let mut rdr = tokio::io::BufReader::new(f);
                rdr.read_to_end(&mut buf).await?;

                serde_json::from_reader(&buf[..]).context("error deserializing session database")?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(e) => {
                return Err(e).context(format!("error opening session file {db_filename:?}"));
            }
        };

        Ok(Self {
            db_filename,
            output_folder,
            db_content: tokio::sync::RwLock::new(db),
            spawner,
        })
    }

    async fn to_hash(&self, id: TorrentIdOrHash) -> anyhow::Result<Id20> {
        match id {
            TorrentIdOrHash::Id(id) => self
                .db_content
                .read()
                .await
                .torrents
                .get(&id)
                .map(|v| *v.info_hash())
                .context("not found"),
            TorrentIdOrHash::Hash(h) => Ok(h),
        }
    }

    async fn mutate<R: Send>(
        &self,
        change: impl FnOnce(&mut SerializedSessionDatabase) -> anyhow::Result<R> + Send,
    ) -> anyhow::Result<R> {
        self.mutate_with_persist(change, crate::bitv::persist_snapshot)
            .await
    }

    async fn mutate_with_persist<R: Send>(
        &self,
        change: impl FnOnce(&mut SerializedSessionDatabase) -> anyhow::Result<R> + Send,
        persist: impl FnOnce(&std::path::Path, &[u8]) -> anyhow::Result<()> + Send,
    ) -> anyhow::Result<R> {
        let mut current = self.db_content.write().await;
        let mut next = current.clone();
        let result = change(&mut next)?;
        let bytes = serde_json::to_vec(&next).context("error serializing session")?;
        let persisted = self
            .spawner
            .block_in_place_with_semaphore(|| persist(&self.db_filename, &bytes))
            .await;
        // Rename can succeed before a directory-sync error. Keep memory aligned
        // with the published bytes even though durability was not confirmed.
        if persisted.is_ok() || std::fs::read(&self.db_filename).is_ok_and(|disk| disk == bytes) {
            *current = next;
        }
        persisted.context("could not durably save session")?;
        Ok(result)
    }

    fn torrent_bytes_filename(&self, info_hash: &Id20) -> PathBuf {
        self.output_folder.join(format!("{info_hash:?}.torrent"))
    }

    fn bitv_filename(&self, info_hash: &Id20) -> PathBuf {
        self.output_folder.join(format!("{info_hash:?}.bitv"))
    }

    async fn update_db(
        &self,
        id: TorrentId,
        torrent: &ManagedTorrentHandle,
        write_torrent_file: bool,
    ) -> anyhow::Result<()> {
        if !torrent
            .shared
            .storage_factory
            .is_type_id(TypeId::of::<FilesystemStorageFactory>())
        {
            bail!("storages other than FilesystemStorageFactory are not supported");
        }

        let st = SerializedTorrent {
            trackers: torrent
                .shared()
                .trackers
                .iter()
                .map(|u| u.to_string())
                .collect(),
            info_hash: torrent.info_hash(),
            // we don't serialize this here, but to a file instead.
            torrent_bytes: Default::default(),
            only_files: torrent.only_files().clone(),
            is_paused: torrent.is_paused(),
            pending_delete: None,
            output_folder: torrent.shared().options.output_folder.clone(),
        };

        let torrent_bytes = torrent
            .metadata
            .load()
            .as_ref()
            .map(|i| i.torrent_bytes.clone())
            .unwrap_or_default();

        if write_torrent_file && !torrent_bytes.is_empty() {
            let filename = self.torrent_bytes_filename(&torrent.info_hash());
            self.spawner
                .block_in_place_with_semaphore(|| {
                    crate::bitv::persist_snapshot(&filename, &torrent_bytes)
                })
                .await
                .context("could not save torrent metadata")?;
        }
        self.mutate(move |db| {
            let mut st = st;
            st.pending_delete = db.torrents.get(&id).and_then(|old| old.pending_delete);
            if st.pending_delete.is_some() {
                st.is_paused = true;
            }
            db.next_id = db
                .next_id
                .max(id.checked_add(1).context("torrent ID exhausted")?);
            db.torrents.insert(id, st);
            Ok(())
        })
        .await?;

        Ok(())
    }
}

#[async_trait::async_trait]
impl BitVFactory for JsonSessionPersistenceStore {
    async fn store_initial_check(
        &self,
        id: TorrentIdOrHash,
        b: BF,
    ) -> anyhow::Result<Box<dyn BitV>> {
        let h = self.to_hash(id).await?;
        let filename = self.bitv_filename(&h);
        self.spawner
            .block_in_place_with_semaphore(|| {
                crate::bitv::persist_snapshot(&filename, b.as_raw_slice())
            })
            .await?;
        trace!(?filename, "stored initial check bitfield");
        Ok(DiskBackedBitV::new(filename, b).into_dyn())
    }
}

#[async_trait]
impl SessionPersistenceStore for JsonSessionPersistenceStore {
    async fn next_id(&self) -> anyhow::Result<TorrentId> {
        let db = self.db_content.read().await;
        let from_entries = match db.torrents.keys().max() {
            Some(id) => id.checked_add(1).context("torrent ID exhausted")?,
            None => 0,
        };
        Ok(db.next_id.max(from_entries))
    }

    async fn mark_delete_pending(&self, id: TorrentId, delete_files: bool) -> anyhow::Result<()> {
        self.mutate(move |db| {
            // A previous removal may have published before directory sync failed.
            // Missing is already removed; republishing confirms durability on retry.
            let Some(torrent) = db.torrents.get_mut(&id) else {
                return Ok(());
            };
            torrent.is_paused = true;
            torrent.pending_delete = Some(torrent.pending_delete.unwrap_or(false) || delete_files);
            Ok(())
        })
        .await
    }

    async fn delete(&self, id: TorrentId) -> anyhow::Result<()> {
        let removed = self.mutate(move |db| Ok(db.torrents.remove(&id))).await?;
        if let Some(t) = removed {
            for file in [
                self.torrent_bytes_filename(&t.info_hash),
                self.bitv_filename(&t.info_hash),
            ] {
                if let Err(error) = tokio::fs::remove_file(&file).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    warn!(%error,"could not remove obsolete metadata file");
                }
            }
        }
        Ok(())
    }

    async fn get(&self, id: TorrentId) -> anyhow::Result<SerializedTorrent> {
        let mut st = self
            .db_content
            .read()
            .await
            .torrents
            .get(&id)
            .cloned()
            .context("no torrent found")?;
        let mut buf = Vec::new();
        let torrent_bytes_filename = self.torrent_bytes_filename(&st.info_hash);
        let mut torrent_bytes_file = match tokio::fs::File::open(&torrent_bytes_filename).await {
            Ok(f) => f,
            Err(e) => {
                warn!(error=?e, filename=?torrent_bytes_filename, "error opening torrent bytes file");
                return Ok(st);
            }
        };
        if let Err(e) = torrent_bytes_file.read_to_end(&mut buf).await {
            warn!(error=?e, filename=?torrent_bytes_filename, "error reading torrent bytes file");
        } else {
            st.torrent_bytes = buf.into();
        }
        return Ok(st);
    }

    async fn stream_all(
        &self,
    ) -> anyhow::Result<BoxStream<'_, anyhow::Result<(TorrentId, SerializedTorrent)>>> {
        let all_ids = self
            .db_content
            .read()
            .await
            .torrents
            .keys()
            .copied()
            .collect_vec();
        Ok(futures::stream::iter(all_ids)
            .then(move |id| async move { self.get(id).await.map(move |st| (id, st)) })
            .boxed())
    }

    async fn store(&self, id: TorrentId, torrent: &ManagedTorrentHandle) -> anyhow::Result<()> {
        self.update_db(id, torrent, true).await
    }

    async fn update_metadata(
        &self,
        id: TorrentId,
        torrent: &ManagedTorrentHandle,
    ) -> anyhow::Result<()> {
        self.update_db(id, torrent, false).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_commit_does_not_publish_memory_and_retry_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let store = JsonSessionPersistenceStore::new(root.path().into(), BlockingSpawner::new(1))
            .await
            .unwrap();
        let entry = SerializedTorrent {
            info_hash: Id20::from_bytes(&[7; 20]).unwrap(),
            torrent_bytes: Default::default(),
            trackers: Default::default(),
            output_folder: root.path().join("data"),
            only_files: None,
            is_paused: false,
            pending_delete: None,
        };
        store
            .mutate(|db| {
                db.torrents.insert(0, entry);
                Ok(())
            })
            .await
            .unwrap();
        let original = std::fs::read(&store.db_filename).unwrap();
        // Inject the error returned by a full disk at the commit boundary.
        let error = store
            .mutate_with_persist(
                |db| {
                    db.torrents.get_mut(&0).unwrap().is_paused = true;
                    Ok(())
                },
                |_, _| {
                    Err(
                        std::io::Error::new(std::io::ErrorKind::StorageFull, "injected full disk")
                            .into(),
                    )
                },
            )
            .await
            .unwrap_err();
        assert!(error.chain().any(|e| {
            e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::StorageFull)
        }));
        assert!(!store.db_content.read().await.torrents[&0].is_paused);
        assert_eq!(std::fs::read(&store.db_filename).unwrap(), original);
        store.mark_delete_pending(0, true).await.unwrap();
        store.mark_delete_pending(0, false).await.unwrap();
        assert_eq!(
            store.db_content.read().await.torrents[&0].pending_delete,
            Some(true)
        );
        let reloaded =
            JsonSessionPersistenceStore::new(root.path().into(), BlockingSpawner::new(1))
                .await
                .unwrap();
        assert!(reloaded.db_content.read().await.torrents[&0].is_paused);
        assert_eq!(
            reloaded.db_content.read().await.torrents[&0].pending_delete,
            Some(true)
        );
        let result = reloaded
            .mutate_with_persist(
                |db| {
                    db.torrents.remove(&0);
                    Ok(())
                },
                |path, bytes| {
                    crate::bitv::persist_snapshot(path, bytes)?;
                    Err(std::io::Error::other("injected error after publication").into())
                },
            )
            .await;
        assert!(result.is_err());
        assert!(reloaded.db_content.read().await.torrents.is_empty());
        reloaded.mark_delete_pending(0, true).await.unwrap();
        reloaded.delete(0).await.unwrap();
        reloaded.delete(0).await.unwrap();
        assert!(reloaded.db_content.read().await.torrents.is_empty());
    }
}
