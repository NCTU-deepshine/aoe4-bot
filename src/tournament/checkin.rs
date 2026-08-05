//! Check-in business logic (docs/tournament.md §8.3, §8.5). Discord/HTTP-free —
//! every branch is a plain DB read/write, which is what keeps this
//! unit-testable without live network (no live network in tests, §10).
//! `commands.rs` (the slash commands) and `dispatch.rs` (the check-in button)
//! both call into this module rather than duplicating any of it.

use crate::tournament::db::{self, Tournament, TournamentEntry};
use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;

/// Check-in only accepts presses while the tournament is in this exact status
/// (§8.3) — before it, check-in hasn't opened yet; after it, the field has
/// already moved on to seeding or beyond.
pub(crate) fn checkin_is_open(status: &str) -> bool {
    status == "checkin"
}

/// `(checked_in, total)` over the entrants check-in ever applied to — `active`
/// plus `no_show`, since a no-show was `active` for the whole time check-in was
/// running. `withdrawn` entries never entered that pool and are excluded
/// either way, matching `panel::render`'s own active-only filter for
/// registration.
pub(crate) fn checkin_counts(entries: &[TournamentEntry]) -> (i64, i64) {
    let counted: Vec<&TournamentEntry> = entries
        .iter()
        .filter(|e| matches!(e.status.as_str(), "active" | "no_show"))
        .collect();
    let checked_in = counted.iter().filter(|e| e.checked_in_at.is_some()).count();
    (
        i64::try_from(checked_in).unwrap_or(0),
        i64::try_from(counted.len()).unwrap_or(0),
    )
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CheckinOutcome {
    CheckedIn { checked_in_count: i64, total_count: i64 },
    AlreadyCheckedIn { checked_in_count: i64, total_count: i64 },
    NotRegistered,
    CheckinNotOpen,
}

impl CheckinOutcome {
    pub(crate) fn message(&self, tournament_name: &str) -> String {
        match self {
            CheckinOutcome::CheckedIn {
                checked_in_count,
                total_count,
            } => {
                format!("You're checked in for **{tournament_name}** ({checked_in_count}/{total_count} checked in).")
            },
            CheckinOutcome::AlreadyCheckedIn {
                checked_in_count,
                total_count,
            } => {
                format!(
                    "You're already checked in for **{tournament_name}** \
                     ({checked_in_count}/{total_count} checked in)."
                )
            },
            CheckinOutcome::NotRegistered => {
                format!("You're not registered for **{tournament_name}** — use `/tournament register` first.")
            },
            CheckinOutcome::CheckinNotOpen => {
                format!("Check-in isn't open for **{tournament_name}** right now.")
            },
        }
    }

    /// Whether this outcome actually changed the check-in state — the caller's
    /// signal for whether the check-in panel needs a (throttled) refresh.
    pub(crate) fn changed_state(&self) -> bool {
        matches!(self, CheckinOutcome::CheckedIn { .. })
    }
}

pub(crate) async fn checkin(
    pool: &SqlitePool,
    tournament: &Tournament,
    user_id: i64,
) -> Result<CheckinOutcome, sqlx::Error> {
    if !checkin_is_open(&tournament.status) {
        return Ok(CheckinOutcome::CheckinNotOpen);
    }

    let Some(entry) = db::get_entry(pool, tournament.id, user_id).await? else {
        return Ok(CheckinOutcome::NotRegistered);
    };
    // A withdrawn entry is not part of the field any more, and no_show/eliminated
    // are unreachable while status is still "checkin" — but treated the same way
    // regardless, rather than assuming which of these it could be.
    if entry.status != "active" {
        return Ok(CheckinOutcome::NotRegistered);
    }
    if entry.checked_in_at.is_some() {
        let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
        let (checked_in_count, total_count) = checkin_counts(&entries);
        return Ok(CheckinOutcome::AlreadyCheckedIn {
            checked_in_count,
            total_count,
        });
    }

    db::set_entry_checked_in(pool, tournament.id, user_id, Utc::now()).await?;
    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
    let (checked_in_count, total_count) = checkin_counts(&entries);
    Ok(CheckinOutcome::CheckedIn {
        checked_in_count,
        total_count,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OpenCheckinOutcome {
    Opened { closes_at: Option<DateTime<Utc>> },
    NotInRegistration { current_status: String },
}

impl OpenCheckinOutcome {
    pub(crate) fn message(&self, tournament_name: &str) -> String {
        match self {
            OpenCheckinOutcome::Opened {
                closes_at: Some(closes_at),
            } => {
                format!(
                    "Check-in is now open for **{tournament_name}**, closing <t:{}:R>.",
                    closes_at.timestamp()
                )
            },
            OpenCheckinOutcome::Opened { closes_at: None } => {
                format!("Check-in is now open for **{tournament_name}**.")
            },
            OpenCheckinOutcome::NotInRegistration { current_status } => {
                format!(
                    "Check-in can only be opened while **{tournament_name}** is still in registration \
                     (currently {current_status})."
                )
            },
        }
    }
}

pub(crate) async fn open(
    pool: &SqlitePool,
    tournament: &Tournament,
    minutes: Option<i64>,
) -> Result<OpenCheckinOutcome, sqlx::Error> {
    if tournament.status != "registration" {
        return Ok(OpenCheckinOutcome::NotInRegistration {
            current_status: tournament.status.clone(),
        });
    }

    let closes_at = minutes.map(|m| Utc::now() + Duration::minutes(m));
    db::update_tournament_status(pool, tournament.id, "checkin").await?;
    db::set_checkin_closes_at(pool, tournament.id, closes_at).await?;
    Ok(OpenCheckinOutcome::Opened { closes_at })
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CloseCheckinOutcome {
    Closed { checked_in_count: i64, no_show_count: i64 },
    NotOpen { current_status: String },
}

impl CloseCheckinOutcome {
    pub(crate) fn message(&self, tournament_name: &str) -> String {
        match self {
            CloseCheckinOutcome::Closed {
                checked_in_count,
                no_show_count,
            } => {
                format!(
                    "Check-in closed for **{tournament_name}** — {checked_in_count} checked in, \
                     {no_show_count} marked no-show."
                )
            },
            CloseCheckinOutcome::NotOpen { current_status } => {
                format!("Check-in isn't open for **{tournament_name}** (currently {current_status}).")
            },
        }
    }
}

/// Marks every `active`, never-checked-in entry `no_show`, then advances the
/// tournament to `seeding`. Not wrapped in a transaction: both statements are
/// independently atomic and there is no genuine concurrent-write race here (see
/// `db::mark_no_shows`'s doc comment) — the same non-transactional-sequence
/// tolerance `commands::create` already relies on.
pub(crate) async fn close(pool: &SqlitePool, tournament: &Tournament) -> Result<CloseCheckinOutcome, sqlx::Error> {
    if !checkin_is_open(&tournament.status) {
        return Ok(CloseCheckinOutcome::NotOpen {
            current_status: tournament.status.clone(),
        });
    }

    db::mark_no_shows(pool, tournament.id).await?;
    db::update_tournament_status(pool, tournament.id, "seeding").await?;

    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
    let (checked_in_count, total_count) = checkin_counts(&entries);
    Ok(CloseCheckinOutcome::Closed {
        checked_in_count,
        no_show_count: total_count - checked_in_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(user_id: i64, status: &str, checked_in_at: Option<DateTime<Utc>>) -> TournamentEntry {
        TournamentEntry {
            tournament_id: 1,
            user_id,
            aoe4_id: user_id,
            seed: None,
            suggested_seed: None,
            display_name: format!("player-{user_id}"),
            elo: None,
            atr: None,
            atr_source: None,
            status: status.to_string(),
            registered_at: Utc::now(),
            checked_in_at,
        }
    }

    #[test]
    fn checkin_is_open_only_during_the_checkin_status() {
        assert!(checkin_is_open("checkin"));
        for status in ["registration", "seeding", "running", "completed", "canceled"] {
            assert!(!checkin_is_open(status), "{status} should not count as open");
        }
    }

    #[test]
    fn counts_active_and_no_show_but_excludes_withdrawn() {
        let now = Utc::now();
        let entries = vec![
            entry(1, "active", Some(now)),
            entry(2, "active", None),
            entry(3, "no_show", None),
            entry(4, "withdrawn", None),
        ];
        assert_eq!(checkin_counts(&entries), (1, 3));
    }

    #[test]
    fn counts_are_zero_with_no_entries() {
        assert_eq!(checkin_counts(&[]), (0, 0));
    }

    #[test]
    fn only_checked_in_changes_panel_state() {
        assert!(
            CheckinOutcome::CheckedIn {
                checked_in_count: 1,
                total_count: 2
            }
            .changed_state()
        );
        assert!(
            !CheckinOutcome::AlreadyCheckedIn {
                checked_in_count: 1,
                total_count: 2
            }
            .changed_state()
        );
        assert!(!CheckinOutcome::NotRegistered.changed_state());
        assert!(!CheckinOutcome::CheckinNotOpen.changed_state());
    }
}
