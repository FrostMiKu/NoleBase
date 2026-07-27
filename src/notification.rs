//! Short-lived, non-blocking notifications rendered by the TUI.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

const DEFAULT_TTL: Duration = Duration::from_secs(4);
const CAPACITY: usize = 8;

struct Notification {
    message: String,
    expires_at: Instant,
}

pub struct NotificationService {
    items: VecDeque<Notification>,
    ttl: Duration,
    pending_bells: usize,
}

impl Default for NotificationService {
    fn default() -> Self {
        Self {
            items: VecDeque::new(),
            ttl: DEFAULT_TTL,
            pending_bells: 0,
        }
    }
}

impl NotificationService {
    pub fn notify(&mut self, message: impl Into<String>) {
        let message = message.into();
        if message.trim().is_empty() {
            return;
        }
        if self.items.len() == CAPACITY {
            self.items.pop_front();
        }
        self.items.push_back(Notification {
            message,
            expires_at: Instant::now() + self.ttl,
        });
        self.pending_bells = self.pending_bells.saturating_add(1);
    }

    pub fn take_bells(&mut self) -> usize {
        std::mem::take(&mut self.pending_bells)
    }

    pub fn visible(&mut self) -> Option<String> {
        let now = Instant::now();
        self.items.retain(|item| item.expires_at > now);
        self.items.back().map(|item| item.message.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_notification_is_visible_and_empty_messages_are_ignored() {
        let mut service = NotificationService::default();
        service.notify("first");
        service.notify("second");
        service.notify("  ");
        assert_eq!(service.visible().as_deref(), Some("second"));
    }

    #[test]
    fn expired_notifications_are_removed() {
        let mut service = NotificationService {
            items: VecDeque::new(),
            ttl: Duration::ZERO,
            pending_bells: 0,
        };
        service.notify("expired");
        assert_eq!(service.visible(), None);
    }

    #[test]
    fn each_nonempty_notification_schedules_one_terminal_bell() {
        let mut service = NotificationService::default();
        assert_eq!(service.take_bells(), 0);

        service.notify("first");
        service.notify("second");
        assert_eq!(service.take_bells(), 2);
        assert_eq!(service.take_bells(), 0);

        service.notify("  ");
        assert_eq!(service.take_bells(), 0);
        service.notify("third");
        assert_eq!(service.take_bells(), 1);
    }
}
