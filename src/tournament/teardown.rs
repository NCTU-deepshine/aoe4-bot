//! Tearing a tournament back down (docs/tournament.md §8.4). Discord-free and
//! pure, like `access::decide` — the four channel deletions and the one cascading
//! row delete both belong to the command; what lives here is only the decision
//! about whether it may run at all.
//!
//! `/tournament cancel` (chunk 13) is the other half of teardown and belongs here
//! when it lands.

use crate::locale::Locale;
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
    pub(crate) fn message(&self, tournament: &Tournament, locale: Locale) -> String {
        let (name, slug) = (&tournament.name, &tournament.slug);
        match self {
            DeleteCheck::Ok => locale.pick(
                format!("已刪除 **{name}**（`{slug}`）以及它建立的頻道。"),
                format!("Deleted **{name}** (`{slug}`) and the channels it created."),
            ),
            DeleteCheck::NotAnnounceChannel => locale.pick(
                format!("請在 **{name}** 的公告頻道執行 — 那是唯一不會被刪除的頻道，在其他地方執行的話這則回覆會跟著頻道一起消失。"),
                format!(
                    "Run this in **{name}**'s announce channel — it's the only one that survives the delete, \
                     so anywhere else this reply would vanish with its own channel."
                ),
            ),
            DeleteCheck::ConfirmMismatch => locale.pick(
                format!("不符合。要刪除 **{name}** 和它的頻道，請執行 `/tournament delete confirm:{slug}`。此操作無法復原。"),
                format!(
                    "That doesn't match. To delete **{name}** and its channels, run \
                     `/tournament delete confirm:{slug}`. This cannot be undone."
                ),
            ),
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
            seed_message_id: None,
            checkin_closes_at: None,
            created_by: 1,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        }
    }

    #[test]
    fn messages_render_in_both_locales() {
        let t = tournament();
        let zh = DeleteCheck::ConfirmMismatch.message(&t, Locale::ZhTw);
        let en = DeleteCheck::ConfirmMismatch.message(&t, Locale::En);
        assert_ne!(zh, en);
        // Both must still tell the admin exactly what to type.
        assert!(zh.contains("confirm:relic-cup"), "{zh}");
        assert!(en.contains("confirm:relic-cup"), "{en}");
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
