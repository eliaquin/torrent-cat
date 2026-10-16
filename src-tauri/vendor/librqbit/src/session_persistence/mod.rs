pub mod json;
#[cfg(feature = "postgres")]
pub mod postgres;

use std::{collections::HashSet, path::PathBuf};

use anyhow::Context;
use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use librqbit_core::Id20;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    AddTorrent, AddTorrentOptions, bitv_factory::BitVFactory, session::TorrentId,
    torrent_state::ManagedTorrentHandle,
};

#[derive(Serialize, Deserialize, Clone)]
pub struct SerializedTorrent {
    #[serde(
        serialize_with = "serialize_info_hash",
        deserialize_with = "deserialize_info_hash"
    )]
    pub(crate) info_hash: Id20,
    #[serde(skip)]
    torrent_bytes: Bytes,
    pub(crate) trackers: HashSet<String>,
    pub(crate) output_folder: PathBuf,
    pub(crate) only_files: Option<Vec<usize>>,
    pub(crate) is_paused: bool,
    #[serde(default = "default_allow_overwrite")]
    pub(crate) allow_overwrite: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pending_delete: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(skip)]
    pub(crate) metadata_error: Option<String>,
}

fn default_allow_overwrite() -> bool {
    true
}

// File creation is not committed until storage initialized successfully. Legacy
// records already owned their files; incomplete new additions retain create_new.
pub(crate) fn may_reuse_files_on_restore(torrent: &ManagedTorrentHandle) -> bool {
    torrent.shared().can_reuse_files()
}

impl SerializedTorrent {
    pub fn info_hash(&self) -> &Id20 {
        &self.info_hash
    }
    pub fn into_add_torrent(self) -> anyhow::Result<(AddTorrent<'static>, AddTorrentOptions)> {
        if let Some(error) = self.metadata_error {
            anyhow::bail!(error);
        }
        anyhow::ensure!(
            !self.torrent_bytes.is_empty(),
            "Saved torrent metadata is missing or empty."
        );
        let parsed = librqbit_core::torrent_metainfo::torrent_from_bytes(&self.torrent_bytes)
            .map_err(|_| anyhow::anyhow!("Saved torrent metadata is invalid"))?;
        anyhow::ensure!(
            parsed.info_hash == self.info_hash,
            "Saved torrent metadata belongs to a different torrent."
        );
        let add_torrent = AddTorrent::TorrentFileBytes(self.torrent_bytes);

        let opts = AddTorrentOptions {
            paused: self.is_paused,
            output_folder: Some(
                self.output_folder
                    .to_str()
                    .context("broken path")?
                    .to_owned(),
            ),
            only_files: self.only_files,
            overwrite: self.allow_overwrite,
            ..Default::default()
        };

        Ok((add_torrent, opts))
    }
}

// TODO: make this info_hash first, ID-second.
#[async_trait]
pub trait SessionPersistenceStore: core::fmt::Debug + Send + Sync + BitVFactory {
    async fn next_id(&self) -> anyhow::Result<TorrentId>;
    async fn store(&self, id: TorrentId, torrent: &ManagedTorrentHandle) -> anyhow::Result<()>;
    async fn mark_delete_pending(&self, _id: TorrentId, _delete_files: bool) -> anyhow::Result<()> {
        anyhow::bail!("persistence backend does not support durable removal")
    }
    async fn delete(&self, id: TorrentId) -> anyhow::Result<()>;
    async fn get(&self, id: TorrentId) -> anyhow::Result<SerializedTorrent>;
    async fn update_metadata(
        &self,
        id: TorrentId,
        torrent: &ManagedTorrentHandle,
    ) -> anyhow::Result<()>;
    async fn stream_all(
        &self,
    ) -> anyhow::Result<BoxStream<'_, anyhow::Result<(TorrentId, SerializedTorrent)>>>;
}

fn serialize_info_hash<S>(id: &Id20, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    id.as_string().serialize(serializer)
}

fn deserialize_info_hash<'de, D>(deserializer: D) -> Result<Id20, D::Error>
where
    D: Deserializer<'de>,
{
    Id20::deserialize(deserializer)
}
