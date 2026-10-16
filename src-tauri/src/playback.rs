//! Transfer priority is separate from piece selection: an incomplete playing
//! torrent can temporarily suspend other downloads while keeping seeders active.
use crate::core::Core;
use anyhow::{Context, Result};
use librqbit::{
    TorrentStatsState,
    api::{ApiTorrentListOpts, TorrentIdOrHash},
};

impl Core {
    pub async fn start_playback(&self, id: usize, index: usize) -> Result<()> {
        let _transition = self.playback_transition.lock().await;
        let details = self.api.api_torrent_details(TorrentIdOrHash::Id(id))?;
        details
            .files
            .as_ref()
            .and_then(|files| files.get(index))
            .context("Unknown media file")?;
        self.restore_playback_locked().await?;
        let playing = self.api.mgr_handle(TorrentIdOrHash::Id(id))?;
        playing.wait_until_initialized().await?;
        if matches!(playing.stats().state, TorrentStatsState::Paused) {
            self.api.session().unpause(&playing).await?;
        }
        if playing.stats().finished || !self.profile.lock().await.prefs.highest_playback_priority {
            return Ok(());
        }
        let list = self
            .api
            .api_torrent_list_ext(ApiTorrentListOpts { with_stats: true });
        for torrent in list.torrents {
            let Some(other) = torrent.id.filter(|other| *other != id) else {
                continue;
            };
            if !torrent.stats.is_some_and(|stats| {
                matches!(stats.state, TorrentStatsState::Live) && !stats.finished
            }) {
                continue;
            }
            // Journal before changing engine state. A crash after this write is
            // safe: startup resumes only entries that are actually paused.
            let mut profile = self.profile.lock().await;
            let mut next = profile.clone();
            next.suspended_for_playback.push(torrent.info_hash);
            self.save(&next).await?;
            *profile = next;
            drop(profile);
            if let Err(error) = self
                .api
                .api_torrent_action_pause(TorrentIdOrHash::Id(other))
                .await
            {
                let _ = self.restore_playback_locked().await;
                return Err(error.into());
            }
        }
        Ok(())
    }

    pub async fn restore_playback_transfers(&self) -> Result<()> {
        let _transition = self.playback_transition.lock().await;
        self.restore_playback_locked().await
    }

    async fn restore_playback_locked(&self) -> Result<()> {
        let hashes = self.profile.lock().await.suspended_for_playback.clone();
        for hash in hashes {
            let list = self.api.api_torrent_list();
            if let Some(id) = list
                .torrents
                .into_iter()
                .find(|t| t.info_hash == hash)
                .and_then(|t| t.id)
            {
                let handle = self.api.mgr_handle(TorrentIdOrHash::Id(id))?;
                handle.wait_until_initialized().await?;
                if matches!(handle.stats().state, TorrentStatsState::Paused) {
                    self.api.session().unpause(&handle).await?;
                }
            }
            let mut profile = self.profile.lock().await;
            let mut next = profile.clone();
            next.suspended_for_playback.retain(|h| h != &hash);
            self.save(&next).await?;
            *profile = next;
        }
        Ok(())
    }
}
