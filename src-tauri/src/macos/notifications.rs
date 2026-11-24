use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{define_class, msg_send, MainThreadOnly};
use objc2_foundation::{MainThreadMarker, NSString};

#[link(name = "UserNotifications", kind = "framework")]
unsafe extern "C" {}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MeterNestNotificationDelegate"]
    struct NotificationDelegate;

    impl NotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(&self, _center: &AnyObject, _notification: &AnyObject, completion: &block2::Block<dyn Fn(usize)>) {
            // Banner and Notification Center list, including while settings is open.
            completion.call(((1 << 3) | (1 << 4),));
        }
    }
);

thread_local! {
    static DELEGATE: std::cell::OnceCell<Retained<NotificationDelegate>> = const { std::cell::OnceCell::new() };
}

pub fn show(app: &tauri::AppHandle, title: &str, message: String) {
    let title = title.to_owned();
    let _ = app.run_on_main_thread(move || {
        let Some(mtm) = MainThreadMarker::new() else { return };
        unsafe {
            let center: *mut AnyObject = msg_send![AnyClass::get(c"UNUserNotificationCenter").unwrap(), currentNotificationCenter];
            DELEGATE.with(|slot| {
                let delegate = slot.get_or_init(|| msg_send![NotificationDelegate::alloc(mtm), init]);
                let _: () = msg_send![center, setDelegate: &**delegate];
            });
            let content: Retained<AnyObject> = msg_send![AnyClass::get(c"UNMutableNotificationContent").unwrap(), new];
            let _: () = msg_send![&*content, setTitle: &*NSString::from_str(&title)];
            let _: () = msg_send![&*content, setBody: &*NSString::from_str(&message)];
            let uuid: *mut AnyObject = msg_send![AnyClass::get(c"NSUUID").unwrap(), UUID];
            let id: *mut AnyObject = msg_send![uuid, UUIDString];
            let request: *mut AnyObject = msg_send![AnyClass::get(c"UNNotificationRequest").unwrap(),
                requestWithIdentifier: id, content: &*content, trigger: std::ptr::null::<AnyObject>()];
            let completion = block2::RcBlock::new(|error: *mut AnyObject| {
                if !error.is_null() {
                    eprintln!("MeterNest: macOS rejected the operation notification");
                }
            });
            let block: &block2::Block<dyn Fn(*mut AnyObject)> = &*completion;
            let _: () = msg_send![center, addNotificationRequest: request, withCompletionHandler: block];
        }
    });
}
