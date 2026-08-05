//! Coalesces panel edits under load (docs/tournament.md §8.5: "Edits must be
//! throttled" — editing the roster or counter on every press is one API call
//! per press against a per-channel edit rate limit). The decision is pure;
//! callers own the actual edit and the "final edit when the phase closes"
//! that keeps the panel eventually consistent.
//!
//! Not consumed yet — no panel exists until chunk 9 — so `mod throttle` in
//! `tournament/mod.rs` carries `#[allow(dead_code)]` until then.

use serenity::model::id::MessageId;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Whether enough time has passed since `last_edit` (`None` = never edited) to
/// allow another edit now. Pure, with both times injected, so every boundary
/// is unit-tested without a wall clock.
pub(crate) fn should_edit(last_edit: Option<Instant>, now: Instant, min_interval: Duration) -> bool {
    match last_edit {
        None => true,
        Some(last) => now.duration_since(last) >= min_interval,
    }
}

/// Per-message last-edit clock. A caller checks in before editing a panel; a
/// press that lands inside the window is simply dropped rather than queued —
/// it is the caller's unconditional edit on phase close (§8.5) that guarantees
/// the panel ends up consistent regardless.
pub(crate) struct EditThrottle {
    min_interval: Duration,
    last_edit: Mutex<HashMap<MessageId, Instant>>,
}

impl EditThrottle {
    pub(crate) fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last_edit: Mutex::new(HashMap::new()),
        }
    }

    /// Atomically checks and, if allowed, marks `message_id` as just-edited, so
    /// two concurrent presses on the same message cannot both win the window.
    pub(crate) fn try_begin_edit(&self, message_id: MessageId, now: Instant) -> bool {
        let mut last_edit = self.last_edit.lock().unwrap();
        let allowed = should_edit(last_edit.get(&message_id).copied(), now, self.min_interval);
        if allowed {
            last_edit.insert(message_id, now);
        }
        allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN_INTERVAL: Duration = Duration::from_secs(3);

    #[test]
    fn a_never_edited_message_is_always_allowed() {
        assert!(should_edit(None, Instant::now(), MIN_INTERVAL));
    }

    #[test]
    fn just_under_the_interval_is_refused() {
        let last = Instant::now();
        let now = last + MIN_INTERVAL - Duration::from_millis(1);
        assert!(!should_edit(Some(last), now, MIN_INTERVAL));
    }

    #[test]
    fn exactly_at_the_interval_is_allowed() {
        let last = Instant::now();
        let now = last + MIN_INTERVAL;
        assert!(should_edit(Some(last), now, MIN_INTERVAL));
    }

    #[test]
    fn well_past_the_interval_is_allowed() {
        let last = Instant::now();
        let now = last + MIN_INTERVAL * 10;
        assert!(should_edit(Some(last), now, MIN_INTERVAL));
    }

    #[test]
    fn a_second_press_inside_the_window_is_refused_but_a_later_one_is_allowed() {
        let throttle = EditThrottle::new(MIN_INTERVAL);
        let message_id = MessageId::new(1);
        let start = Instant::now();

        assert!(throttle.try_begin_edit(message_id, start));
        assert!(!throttle.try_begin_edit(message_id, start + Duration::from_secs(1)));
        assert!(throttle.try_begin_edit(message_id, start + MIN_INTERVAL));
    }

    #[test]
    fn different_messages_never_block_each_other() {
        let throttle = EditThrottle::new(MIN_INTERVAL);
        let now = Instant::now();

        assert!(throttle.try_begin_edit(MessageId::new(1), now));
        assert!(throttle.try_begin_edit(MessageId::new(2), now));
    }
}
