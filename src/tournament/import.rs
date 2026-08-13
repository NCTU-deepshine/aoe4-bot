//! Result import: syncing a set against its draft, rather than an organizer
//! typing the result by hand.
//!
//! `map_games`, `seat_state` and `score_mismatch` are pure and are what this
//! module's own tests cover. `sync`/`apply` are the effectful half — they
//! settle a decided set through `completion::finish` rather than duplicating
//! that logic, so a set decided by import behaves exactly like one decided
//! by hand. A head start is assumed to always be zero and is not modeled —
//! see the note on `drafttool::DraftState`. Callable but uncalled: `/set
//! done` and the background poll each add a caller, in their own chunks.

use crate::Error;
use crate::drafttool::{self, DraftGame, DraftSeat, DraftState, SlotValues};
use crate::tournament::completion::{self, CompleteOutcome, Tally};
use crate::tournament::db::{self, NewGame, Tournament, TournamentSet};
use serenity::all::CacheHttp;
use sqlx::SqlitePool;
use tracing::warn;

/// Maps a draft's games onto `tournament_games` rows. Slot 1 is the higher
/// seed by instruction (§7, §8.7) — an assertion, never a detection.
///
/// A game with no map, no civ and no winner is skipped outright rather than
/// written as an empty placeholder row. One with a winner is `completed`; one
/// with only a map or a civ so far is `in_progress` — the schema's own fourth
/// state, otherwise unwritten by anything today.
pub(crate) fn map_games(games: &[DraftGame], set_id: i64, slot1_user_id: i64, slot2_user_id: i64) -> Vec<NewGame> {
    games
        .iter()
        .filter(|g| {
            g.map.is_some() || g.civ_by_slot.slot1.is_some() || g.civ_by_slot.slot2.is_some() || g.winner_slot.is_some()
        })
        .map(|g| NewGame {
            set_id,
            game_number: g.number,
            map: g.map.clone(),
            slot1_civ: g.civ_by_slot.slot1.clone(),
            slot2_civ: g.civ_by_slot.slot2.clone(),
            winner_user_id: winner_user_id_for_slot(g.winner_slot, slot1_user_id, slot2_user_id),
            status: if g.winner_slot.is_some() {
                "completed"
            } else {
                "in_progress"
            }
            .to_string(),
            source: "draft_import".to_string(),
            reported_by: None,
            reported_at: None,
        })
        .collect()
}

fn winner_user_id_for_slot(slot: Option<i64>, slot1_user_id: i64, slot2_user_id: i64) -> Option<i64> {
    match slot {
        Some(1) => Some(slot1_user_id),
        Some(2) => Some(slot2_user_id),
        _ => None,
    }
}

/// What a sync found before there was anything to import — the three-way
/// split §3.2 item 1's payload exists for: "nobody has taken a seat" reads
/// differently from "waiting in the lobby" and from "paused". `None` means go
/// on and import.
pub(crate) fn seat_state(status: &str, seats: &[DraftSeat]) -> Option<SyncOutcome> {
    match status {
        "paused" => Some(SyncOutcome::Paused),
        "lobby" => {
            let claimed = seats.iter().filter(|s| s.claimed).count();
            Some(if claimed == 0 {
                SyncOutcome::NotSeated
            } else {
                SyncOutcome::AwaitingSeat
            })
        },
        _ => None,
    }
}

/// Whether the draft's own reported score disagrees with a tally of *our*
/// `draft_import` games. A preserved `manual` row is a deliberate divergence,
/// not a mismatch — `tally` must already be restricted to `draft_import` rows
/// before this is called.
pub(crate) fn score_mismatch(reported: &SlotValues, tally: &Tally) -> bool {
    reported.slot1 != tally.slot1_wins || reported.slot2 != tally.slot2_wins
}

#[derive(Debug)]
pub(crate) enum SyncOutcome {
    AlreadyComplete,
    /// The set has no `draft_external_id` — never opened, or a redraft
    /// cleared it.
    NoPointer,
    /// The pointer changed between the fetch and the write — a redraft that
    /// landed mid-sync. Nothing was written.
    Superseded,
    /// `fetch_draft_state` returned `None` — a missing draft or a transport
    /// failure, collapsed like `drafttool`'s other reads.
    Unreachable,
    NotSeated,
    AwaitingSeat,
    Paused,
    Progress {
        outcome: CompleteOutcome,
        score_mismatch: bool,
    },
}

/// Syncs `set` against its current draft: fetches, then hands off to `apply`.
pub(crate) async fn sync(
    http: impl CacheHttp,
    pool: &SqlitePool,
    tournament: &Tournament,
    set: &TournamentSet,
) -> Result<SyncOutcome, Error> {
    if completion::is_decided(&set.status) {
        return Ok(SyncOutcome::AlreadyComplete);
    }
    let Some(external_id) = set.draft_external_id.clone() else {
        return Ok(SyncOutcome::NoPointer);
    };
    let Some(state) = drafttool::fetch_draft_state(&external_id).await else {
        return Ok(SyncOutcome::Unreachable);
    };
    apply(http, pool, tournament, set, &external_id, state).await
}

/// The DB-writing half, taking an already-fetched `DraftState` so a test
/// needs no network at all.
pub(crate) async fn apply(
    http: impl CacheHttp,
    pool: &SqlitePool,
    tournament: &Tournament,
    set: &TournamentSet,
    external_id: &str,
    state: DraftState,
) -> Result<SyncOutcome, Error> {
    let Some(current) = db::get_set(pool, set.id).await? else {
        return Ok(SyncOutcome::NoPointer);
    };
    if current.draft_external_id.as_deref() != Some(external_id) {
        return Ok(SyncOutcome::Superseded);
    }
    if let Some(outcome) = seat_state(&state.status, &state.seats) {
        return Ok(outcome);
    }
    let (Some(slot1_user_id), Some(slot2_user_id)) = (set.slot1_user_id, set.slot2_user_id) else {
        return Ok(SyncOutcome::Progress {
            outcome: CompleteOutcome::NotPlayable,
            score_mismatch: false,
        });
    };

    for game in map_games(&state.games, set.id, slot1_user_id, slot2_user_id) {
        db::upsert_draft_import_game(pool, game).await?;
    }
    db::set_draft_synced_at(pool, set.id, chrono::Utc::now()).await?;

    let games = db::list_games_for_set(pool, set.id).await?;
    let imported: Vec<_> = games.iter().filter(|g| g.source == "draft_import").cloned().collect();
    let tally = completion::tally(&imported, slot1_user_id, slot2_user_id);
    let mismatch = score_mismatch(&state.score, &tally);
    if mismatch {
        warn!(
            "set {} score mismatch: draft reports {}-{}, our draft_import tally is {}-{}",
            set.id, state.score.slot1, state.score.slot2, tally.slot1_wins, tally.slot2_wins
        );
    }

    let full_tally = completion::tally(&games, slot1_user_id, slot2_user_id);
    let outcome = if completion::decide(&full_tally, state.best_of).is_some() {
        completion::finish(http, pool, tournament, set).await?
    } else {
        // A head start could make `finished` true here — the bot assumes it is
        // always zero and does not act on it, but this is cheap to notice.
        if state.finished {
            warn!(
                "set {} reported finished with no majority reached — head start is assumed zero \
                 and not modeled; leaving it open",
                set.id
            );
        }
        CompleteOutcome::StillPlaying {
            tally: full_tally,
            needed: completion::majority(state.best_of),
        }
    };

    Ok(SyncOutcome::Progress {
        outcome,
        score_mismatch: mismatch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drafttool::CivBySlot;

    fn game(number: i64, map: Option<&str>, winner_slot: Option<i64>) -> DraftGame {
        DraftGame {
            number,
            map: map.map(str::to_string),
            civ_by_slot: CivBySlot {
                slot1: None,
                slot2: None,
            },
            winner_slot,
        }
    }

    #[test]
    fn slot_1_maps_to_the_higher_seed() {
        let games = vec![game(1, Some("prairie"), Some(1))];
        let mapped = map_games(&games, 42, 100, 200);
        assert_eq!(
            mapped[0].winner_user_id,
            Some(100),
            "slot 1 -> the higher seed's user id"
        );
    }

    #[test]
    fn slot_2_maps_to_the_lower_seed() {
        let games = vec![game(1, Some("prairie"), Some(2))];
        let mapped = map_games(&games, 42, 100, 200);
        assert_eq!(mapped[0].winner_user_id, Some(200));
    }

    #[test]
    fn a_fully_untouched_game_is_skipped() {
        let games = vec![game(1, None, None)];
        assert!(map_games(&games, 42, 100, 200).is_empty());
    }

    #[test]
    fn a_decided_game_is_completed_and_a_started_one_is_in_progress() {
        let games = vec![game(1, Some("prairie"), Some(1)), game(2, Some("dry-arabia"), None)];
        let mapped = map_games(&games, 42, 100, 200);
        assert_eq!(mapped[0].status, "completed");
        assert_eq!(mapped[1].status, "in_progress");
    }

    #[test]
    fn every_mapped_game_is_a_draft_import_with_no_reporter() {
        let games = vec![game(1, Some("prairie"), Some(1))];
        let mapped = map_games(&games, 42, 100, 200);
        assert_eq!(mapped[0].source, "draft_import");
        assert_eq!(mapped[0].reported_by, None);
        assert_eq!(mapped[0].reported_at, None);
    }

    fn seat(claimed: bool) -> DraftSeat {
        DraftSeat { claimed }
    }

    #[test]
    fn nobody_has_taken_a_seat_reads_differently_from_one_seat_taken() {
        assert!(matches!(
            seat_state("lobby", &[seat(false), seat(false)]),
            Some(SyncOutcome::NotSeated)
        ));
        assert!(matches!(
            seat_state("lobby", &[seat(true), seat(false)]),
            Some(SyncOutcome::AwaitingSeat)
        ));
    }

    #[test]
    fn paused_is_its_own_outcome() {
        assert!(matches!(
            seat_state("paused", &[seat(true), seat(true)]),
            Some(SyncOutcome::Paused)
        ));
    }

    #[test]
    fn running_and_finished_go_on_to_import() {
        let seats = [seat(true), seat(true)];
        assert!(seat_state("running", &seats).is_none());
        assert!(seat_state("finished", &seats).is_none());
    }

    fn slot_values(slot1: i64, slot2: i64) -> SlotValues {
        SlotValues { slot1, slot2 }
    }

    #[test]
    fn a_matching_score_is_not_a_mismatch() {
        let tally = Tally {
            slot1_wins: 2,
            slot2_wins: 0,
        };
        assert!(!score_mismatch(&slot_values(2, 0), &tally));
    }

    #[test]
    fn a_disagreeing_score_is_flagged() {
        let tally = Tally {
            slot1_wins: 1,
            slot2_wins: 0,
        };
        assert!(score_mismatch(&slot_values(2, 0), &tally));
    }
}
