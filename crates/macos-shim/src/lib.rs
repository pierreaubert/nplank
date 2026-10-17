//! macOS Objective-C surface for nplank: menu-bar status item and
//! user-notification click-through.
//!
//! This crate is the single quarantined home of `unsafe` code (the main
//! crate forbids it). All AppKit/UserNotifications work happens on the
//! main thread; cross-thread state moves through mutex-guarded queues
//! drained by the app's poll loop.

#![allow(unsafe_code)]

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, ClassType, MainThreadMarker, define_class, msg_send, sel};
use objc2_app_kit::{NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength};
use objc2_foundation::{NSDictionary, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationInterruptionLevel, UNNotificationPresentationOptions, UNNotificationRequest,
    UNNotificationResponse, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use std::cell::RefCell;
use std::sync::Mutex;

/// Actions requested from the menu-bar item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusAction {
    /// Bring the app forward.
    Open,
    /// Stop every busy run.
    StopAll,
    /// Quit the app.
    Quit,
}

static ACTIONS: Mutex<Vec<StatusAction>> = Mutex::new(Vec::new());
static CLICKS: Mutex<Vec<String>> = Mutex::new(Vec::new());

thread_local! {
    static STATUS_ITEM: RefCell<Option<Retained<NSStatusItem>>> =
        const { RefCell::new(None) };
    static HANDLER: RefCell<Option<Retained<StatusHandler>>> =
        const { RefCell::new(None) };
}

fn push_action(action: StatusAction) {
    ACTIONS.lock().expect("actions lock").push(action);
}

fn push_click(session_id: String) {
    CLICKS.lock().expect("clicks lock").push(session_id);
}

/// Drains pending menu-bar actions (call from the UI poll loop).
pub fn drain_actions() -> Vec<StatusAction> {
    std::mem::take(&mut *ACTIONS.lock().expect("actions lock"))
}

/// Drains session ids of clicked notifications.
pub fn drain_notification_clicks() -> Vec<String> {
    std::mem::take(&mut *CLICKS.lock().expect("clicks lock"))
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NPlankStatusHandler"]
    #[ivars = ()]
    struct StatusHandler;

    impl StatusHandler {
        #[unsafe(method(openApp:))]
        fn open_app(&self, _sender: &AnyObject) {
            push_action(StatusAction::Open);
        }

        #[unsafe(method(stopAll:))]
        fn stop_all(&self, _sender: &AnyObject) {
            push_action(StatusAction::StopAll);
        }

        #[unsafe(method(quitApp:))]
        fn quit_app(&self, _sender: &AnyObject) {
            push_action(StatusAction::Quit);
        }
    }

    unsafe impl NSObjectProtocol for StatusHandler {}

    unsafe impl UNUserNotificationCenterDelegate for StatusHandler {
        #[allow(non_snake_case)]
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn userNotificationCenter_didReceiveNotificationResponse_withCompletionHandler(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            let content = response.notification().request().content();
            let user_info = content.userInfo();
            let key = NSString::from_str("session_id");
            if let Some(value) = user_info.objectForKey(&key)
                && let Ok(text) = value.downcast::<NSString>()
            {
                push_click(text.to_string());
            }
            completion.call(());
        }

        #[allow(non_snake_case)]
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn userNotificationCenter_willPresentNotification_withCompletionHandler(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List,));
        }
    }
);

fn handler() -> Retained<StatusHandler> {
    HANDLER.with(|slot| {
        if let Some(existing) = slot.borrow().as_ref() {
            return existing.clone();
        }
        let created: Retained<StatusHandler> = {
            let alloc = StatusHandler::alloc().set_ivars(());
            unsafe { msg_send![super(alloc), init] }
        };
        *slot.borrow_mut() = Some(created.clone());
        created
    })
}

/// Creates the menu-bar status item (idempotent, main thread only).
pub fn install_status_item() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let already = STATUS_ITEM.with(|slot| slot.borrow().is_some());
    if already {
        return;
    }
    let item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = item.button(mtm) {
        button.setTitle(&NSString::from_str("nplank"));
    }
    let target = handler();
    let menu = NSMenu::new(mtm);
    let entries: [(&str, objc2::runtime::Sel); 3] = [
        ("Open NPlank", sel!(openApp:)),
        ("Stop All Runs", sel!(stopAll:)),
        ("Quit", sel!(quitApp:)),
    ];
    for (title, action) in entries {
        let entry = NSMenuItem::new(mtm);
        entry.setTitle(&NSString::from_str(title));
        unsafe { entry.setAction(Some(action)) };
        let target_obj: &AnyObject = &target;
        unsafe { entry.setTarget(Some(target_obj)) };
        menu.addItem(&entry);
    }
    item.setMenu(Some(&menu));
    STATUS_ITEM.with(|slot| *slot.borrow_mut() = Some(item));
}

/// Updates the menu-bar label (e.g. `nplank · 2 running`).
pub fn set_status_label(label: &str) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    STATUS_ITEM.with(|slot| {
        if let Some(item) = slot.borrow().as_ref()
            && let Some(button) = item.button(mtm)
        {
            button.setTitle(&NSString::from_str(label));
        }
    });
}

/// Whether the process runs inside an `.app` bundle (required for
/// UNUserNotificationCenter; raw binaries must keep the fallback path).
pub fn inside_app_bundle() -> bool {
    std::env::current_exe()
        .map(|path| path.to_string_lossy().contains(".app/Contents/MacOS/"))
        .unwrap_or(false)
}

/// Installs the notification delegate and requests authorization.
/// Only call when [inside_app_bundle] is true.
pub fn install_notification_delegate() {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let delegate = handler();
    let proto: &ProtocolObject<dyn UNUserNotificationCenterDelegate> =
        ProtocolObject::from_ref(&*delegate);
    center.setDelegate(Some(proto));
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &block2::RcBlock::new(|_granted, _error| {}),
    );
}

/// Posts a notification through UNUserNotificationCenter; clicking it
/// queues the session id for [drain_notification_clicks].
pub fn post_notification(id: &str, title: &str, body: &str, session_id: &str) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    content.setInterruptionLevel(UNNotificationInterruptionLevel::Active);
    let key = NSString::from_str("session_id");
    let value = NSString::from_str(session_id);
    let user_info: Retained<NSDictionary<AnyObject, AnyObject>> = unsafe {
        msg_send![
            NSDictionary::<AnyObject, AnyObject>::class(),
            dictionaryWithObject: &*value,
            forKey: &*key
        ]
    };
    unsafe { content.setUserInfo(&user_info) };
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(id),
        &content,
        None,
    );
    center.addNotificationRequest_withCompletionHandler(&request, None);
}

#[cfg(test)]
mod tests {
    #[test]
    fn queues_drain() {
        super::push_action(super::StatusAction::Open);
        super::push_click("s1".into());
        assert_eq!(super::drain_actions(), [super::StatusAction::Open]);
        assert_eq!(super::drain_notification_clicks(), ["s1"]);
        assert!(super::drain_actions().is_empty());
    }

    #[test]
    fn bundle_detection_is_path_based() {
        // The test binary does not live in an .app bundle.
        assert!(!super::inside_app_bundle());
    }
}
