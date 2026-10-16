//! macOS owns these associations; never store a separate preference for them.
use block2::RcBlock;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSBundle, NSError, NSString, NSURL};
use serde::Serialize;
use std::{path::PathBuf, sync::Mutex};
use tauri::Manager;
use tokio::sync::oneshot;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultHandlers {
    torrent_files: bool,
    magnet_links: bool,
}

// NSWorkspace determines the content type from a real .torrent file. Keep it
// outside Downloads and alive until any macOS confirmation has completed.
struct Probe(PathBuf);
impl Probe {
    fn new(app: &tauri::AppHandle) -> Result<Self, String> {
        let folder = app.path().app_cache_dir().map_err(crate::err)?;
        std::fs::create_dir_all(&folder).map_err(crate::err)?;
        let path = folder.join(format!("association-{}.torrent", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"de").map_err(crate::err)?;
        Ok(Self(path))
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn file_url(path: &std::path::Path) -> objc2::rc::Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

fn is_this_app(url: Option<objc2::rc::Retained<NSURL>>) -> bool {
    let ours = NSBundle::mainBundle().bundleURL().path();
    url.and_then(|url| url.path())
        .zip(ours)
        .is_some_and(|(other, ours)| {
            std::fs::canonicalize(other.to_string())
                .ok()
                .zip(std::fs::canonicalize(ours.to_string()).ok())
                .is_some_and(|(other, ours)| other == ours)
        })
}

pub async fn status(app: tauri::AppHandle) -> Result<DefaultHandlers, String> {
    let probe = Probe::new(&app)?;
    let path = probe.0.clone();
    let (tx, rx) = oneshot::channel();
    app.run_on_main_thread(move || {
        let workspace = NSWorkspace::sharedWorkspace();
        let magnet = NSURL::URLWithString(&NSString::from_str("magnet:")).unwrap();
        let _ = tx.send(DefaultHandlers {
            torrent_files: is_this_app(workspace.URLForApplicationToOpenURL(&file_url(&path))),
            magnet_links: is_this_app(workspace.URLForApplicationToOpenURL(&magnet)),
        });
    })
    .map_err(crate::err)?;
    rx.await.map_err(crate::err)
}

pub async fn set(app: tauri::AppHandle) -> Result<DefaultHandlers, String> {
    let before = status(app.clone()).await?;
    let probe = Probe::new(&app)?;
    for (torrent, already_default) in [(true, before.torrent_files), (false, before.magnet_links)] {
        if already_default {
            continue;
        }
        let path = probe.0.clone();
        let (tx, rx) = oneshot::channel();
        app.run_on_main_thread(move || {
            let application = NSBundle::mainBundle().bundleURL();
            if application
                .pathExtension()
                .is_none_or(|ext| ext.to_string() != "app")
            {
                let _ = tx.send(Err(
                    "Install TorrentCat in Applications before making it the default.".into(),
                ));
                return;
            }
            let sender = Mutex::new(Some(tx));
            let completion = RcBlock::new(move |error: *mut NSError| {
                // NSError is borrowed only for the duration of this callback.
                let result = unsafe { error.as_ref() }
                    .map(|error| Err(error.localizedDescription().to_string()))
                    .unwrap_or(Ok(()));
                if let Some(tx) = sender.lock().unwrap().take() {
                    let _ = tx.send(result);
                }
            });
            let workspace = NSWorkspace::sharedWorkspace();
            if torrent {
                workspace
                    .setDefaultApplicationAtURL_toOpenContentTypeOfFileAtURL_completionHandler(
                        &application,
                        &file_url(&path),
                        Some(&completion),
                    );
            } else {
                workspace.setDefaultApplicationAtURL_toOpenURLsWithScheme_completionHandler(
                    &application,
                    &NSString::from_str("magnet"),
                    Some(&completion),
                );
            }
        })
        .map_err(crate::err)?;
        rx.await.map_err(crate::err)??;
    }
    status(app).await
}
