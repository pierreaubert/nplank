//! Ordered preference writes with coalescing and failure retention.

// Rust guideline compliant 2026-02-21
#[derive(Default)]
pub(crate) struct Queue {
    pending: Option<Save>,
    running: bool,
    error: Option<String>,
}

pub(crate) struct Save {
    pub preferences: Option<String>,
    pub appearance: String,
}

impl std::fmt::Debug for Queue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Queue")
            .field("pending", &self.pending.is_some())
            .field("running", &self.running)
            .finish_non_exhaustive()
    }
}

impl Queue {
    /// Queues the newest snapshot and returns whether a writer must start.
    pub fn enqueue(&mut self, mut save: Save) -> bool {
        if save.preferences.is_none() {
            save.preferences = self.pending.take().and_then(|pending| pending.preferences);
        }
        self.pending = Some(save);
        if self.running {
            false
        } else {
            self.running = true;
            true
        }
    }

    pub fn next(&mut self) -> Option<Save> {
        let save = self.pending.take();
        if save.is_none() {
            self.running = false;
        }
        save
    }

    pub fn failed(&mut self, save: Save, error: String) {
        match &mut self.pending {
            Some(pending) if pending.preferences.is_none() => {
                pending.preferences = save.preferences
            }
            None => self.pending = Some(save),
            Some(_) => {}
        }
        self.running = false;
        self.error = Some(error);
    }

    pub fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn save(text: Option<&str>, appearance: &str) -> Save {
        Save {
            preferences: text.map(String::from),
            appearance: appearance.into(),
        }
    }

    #[test]
    fn preference_writes_coalesce_in_order_and_preserve_failed_drafts() {
        let mut queue = Queue::default();
        assert!(queue.enqueue(save(Some("first"), "light")));
        let first = queue.next().unwrap();
        assert!(!queue.enqueue(save(Some("second"), "light")));
        assert!(!queue.enqueue(save(None, "dark")));
        queue.failed(first, "storage unavailable".into());
        assert_eq!(queue.take_error().as_deref(), Some("storage unavailable"));
        assert!(queue.enqueue(save(None, "breeze")));
        let pending = queue.next().unwrap();
        assert_eq!(pending.preferences.as_deref(), Some("second"));
        assert_eq!(pending.appearance, "breeze");
        queue.failed(pending, "retry".into());
        assert!(queue.enqueue(save(Some("latest"), "dark")));
        assert_eq!(queue.next().unwrap().preferences.as_deref(), Some("latest"));
        assert!(queue.next().is_none());
        assert!(queue.enqueue(save(None, "light")));
        assert!(queue.next().unwrap().preferences.is_none());
        assert!(!format!("{queue:?}").contains("latest"));
    }
}
