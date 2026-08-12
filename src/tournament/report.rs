//! An organizer's own record of a game, for sets played outside the draft tool,
//! drafts abandoned mid-way, and outages.
//!
//! The fallback rather than the primary path: rows written here are marked
//! `manual` so a later sync from the tool overwrites its own imports and leaves
//! these alone. Discord-free and DB-only, like `checkin`, so every branch is
//! testable without a Discord context — the command in `commands.rs` adds the
//! Discord half and then hands off to `completion::finish`.

use crate::locale::Locale;
use crate::tournament::completion::{self, Tally};
use crate::tournament::db::{self, TournamentSet};
use sqlx::SqlitePool;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ReportOutcome {
    Recorded {
        game_number: i64,
        winner_name: String,
        tally: Tally,
    },
    /// The set is already decided. Its winner has advanced, its loser is out and
    /// its thread is closed, so a later correction is not a matter of rewriting
    /// one row.
    AlreadyComplete,
    /// A slot is still empty, or the set was settled as a bye — either way there
    /// is no game anyone could have played.
    NotPlayable,
    /// Outside `1..=best_of`. A series cannot have a game it could never reach.
    BadGameNumber { best_of: i64 },
    /// The named winner is neither of the two players in this set.
    NotInSet,
}

impl ReportOutcome {
    pub(crate) fn message(&self, locale: Locale) -> String {
        match self {
            ReportOutcome::Recorded {
                game_number,
                winner_name,
                tally,
            } => {
                let score = format!("{}-{}", tally.slot1_wins, tally.slot2_wins);
                locale.pick(
                    format!("第 {game_number} 局記為 **{winner_name}** 獲勝，目前 {score}。"),
                    format!("Game {game_number} recorded for **{winner_name}** — now {score}."),
                )
            },
            ReportOutcome::AlreadyComplete => locale.pick(
                "這場對戰已經結束，無法再改動。若結果有誤，請找管理員處理。".to_string(),
                "That set is already finished and can't be changed. If the result is wrong, an organizer \
                 has to sort it out by hand."
                    .to_string(),
            ),
            ReportOutcome::NotPlayable => locale.pick(
                "這場對戰還沒有兩位選手，沒有比賽結果可以回報。".to_string(),
                "That set doesn't have both players yet, so there is no game to report.".to_string(),
            ),
            ReportOutcome::BadGameNumber { best_of } => locale.pick(
                format!("這是 Bo{best_of}，局數必須介於 1 到 {best_of} 之間。"),
                format!("This is a Bo{best_of}, so the game number has to be between 1 and {best_of}."),
            ),
            ReportOutcome::NotInSet => locale.pick(
                "那位玩家不在這場對戰裡。".to_string(),
                "That player isn't in this set.".to_string(),
            ),
        }
    }

    /// Whether anything was written — the caller's signal for whether to ask
    /// `completion::finish` if the set is now decided.
    pub(crate) fn recorded(&self) -> bool {
        matches!(self, ReportOutcome::Recorded { .. })
    }
}

/// Why a report would be refused, if it would be.
///
/// Pure, and separate from the write so the order of the checks is pinned by a
/// test rather than by reading the function that performs them. The order is the
/// order an organizer would hit them: a finished set first, since nothing else
/// matters once it is.
pub(crate) fn refuse(
    set: &TournamentSet,
    game_number: i64,
    winner_user_id: i64,
    best_of: i64,
) -> Option<ReportOutcome> {
    if completion::is_decided(&set.status) {
        return Some(ReportOutcome::AlreadyComplete);
    }
    let (Some(slot1), Some(slot2)) = (set.slot1_user_id, set.slot2_user_id) else {
        return Some(ReportOutcome::NotPlayable);
    };
    if game_number < 1 || game_number > best_of {
        return Some(ReportOutcome::BadGameNumber { best_of });
    }
    if winner_user_id != slot1 && winner_user_id != slot2 {
        return Some(ReportOutcome::NotInSet);
    }
    None
}

/// Records one game's winner, replacing any earlier record of that game number.
///
/// Deliberately does not decide the set: the caller reports what was written and
/// then asks `completion::finish`, which is the one path every kind of result
/// report runs through.
pub(crate) struct Report {
    pub game_number: i64,
    pub winner_user_id: i64,
    /// Carried through so the reply can name the winner without a second lookup.
    pub winner_name: String,
    pub reported_by: i64,
    pub map: Option<String>,
    pub slot1_civ: Option<String>,
    pub slot2_civ: Option<String>,
}

pub(crate) async fn report_game(
    pool: &SqlitePool,
    set: &TournamentSet,
    report: Report,
) -> Result<ReportOutcome, sqlx::Error> {
    let Some(round) = db::get_round(pool, set.round_id).await? else {
        return Ok(ReportOutcome::NotPlayable);
    };
    if let Some(refusal) = refuse(set, report.game_number, report.winner_user_id, round.best_of) {
        return Ok(refusal);
    }

    db::record_manual_game(
        pool,
        db::ManualGame {
            set_id: set.id,
            game_number: report.game_number,
            winner_user_id: report.winner_user_id,
            reported_by: report.reported_by,
            map: report.map,
            slot1_civ: report.slot1_civ,
            slot2_civ: report.slot2_civ,
        },
    )
    .await?;

    // Read back rather than counting in memory, so the running score reported to
    // the organizer is the same one the completion check will see.
    let games = db::list_games_for_set(pool, set.id).await?;
    let tally = completion::tally(
        &games,
        set.slot1_user_id.unwrap_or_default(),
        set.slot2_user_id.unwrap_or_default(),
    );

    Ok(ReportOutcome::Recorded {
        game_number: report.game_number,
        winner_name: report.winner_name,
        tally,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(status: &str, slot1: Option<i64>, slot2: Option<i64>) -> TournamentSet {
        TournamentSet {
            id: 1,
            tournament_id: 1,
            round_id: 1,
            position: 1,
            slot1_user_id: slot1,
            slot2_user_id: slot2,
            slot1_wins: 0,
            slot2_wins: 0,
            winner_user_id: None,
            status: status.to_string(),
            draft_external_id: None,
            draft_synced_at: None,
            draft_announce_message_id: None,
            redraft_count: 0,
            thread_id: Some(555),
            panel_message_id: None,
            winner_advances_to_set_id: None,
            winner_advances_to_slot: None,
            loser_advances_to_set_id: None,
            loser_advances_to_slot: None,
            scheduled_at: None,
            completed_at: None,
        }
    }

    fn playable() -> TournamentSet {
        set("ready", Some(10), Some(20))
    }

    #[test]
    fn a_playable_set_with_a_sane_game_and_winner_is_accepted() {
        assert_eq!(refuse(&playable(), 1, 10, 3), None);
        assert_eq!(refuse(&playable(), 3, 20, 3), None);
    }

    #[test]
    fn a_finished_set_is_refused_before_anything_else_is_looked_at() {
        // Both later checks would also fail here; the reply has to name the one
        // that actually matters, or an organizer goes hunting for a typo.
        let done = set("completed", Some(10), Some(20));
        assert_eq!(refuse(&done, 99, 999, 3), Some(ReportOutcome::AlreadyComplete));
        let bye = set("bye", Some(10), None);
        assert_eq!(refuse(&bye, 1, 10, 3), Some(ReportOutcome::AlreadyComplete));
    }

    #[test]
    fn a_half_filled_set_has_no_game_to_report() {
        assert_eq!(
            refuse(&set("pending", Some(10), None), 1, 10, 3),
            Some(ReportOutcome::NotPlayable)
        );
        assert_eq!(
            refuse(&set("pending", None, None), 1, 10, 3),
            Some(ReportOutcome::NotPlayable)
        );
    }

    #[test]
    fn a_game_number_the_series_could_never_reach_is_refused() {
        let expected = Some(ReportOutcome::BadGameNumber { best_of: 3 });
        assert_eq!(refuse(&playable(), 0, 10, 3), expected);
        assert_eq!(refuse(&playable(), 4, 10, 3), expected);
        assert_eq!(refuse(&playable(), -1, 10, 3), expected);
        // The boundaries themselves are fine.
        assert_eq!(refuse(&playable(), 1, 10, 3), None);
        assert_eq!(refuse(&playable(), 3, 10, 3), None);
    }

    #[test]
    fn a_winner_from_another_set_is_refused() {
        assert_eq!(refuse(&playable(), 1, 999, 3), Some(ReportOutcome::NotInSet));
    }

    #[test]
    fn the_game_number_is_checked_before_the_winner() {
        // Both are wrong; the number is the one an organizer mistypes.
        assert_eq!(
            refuse(&playable(), 9, 999, 3),
            Some(ReportOutcome::BadGameNumber { best_of: 3 })
        );
    }

    #[test]
    fn messages_render_in_both_locales() {
        let outcome = ReportOutcome::Recorded {
            game_number: 2,
            winner_name: "MarineLorD".to_string(),
            tally: Tally {
                slot1_wins: 1,
                slot2_wins: 1,
            },
        };
        let zh = outcome.message(Locale::ZhTw);
        let en = outcome.message(Locale::En);
        assert_ne!(zh, en);
        // The game number, the name and the running score are data and must
        // survive either rendering.
        for rendered in [&zh, &en] {
            assert!(rendered.contains('2'), "{rendered}");
            assert!(rendered.contains("MarineLorD"), "{rendered}");
            assert!(rendered.contains("1-1"), "{rendered}");
        }
    }

    #[test]
    fn every_refusal_renders_in_both_locales() {
        let refusals = [
            ReportOutcome::AlreadyComplete,
            ReportOutcome::NotPlayable,
            ReportOutcome::BadGameNumber { best_of: 5 },
            ReportOutcome::NotInSet,
        ];
        for outcome in refusals {
            assert_ne!(
                outcome.message(Locale::ZhTw),
                outcome.message(Locale::En),
                "{outcome:?} should differ between locales"
            );
            assert!(!outcome.recorded(), "{outcome:?} must not look like a write");
        }
        assert!(
            ReportOutcome::BadGameNumber { best_of: 5 }
                .message(Locale::En)
                .contains('5')
        );

        let recorded = ReportOutcome::Recorded {
            game_number: 1,
            winner_name: "A".to_string(),
            tally: Tally {
                slot1_wins: 1,
                slot2_wins: 0,
            },
        };
        assert!(recorded.recorded(), "only a write should look like one");
    }
}
