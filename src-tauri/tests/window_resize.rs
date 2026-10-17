//! AppKit needs the process main thread, so this regression test has no libtest harness.
//! The window is never displayed and no torrent or VLC installation is needed.
#[path = "../src/native.rs"]
#[allow(dead_code, unused_imports)]
mod native;

use objc2::{class, msg_send, runtime::AnyObject};
use objc2_foundation::{NSPoint, NSRect, NSSize};

fn main() {
    objc2::rc::autoreleasepool(|_| unsafe {
        let _app = objc2_app_kit::NSApplication::sharedApplication(objc2::MainThreadMarker::new().expect("AppKit main thread"));
        let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
        let window: *mut AnyObject = msg_send![window,
            initWithContentRect: NSRect::new(NSPoint::new(100.0, 100.0), NSSize::new(1120.0, 780.0)),
            styleMask: 15usize, backing: 2usize, defer: false];
        let _: () = msg_send![window, setReleasedWhenClosed: false];
        let minimum = NSSize::new(660.0, 480.0);
        let _: () = msg_send![window, setContentMinSize: minimum];
        let host: *mut AnyObject = msg_send![window, contentView];
        let initial_aspect: NSSize = msg_send![window, contentAspectRatio];
        assert_eq!(initial_aspect, NSSize::new(0.0, 0.0));

        // Returning to the library, then title-bar zooming, used to abort AppKit
        // with frame={{0, nan}, {screen_width, nan}} after restoring a zero ratio.
        for (width, height) in [(1920.0, 1080.0), (720.0, 1280.0), (1920.0, 1080.0)] {
            native::fit_window(host as usize, width, height).unwrap();
            native::fit_window(host as usize, 0.0, 0.0).unwrap();
            native::fit_window(host as usize, 0.0, 0.0).unwrap(); // repeated cleanup
            let restored_minimum: NSSize = msg_send![window, contentMinSize];
            assert_eq!(restored_minimum, minimum);
            for _ in 0..2 {
                let _: () = msg_send![window, performZoom: std::ptr::null::<AnyObject>()];
                let frame: NSRect = msg_send![window, frame];
                assert!(frame.origin.x.is_finite() && frame.origin.y.is_finite());
                assert!(frame.size.width.is_finite() && frame.size.width > 0.0);
                assert!(frame.size.height.is_finite() && frame.size.height > 0.0);
            }
        }

        // Preserve a genuine pre-existing constraint, including resize increments.
        let increments = NSSize::new(4.0, 3.0);
        let _: () = msg_send![window, setContentResizeIncrements: increments];
        native::fit_window(host as usize, 1920.0, 1080.0).unwrap();
        native::fit_window(host as usize, 0.0, 0.0).unwrap();
        let restored: NSSize = msg_send![window, contentResizeIncrements];
        assert_eq!(restored, increments);
        let aspect = NSSize::new(4.0, 3.0);
        let _: () = msg_send![window, setContentAspectRatio: aspect];
        native::fit_window(host as usize, 1920.0, 1080.0).unwrap();
        native::fit_window(host as usize, 0.0, 0.0).unwrap();
        let restored: NSSize = msg_send![window, contentAspectRatio];
        assert_eq!(restored, aspect);
        let _: () = msg_send![window, close];
        let _: () = msg_send![window, release];
    });
    println!("Window resize regression passed: playback → restore → zoom, repeated cleanup, and prior constraints");
}
