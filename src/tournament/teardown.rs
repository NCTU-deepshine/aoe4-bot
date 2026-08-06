//! Tearing a tournament back down (docs/tournament.md §8.4). Discord-free and
//! pure, like `access::decide` — the four channel deletions and the one cascading
//! row delete both belong to the command; what lives here is only the decision
//! about whether it may run at all.
//!
//! `/tournament cancel` (chunk 13) is the other half of teardown and belongs here
//! when it lands.

use crate::tournament::db::Tournament;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DeleteCheck {
    Ok,
    NotAnnounceChannel,
    ConfirmMismatch,
}

impl DeleteCheck {
    /// Takes the whole tournament rather than just its name: both refusals have to
    /// tell the admin what to type or where to go, and the slug is the answer.
    pub(crate) fn message(&self, tournament: &Tournament) -> String {
        match self {
            DeleteCheck::Ok => {
                format!(
                    "Deleted **{}** (`{}`) and the channels it created.",
                    tournament.name, tournament.slug
                )
            },
            DeleteCheck::NotAnnounceChannel => {
                format!(
                    "Run this in **{}**'s announce channel — it's the only one that survives the delete, \
                     so anywhere else this reply would vanish with its own channel.",
                    tournament.name
                )
            },
            DeleteCheck::ConfirmMismatch => {
                format!(
                    "That doesn't match. To delete **{}** and its channels, run \
                     `/tournament delete confirm:{}`. This cannot be undone.",
                    tournament.name, tournament.slug
                )
            },
        }
    }
}

/// Pure. Channel first, then the confirmation — being in the wrong place is the
/// more basic mistake, and answering it first avoids telling someone their slug
/// was wrong when the command would have been refused either way.
///
/// The channel test is deliberately stricter than `resolve_tournament_by_channel`,
/// which matches any of the five: every other command is happy to run from
/// wherever the admin already is, and this one cannot be.
pub(crate) fn check_delete(tournament: &Tournament, confirm: &str, invoking_channel_id: i64) -> DeleteCheck {
    if tournament.announce_channel_id != Some(invoking_channel_id) {
        return DeleteCheck::NotAnnounceChannel;
    }
    if confirm != tournament.slug {
        return DeleteCheck::ConfirmMismatch;
    }
    DeleteCheck::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn tournament() -> Tournament {
        Tournament {
            id: 1,
            slug: "relic-cup".to_string(),
            name: "Relic Cup".to_string(),
            status: "registration".to_string(),
            draft_base_url: None,
            announce_channel_id: Some(10),
            category_id: Some(20),
            register_channel_id: Some(11),
            register_message_id: None,
            bracket_channel_id: Some(12),
            matches_channel_id: Some(13),
            draft_channel_id: Some(14),
            checkin_message_id: None,
            checkin_closes_at: None,
            created_by: 1,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        }
    }

    #[test]
    fn accepts_the_announce_channel_with_an_exact_slug() {
        assert_eq!(check_delete(&tournament(), "relic-cup", 10), DeleteCheck::Ok);
    }

    #[test]
    fn refuses_every_channel_but_the_announce_one() {
        // The other four are exactly what `resolve_tournament_by_channel` would
        // happily accept, which is the leak this guards against.
        for channel_id in [11, 12, 13, 14, 20, 99] {
            assert_eq!(
                check_delete(&tournament(), "relic-cup", channel_id),
                DeleteCheck::NotAnnounceChannel,
                "channel {channel_id} should not be accepted"
            );
        }
    }

    #[test]
    fn refuses_a_slug_that_is_not_an_exact_match() {
        for confirm in ["", "relic", "relic-cup ", "Relic-Cup", "some-other-cup"] {
            assert_eq!(
                check_delete(&tournament(), confirm, 10),
                DeleteCheck::ConfirmMismatch,
                "{confirm:?} should not confirm"
            );
        }
    }

    #[test]
    fn reports_the_wrong_channel_before_the_wrong_slug() {
        assert_eq!(
            check_delete(&tournament(), "nonsense", 11),
            DeleteCheck::NotAnnounceChannel
        );
    }

    #[test]
    fn a_tournament_with_no_announce_channel_can_never_be_deleted() {
        // Unreachable via `create`, which always stores one — but defaulting to
        // "refuse" beats matching `None == None` and deleting from anywhere.
        let mut tournament = tournament();
        tournament.announce_channel_id = None;
        assert_eq!(
            check_delete(&tournament, "relic-cup", 10),
            DeleteCheck::NotAnnounceChannel
        );
    }
}
