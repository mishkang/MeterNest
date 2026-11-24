use objc2::rc::Retained;
use objc2::{define_class, msg_send, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSPanel, NSResponder, NSView, NSWindow, NSWindowAnimationBehavior,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
use std::cell::RefCell;
use tauri::{AppHandle, Manager, WebviewWindow};

define_class!(
    #[unsafe(super(NSPanel, NSWindow, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MeterNestQuotaPanel"]
    struct QuotaPanel;

    impl QuotaPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }
    }
);

thread_local! {
    static PANEL: RefCell<Option<Retained<QuotaPanel>>> = const { RefCell::new(None) };
}

pub fn attach(window: &WebviewWindow) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    let scheduled = window.with_webview(move |webview| {
        let attached = MainThreadMarker::new().is_some_and(|mtm| unsafe {
            let owner = &*webview.ns_window().cast::<NSWindow>();
            let Some(content) = owner.contentView() else {
                return false;
            };
            let panel: Retained<QuotaPanel> = msg_send![
                QuotaPanel::alloc(mtm),
                initWithContentRect: owner.frame(),
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ];
            panel.setReleasedWhenClosed(false);
            panel.setHidesOnDeactivate(false);
            panel.setBecomesKeyOnlyIfNeeded(false);
            panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
            panel.setLevel(super::POPUP_MENU_WINDOW_LEVEL as _);
            panel.setCollectionBehavior(NSWindowCollectionBehavior::MoveToActiveSpace | NSWindowCollectionBehavior::FullScreenAuxiliary);
            panel.setOpaque(false);
            panel.setBackgroundColor(Some(&NSColor::clearColor()));
            panel.setHasShadow(false);
            panel.setAcceptsMouseMovedEvents(true);

            // Keep Tauri's hidden window as the owner of the existing WebView
            // and its IPC. AppKit presents the same content in the quota panel.
            owner.setContentView(None);
            panel.setContentView(Some(&content));
            PANEL.with_borrow_mut(|current| *current = Some(panel));
            true
        });
        let _ = tx.send(attached);
    });
    scheduled.is_ok() && rx.recv().unwrap_or(false)
}

pub fn set_bounds(app: &AppHandle, (x, y, width, height): (f64, f64, f64, f64)) {
    let _ = app.run_on_main_thread(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if let Some(panel) = PANEL.with_borrow(|current| current.clone()) {
            let origin = NSPoint::new(x, super::primary_max_y(mtm) - y - height);
            let frame = NSRect::new(origin, NSSize::new(width, height));
            if panel.frame() != frame {
                panel.setFrame_display(frame, panel.isVisible());
            }
        }
    });
}

pub fn show(window: &WebviewWindow) {
    let _ = window.with_webview(|webview| {
        if let Some(panel) = PANEL.with_borrow(|current| current.clone()) {
            if !panel.isVisible() || !panel.isKeyWindow() {
                panel.makeKeyAndOrderFront(None);
                let view = unsafe { &*webview.inner().cast::<NSView>() };
                panel.makeFirstResponder(Some(view));
                panel.invalidateCursorRectsForView(view);
            }
        }
    });
}

pub fn hide(app: &AppHandle) {
    let _ = app.run_on_main_thread(|| {
        if let Some(panel) = PANEL.with_borrow(|current| current.clone()) {
            panel.orderOut(None);
        }
    });
}

pub fn dispose(window: &WebviewWindow) {
    let window = window.clone();
    let app = window.app_handle().clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(panel) = PANEL.with_borrow_mut(Option::take) {
            panel.orderOut(None);
            if let Ok(owner) = window.ns_window() {
                let owner = unsafe { &*owner.cast::<NSWindow>() };
                let content = panel.contentView();
                panel.setContentView(None);
                owner.setContentView(content.as_deref());
            }
        }
    });
}
