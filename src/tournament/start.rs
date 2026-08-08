//! `/tournament start`: turn a seeded field into a
//! persisted bracket and open round one.
//!
//! The gates are pure functions over data the caller has already read, so every
//! refusal is testable without touching a database or Discord.

use crate::locale::Locale;
use crate::tournament::db::{self, Tournament, TournamentEntry};
use crate::tournament::{bracket, seeding, setup};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// The seeded field, in seed order, if the seeds are usable.
///
/// `start` needs seeds to be exactly 1..n with no gaps and no duplicates —
/// generation indexes straight into them. A gap is not hypothetical: registration
/// closes at check-in but **withdrawal stays open through seeding**, so an
/// entrant leaving after the seeding pass takes their number with them.
pub(crate) fn seeded_field(entries: &[TournamentEntry]) -> Option<Vec<&TournamentEntry>> {
    let mut field: Vec<&TournamentEntry> = seeding::seedable(entries);
    if field.is_empty() {
        return None;
    }

    let mut seeds: Vec<i64> = field.iter().filter_map(|e| e.seed).collect();
    if seeds.len() != field.len() {
        return None; // somebody has no seed at all
    }
    seeds.sort_unstable();
    if seeds != (1..=i64::try_from(field.len()).ok()?).collect::<Vec<_>>() {
        return None; // a gap, or a duplicate
    }

    field.sort_by_key(|e| e.seed);
    Some(field)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StartOutcome {
    Started { entrants: usize, rounds: usize },
    NotSeeding { current_status: String },
    NotConfigured,
    SeedsNotContiguous,
    TooFewEntrants,
    TooEarly { scheduled_start_at: DateTime<Utc> },
}

impl StartOutcome {
    pub(crate) fn message(&self, tournament_name: &str, locale: Locale) -> String {
        match self {
            StartOutcome::Started { entrants, rounds } => locale.pick(
                format!(
                    "**{tournament_name}** 開賽了！{entrants} 位參賽者，{rounds} 輪賽程。\
                     種子順序已固定，賽程表就在賽表頻道。"
                ),
                format!(
                    "**{tournament_name}** has started — {entrants} entrants over {rounds} rounds. Seeds are \
                     frozen now, and the bracket is in the bracket channel."
                ),
            ),
            StartOutcome::NotSeeding { current_status } => locale.pick(
                format!("只有在排種子階段才能開賽（**{tournament_name}** 目前為 {current_status}）。"),
                format!("A tournament can only start from seeding (**{tournament_name}** is {current_status})."),
            ),
            StartOutcome::NotConfigured => locale.pick(
                "比賽還沒設定完成 — 請先用 `/tournament setup` 查看還缺什麼。".to_string(),
                "Setup isn't finished — run `/tournament setup` to see what's still needed.".to_string(),
            ),
            StartOutcome::SeedsNotContiguous => locale.pick(
                "種子順序有缺號（通常是排種子後有人退賽）— 請用 `/tournament seed refresh` 重新編號。".to_string(),
                "The seed order has a gap, usually because someone withdrew after seeding — run \
                 `/tournament seed refresh` to renumber."
                    .to_string(),
            ),
            StartOutcome::TooFewEntrants => locale.pick(
                "至少要有兩位已簽到的參賽者才能開賽。".to_string(),
                "A bracket needs at least two checked-in entrants.".to_string(),
            ),
            StartOutcome::TooEarly { scheduled_start_at } => locale.pick(
                format!(
                    "預定開賽時間是 <t:{0}:F>（<t:{0}:R>）。如果時間不對，請用 `/tournament setup start_time:` 更新。",
                    scheduled_start_at.timestamp()
                ),
                format!(
                    "This is scheduled to start <t:{0}:F> (<t:{0}:R>). If that's wrong, set the real time with \
                     `/tournament setup start_time:`.",
                    scheduled_start_at.timestamp()
                ),
            ),
        }
    }
}

/// Generates the bracket, persists it, and opens round one.
///
/// Every refusal happens before anything is written, so a rejected start leaves
/// the tournament exactly as it was.
pub(crate) async fn start(pool: &sqlx::SqlitePool, tournament: &Tournament) -> Result<StartOutcome, sqlx::Error> {
    if tournament.status != "seeding" {
        return Ok(StartOutcome::NotSeeding {
            current_status: tournament.status.clone(),
        });
    }

    let presets = db::list_round_presets(pool, tournament.id).await?;
    if !setup::missing(&presets).is_empty() {
        return Ok(StartOutcome::NotConfigured);
    }
    if let Some(scheduled) = tournament.scheduled_start_at
        && !setup::may_start_at(Some(scheduled), Utc::now())
    {
        return Ok(StartOutcome::TooEarly {
            scheduled_start_at: scheduled,
        });
    }

    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
    let Some(field) = seeded_field(&entries) else {
        return Ok(StartOutcome::SeedsNotContiguous);
    };
    if field.len() < 2 {
        return Ok(StartOutcome::TooFewEntrants);
    }

    let round_count = bracket::round_count(bracket::size(field.len()));
    // One resolution, two shapes taken off it: the series lengths the bracket is
    // built from, and the preset ids its rounds record.
    let Some(per_round) = setup::presets_per_round(&presets, round_count) else {
        return Ok(StartOutcome::NotConfigured);
    };
    let Some(best_of) = per_round
        .iter()
        .map(|preset| u8::try_from(preset.best_of).ok())
        .collect::<Option<Vec<u8>>>()
    else {
        return Ok(StartOutcome::NotConfigured);
    };
    let Ok(built) = bracket::build(field.len(), &best_of) else {
        return Ok(StartOutcome::TooFewEntrants);
    };

    let seed_to_user: HashMap<u32, i64> = field
        .iter()
        .filter_map(|e| Some((u32::try_from(e.seed?).ok()?, e.user_id)))
        .collect();

    db::insert_bracket(pool, tournament.id, &built, &seed_to_user, &per_round).await?;
    open_round_one(pool, tournament.id).await?;

    db::update_tournament_status(pool, tournament.id, "running").await?;
    db::set_tournament_started_at(pool, tournament.id, Utc::now()).await?;

    Ok(StartOutcome::Started {
        entrants: field.len(),
        rounds: built.rounds.len(),
    })
}

/// Resolves byes and marks every playable set ready.
///
/// A bye is a set with one occupant, which generation places against the top
/// seeds. It is
/// decided the moment the bracket opens: recorded `bye` with its occupant as
/// winner, and that occupant written into the next set.
///
/// **Readiness is then decided by both slots being filled, not by being in round
/// one.** With 5 entrants in an 8-bracket, round two's lower set is fed by two
/// byes and is playable immediately. Byes never cascade further than that:
/// `next_power_of_two` leaves under half the slots empty and reflection puts each
/// against a distinct seed, so no set is ever fully empty.
async fn open_round_one(pool: &sqlx::SqlitePool, tournament_id: i64) -> Result<(), sqlx::Error> {
    let sets = db::list_sets_for_tournament(pool, tournament_id).await?;
    for set in &sets {
        let ((Some(winner), None) | (None, Some(winner))) = (set.slot1_user_id, set.slot2_user_id) else {
            continue;
        };
        db::record_set_result(pool, set.id, 0, 0, Some(winner), "bye", Some(Utc::now())).await?;

        if let (Some(next), Some(slot)) = (set.winner_advances_to_set_id, set.winner_advances_to_slot) {
            let slot = if slot == 1 {
                bracket::Slot::One
            } else {
                bracket::Slot::Two
            };
            db::set_slot(pool, next, slot, winner).await?;
        }
    }

    // Re-read: the byes above have filled slots in later rounds.
    for set in db::list_sets_for_tournament(pool, tournament_id).await? {
        if set.status == "pending" && set.slot1_user_id.is_some() && set.slot2_user_id.is_some() {
            db::update_set_status(pool, set.id, "ready").await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn entry(user_id: i64, seed: Option<i64>, status: &str) -> TournamentEntry {
        TournamentEntry {
            tournament_id: 1,
            user_id,
            aoe4_id: user_id * 100,
            seed,
            suggested_seed: seed,
            display_name: format!("P{user_id}"),
            elo: None,
            atr: None,
            atr_source: None,
            status: status.to_string(),
            registered_at: Utc::now(),
            checked_in_at: Some(Utc::now()),
        }
    }

    fn seeded(n: i64) -> Vec<TournamentEntry> {
        (1..=n).map(|i| entry(i, Some(i), "active")).collect()
    }

    #[test]
    fn a_contiguous_field_comes_back_in_seed_order() {
        let entries = vec![
            entry(1, Some(3), "active"),
            entry(2, Some(1), "active"),
            entry(3, Some(2), "active"),
        ];
        let field = seeded_field(&entries).expect("1..3 is contiguous");
        assert_eq!(field.iter().map(|e| e.user_id).collect::<Vec<_>>(), vec![2, 3, 1]);
    }

    #[test]
    fn a_gap_is_refused() {
        // What a withdrawal after seeding leaves behind.
        let entries = vec![entry(1, Some(1), "active"), entry(2, Some(3), "active")];
        assert!(seeded_field(&entries).is_none());
    }

    #[test]
    fn a_duplicate_seed_is_refused() {
        let entries = vec![entry(1, Some(1), "active"), entry(2, Some(1), "active")];
        assert!(seeded_field(&entries).is_none());
    }

    #[test]
    fn an_unseeded_entrant_is_refused() {
        // Someone who registered during seeding and was never renumbered.
        let entries = vec![entry(1, Some(1), "active"), entry(2, None, "active")];
        assert!(seeded_field(&entries).is_none());
    }

    #[test]
    fn withdrawn_and_no_show_entrants_are_not_part_of_the_field() {
        let mut entries = seeded(2);
        entries.push(entry(3, None, "withdrawn"));
        entries.push(entry(4, None, "no_show"));
        let field = seeded_field(&entries).expect("the two active seeds are contiguous");
        assert_eq!(field.len(), 2);
    }

    #[test]
    fn an_empty_field_is_refused_rather_than_treated_as_contiguous() {
        assert!(seeded_field(&[]).is_none());
    }

    #[test]
    fn messages_render_in_both_locales() {
        let outcome = StartOutcome::SeedsNotContiguous;
        let zh = outcome.message("Relic Cup", Locale::ZhTw);
        let en = outcome.message("Relic Cup", Locale::En);
        assert_ne!(zh, en);
        // Both have to name the command that fixes it.
        assert!(zh.contains("/tournament seed refresh"), "{zh}");
        assert!(en.contains("/tournament seed refresh"), "{en}");
    }
}
