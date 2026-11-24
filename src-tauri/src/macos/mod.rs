pub mod power;
pub mod panel;
pub mod notifications;

use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_foundation::{MainThreadMarker, NSPoint, NSRange, NSRect, NSSize};
use tauri::{Manager, WebviewWindow};

const TRAILING_ADVANCE_ADJUSTMENT: f64 = -2.0;
const BASELINE_OFFSET: f64 = -0.5;
/// NSPopUpMenuWindowLevel — matches Electron's `pop-up-menu` panel level.
pub const POPUP_MENU_WINDOW_LEVEL: i64 = 101;

// NSAttributedString attribute keys are extern NSString constants in AppKit.
unsafe extern "C" {
    static NSFontAttributeName: *mut AnyObject;
    static NSBaselineOffsetAttributeName: *mut AnyObject;
    static NSKernAttributeName: *mut AnyObject;
}

unsafe fn class_name(pointer: *mut AnyObject) -> String {
    let name = objc2::ffi::object_getClassName(pointer);
    std::ffi::CStr::from_ptr(name).to_string_lossy().to_string()
}

unsafe fn array_count(array: *mut AnyObject) -> usize {
    if array.is_null() {
        return 0;
    }
    msg_send![array, count]
}

unsafe fn array_object(array: *mut AnyObject, index: usize) -> *mut AnyObject {
    msg_send![array, objectAtIndex: index]
}

unsafe fn search_status_button(view: *mut AnyObject) -> Option<*mut AnyObject> {
    if view.is_null() {
        return None;
    }
    if class_name(view) == "NSStatusBarButton" {
        return Some(view);
    }
    let subviews: *mut AnyObject = msg_send![view, subviews];
    for index in 0..array_count(subviews) {
        if let Some(found) = search_status_button(array_object(subviews, index)) {
            return Some(found);
        }
    }
    None
}

/// Find this process's single status bar button (our tray). Only our own
/// windows are traversed, so other apps' menu bar items can never match.
fn find_status_bar_button(_mtm: objc2_foundation::MainThreadMarker) -> Option<*mut AnyObject> {
    unsafe {
        let app: *mut AnyObject = msg_send![AnyClass::get(c"NSApplication").expect("NSApplication"), sharedApplication];
        let windows: *mut AnyObject = msg_send![app, windows];
        for index in 0..array_count(windows) {
            let window = array_object(windows, index);
            let content: *mut AnyObject = msg_send![window, contentView];
            if let Some(found) = search_status_button(content) {
                return Some(found);
            }
        }
    }
    None
}

fn ns_string(value: &str) -> *mut AnyObject {
    unsafe {
        let string = objc2_foundation::NSString::from_str(value);
        let pointer: *mut AnyObject = msg_send![&*string, retain];
        pointer
    }
}

unsafe fn release(pointer: *mut AnyObject) {
    if !pointer.is_null() {
        let _: () = msg_send![pointer, release];
    }
}

/// title-baseline.mm apply(): baseline down 0.5pt and reduced trailing
/// advance on the last glyph, preserving the system font/color arrangement.
/// Passing None/empty clears the title, restoring the icon-only item width.
pub fn apply_tray_title(text: Option<&str>, mtm: objc2_foundation::MainThreadMarker) -> bool {
    let Some(button) = find_status_bar_button(mtm) else {
        return false;
    };
    unsafe {
        let content = text.unwrap_or("");
        let mutable: *mut AnyObject = msg_send![AnyClass::get(c"NSMutableAttributedString").expect("NSMutableAttributedString"), alloc];
        let string = ns_string(content);
        let mutable: *mut AnyObject = msg_send![mutable, initWithString: string];
        if content.is_empty() {
            let _: () = msg_send![button, setAttributedTitle: mutable];
            let _: () = msg_send![button, setNeedsDisplay: true];
            sync_tray_click_target(button);
            release(mutable);
            release(string);
            return true;
        }
        let length: usize = msg_send![mutable, length];
        if length == 0 {
            release(mutable);
            release(string);
            return false;
        }
        let button_font: *mut AnyObject = msg_send![button, font];
        let point_size: f64 = if button_font.is_null() { 13.0 } else { msg_send![button_font, pointSize] };
        let font_class = AnyClass::get(c"NSFont").expect("NSFont");
        let font: *mut AnyObject = msg_send![font_class, monospacedDigitSystemFontOfSize: point_size, weight: 0.0f64];
        let baseline_class = AnyClass::get(c"NSNumber").expect("NSNumber");
        let baseline: *mut AnyObject = msg_send![baseline_class, numberWithDouble: BASELINE_OFFSET];
        let kern: *mut AnyObject = msg_send![baseline_class, numberWithDouble: TRAILING_ADVANCE_ADJUSTMENT];
        let full = NSRange::new(0, length);
        let _: () = msg_send![mutable, addAttribute: NSFontAttributeName, value: font, range: full];
        let _: () = msg_send![mutable, addAttribute: NSBaselineOffsetAttributeName, value: baseline, range: full];
        let _: () = msg_send![mutable, addAttribute: NSKernAttributeName, value: kern, range: NSRange::new(length - 1, 1)];
        let _: () = msg_send![button, setAttributedTitle: mutable];
        let _: () = msg_send![button, setNeedsDisplay: true];
        sync_tray_click_target(button);
        release(mutable);
        release(string);
        true
    }
}

/// The tray-icon crate routes every mouse event through a private
/// `TaoTrayTarget` view installed inside the status bar button, but that view
/// keeps the frame it was created with (icon-only width). Adding the value
/// readout as a title widens the button beyond that frame, leaving the text
/// outside the clickable area. Align the target view with the button so the
/// whole menu bar item responds to left/right clicks.
unsafe fn sync_tray_click_target(button: *mut AnyObject) {
    let subviews: *mut AnyObject = msg_send![button, subviews];
    for index in 0..array_count(subviews) {
        let subview = array_object(subviews, index);
        if class_name(subview) == "TaoTrayTarget" {
            let bounds: NSRect = msg_send![button, bounds];
            let _: () = msg_send![subview, setFrame: bounds];
            return;
        }
    }
}

/// Build a template NSImage with 1x/2x/3x representations for the M badge.
pub fn apply_tray_icon(reps: &[(u32, Vec<u8>)], mtm: objc2_foundation::MainThreadMarker) -> bool {
    let Some(button) = find_status_bar_button(mtm) else {
        return false;
    };
    unsafe {
        let image: *mut AnyObject = msg_send![AnyClass::get(c"NSImage").expect("NSImage"), alloc];
        let size = NSSize::new(18.0, 18.0);
        let image: *mut AnyObject = msg_send![image, initWithSize: size];
        for (scale, pixels) in reps {
            let width = 18 * *scale;
            let png = crate::monitor::tray_icon::encode_png(pixels, width, width);
            let data = objc2_foundation::NSData::with_bytes(&png);
            let rep_class = AnyClass::get(c"NSBitmapImageRep").expect("NSBitmapImageRep");
            let rep: *mut AnyObject = msg_send![rep_class, imageRepWithData: &*data];
            if !rep.is_null() {
                let _: () = msg_send![rep, setSize: size];
                let _: () = msg_send![image, addRepresentation: rep];
            }
        }
        let _: () = msg_send![image, setTemplate: true];
        let _: () = msg_send![button, setImage: image];
        let _: () = msg_send![button, setNeedsDisplay: true];
        sync_tray_click_target(button);
        release(image);
        true
    }
}

fn primary_max_y(_mtm: objc2_foundation::MainThreadMarker) -> f64 {
    unsafe {
        let screens: *mut AnyObject = msg_send![AnyClass::get(c"NSScreen").expect("NSScreen"), screens];
        if array_count(screens) == 0 {
            return 0.0;
        }
        let primary = array_object(screens, 0);
        let frame: NSRect = msg_send![primary, frame];
        frame.origin.y + frame.size.height
    }
}

/// tray.getBounds(): the status item frame in Electron top-left DIP space.
pub fn tray_bounds(mtm: objc2_foundation::MainThreadMarker) -> Option<(f64, f64, f64, f64)> {
    let button = find_status_bar_button(mtm)?;
    unsafe {
        let window: *mut AnyObject = msg_send![button, window];
        if window.is_null() {
            return None;
        }
        let frame: NSRect = msg_send![window, frame];
        let primary_max = primary_max_y(mtm);
        Some((frame.origin.x, primary_max - (frame.origin.y + frame.size.height), frame.size.width, frame.size.height))
    }
}

/// Use the click's own position, even if the pointer has moved since it occurred.
unsafe fn is_tray_click(event: *mut AnyObject, mtm: MainThreadMarker) -> bool {
    let Some((x, y, width, height)) = tray_bounds(mtm) else {
        return false;
    };
    let event_window: *mut AnyObject = msg_send![event, window];
    let location: NSPoint = msg_send![event, locationInWindow];
    let screen_point: NSPoint = if event_window.is_null() {
        location
    } else {
        msg_send![event_window, convertPointToScreen: location]
    };
    let screen_y = primary_max_y(mtm) - screen_point.y;
    screen_point.x >= x && screen_point.x < x + width && screen_y >= y && screen_y < y + height
}

/// Global geometry in Electron coordinates (top-left origin, points).
pub fn screens(_mtm: objc2_foundation::MainThreadMarker) -> Vec<ScreenFrame> {
    let mut result = Vec::new();
    unsafe {
        let screens: *mut AnyObject = msg_send![AnyClass::get(c"NSScreen").expect("NSScreen"), screens];
        let count = array_count(screens);
        if count == 0 {
            return result;
        }
        let primary_frame: NSRect = msg_send![array_object(screens, 0), frame];
        let primary_max = primary_frame.origin.y + primary_frame.size.height;
        for index in 0..count {
            let screen = array_object(screens, index);
            let frame: NSRect = msg_send![screen, frame];
            let visible: NSRect = msg_send![screen, visibleFrame];
            result.push(ScreenFrame {
                x: visible.origin.x,
                y: primary_max - (visible.origin.y + visible.size.height),
                width: visible.size.width,
                height: visible.size.height,
                frame_x: frame.origin.x,
                frame_y: primary_max - (frame.origin.y + frame.size.height),
                frame_width: frame.size.width,
                frame_height: frame.size.height,
            });
        }
    }
    result
}

#[derive(Debug, Clone, Copy)]
pub struct ScreenFrame {
    /// visibleFrame (work area) in top-left DIP
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// full frame in top-left DIP
    pub frame_x: f64,
    pub frame_y: f64,
    pub frame_width: f64,
    pub frame_height: f64,
}

/// screen.getCursorScreenPoint(): NSEvent mouseLocation flipped to top-left.
pub fn cursor_screen_point(mtm: objc2_foundation::MainThreadMarker) -> (f64, f64) {
    unsafe {
        let point: NSSize = msg_send![AnyClass::get(c"NSEvent").expect("NSEvent"), mouseLocation];
        (point.width, primary_max_y(mtm) - point.height)
    }
}

/// screen.getDisplayMatching(bounds).workArea — nearest screen by overlap.
pub fn work_area_for(anchor: (f64, f64, f64, f64), mtm: objc2_foundation::MainThreadMarker) -> ScreenFrame {
    let list = screens(mtm);
    let mut best = list.first().copied();
    let mut best_overlap = 0.0f64;
    for screen in &list {
        let overlap_x = (anchor.0 + anchor.2).min(screen.frame_x + screen.frame_width) - anchor.0.max(screen.frame_x);
        let overlap_y = (anchor.1 + anchor.3).min(screen.frame_y + screen.frame_height) - anchor.1.max(screen.frame_y);
        let overlap = overlap_x.max(0.0) * overlap_y.max(0.0);
        if overlap > best_overlap {
            best_overlap = overlap;
            best = Some(*screen);
        }
    }
    best.unwrap_or(ScreenFrame {
        x: 0.0,
        y: 0.0,
        width: 1440.0,
        height: 900.0,
        frame_x: 0.0,
        frame_y: 0.0,
        frame_width: 1440.0,
        frame_height: 900.0,
    })
}

pub fn with_ns_window<F: FnOnce(*mut AnyObject)>(window: &WebviewWindow, apply: F) {
    if let Ok(pointer) = window.ns_window() {
        apply(pointer as *mut AnyObject);
    }
}

pub fn configure_hover_window(window: &WebviewWindow) {
    let window = window.clone();
    let handle = window.app_handle().clone();
    let _ = handle.run_on_main_thread(move || {
        with_ns_window(&window, |ns_window| unsafe {
            let _: () = msg_send![ns_window, setLevel: POPUP_MENU_WINDOW_LEVEL];
            let _: () = msg_send![ns_window, setAnimationBehavior: objc2_app_kit::NSWindowAnimationBehavior::None];
        });
    });
}

pub fn order_front_regardless(window: &WebviewWindow) {
    let window = window.clone();
    let handle = window.app_handle().clone();
    let _ = handle.run_on_main_thread(move || {
        with_ns_window(&window, |ns_window| unsafe {
            let _: () = msg_send![ns_window, orderFrontRegardless];
        });
    });
}

pub fn order_out(window: &WebviewWindow) {
    let window = window.clone();
    let handle = window.app_handle().clone();
    let _ = handle.run_on_main_thread(move || {
        with_ns_window(&window, |ns_window| unsafe {
            let _: () = msg_send![ns_window, orderOut: std::ptr::null::<AnyObject>()];
        });
    });
}

pub fn open_url(url: &str) -> bool {
    std::process::Command::new("open")
        .arg(url)
        .spawn()
        .map(|_| true)
        .unwrap_or(false)
}
