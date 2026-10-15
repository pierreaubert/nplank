//! Ephemeral viewing leases; disconnected clients never suppress notifications forever.

// Rust guideline compliant 2026-02-21
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Debug)]
struct View {
    session: String,
    generation: u64,
    seen: Instant,
}

#[derive(Debug, Default)]
pub(crate) struct Views(HashMap<String, View>);

impl Views {
    pub(crate) fn update(
        &mut self,
        client: &str,
        session: Option<(&str, u64)>,
        now: Instant,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            uuid::Uuid::parse_str(client).is_ok(),
            "Invalid focus client identity"
        );
        self.0
            .retain(|_, view| now.saturating_duration_since(view.seen) < Duration::from_secs(3));
        if let Some((session, generation)) = session {
            // Bound authenticated clients that never send a release.
            anyhow::ensure!(
                self.0.contains_key(client) || self.0.len() < 128,
                "Too many focus clients"
            );
            self.0.insert(
                client.into(),
                View {
                    session: session.into(),
                    generation,
                    seen: now,
                },
            );
        } else {
            self.0.remove(client);
        }
        Ok(())
    }

    pub(crate) fn focused(&self, session: &str, generation: u64, now: Instant) -> bool {
        self.0.values().any(|view| {
            view.session == session
                && view.generation == generation
                && now.saturating_duration_since(view.seen) < Duration::from_secs(3)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gui_focus_tracks_sessions_generations_windows_and_expiry() {
        let mut views = Views::default();
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let now = Instant::now();
        views.update(&a, Some(("one", 1)), now).unwrap();
        views.update(&b, Some(("one", 1)), now).unwrap();
        assert!(views.focused("one", 1, now));
        assert!(!views.focused("one", 2, now));
        views.update(&a, Some(("two", 1)), now).unwrap();
        assert!(views.focused("one", 1, now));
        views.update(&b, None, now).unwrap();
        assert!(!views.focused("one", 1, now));
        assert!(views.focused("two", 1, now));
        assert!(!views.focused("two", 1, now + Duration::from_secs(3)));
        assert!(views.update("invalid", Some(("two", 1)), now).is_err());
    }
}
