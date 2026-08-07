//! Parsing and building `custom_id`s for the button panels (docs/tournament.md
//! §8.5, §8.7): `"<action>:<entity_id>"`, e.g. `register:42`. Pure and
//! Discord-free, so every case is unit-tested directly; `dispatch::Dispatcher`
//! parses every custom_id, and `panel::render` (chunk 9) is the first to build
//! one — later panel chunks (10, 20, 22) will build theirs through
//! `Action::custom_id` too, so every button round-trips through the same code
//! path `parse_custom_id` reads.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Action {
    Register,
    Withdraw,
    Checkin,
    SetDone,
    Redraft,
    CallAdmin,
}

impl Action {
    fn tag(self) -> &'static str {
        match self {
            Action::Register => "register",
            Action::Withdraw => "withdraw",
            Action::Checkin => "checkin",
            Action::SetDone => "setdone",
            Action::Redraft => "redraft",
            Action::CallAdmin => "calladmin",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "register" => Some(Action::Register),
            "withdraw" => Some(Action::Withdraw),
            "checkin" => Some(Action::Checkin),
            "setdone" => Some(Action::SetDone),
            "redraft" => Some(Action::Redraft),
            "calladmin" => Some(Action::CallAdmin),
            _ => None,
        }
    }

    /// `Register` (aoe4world lookup) and `SetDone` (draft-tool fetch) make an
    /// outbound HTTP call that can outlast Discord's 3s ack window; the rest are
    /// a local DB write and can answer immediately (§8.5).
    pub(crate) fn requires_defer(self) -> bool {
        matches!(self, Action::Register | Action::SetDone)
    }

    /// The `custom_id` a button carries. Built by `panel::render`'s Register and
    /// Withdraw buttons (chunk 9) — later panel chunks (10, 20, 22) will build
    /// theirs through this too, so every button round-trips through the same
    /// `parse_custom_id` this module tests directly.
    pub(crate) fn custom_id(self, entity_id: i64) -> String {
        format!("{}:{entity_id}", self.tag())
    }
}

/// Pure. `None` covers both a malformed id and one naming an action this
/// deploy doesn't recognize — §8.5: "unknown or malformed custom_ids must be
/// ignored, not panic," since a button from an older deploy may still be
/// pressed.
pub(crate) fn parse_custom_id(custom_id: &str) -> Option<(Action, i64)> {
    let (tag, entity_id) = custom_id.split_once(':')?;
    let action = Action::from_tag(tag)?;
    let entity_id = entity_id.parse().ok()?;
    Some((action, entity_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_ACTIONS: [Action; 6] = [
        Action::Register,
        Action::Withdraw,
        Action::Checkin,
        Action::SetDone,
        Action::Redraft,
        Action::CallAdmin,
    ];

    #[test]
    fn every_action_round_trips_through_its_custom_id() {
        for action in ALL_ACTIONS {
            for entity_id in [0, 1, 42, i64::MAX] {
                assert_eq!(parse_custom_id(&action.custom_id(entity_id)), Some((action, entity_id)));
            }
        }
    }

    #[test]
    fn an_unknown_action_is_ignored() {
        assert_eq!(parse_custom_id("frobnicate:1"), None);
    }

    #[test]
    fn a_missing_colon_is_ignored() {
        assert_eq!(parse_custom_id("register1"), None);
    }

    #[test]
    fn a_non_numeric_entity_id_is_ignored() {
        assert_eq!(parse_custom_id("register:not-a-number"), None);
    }

    #[test]
    fn an_empty_custom_id_is_ignored() {
        assert_eq!(parse_custom_id(""), None);
    }

    #[test]
    fn only_register_and_setdone_require_a_defer() {
        assert!(Action::Register.requires_defer());
        assert!(Action::SetDone.requires_defer());
        assert!(!Action::Withdraw.requires_defer());
        assert!(!Action::Checkin.requires_defer());
        assert!(!Action::Redraft.requires_defer());
        assert!(!Action::CallAdmin.requires_defer());
    }
}
