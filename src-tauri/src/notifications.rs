use crate::core::Core;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tauri::Emitter;
use tauri_plugin_notification::NotificationExt;

#[derive(Default)]
pub(crate) struct Completions(HashMap<String, bool>);
impl Completions {
    // The first observation establishes a baseline: restarting or adding a
    // completed seed must not announce an old download as a new completion.
    pub fn update(&mut self, entries: Vec<(String, String, bool)>) -> Vec<String> {
        let mut next = HashMap::new();
        let mut finished = Vec::new();
        for (hash, name, complete) in entries {
            if complete && self.0.get(&hash) == Some(&false) {
                finished.push(name);
            }
            next.insert(hash, complete);
        }
        self.0 = next;
        finished
    }
}

pub fn start(core: Arc<Core>, app: tauri::AppHandle) {
    let session = core.api.session().clone();
    session.spawn(
        tracing::Span::current(),
        "completion-notifications",
        async move {
            let mut completions = Completions::default();
            let mut timer = tokio::time::interval(Duration::from_secs(1));
            loop {
                timer.tick().await;
                let list = core
                    .api
                    .api_torrent_list_ext(librqbit::api::ApiTorrentListOpts { with_stats: true });
                let entries = list
                    .torrents
                    .into_iter()
                    .filter_map(|t| {
                        let stats = t.stats?;
                        if matches!(
                            stats.state,
                            librqbit::TorrentStatsState::Initializing { .. }
                        ) {
                            return None;
                        }
                        Some((
                            t.info_hash,
                            t.name.unwrap_or_else(|| "Torrent".into()),
                            stats.finished,
                        ))
                    })
                    .collect();
                for name in completions.update(entries) {
                    let _ = app.emit("download-complete", &name);
                    if core.profile.lock().await.prefs.completion_notifications
                        && matches!(
                            app.notification().permission_state(),
                            Ok(tauri_plugin_notification::PermissionState::Granted)
                        )
                    {
                        let _ = app
                            .notification()
                            .builder()
                            .title("Download complete")
                            .body(name)
                            .show();
                    }
                }
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_new_completions_notify() {
        let mut tracker = Completions::default();
        let entry = |done| vec![("hash".into(), "Movie".into(), done)];
        assert!(tracker.update(entry(true)).is_empty());
        assert!(tracker.update(entry(false)).is_empty());
        assert_eq!(tracker.update(entry(true)), vec!["Movie"]);
        assert!(tracker.update(entry(true)).is_empty());
        tracker.update(vec![]);
        assert!(tracker.update(entry(true)).is_empty());
    }
}
