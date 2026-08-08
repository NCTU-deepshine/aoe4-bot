//! Set completion and advancement.
//!
//! The engine rather than a command: an organizer typing a result and a sync from
//! the draft tool are two ways of reporting the same thing, and both come through
//! here, so a set decided by hand and a set decided by import cannot behave
//! differently. Deciding is pure and is what this module's own tests cover;
//! `finish` is the single place that turns a decision into a transaction and then
//! into Discord.

use crate::Error;
use crate::locale::Locale;
use crate::tournament::bracket::Slot;
use crate::tournament::db::{self, Tournament, TournamentGame, TournamentSet};
use crate::tournament::{bracket_view, set_thread};
use serenity::all::CacheHttp;
use sqlx::SqlitePool;

/// Games won by each side of a set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tally {
    pub slot1_wins: i64,
    pub slot2_wins: i64,
}

/// Counts a set's games onto its two slots.
///
/// Only `completed` games count: a voided game is one whose draft was regenerated
/// afterwards, so it is no longer the record of anything. A `winner_user_id`
/// belonging to neither slot cannot decide the set either. Both are skipped rather
/// than rejected, so this is total for any rows the database can hold.
pub(crate) fn tally(games: &[TournamentGame], slot1_user_id: i64, slot2_user_id: i64) -> Tally {
    let mut tally = Tally {
        slot1_wins: 0,
        slot2_wins: 0,
    };
    for game in games.iter().filter(|g| g.status == "completed") {
        match game.winner_user_id {
            Some(winner) if winner == slot1_user_id => tally.slot1_wins += 1,
            Some(winner) if winner == slot2_user_id => tally.slot2_wins += 1,
            _ => {},
        }
    }
    tally
}

/// Games needed to take a series. `best_of` is odd — the schema rejects an even
/// one — so "more than half" has no rounding to argue about, and no threshold of
/// our own that could drift from the draft tool's.
pub(crate) fn majority(best_of: i64) -> i64 {
    best_of / 2 + 1
}

/// Which side has won the set, if either has yet.
///
/// Derived from the score, never read off a status. The draft tool stores no
/// finished state, so a decided series still reads as running there — a 2-0 in a
/// Bo3 is a finished set whatever anything else claims about it.
pub(crate) fn decide(tally: &Tally, best_of: i64) -> Option<Slot> {
    let needed = majority(best_of);
    match (tally.slot1_wins >= needed, tally.slot2_wins >= needed) {
        (true, false) => Some(Slot::One),
        (false, true) => Some(Slot::Two),
        // Unreachable with an odd `best_of` and a sane score; total anyway.
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CompleteOutcome {
    Completed {
        winner_name: String,
        loser_name: String,
        tally: Tally,
        opened_next: bool,
        tournament_complete: bool,
    },
    /// Nobody has a majority yet, so nothing was written.
    StillPlaying {
        tally: Tally,
        needed: i64,
    },
    AlreadyComplete,
    /// Not a set results can be reported for: a slot is still empty, or it was
    /// settled as a bye.
    NotPlayable,
}

impl CompleteOutcome {
    pub(crate) fn message(&self, locale: Locale) -> String {
        match self {
            CompleteOutcome::Completed {
                winner_name,
                loser_name,
                tally,
                opened_next,
                tournament_complete,
            } => {
                let (score, next) = (
                    format!("{}-{}", tally.slot1_wins, tally.slot2_wins),
                    match (tournament_complete, opened_next) {
                        (true, _) => locale.pick("賽事到此結束。", "That was the final — the tournament is over."),
                        (false, true) => locale.pick("下一場對戰已開啟。", "The next set is open."),
                        (false, false) => locale.pick(
                            "下一場要等另一半也分出勝負。",
                            "The next set waits on the other half of the bracket.",
                        ),
                    },
                );
                locale.pick(
                    format!("**{winner_name}** 以 {score} 擊敗 **{loser_name}**。{next}"),
                    format!("**{winner_name}** beat **{loser_name}** {score}. {next}"),
                )
            },
            CompleteOutcome::StillPlaying { tally, needed } => {
                let score = format!("{}-{}", tally.slot1_wins, tally.slot2_wins);
                locale.pick(
                    format!("目前 {score}，尚未結束——先拿到 {needed} 勝的一方獲勝。"),
                    format!("Currently {score} — not decided yet; {needed} wins takes the set."),
                )
            },
            CompleteOutcome::AlreadyComplete => locale.pick(
                "這場對戰已經結束了。".to_string(),
                "That set is already finished.".to_string(),
            ),
            CompleteOutcome::NotPlayable => locale.pick(
                "這場對戰還沒有兩位選手，無法回報結果。".to_string(),
                "That set doesn't have both players yet, so there is nothing to report.".to_string(),
            ),
        }
    }
}

/// Completes `set` if its games have decided it, then brings Discord in line.
///
/// The one path every way of reporting a result runs. The database half is a
/// single transaction and is authoritative; everything after it is best-effort,
/// because a bracket that advanced in the database and not in the channel is a
/// display problem an organizer can repair with `/tournament refresh`, whereas
/// refusing to advance because Discord was unreachable stalls the event.
pub(crate) async fn finish(
    http: impl CacheHttp,
    pool: &SqlitePool,
    tournament: &Tournament,
    set: &TournamentSet,
) -> Result<CompleteOutcome, Error> {
    if set.status == "completed" {
        return Ok(CompleteOutcome::AlreadyComplete);
    }
    let (Some(slot1), Some(slot2)) = (set.slot1_user_id, set.slot2_user_id) else {
        return Ok(CompleteOutcome::NotPlayable);
    };
    let Some(round) = db::get_round(pool, set.round_id).await? else {
        return Ok(CompleteOutcome::NotPlayable);
    };

    let games = db::list_games_for_set(pool, set.id).await?;
    let tally = tally(&games, slot1, slot2);
    let Some(winning_slot) = decide(&tally, round.best_of) else {
        return Ok(CompleteOutcome::StillPlaying {
            tally,
            needed: majority(round.best_of),
        });
    };
    let (winner_user_id, loser_user_id) = match winning_slot {
        Slot::One => (slot1, slot2),
        Slot::Two => (slot2, slot1),
    };

    let advanced = db::complete_set_and_advance(
        pool,
        db::SetResult {
            set_id: set.id,
            tournament_id: tournament.id,
            slot1_wins: tally.slot1_wins,
            slot2_wins: tally.slot2_wins,
            winner_user_id,
            loser_user_id,
        },
    )
    .await?;
    if !advanced.completed {
        // Someone else got there between the read above and the update.
        return Ok(CompleteOutcome::AlreadyComplete);
    }

    let winner = set_thread::player(pool, tournament.id, winner_user_id).await?;
    let loser = set_thread::player(pool, tournament.id, loser_user_id).await?;
    set_thread::close(&http, pool, set, &winner, &loser, &tally).await;

    // The bracket is redrawn from the rows just written: `played_match` derives a
    // winner from `winner_user_id` and never reads `status`, so this needs no
    // completion-specific rendering of its own.
    if let Err(err) = bracket_view::reconcile(&http, pool, tournament).await {
        tracing::error!("failed to redraw the bracket after set {} completed: {err:?}", set.id);
    }
    set_thread::open_ready(&http, pool, tournament).await;

    Ok(CompleteOutcome::Completed {
        winner_name: winner.name,
        loser_name: loser.name,
        tally,
        opened_next: advanced.target_became_ready,
        tournament_complete: advanced.tournament_completed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(game_number: i64, winner_user_id: Option<i64>, status: &str) -> TournamentGame {
        TournamentGame {
            id: game_number,
            set_id: 1,
            game_number,
            map: None,
            slot1_civ: None,
            slot2_civ: None,
            winner_user_id,
            status: status.to_string(),
            source: "manual".to_string(),
            reported_by: None,
            reported_at: None,
        }
    }

    fn played(winners: &[i64]) -> Vec<TournamentGame> {
        winners
            .iter()
            .enumerate()
            .map(|(index, winner)| game(i64::try_from(index).unwrap() + 1, Some(*winner), "completed"))
            .collect()
    }

    #[test]
    fn a_majority_is_more_than_half_of_an_odd_series() {
        assert_eq!(majority(1), 1);
        assert_eq!(majority(3), 2);
        assert_eq!(majority(5), 3);
        assert_eq!(majority(7), 4);
    }

    #[test]
    fn a_bo3_is_decided_at_two_wins_and_not_before() {
        let sweep = tally(&played(&[10, 10]), 10, 20);
        assert_eq!(decide(&sweep, 3), Some(Slot::One));

        let one_nil = tally(&played(&[10]), 10, 20);
        assert_eq!(decide(&one_nil, 3), None, "1-0 in a Bo3 is not a result");

        let decider = tally(&played(&[10, 20, 20]), 10, 20);
        assert_eq!(decider.slot2_wins, 2);
        assert_eq!(decide(&decider, 3), Some(Slot::Two));

        let level = tally(&played(&[10, 20]), 10, 20);
        assert_eq!(decide(&level, 3), None, "1-1 decides nothing");
    }

    #[test]
    fn a_bo5_needs_three_so_two_all_is_still_open() {
        let two_all = tally(&played(&[10, 20, 10, 20]), 10, 20);
        assert_eq!(decide(&two_all, 5), None);

        let three_two = tally(&played(&[10, 20, 10, 20, 10]), 10, 20);
        assert_eq!(decide(&three_two, 5), Some(Slot::One));
    }

    #[test]
    fn a_score_neither_side_could_have_reached_decides_nothing() {
        // 3-2 cannot happen in a Bo3 — both sides clear the majority, so the
        // score is not a result and `decide` refuses rather than picking one.
        let impossible = tally(&played(&[10, 20, 10, 20, 10]), 10, 20);
        assert_eq!(decide(&impossible, 3), None);
    }

    #[test]
    fn voided_games_never_carry_a_set_over_the_line() {
        // What regenerating a draft leaves behind: the imported record of a game that
        // is no longer the record.
        let games = vec![
            game(1, Some(10), "completed"),
            game(2, Some(10), "void"),
            game(3, Some(10), "pending"),
        ];
        let tally = tally(&games, 10, 20);
        assert_eq!(tally.slot1_wins, 1, "only the completed game counts");
        assert_eq!(decide(&tally, 3), None);
    }

    #[test]
    fn a_game_won_by_nobody_in_this_set_is_ignored_rather_than_counted() {
        let games = vec![
            game(1, Some(10), "completed"),
            game(2, None, "completed"),
            game(3, Some(999), "completed"),
        ];
        let tally = tally(&games, 10, 20);
        assert_eq!(
            tally,
            Tally {
                slot1_wins: 1,
                slot2_wins: 0
            }
        );
    }

    #[test]
    fn an_unplayed_set_tallies_to_nothing() {
        let tally = tally(&[], 10, 20);
        assert_eq!(
            tally,
            Tally {
                slot1_wins: 0,
                slot2_wins: 0
            }
        );
        assert_eq!(decide(&tally, 3), None);
    }

    #[test]
    fn messages_render_in_both_locales() {
        let outcome = CompleteOutcome::Completed {
            winner_name: "MarineLorD".to_string(),
            loser_name: "Beasty".to_string(),
            tally: Tally {
                slot1_wins: 2,
                slot2_wins: 1,
            },
            opened_next: true,
            tournament_complete: false,
        };
        let zh = outcome.message(Locale::ZhTw);
        let en = outcome.message(Locale::En);
        assert_ne!(zh, en);
        // The score and both names are data and must survive either rendering.
        for rendered in [&zh, &en] {
            assert!(rendered.contains("2-1"), "{rendered}");
            assert!(
                rendered.contains("MarineLorD") && rendered.contains("Beasty"),
                "{rendered}"
            );
        }
    }

    #[test]
    fn the_final_says_the_tournament_is_over_rather_than_naming_a_next_set() {
        let outcome = CompleteOutcome::Completed {
            winner_name: "MarineLorD".to_string(),
            loser_name: "Beasty".to_string(),
            tally: Tally {
                slot1_wins: 3,
                slot2_wins: 0,
            },
            opened_next: false,
            tournament_complete: true,
        };
        assert!(outcome.message(Locale::En).contains("tournament is over"));
        assert!(outcome.message(Locale::ZhTw).contains("賽事到此結束"));
    }

    #[test]
    fn still_playing_names_what_it_would_take_to_win() {
        let outcome = CompleteOutcome::StillPlaying {
            tally: Tally {
                slot1_wins: 1,
                slot2_wins: 0,
            },
            needed: 2,
        };
        let en = outcome.message(Locale::En);
        assert!(en.contains("1-0") && en.contains('2'), "{en}");
        assert_ne!(en, outcome.message(Locale::ZhTw));
    }
}
