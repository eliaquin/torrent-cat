#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod core;
mod defaults;
mod inbox;
#[cfg(test)]
mod integration_tests;
mod jobs;
mod media;
mod native;
mod notifications;
mod piece_map;
mod playback;
mod power;
#[cfg(test)]
mod tracker_tests;
mod watcher;
use core::{Core, Preferences};
use librqbit::api::TorrentIdOrHash;
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{Emitter, Manager};
use tauri_plugin_deep_link::DeepLinkExt;
type Cmd<T> = Result<T, String>;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
#[tauri::command]
async fn default_handlers(app: tauri::AppHandle) -> Cmd<defaults::DefaultHandlers> {
    defaults::status(app).await
}
#[tauri::command]
async fn set_default_handlers(app: tauri::AppHandle) -> Cmd<defaults::DefaultHandlers> {
    defaults::set(app).await
}
struct AppState {
    core: Arc<Core>,
    inputs: std::sync::Mutex<inbox::Inbox>,
    power: tokio::sync::Mutex<power::PlaybackPower>,
}
#[tauri::command]
async fn snapshot(s: tauri::State<'_, AppState>) -> Cmd<Value> {
    s.core.snapshot().await.map_err(err)
}
#[tauri::command]
async fn file_pieces(
    s: tauri::State<'_, AppState>,
    id: usize,
    index: usize,
) -> Cmd<Option<piece_map::FilePieces>> {
    s.core.file_pieces(id, index).map_err(err)
}
#[tauri::command]
async fn add_torrent(
    s: tauri::State<'_, AppState>,
    address: String,
    output: Option<String>,
    paused: Option<bool>,
    reuse: Option<bool>,
) -> Cmd<Value> {
    s.core
        .add(
            address,
            output,
            paused.unwrap_or(false),
            reuse.unwrap_or(false),
        )
        .await
        .map_err(err)
}
#[tauri::command]
async fn torrent_action(s: tauri::State<'_, AppState>, id: usize, action: String) -> Cmd<()> {
    s.core.action(id, &action).await.map_err(err)
}
#[tauri::command]
async fn select_files(s: tauri::State<'_, AppState>, id: usize, files: Vec<usize>) -> Cmd<()> {
    s.core
        .api
        .api_torrent_action_update_only_files(TorrentIdOrHash::Id(id), &files.into_iter().collect())
        .await
        .map_err(err)?;
    Ok(())
}
#[tauri::command]
async fn preferences(s: tauri::State<'_, AppState>, prefs: Preferences) -> Cmd<()> {
    if !std::path::Path::new(&prefs.download_path).is_dir() {
        return Err("Choose an existing download folder".into());
    }
    if !matches!(prefs.video_engine.as_str(), "builtin" | "vlc" | "external") {
        return Err("Unknown video player".into());
    }
    if prefs.auto_add_torrents && !std::path::Path::new(&prefs.watch_folder).is_dir() {
        return Err(
            "Choose an existing folder to watch before enabling automatic additions".into(),
        );
    }
    let mut p = s.core.profile.lock().await;
    let mut updated = p.clone();
    updated.prefs = prefs;
    s.core.save(&updated).await.map_err(err)?;
    *p = updated;
    Ok(())
}
#[tauri::command]
async fn save_position(s: tauri::State<'_, AppState>, key: String, position: f64) -> Cmd<()> {
    if !position.is_finite() || position < 0.0 {
        return Err("Invalid position".into());
    }
    let mut p = s.core.profile.lock().await;
    p.positions.insert(key, position);
    s.core.save(&p).await.map_err(err)
}
#[tauri::command]
async fn stream_file(s: tauri::State<'_, AppState>, id: usize, index: usize) -> Cmd<String> {
    // Serialize the complete start/stop transition so a pending start cannot
    // acquire a new assertion after stop_stream has already released it.
    let mut power = s.power.lock().await;
    power.ensure_open().map_err(err)?;
    s.core.start_playback(id, index).await.map_err(err)?;
    s.core.media.revoke_all().await;
    if let Err(error) = power.acquire() {
        eprintln!("Unable to prevent sleep during playback: {error}");
    }
    Ok(s.core.media.grant(media::Source::Torrent(id, index)).await)
}
#[tauri::command]
async fn stop_stream(s: tauri::State<'_, AppState>) -> Cmd<()> {
    let mut power = s.power.lock().await;
    let released = power.release().await.map_err(err);
    s.core.media.revoke_all().await;
    s.core.restore_playback_transfers().await.map_err(err)?;
    released
}
#[tauri::command]
async fn media_probe(s: tauri::State<'_, AppState>, id: usize, index: usize) -> Cmd<Value> {
    jobs::probe(&s.core, id, index).await.map_err(err)
}
#[tauri::command]
async fn media_convert(
    s: tauri::State<'_, AppState>,
    id: usize,
    index: usize,
    audio: Option<usize>,
) -> Cmd<String> {
    jobs::convert(&s.core, id, index, audio).await.map_err(err)
}
#[tauri::command]
async fn media_poster(s: tauri::State<'_, AppState>, id: usize, index: usize) -> Cmd<String> {
    jobs::poster(&s.core, id, index).await.map_err(err)
}
#[tauri::command]
async fn subtitle_file(s: tauri::State<'_, AppState>, path: String) -> Cmd<String> {
    jobs::subtitle(&s.core, path).await.map_err(err)
}
#[tauri::command]
async fn subtitle_embedded(
    s: tauri::State<'_, AppState>,
    id: usize,
    index: usize,
    stream: usize,
) -> Cmd<String> {
    jobs::embedded(&s.core, id, index, stream)
        .await
        .map_err(err)
}
#[tauri::command]
async fn create_torrent(
    s: tauri::State<'_, AppState>,
    path: String,
    destination: String,
    private: bool,
    comment: String,
    trackers: Vec<String>,
) -> Cmd<Value> {
    s.core
        .create(path, destination, private, comment, trackers)
        .await
        .map_err(err)
}
#[tauri::command]
async fn import_legacy(s: tauri::State<'_, AppState>, path: String) -> Cmd<Value> {
    s.core.import_legacy(path).await.map_err(err)
}
#[tauri::command]
async fn reveal_torrent(s: tauri::State<'_, AppState>, id: usize) -> Cmd<()> {
    let d = s
        .core
        .api
        .api_torrent_details(TorrentIdOrHash::Id(id))
        .map_err(err)?;
    tokio::process::Command::new("/usr/bin/open")
        .arg(&d.output_folder)
        .status()
        .await
        .map_err(err)?;
    Ok(())
}
#[tauri::command]
async fn export_torrent(s: tauri::State<'_, AppState>, id: usize, path: String) -> Cmd<()> {
    let h = s
        .core
        .api
        .mgr_handle(TorrentIdOrHash::Id(id))
        .map_err(err)?;
    let metadata = h.metadata.load_full().ok_or("Torrent metadata not ready")?;
    tokio::fs::write(path, &metadata.torrent_bytes)
        .await
        .map_err(err)
}
#[tauri::command]
async fn external_player(s: tauri::State<'_, AppState>, url: String) -> Cmd<()> {
    if !s.core.media.authorized(&url).await {
        return Err("Media session expired".into());
    }
    let path = s
        .core
        .profile
        .lock()
        .await
        .prefs
        .external_player_path
        .clone();
    if !std::path::Path::new(&path).is_dir() || !path.ends_with(".app") {
        return Err("Choose a player application in Preferences".into());
    }
    let result = tokio::process::Command::new("/usr/bin/open")
        .args(["-a", &path, &url])
        .status()
        .await
        .map_err(err)?;
    if !result.success() {
        return Err("External player could not start".into());
    }
    Ok(())
}
#[tauri::command]
async fn open_vlc_download() -> Cmd<()> {
    let result = tokio::process::Command::new("/usr/bin/open")
        .arg("https://www.videolan.org/vlc/download-macosx.html")
        .status()
        .await
        .map_err(err)?;
    if !result.success() {
        return Err("Could not open the VLC download page".into());
    }
    Ok(())
}
async fn on_main<F>(window: tauri::WebviewWindow, action: F) -> Cmd<Value>
where
    F: FnOnce(usize) -> anyhow::Result<Value> + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    let host = window.ns_view().map_err(err)? as usize;
    window
        .run_on_main_thread(move || {
            let _ = tx.send(action(host).map_err(err));
        })
        .map_err(err)?;
    rx.await.map_err(err)?
}
#[tauri::command]
async fn native_open(
    window: tauri::WebviewWindow,
    s: tauri::State<'_, AppState>,
    url: String,
    rect: native::Rect,
    position: f64,
) -> Cmd<Value> {
    if !s.core.media.authorized(&url).await {
        return Err("Media session expired".into());
    }
    on_main(window, move |host| native::open(host, url, rect, position)).await
}
#[tauri::command]
async fn native_subtitle(
    window: tauri::WebviewWindow,
    s: tauri::State<'_, AppState>,
    url: String,
) -> Cmd<Value> {
    if !s.core.media.authorized(&url).await {
        return Err("Subtitle session expired".into());
    }
    on_main(window, move |_| native::subtitle(url)).await
}
#[tauri::command]
async fn native_close(window: tauri::WebviewWindow) -> Cmd<Value> {
    on_main(window, move |_| native::close()).await
}
#[tauri::command]
async fn native_resize(window: tauri::WebviewWindow, rect: native::Rect) -> Cmd<Value> {
    on_main(window, move |host| native::resize(host, rect)).await
}
#[tauri::command]
async fn native_action(window: tauri::WebviewWindow, action: String, value: f64) -> Cmd<Value> {
    if !value.is_finite() {
        return Err("Invalid control value".into());
    }
    on_main(window, move |_| native::action(&action, value)).await
}
#[tauri::command]
fn take_inputs(s: tauri::State<AppState>) -> Vec<String> {
    s.inputs.lock().unwrap().ready()
}

fn deliver_input(app: &tauri::AppHandle, value: String) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Some(input) = state.inputs.lock().unwrap().push(value) {
            let _ = app.emit("open-input", input);
        }
    }
}

fn menus(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
    let appmenu = SubmenuBuilder::new(app, "TorrentCat")
        .item(&PredefinedMenuItem::about(
            app,
            Some("About TorrentCat"),
            None,
        )?)
        .separator()
        .item(
            &MenuItemBuilder::with_id("preferences", "Preferences…")
                .accelerator("CmdOrCtrl+,")
                .build(app)?,
        )
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;
    let file = SubmenuBuilder::new(app, "File")
        .item(
            &MenuItemBuilder::with_id("add", "Open Torrent Address…")
                .accelerator("CmdOrCtrl+U")
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("open", "Open Torrent File…")
                .accelerator("CmdOrCtrl+O")
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("create", "Create Torrent…")
                .accelerator("CmdOrCtrl+N")
                .build(app)?,
        )
        .separator()
        .close_window()
        .build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let transfer = SubmenuBuilder::new(app, "Transfers")
        .text("pause-all", "Pause All")
        .text("resume-all", "Resume All")
        .build()?;
    let view = SubmenuBuilder::new(app, "View")
        .text("library", "Library")
        .fullscreen()
        .build()?;
    app.set_menu(
        MenuBuilder::new(app)
            .items(&[&appmenu, &file, &edit, &view, &transfer])
            .build()?,
    )?;
    app.on_menu_event(|app, event| {
        let _ = app.emit("menu", event.id().as_ref());
    });
    Ok(())
}
fn main() {
    // Finder launches with a small descriptor budget shared by torrent sockets
    // and VLC. Raise this process's soft limit without changing the hard limit.
    #[cfg(target_os = "macos")]
    unsafe {
        let mut limit = std::mem::zeroed::<libc::rlimit>();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 {
            let requested = limit.rlim_max.min(4096);
            if limit.rlim_cur < requested {
                limit.rlim_cur = requested;
                if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                    eprintln!("Unable to increase open-file limit: {}", std::io::Error::last_os_error());
                }
            }
        }
    }
    // libVLC discovers its codecs through this path. Set it before Tauri/Tokio
    // start worker threads; mutating the process environment later is unsafe.
    unsafe {
        std::env::set_var(
            "VLC_PLUGIN_PATH",
            "/Applications/VLC.app/Contents/MacOS/plugins",
        );
    }
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let quitting = Arc::new(AtomicBool::new(false));
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
            for value in args.into_iter().skip(1) {
                deliver_input(app, value);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .setup(|app| {
            let directory = std::env::var_os("TORRENTCAT_PROFILE")
                .map(PathBuf::from)
                .unwrap_or(app.path().app_data_dir()?);
            let core = tauri::async_runtime::block_on(Core::new(
                directory,
                std::env::var_os("TORRENTCAT_OFFLINE").is_some(),
            ))?;
            let mut inputs: Vec<String> = std::env::args()
                .skip(1)
                .filter(|s| s.ends_with(".torrent") || s.starts_with("magnet:"))
                .collect();
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                inputs.extend(urls.into_iter().map(|u| u.to_string()));
            }
            let mut inbox = inbox::Inbox::default();
            for input in inputs {
                inbox.push(input);
            }
            app.manage(AppState {
                core: core.clone(),
                inputs: std::sync::Mutex::new(inbox),
                power: tokio::sync::Mutex::new(power::PlaybackPower::default()),
            });
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    deliver_input(&handle, url.to_string());
                }
            });
            menus(app)?;
            watcher::start(core.clone(), app.handle().clone());
            notifications::start(core, app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            default_handlers,
            set_default_handlers,
            snapshot,
            file_pieces,
            add_torrent,
            torrent_action,
            select_files,
            preferences,
            save_position,
            stream_file,
            stop_stream,
            media_probe,
            media_convert,
            media_poster,
            subtitle_file,
            subtitle_embedded,
            create_torrent,
            import_legacy,
            reveal_torrent,
            export_torrent,
            external_player,
            open_vlc_download,
            native_open,
            native_subtitle,
            native_close,
            native_resize,
            native_action,
            take_inputs
        ])
        .build(tauri::generate_context!())
        .expect("Unable to start TorrentCat");
    app.run(move |app, event| match event {
        tauri::RunEvent::ExitRequested { api, .. } => {
            if !quitting.swap(true, Ordering::SeqCst) {
                api.prevent_exit();
                let h = app.clone();
                let c = app.state::<AppState>().core.clone();
                let _ = native::close();
                tauri::async_runtime::spawn(async move {
                    // Release before transfer shutdown, which can take time.
                    // Mark closed under the same lock used by stream_file.
                    if let Err(error) = h.state::<AppState>().power.lock().await.shutdown().await {
                        eprintln!("Unable to release playback sleep prevention: {error}");
                    }
                    c.media.revoke_all().await;
                    let _ = c.restore_playback_transfers().await;
                    c.api.session().stop().await;
                    h.exit(0);
                });
            }
        }
        tauri::RunEvent::Opened { urls } => {
            for url in urls {
                let input = url
                    .to_file_path()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| url.to_string());
                deliver_input(app, input);
            }
        }
        tauri::RunEvent::Reopen { .. } => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
        _ => {}
    });
}
