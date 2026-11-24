use crate::core::CoreHandle;
use block2::RcBlock;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_app_kit::{NSEvent, NSEventMask};
use objc2_foundation::MainThreadMarker;
use std::cell::RefCell;
use std::ptr::NonNull;

thread_local! {
    static KEEP_ALIVE: RefCell<Vec<RcBlock<dyn Fn(*mut AnyObject)>>> = RefCell::new(Vec::new());
}

type NotifyBlock = RcBlock<dyn Fn(*mut AnyObject)>;

pub fn install_workspace_monitors(core: CoreHandle, _mtm: MainThreadMarker) {
    unsafe {
        let center: *mut AnyObject = {
            let workspace: *mut AnyObject = msg_send![AnyClass::get(c"NSWorkspace").expect("NSWorkspace"), sharedWorkspace];
            let center: *mut AnyObject = msg_send![workspace, notificationCenter];
            center
        };
        install_notification(center, "NSWorkspaceWillSleepNotification", {
            let core = core.clone();
            move |_note| {
                let sleeping_at = crate::util::now_ms();
                core.post_job(Box::new(move |core| {
                    crate::automations::system_wake::prepare_for_sleep(core, sleeping_at);
                }));
            }
        });
        install_notification(center, "NSWorkspaceDidWakeNotification", {
            let core = core.clone();
            move |_note| {
                let resumed_at = crate::util::now_ms();
                core.post_job(Box::new(move |core: &mut crate::core::Core| {
                    crate::core::on_system_resume(core, resumed_at);
                }));
            }
        });
        install_notification(center, "NSWorkspaceDidActivateApplicationNotification", {
            let core = core.clone();
            move |note| {
                let user_info: *mut AnyObject = msg_send![note, userInfo];
                let key = objc2_foundation::NSString::from_str("NSWorkspaceApplicationKey");
                let app: *mut AnyObject = msg_send![user_info, objectForKey: &*key];
                if app.is_null() {
                    return;
                }
                let pid: i32 = msg_send![app, processIdentifier];
                if pid == std::process::id() as i32 {
                    return;
                }
                core.post_job(Box::new(|core: &mut crate::core::Core| {
                    if core.monitor.panel_visible {
                        crate::monitor::hide_panel(core);
                    }
                }));
            }
        });

        let outside = RcBlock::new(move |event: NonNull<NSEvent>| {
            if MainThreadMarker::new().is_some_and(|mtm| super::is_tray_click(event.as_ptr().cast(), mtm)) {
                return;
            }
            core.post_job(Box::new(|core: &mut crate::core::Core| {
                crate::monitor::hide_panel(core);
            }));
        });
        let _ = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(NSEventMask::LeftMouseDown, &outside);
    }
}

unsafe fn install_notification(center: *mut AnyObject, name: &str, handler: impl Fn(*mut AnyObject) + 'static) {
    let name = objc2_foundation::NSString::from_str(name);
    let block: NotifyBlock = RcBlock::new(handler);
    let block_ref: &block2::Block<dyn Fn(*mut AnyObject)> = &*block;
    let observer: *mut AnyObject = msg_send![
        center,
        addObserverForName: &*name,
        object: std::ptr::null::<AnyObject>(),
        queue: std::ptr::null::<AnyObject>(),
        usingBlock: block_ref
    ];
    let _ = observer;
    KEEP_ALIVE.with(|keep| keep.borrow_mut().push(block));
}

// IOKit power assertion — Electron powerSaveBlocker 'prevent-app-suspension'.
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: *const AnyObject,
        level: u32,
        assertion_name: *const AnyObject,
        assertion_id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(assertion_id: u32) -> i32;
}

pub struct PowerAssertion {
    id: u32,
}

impl PowerAssertion {
    pub fn start() -> Option<PowerAssertion> {
        unsafe {
            let assertion_type = objc2_foundation::NSString::from_str("PreventUserIdleSystemSleep");
            let name = objc2_foundation::NSString::from_str("MeterNest Codex warmup");
            let mut assertion: u32 = 0;
            let result = IOPMAssertionCreateWithName(
                (&*assertion_type as *const objc2_foundation::NSString).cast(),
                255,
                (&*name as *const objc2_foundation::NSString).cast(),
                &mut assertion,
            );
            if result == 0 {
                Some(PowerAssertion { id: assertion })
            } else {
                None
            }
        }
    }

    pub fn stop(&mut self) {
        if self.id != 0 {
            unsafe {
                IOPMAssertionRelease(self.id);
            }
            self.id = 0;
        }
    }
}

impl Drop for PowerAssertion {
    fn drop(&mut self) {
        self.stop();
    }
}

// Carbon Esc hotkey for dismissing the panel even if another window has focus.
#[repr(C)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> *mut AnyObject;
    fn InstallEventHandler(
        target: *mut AnyObject,
        handler: unsafe extern "C" fn(*mut AnyObject, *mut AnyObject, *mut std::ffi::c_void) -> i32,
        num_types: u32,
        type_list: *const EventTypeSpec,
        user_data: *mut std::ffi::c_void,
        out_ref: *mut *mut AnyObject,
    ) -> i32;
    fn RegisterEventHotKey(key_code: u32, modifiers: u32, hot_key_id: EventHotKeyID, target: *mut AnyObject, options: *mut std::ffi::c_void, out_ref: *mut *mut AnyObject) -> i32;
    fn UnregisterEventHotKey(in_ref: *mut AnyObject) -> i32;
    fn GetEventParameter(
        event: *mut AnyObject,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        size: usize,
        actual_size: *mut u32,
        data: *mut std::ffi::c_void,
    ) -> i32;
}

const EVENT_PARAM_DIRECT_OBJECT: u32 = 0x2d2d2d2d; // '----'
const TYPE_EVENT_HOT_KEY_ID: u32 = 0x686b6964; // 'hkid' — Carbon typeEventHotKeyID
const EVENT_CLASS_KEYBOARD: u32 = 0x6b657962; // 'keyb'
const EVENT_HOT_KEY_PRESSED: u32 = 5;
const ESC_KEY_CODE: u32 = 53;

static ESC_SENDER: std::sync::OnceLock<CoreHandle> = std::sync::OnceLock::new();
static mut CURRENT_HOT_KEY: *mut AnyObject = std::ptr::null_mut();

unsafe extern "C" fn hot_key_handler(_target: *mut AnyObject, event: *mut AnyObject, _user: *mut std::ffi::c_void) -> i32 {
    let mut hot_id = EventHotKeyID { signature: 0, id: 0 };
    let mut actual: u32 = 0;
    let status = GetEventParameter(
        event,
        EVENT_PARAM_DIRECT_OBJECT,
        TYPE_EVENT_HOT_KEY_ID,
        std::ptr::null_mut(),
        std::mem::size_of::<EventHotKeyID>(),
        &mut actual,
        &mut hot_id as *mut _ as *mut std::ffi::c_void,
    );
    if status == 0 && hot_id.id == 1 {
        if let Some(sender) = ESC_SENDER.get() {
            sender.post_job(Box::new(|core: &mut crate::core::Core| {
                crate::monitor::on_escape_key(core);
            }));
        }
    }
    0
}

/// Register Esc as a global hotkey while the panel is visible.
pub fn register_escape(core: CoreHandle) -> bool {
    let _ = ESC_SENDER.set(core);
    unsafe {
        if !CURRENT_HOT_KEY.is_null() {
            return true;
        }
        let target = GetApplicationEventTarget();
        let types = [EventTypeSpec { event_class: EVENT_CLASS_KEYBOARD, event_kind: EVENT_HOT_KEY_PRESSED }];
        let mut handler_ref: *mut AnyObject = std::ptr::null_mut();
        let install = InstallEventHandler(target, hot_key_handler, 1, types.as_ptr(), std::ptr::null_mut(), &mut handler_ref);
        if install != 0 {
            return false;
        }
        let id = EventHotKeyID { signature: 0x4d_455445, id: 1 };
        let mut hot_key: *mut AnyObject = std::ptr::null_mut();
        let register = RegisterEventHotKey(ESC_KEY_CODE, 0, id, target, std::ptr::null_mut(), &mut hot_key);
        if register != 0 {
            return false;
        }
        CURRENT_HOT_KEY = hot_key;
        true
    }
}

pub fn unregister_escape() {
    unsafe {
        if !CURRENT_HOT_KEY.is_null() {
            UnregisterEventHotKey(CURRENT_HOT_KEY);
            CURRENT_HOT_KEY = std::ptr::null_mut();
        }
    }
}

