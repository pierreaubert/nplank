//! Bounded completion notifications with client-side focus policy.
//!
//! Notifications never enter transcripts, complete requests, or acknowledge attention.

// Rust guideline compliant 2026-02-21
use serde::{Deserialize, Serialize};

pub use crate::livecontrol::NotifyMode as Mode;

/// A short-lived notice scoped to its engine generation.
#[derive(Clone, Serialize, Deserialize)]
pub struct Notification {
    /// Opaque identity for client deduplication.
    pub id: String,
    /// Owning workspace session.
    pub session_id: String,
    /// Owning engine generation.
    pub generation: u64,
    /// Bounded headline.
    pub title: String,
    /// Bounded body.
    pub body: String,
    /// Engine notification policy.
    pub mode: Mode,
    /// Expiration in Unix milliseconds.
    pub expires_ms: u64,
}

impl std::fmt::Debug for Notification {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Notification")
            .field("id", &self.id)
            .field("session_id", &self.session_id)
            .field("generation", &self.generation)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl Notification {
    /// Rebases a server-relative expiration without requiring synchronized clocks.
    #[cfg(any(feature = "gui", test))]
    pub(crate) fn rebase(&mut self, server_now: u64, client_now: u64) -> bool {
        let remaining = self.expires_ms.saturating_sub(server_now).min(30_000);
        if server_now == 0 || remaining == 0 {
            return false;
        }
        self.expires_ms = client_now.saturating_add(remaining);
        self.valid(client_now)
    }
    #[cfg(any(not(target_family = "wasm"), test))]
    pub(crate) fn from_remote(
        session: &str,
        generation: u64,
        value: &serde_json::Value,
        now: u64,
    ) -> Option<Self> {
        let notification = Self {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session.into(),
            generation,
            title: value["title"].as_str()?.into(),
            body: value["body"].as_str()?.into(),
            mode: serde_json::from_value(value["mode"].clone()).ok()?,
            // Covers polling/reconnect latency without notifying newly opened clients.
            expires_ms: now.saturating_add(30_000),
        };
        notification.valid(now).then_some(notification)
    }

    pub(crate) fn valid(&self, now: u64) -> bool {
        uuid::Uuid::parse_str(&self.id).is_ok()
            && !self.session_id.is_empty()
            && self.session_id.len() <= 128
            && !self.title.trim().is_empty()
            && self.title.len() <= 1024
            && self.body.len() <= 4096
            && !self.title.chars().any(char::is_control)
            && !self
                .body
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
            && self.expires_ms > now
            && self.expires_ms <= now.saturating_add(30_000)
    }
}

pub(crate) fn now_ms() -> u64 {
    #[cfg(target_family = "wasm")]
    {
        js_sys::Date::now().max(0.0) as u64
    }
    #[cfg(not(target_family = "wasm"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            })
    }
}

#[cfg(not(target_family = "wasm"))]
#[derive(Debug, Default)]
pub(crate) struct Feed(std::collections::VecDeque<Notification>);

#[cfg(not(target_family = "wasm"))]
impl Feed {
    pub(crate) fn push(&mut self, notice: Notification, now: u64) {
        self.0.retain(|notice| notice.expires_ms > now);
        if self.0.len() == 64 {
            self.0.pop_front();
        }
        self.0.push_back(notice);
    }
    pub(crate) fn clear(&mut self, session: &str) {
        self.0.retain(|notice| notice.session_id != session);
    }
    pub(crate) fn snapshot(
        &mut self,
        sessions: &[crate::model::Session],
        now: u64,
    ) -> Vec<Notification> {
        self.0.retain(|notice| {
            notice.expires_ms > now
                && sessions.iter().any(|session| {
                    session.id == notice.session_id
                        && session.generation == notice.generation
                        && !session.archived
                        && !session.recovery_required
                })
        });
        self.0.iter().cloned().collect()
    }
}

#[cfg(any(feature = "gui", test))]
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    initialized: bool,
    seen: std::collections::HashSet<String>,
}

#[cfg(any(feature = "gui", test))]
impl Tracker {
    pub(crate) fn take(
        &mut self,
        notices: &[Notification],
        now: u64,
        focused: impl Fn(&str) -> bool,
    ) -> Vec<Notification> {
        self.seen
            .retain(|id| notices.iter().any(|notice| notice.id == *id));
        let mut deliver = Vec::new();
        for notice in notices.iter().take(64).filter(|notice| notice.valid(now)) {
            if self.seen.insert(notice.id.clone())
                && self.initialized
                && (notice.mode == Mode::Always
                    || (notice.mode == Mode::Unfocused && !focused(&notice.session_id)))
            {
                deliver.push(notice.clone());
            }
        }
        self.initialized = true;
        deliver
    }
}

/// Best-effort desktop delivery, isolated from the UI and engine task.
#[cfg(all(feature = "gui", not(target_family = "wasm")))]
#[derive(Debug)]
pub(crate) struct Delivery(Option<std::sync::mpsc::SyncSender<Notification>>);

#[cfg(all(feature = "gui", not(target_family = "wasm")))]
impl Default for Delivery {
    fn default() -> Self {
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        {
            // One bounded worker per view: a blocked desktop service cannot block UI.
            let (send, receive) = std::sync::mpsc::sync_channel::<Notification>(8);
            let worker = std::thread::Builder::new()
                .name("nplank-notifications".into())
                .spawn(move || {
                    #[cfg(target_os = "macos")]
                    let _ = notify_rust::set_application("org.spinorama.nplank");
                    while let Ok(notice) = receive.recv() {
                        if notice.expires_ms > now_ms() {
                            let _ = notify_rust::Notification::new()
                                .appname("nplank")
                                .summary(&notice.title)
                                .body(&notice.body)
                                .show();
                        }
                    }
                });
            Self(worker.ok().map(|_| send))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Self(None)
        }
    }
}

#[cfg(all(feature = "gui", not(target_family = "wasm")))]
impl Delivery {
    pub(crate) fn send(&self, notice: Notification) {
        #[cfg(target_os = "macos")]
        if nplank_macos_shim::inside_app_bundle() {
            // UNUserNotificationCenter gives click-through to the session;
            // only valid bundled, so unbundled builds keep notify-rust.
            nplank_macos_shim::post_notification(
                &notice.id,
                &notice.title,
                &notice.body,
                &notice.session_id,
            );
            return;
        }
        if let Some(sender) = &self.0 {
            let _ = sender.try_send(notice);
        }
    }
}

#[cfg(all(feature = "gui", target_family = "wasm"))]
#[derive(Debug, Default)]
pub(crate) struct Delivery;

#[cfg(all(feature = "gui", target_family = "wasm"))]
impl Delivery {
    pub(crate) fn permission() -> Option<web_sys::NotificationPermission> {
        let constructor = js_sys::Reflect::get(&js_sys::global(), &"Notification".into()).ok()?;
        if constructor.is_undefined() || constructor.is_null() {
            return None;
        }
        Some(web_sys::Notification::permission())
    }

    pub(crate) fn enable(&self) {
        // Called only from the settings button's user gesture.
        if Self::permission() == Some(web_sys::NotificationPermission::Default) {
            let _ = web_sys::Notification::request_permission();
        }
    }

    pub(crate) fn send(&self, notice: Notification) {
        if Self::permission() == Some(web_sys::NotificationPermission::Granted) {
            let options = web_sys::NotificationOptions::new();
            options.set_body(&notice.body);
            options.set_tag(&notice.id);
            let _ = web_sys::Notification::new_with_options(&notice.title, &options);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Mode, Notification, Tracker};

    fn notice(mode: Mode, now: u64) -> Notification {
        Notification::from_remote(
            "session",
            3,
            &serde_json::json!({"title":"private headline", "body":"private body", "mode":mode}),
            now,
        )
        .unwrap()
    }

    #[test]
    fn notifications_seed_replay_and_deliver_new_events_once() {
        let now = 100_000;
        let old = notice(Mode::Always, now);
        let new = notice(Mode::Always, now);
        let mut tracker = Tracker::default();
        assert!(
            tracker
                .take(std::slice::from_ref(&old), now, |_| false)
                .is_empty()
        );
        let batch = [old, new.clone()];
        let delivered = tracker.take(&batch, now, |_| false);
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].id, new.id);
        assert!(tracker.take(&batch, now, |_| false).is_empty());
    }

    #[test]
    fn notifications_apply_client_focus_without_delayed_banners() {
        let now = 100_000;
        let mut tracker = Tracker::default();
        tracker.take(&[], now, |_| false);
        let mut background = notice(Mode::Unfocused, now);
        background.session_id = "background".into();
        let batch = [
            notice(Mode::Unfocused, now),
            background,
            notice(Mode::Never, now),
            notice(Mode::Always, now),
        ];
        let delivered = tracker.take(&batch, now, |id| id == "session");
        assert_eq!(delivered.len(), 2);
        assert_eq!(delivered[0].session_id, "background");
        assert_eq!(delivered[1].mode, Mode::Always);
        assert!(tracker.take(&batch, now, |_| false).is_empty());
        let later = notice(Mode::Unfocused, now);
        assert_eq!(tracker.take(&[later], now, |_| false).len(), 1);
    }

    #[test]
    fn notifications_rebase_clocks_and_reject_expired_events() {
        for client_now in [1_000, 1_000_000] {
            let mut notification = notice(Mode::Always, 100_000);
            assert!(notification.rebase(110_000, client_now));
            assert_eq!(notification.expires_ms, client_now + 20_000);
            let mut tracker = Tracker::default();
            tracker.take(&[], client_now, |_| false);
            assert_eq!(
                tracker.take(&[notification], client_now, |_| false).len(),
                1
            );
        }
        assert!(!notice(Mode::Always, 100_000).rebase(130_000, 5_000));
        assert!(!notice(Mode::Always, 100_000).rebase(0, 5_000));
        let mut future = notice(Mode::Always, 100_000);
        future.expires_ms = u64::MAX;
        assert!(future.rebase(100_000, 5_000));
        assert_eq!(future.expires_ms, 35_000);
    }

    #[test]
    fn notifications_validate_bounds_and_redact_debug() {
        let now = 100_000;
        let notification = notice(Mode::Always, now);
        let debug = format!("{notification:?}");
        assert!(!debug.contains(&notification.title));
        assert!(!debug.contains(&notification.body));
        let mut value = serde_json::json!({"title":"done", "body":"a\nb\tc", "mode":"always"});
        assert!(Notification::from_remote("session", 1, &value, now).is_some());
        for invalid in [serde_json::Value::Null, serde_json::json!("unknown")] {
            value["mode"] = invalid;
            assert!(Notification::from_remote("session", 1, &value, now).is_none());
        }
        value["mode"] = "always".into();
        for title in [" ".into(), "bad\nheadline".into(), "x".repeat(1025)] {
            value["title"] = title.into();
            assert!(Notification::from_remote("session", 1, &value, now).is_none());
        }
        value["title"] = "done".into();
        value["body"] = "x".repeat(4097).into();
        assert!(Notification::from_remote("session", 1, &value, now).is_none());
        assert!(!notification.valid(now + 30_000));
    }
}
