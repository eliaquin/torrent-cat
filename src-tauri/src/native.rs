//! The libVLC instance and Cocoa surface are owned together. All calls are serialized
//! on the main thread; the libraries stay loaded until the player has been released.
use anyhow::{Context, Result, bail};
use libloading::Library;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    sync::Mutex,
};
static PLAYER: Mutex<Option<Player>> = Mutex::new(None);
static WINDOW_FIT: Mutex<Option<(NSSize, NSSize)>> = Mutex::new(None);

fn fitted_size(current: NSSize, minimum: NSSize, maximum: NSSize, ratio: f64) -> NSSize {
    let max_width = maximum.width.min(maximum.height * ratio);
    let min_width = minimum.width.max(minimum.height * ratio).min(max_width);
    let width = current.width.max(min_width).min(max_width);
    NSSize::new(width, width / ratio)
}

pub fn fit_window(host: usize, width: f64, height: f64) -> Result<Value> {
    if !width.is_finite() || !height.is_finite() || width < 0.0 || height < 0.0 {
        bail!("Invalid video dimensions");
    }
    unsafe {
        let host = host as *mut AnyObject;
        let window: *mut AnyObject = msg_send![host, window];
        let mut saved = WINDOW_FIT.lock().unwrap();
        if width == 0.0 || height == 0.0 {
            if let Some((minimum, aspect)) = saved.take() {
                let _: () = msg_send![window, setContentAspectRatio: aspect];
                let _: () = msg_send![window, setContentMinSize: minimum];
            }
            return Ok(json!({}));
        }
        let screen: *mut AnyObject = msg_send![window, screen];
        if screen.is_null() { return Ok(json!({})); }
        let visible: NSRect = msg_send![screen, visibleFrame];
        let bounds: NSRect = msg_send![host, bounds];
        let mut frame: NSRect = msg_send![window, frame];
        let minimum: NSSize = msg_send![window, contentMinSize];
        let aspect: NSSize = msg_send![window, contentAspectRatio];
        let (minimum, _) = *saved.get_or_insert((minimum, aspect));
        let ratio = width / height;
        if !ratio.is_finite() || ratio <= 0.0 { bail!("Invalid video aspect ratio"); }
        let _: () = msg_send![window, setContentAspectRatio: NSSize::new(ratio, 1.0)];
        // Fullscreen is controlled by macOS; fit again after returning windowed.
        let style: usize = msg_send![window, styleMask];
        if style & (1 << 14) != 0 { return Ok(json!({})); }
        let chrome = NSSize::new(frame.size.width - bounds.size.width, frame.size.height - bounds.size.height);
        let maximum = NSSize::new((visible.size.width - chrome.width).max(1.0), (visible.size.height - chrome.height).max(1.0));
        let size = fitted_size(bounds.size, minimum, maximum, ratio);
        let player_min = fitted_size(NSSize::new(0.0, 0.0), minimum, maximum, ratio);
        let _: () = msg_send![window, setContentMinSize: player_min];
        let top = frame.origin.y + frame.size.height;
        frame.size = NSSize::new(size.width + chrome.width, size.height + chrome.height);
        frame.origin.x = frame.origin.x.max(visible.origin.x).min(visible.origin.x + visible.size.width - frame.size.width);
        frame.origin.y = (top - frame.size.height).max(visible.origin.y).min(visible.origin.y + visible.size.height - frame.size.height);
        let _: () = msg_send![window, setFrame: frame, display: true];
    }
    Ok(json!({}))
}
struct Player {
    _core: Library,
    lib: Library,
    instance: usize,
    player: usize,
    view: usize,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
macro_rules! call {($s:expr,$name:literal,$ty:ty $(,$arg:expr)*)=>{{let f:libloading::Symbol<$ty>=$s.lib.get(concat!($name,"\0").as_bytes())?;f($($arg),*)}};}
unsafe fn frame(host: *mut AnyObject, r: Rect) -> NSRect {
    unsafe {
        let bounds: NSRect = msg_send![host, bounds];
        let flipped: bool = msg_send![host, isFlipped];
        NSRect::new(
            NSPoint::new(
                r.x,
                if flipped {
                    r.y
                } else {
                    bounds.size.height - r.y - r.height
                },
            ),
            NSSize::new(r.width.max(1.0), r.height.max(1.0)),
        )
    }
}
pub fn open(host: usize, url: String, r: Rect, position: f64) -> Result<Value> {
    if !position.is_finite()
        || position < 0.0
        || ![r.x, r.y, r.width, r.height].iter().all(|v| v.is_finite())
    {
        bail!("Invalid playback position or video dimensions");
    }
    let loc = CString::new(url)?;
    close()?;
    unsafe {
        let root = "/Applications/VLC.app/Contents/MacOS";
        let core: Library = libloading::os::unix::Library::open(
            Some(format!("{root}/lib/libvlccore.dylib")),
            libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_GLOBAL,
        )?
        .into();
        let lib = Library::new(format!("{root}/lib/libvlc.dylib"))
            .context("Install VLC 3 matching this Mac's architecture")?;
        let version: libloading::Symbol<unsafe extern "C" fn() -> *const c_char> =
            lib.get(b"libvlc_get_version\0")?;
        if !CStr::from_ptr(version())
            .to_string_lossy()
            .starts_with("3.")
        {
            bail!("Native playback requires VLC 3");
        }
        let options = [
            "--no-video-title-show",
            "--no-osd",
            "--no-snapshot-preview",
            "--no-video-on-top",
            "--vout=macosx",
            "--aout=auhal",
        ]
        .map(|s| CString::new(s).unwrap());
        let ptrs: Vec<_> = options.iter().map(|s| s.as_ptr()).collect();
        let new: libloading::Symbol<
            unsafe extern "C" fn(i32, *const *const c_char) -> *mut c_void,
        > = lib.get(b"libvlc_new\0")?;
        let instance = new(ptrs.len() as i32, ptrs.as_ptr());
        if instance.is_null() {
            let error: libloading::Symbol<unsafe extern "C" fn() -> *const c_char> =
                lib.get(b"libvlc_errmsg\0")?;
            let error = error();
            let detail = if error.is_null() {
                "No codec plugins were found".into()
            } else {
                CStr::from_ptr(error).to_string_lossy()
            };
            bail!("VLC initialization failed: {detail}");
        }
        let mut state = Player {
            _core: core,
            lib,
            instance: instance as usize,
            player: 0,
            view: 0,
        };
        let player = call!(
            state,
            "libvlc_media_player_new",
            unsafe extern "C" fn(*mut c_void) -> *mut c_void,
            instance
        );
        if player.is_null() {
            bail!("VLC could not create a player");
        }
        state.player = player as usize;
        let media = call!(
            state,
            "libvlc_media_new_location",
            unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
            instance,
            loc.as_ptr()
        );
        if media.is_null() {
            bail!("VLC could not open this stream");
        }
        if position > 0.0 {
            let option = CString::new(format!(":start-time={position}"))?;
            call!(
                state,
                "libvlc_media_add_option",
                unsafe extern "C" fn(*mut c_void, *const c_char),
                media,
                option.as_ptr()
            );
        }
        let host = host as *mut AnyObject;
        let view: *mut AnyObject = msg_send![class!(NSView), alloc];
        let view: *mut AnyObject = msg_send![view,initWithFrame:frame(host,r)];
        // Keep the transparent WKWebView above the video. Its DOM owns input and
        // overlays, including fullscreen controls over VLC and its letterboxing.
        let _: () = msg_send![view, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![view, layer];
        let black: *mut AnyObject = msg_send![class!(NSColor), blackColor];
        let color: *const c_void = msg_send![black, CGColor];
        let _: () = msg_send![layer, setBackgroundColor: color];
        let _: () = msg_send![host, addSubview: view, positioned: -1isize, relativeTo: std::ptr::null::<AnyObject>()];
        state.view = view as usize;
        call!(
            state,
            "libvlc_media_player_set_nsobject",
            unsafe extern "C" fn(*mut c_void, *mut c_void),
            player,
            view.cast()
        );
        call!(
            state,
            "libvlc_video_set_mouse_input",
            unsafe extern "C" fn(*mut c_void, u32),
            player,
            0
        );
        call!(
            state,
            "libvlc_video_set_key_input",
            unsafe extern "C" fn(*mut c_void, u32),
            player,
            0
        );
        call!(
            state,
            "libvlc_media_player_set_media",
            unsafe extern "C" fn(*mut c_void, *mut c_void),
            player,
            media
        );
        call!(
            state,
            "libvlc_media_release",
            unsafe extern "C" fn(*mut c_void),
            media
        );
        let result = call!(
            state,
            "libvlc_media_player_play",
            unsafe extern "C" fn(*mut c_void) -> i32,
            player
        );
        *PLAYER.lock().unwrap() = Some(state);
        if result != 0 {
            close()?;
            bail!("VLC could not start playback");
        }
    }
    Ok(json!({"started":true}))
}
impl Drop for Player {
    fn drop(&mut self) {
        // Player is created and dropped only on the main thread. Cleanup must
        // continue even if a symbol is unexpectedly unavailable.
        unsafe {
            for (symbol, ptr) in [
                (b"libvlc_media_player_stop\0".as_slice(), self.player),
                (b"libvlc_media_player_release\0".as_slice(), self.player),
                (b"libvlc_release\0".as_slice(), self.instance),
            ] {
                if ptr != 0 {
                    if let Ok(f) = self.lib.get::<unsafe extern "C" fn(*mut c_void)>(symbol) {
                        f(ptr as _);
                    }
                }
            }
            if self.view != 0 {
                let view = self.view as *mut AnyObject;
                let _: () = msg_send![view, removeFromSuperview];
                let _: () = msg_send![view, release];
            }
        }
    }
}
pub fn close() -> Result<Value> {
    drop(PLAYER.lock().unwrap().take());
    Ok(json!({}))
}
pub fn resize(host: usize, r: Rect) -> Result<Value> {
    let guard = PLAYER.lock().unwrap();
    if let Some(s) = guard.as_ref() {
        unsafe {
            let v = s.view as *mut AnyObject;
            let _: () = msg_send![v,setFrame:frame(host as _,r)];
        }
    }
    Ok(json!({}))
}
pub fn action(action: &str, value: f64) -> Result<Value> {
    let guard = PLAYER.lock().unwrap();
    let s = guard.as_ref().context("Native player is not open")?;
    let p = s.player as *mut c_void;
    unsafe {
        match action {
            "replay" => {
                call!(
                    s,
                    "libvlc_media_player_play",
                    unsafe extern "C" fn(*mut c_void) -> i32,
                    p
                );
                call!(
                    s,
                    "libvlc_media_player_set_time",
                    unsafe extern "C" fn(*mut c_void, i64),
                    p,
                    0
                );
            }
            "pause" => {
                call!(
                    s,
                    "libvlc_media_player_set_pause",
                    unsafe extern "C" fn(*mut c_void, i32),
                    p,
                    value as i32
                );
            }
            "seek" => {
                call!(
                    s,
                    "libvlc_media_player_set_time",
                    unsafe extern "C" fn(*mut c_void, i64),
                    p,
                    (value * 1000.0) as i64
                );
            }
            "volume" => {
                call!(
                    s,
                    "libvlc_audio_set_volume",
                    unsafe extern "C" fn(*mut c_void, i32) -> i32,
                    p,
                    (value.clamp(0.0, 1.0) * 100.0) as i32
                );
            }
            "rate" => {
                call!(
                    s,
                    "libvlc_media_player_set_rate",
                    unsafe extern "C" fn(*mut c_void, f32) -> i32,
                    p,
                    value as f32
                );
            }
            "audio" => {
                call!(
                    s,
                    "libvlc_audio_set_track",
                    unsafe extern "C" fn(*mut c_void, i32) -> i32,
                    p,
                    value as i32
                );
            }
            "subtitle" => {
                call!(
                    s,
                    "libvlc_video_set_spu",
                    unsafe extern "C" fn(*mut c_void, i32) -> i32,
                    p,
                    value as i32
                );
            }
            "status" => {}
            _ => bail!("Unknown player action"),
        };
        let time = call!(
            s,
            "libvlc_media_player_get_time",
            unsafe extern "C" fn(*mut c_void) -> i64,
            p
        );
        let length = call!(
            s,
            "libvlc_media_player_get_length",
            unsafe extern "C" fn(*mut c_void) -> i64,
            p
        );
        let state = call!(
            s,
            "libvlc_media_player_get_state",
            unsafe extern "C" fn(*mut c_void) -> i32,
            p
        );
        let audio_track = call!(
            s, "libvlc_audio_get_track",
            unsafe extern "C" fn(*mut c_void) -> i32, p
        );
        let subtitle_track = call!(
            s, "libvlc_video_get_spu",
            unsafe extern "C" fn(*mut c_void) -> i32, p
        );
        let (mut width, mut height) = (0u32, 0u32);
        let _ = call!(s, "libvlc_video_get_size",
            unsafe extern "C" fn(*mut c_void, u32, *mut u32, *mut u32) -> i32,
            p, 0, &mut width, &mut height);
        Ok(
            json!({"time":(time.max(0) as f64)/1000.0,"duration":(length.max(0) as f64)/1000.0,"state":state,"width":width,"height":height,"audio":tracks(s,false)?,"subtitles":tracks(s,true)?,"audioTrack":audio_track,"subtitleTrack":subtitle_track}),
        )
    }
}
#[repr(C)]
struct Track {
    id: i32,
    name: *mut c_char,
    next: *mut Track,
}
unsafe fn tracks(s: &Player, subtitles: bool) -> Result<Value> {
    unsafe {
        let first = if subtitles {
            call!(
                s,
                "libvlc_video_get_spu_description",
                unsafe extern "C" fn(*mut c_void) -> *mut Track,
                s.player as _
            )
        } else {
            call!(
                s,
                "libvlc_audio_get_track_description",
                unsafe extern "C" fn(*mut c_void) -> *mut Track,
                s.player as _
            )
        };
        let mut cur = first;
        let mut result = Vec::new();
        while !cur.is_null() && result.len() < 100 {
            let t = &*cur;
            result.push(json!({"id":t.id,"name":if t.name.is_null(){String::new()}else{CStr::from_ptr(t.name).to_string_lossy().into_owned()}}));
            cur = t.next;
        }
        if !first.is_null() {
            call!(
                s,
                "libvlc_track_description_list_release",
                unsafe extern "C" fn(*mut Track),
                first
            );
        }
        Ok(json!(result))
    }
}

pub fn subtitle(url: String) -> Result<Value> {
    let guard = PLAYER.lock().unwrap();
    let s = guard.as_ref().context("Native player is not open")?;
    let url = CString::new(url)?;
    unsafe {
        let result = call!(
            s,
            "libvlc_media_player_add_slave",
            unsafe extern "C" fn(*mut c_void, u32, *const c_char, bool) -> i32,
            s.player as _,
            0,
            url.as_ptr(),
            true
        );
        if result != 0 {
            bail!("VLC could not load subtitles");
        }
    }
    Ok(json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widescreen_fits_existing_width_without_cropping() {
        let size = fitted_size(NSSize::new(1612.0, 1062.0), NSSize::new(660.0, 480.0), NSSize::new(3440.0, 1380.0), 16.0 / 9.0);
        assert_eq!(size.width, 1612.0);
        assert!((size.height - 906.75).abs() < 0.01);
    }

    #[test]
    fn portrait_video_fits_on_screen_even_when_library_minimum_is_too_wide() {
        let size = fitted_size(NSSize::new(1612.0, 1062.0), NSSize::new(660.0, 480.0), NSSize::new(1920.0, 1000.0), 9.0 / 16.0);
        assert_eq!(size, NSSize::new(562.5, 1000.0));
    }

    #[test]
    fn small_window_preserves_minimum_height_and_video_proportions() {
        let ratio = 16.0 / 9.0;
        let size = fitted_size(NSSize::new(660.0, 480.0), NSSize::new(660.0, 480.0), NSSize::new(1920.0, 1000.0), ratio);
        assert_eq!(size.height, 480.0);
        assert!((size.width / size.height - ratio).abs() < 0.0001);
    }
}
