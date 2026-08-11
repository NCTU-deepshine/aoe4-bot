//! Check-in business logic. Discord/HTTP-free —
//! every branch is a plain DB read/write, which is what keeps this
//! unit-testable without a live network.
//! `commands.rs` (the slash commands) and `dispatch.rs` (the check-in button)
//! both call into this module rather than duplicating any of it.

use crate::locale::Locale;
use crate::tournament::db::{self, Tournament, TournamentEntry};
use crate::tournament::seeding::SeedPolicy;
use crate::tournament::setup;
use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;

/// Check-in only accepts presses while the tournament is in this exact status
/// — before it, check-in hasn't opened yet; after it, the field has
/// already moved on to seeding or beyond.
pub(crate) fn checkin_is_open(status: &str) -> bool {
    status == "checkin"
}

/// Whether the check-in panel belongs in `#{slug}-register` right now: from the
/// moment check-in opens, and afterwards in its closed form.
///
/// A repair has to ask the phase rather than whether `checkin_message_id` is
/// set — if the original post failed, that id was never written, and keying off
/// it means the one case needing repair is the one case skipped.
pub(crate) fn checkin_panel_expected(status: &str) -> bool {
    matches!(status, "checkin" | "seeding" | "running" | "completed")
}

/// The one backward edge in the lifecycle starts from exactly these:
/// before them there is no check-in round to undo, after them the field is
/// locked in and the recovery is `/tournament cancel` instead.
pub(crate) fn registration_is_reopenable(status: &str) -> bool {
    matches!(status, "checkin" | "seeding")
}

/// `(checked_in, total)` over the entrants check-in ever applied to — `active`
/// plus `no_show`, since a no-show was `active` for the whole time check-in was
/// running. `withdrawn` entries never entered that pool and are excluded
/// either way, matching `panel::render`'s own active-only filter for
/// registration.
///
/// Invited entrants are excluded from both numbers: nobody asked them to
/// confirm, so counting them would make the panel read `2/8` for a field that is
/// entirely present. One who presses the button anyway is still not counted —
/// the denominator has to mean the same thing all the way through.
pub(crate) fn checkin_counts(entries: &[TournamentEntry]) -> (i64, i64) {
    let counted: Vec<&TournamentEntry> = entries
        .iter()
        .filter(|e| matches!(e.status.as_str(), "active" | "no_show") && e.invited_by.is_none())
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
    pub(crate) fn message(&self, tournament_name: &str, locale: Locale) -> String {
        match self {
            CheckinOutcome::CheckedIn {
                checked_in_count,
                total_count,
            } => locale.pick(
                format!("你已完成 **{tournament_name}** 的簽到（{checked_in_count}/{total_count} 已簽到）。"),
                format!("You're checked in for **{tournament_name}** ({checked_in_count}/{total_count} checked in)."),
            ),
            CheckinOutcome::AlreadyCheckedIn {
                checked_in_count,
                total_count,
            } => locale.pick(
                format!("你已經簽到過 **{tournament_name}** 了（{checked_in_count}/{total_count} 已簽到）。"),
                format!(
                    "You're already checked in for **{tournament_name}** \
                     ({checked_in_count}/{total_count} checked in)."
                ),
            ),
            CheckinOutcome::NotRegistered => locale.pick(
                format!("你沒有報名 **{tournament_name}** — 請先用 `/tournament register`。"),
                format!("You're not registered for **{tournament_name}** — use `/tournament register` first."),
            ),
            CheckinOutcome::CheckinNotOpen => locale.pick(
                format!("**{tournament_name}** 目前沒有開放簽到。"),
                format!("Check-in isn't open for **{tournament_name}** right now."),
            ),
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
    Opened {
        closes_at: Option<DateTime<Utc>>,
    },
    NotInRegistration {
        current_status: String,
    },
    /// Too far ahead of the scheduled start. There is deliberately no
    /// force flag: the way past this is to set the real start time, which keeps
    /// the schedule honest instead of letting it drift.
    TooEarly {
        opens_at: DateTime<Utc>,
    },
}

impl OpenCheckinOutcome {
    pub(crate) fn message(&self, tournament_name: &str, locale: Locale) -> String {
        match self {
            OpenCheckinOutcome::Opened {
                closes_at: Some(closes_at),
            } => locale.pick(
                format!(
                    "**{tournament_name}** 的簽到已開放，<t:{}:R> 截止。",
                    closes_at.timestamp()
                ),
                format!(
                    "Check-in is now open for **{tournament_name}**, closing <t:{}:R>.",
                    closes_at.timestamp()
                ),
            ),
            OpenCheckinOutcome::Opened { closes_at: None } => locale.pick(
                format!("**{tournament_name}** 的簽到已開放。"),
                format!("Check-in is now open for **{tournament_name}**."),
            ),
            OpenCheckinOutcome::TooEarly { opens_at } => locale.pick(
                format!(
                    "簽到會在開賽前一小時開放，也就是 <t:{0}:F>（<t:{0}:R>）。\
                     如果開賽時間不對，請用 `/tournament setup start_time:` 更新。",
                    opens_at.timestamp()
                ),
                format!(
                    "Check-in opens an hour before the scheduled start, at <t:{0}:F> (<t:{0}:R>). If that's \
                     wrong, set the real time with `/tournament setup start_time:`.",
                    opens_at.timestamp()
                ),
            ),
            OpenCheckinOutcome::NotInRegistration { current_status } => locale.pick(
                format!("只有在 **{tournament_name}** 還在報名階段時才能開放簽到（目前為 {current_status}）。"),
                format!(
                    "Check-in can only be opened while **{tournament_name}** is still in registration \
                     (currently {current_status})."
                ),
            ),
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

    if let Some(scheduled) = tournament.scheduled_start_at {
        let opens_at = setup::checkin_opens_at(scheduled);
        if Utc::now() < opens_at {
            return Ok(OpenCheckinOutcome::TooEarly { opens_at });
        }
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
    pub(crate) fn message(&self, tournament_name: &str, locale: Locale) -> String {
        match self {
            CloseCheckinOutcome::Closed {
                checked_in_count,
                no_show_count,
            } => locale.pick(
                format!("**{tournament_name}** 的簽到已結束 — {checked_in_count} 人簽到，{no_show_count} 人未到。"),
                format!(
                    "Check-in closed for **{tournament_name}** — {checked_in_count} checked in, \
                     {no_show_count} marked no-show."
                ),
            ),
            CloseCheckinOutcome::NotOpen { current_status } => locale.pick(
                format!("**{tournament_name}** 目前沒有開放簽到（目前為 {current_status}）。"),
                format!("Check-in isn't open for **{tournament_name}** (currently {current_status})."),
            ),
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

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ReopenRegistrationOutcome {
    Reopened { restored_count: u64, cleared_count: u64 },
    AlreadyInRegistration,
    NotReopenable { current_status: String },
}

impl ReopenRegistrationOutcome {
    pub(crate) fn message(&self, tournament_name: &str, locale: Locale) -> String {
        match self {
            ReopenRegistrationOutcome::Reopened {
                restored_count,
                cleared_count,
            } => locale.pick(
                format!(
                    "**{tournament_name}** 重新開放報名 — 簽到已重置\
                     （清除 {cleared_count} 筆簽到，恢復 {restored_count} 位未到者）。\
                     要重新簽到時請用 `/tournament open-checkin`。"
                ),
                format!(
                    "Registration is open again for **{tournament_name}** — check-in was reset \
                     ({cleared_count} check-ins cleared, {restored_count} no-shows restored). \
                     Use `/tournament open-checkin` when you're ready to run check-in again."
                ),
            ),
            ReopenRegistrationOutcome::AlreadyInRegistration => locale.pick(
                format!("**{tournament_name}** 已經在報名階段 — 沒有需要重開的東西。"),
                format!("**{tournament_name}** is already in registration — nothing to reopen."),
            ),
            ReopenRegistrationOutcome::NotReopenable { current_status } => locale.pick(
                format!("只有在 **{tournament_name}** 處於簽到或排種子階段時才能重開報名（目前為 {current_status}）。"),
                format!(
                    "Registration can only be reopened while **{tournament_name}** is in check-in or seeding \
                     (currently {current_status})."
                ),
            ),
        }
    }

    /// Whether anything actually changed — the caller's signal for whether to
    /// delete the check-in panel and re-render the registration one.
    pub(crate) fn changed_state(&self) -> bool {
        matches!(self, ReopenRegistrationOutcome::Reopened { .. })
    }
}

/// Walks `checkin`/`seeding` back to `registration` as a full reset of the
/// check-in round: no-shows return to `active`, every `checked_in_at`
/// clears, and both panels' handles are dropped so the next post is a fresh one
/// rather than an edit of a message the caller has since deleted. Deleting those
/// messages is the caller's job — this module stays Discord-free.
///
/// A seed order the organizers set by hand is the one thing that survives.
///
/// Not transactional, for the same reason `close` isn't (see its doc comment):
/// each statement is independently atomic and nothing else races these rows.
pub(crate) async fn reopen_registration(
    pool: &SqlitePool,
    tournament: &Tournament,
) -> Result<ReopenRegistrationOutcome, sqlx::Error> {
    if tournament.status == "registration" {
        return Ok(ReopenRegistrationOutcome::AlreadyInRegistration);
    }
    if !registration_is_reopenable(&tournament.status) {
        return Ok(ReopenRegistrationOutcome::NotReopenable {
            current_status: tournament.status.clone(),
        });
    }

    let restored_count = db::revert_no_shows(pool, tournament.id).await?;
    let cleared_count = db::clear_checkins(pool, tournament.id).await?;
    // A suggested order is stale the moment the field can change again; one the
    // organizers made by hand is the whole point of a curated field, and survives.
    if SeedPolicy::from_source(&tournament.seed_source) == SeedPolicy::Suggest {
        db::clear_seeds(pool, tournament.id).await?;
    }
    db::update_tournament_status(pool, tournament.id, "registration").await?;
    db::set_checkin_closes_at(pool, tournament.id, None).await?;
    db::set_checkin_message_id(pool, tournament.id, None).await?;
    // `seed_message_id` deliberately survives: nothing deletes that panel.

    Ok(ReopenRegistrationOutcome::Reopened {
        restored_count,
        cleared_count,
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
            invited_by: None,
            seed: None,
            suggested_seed: None,
            manual_seed: None,
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
    fn messages_render_in_both_locales() {
        let outcome = CheckinOutcome::CheckedIn {
            checked_in_count: 1,
            total_count: 2,
        };
        let zh = outcome.message("Relic Cup", Locale::ZhTw);
        let en = outcome.message("Relic Cup", Locale::En);
        assert_ne!(zh, en);
        assert!(zh.contains("已完成"), "{zh}");
        assert!(en.contains("checked in"), "{en}");
        // Counts are data and must survive both renderings.
        assert!(zh.contains("1/2") && en.contains("1/2"));
    }

    #[test]
    fn the_checkin_panel_is_expected_from_checkin_onward() {
        // Repair keys off the phase, not off `checkin_message_id` — a panel
        // whose first post failed has no id, and is the one needing repair.
        for status in ["checkin", "seeding", "running", "completed"] {
            assert!(checkin_panel_expected(status), "{status} should still show the panel");
        }
        for status in ["registration", "canceled"] {
            assert!(!checkin_panel_expected(status), "{status} should not have one");
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
    fn invited_entrants_are_in_neither_half_of_the_counter() {
        // Nobody asked them to confirm, so an all-invited field must not read
        // `0/8` — and one who presses the button anyway must not read `1/8`
        // either, or the denominator changes meaning halfway through.
        let now = Utc::now();
        let invited = |user_id, checked_in_at| TournamentEntry {
            invited_by: Some(99),
            ..entry(user_id, "active", checked_in_at)
        };
        let entries = vec![
            entry(1, "active", Some(now)),
            entry(2, "active", None),
            invited(3, None),
            invited(4, Some(now)),
        ];
        assert_eq!(checkin_counts(&entries), (1, 2));
    }

    #[test]
    fn registration_is_reopenable_only_from_checkin_and_seeding() {
        assert!(registration_is_reopenable("checkin"));
        assert!(registration_is_reopenable("seeding"));
        for status in ["registration", "running", "completed", "canceled"] {
            assert!(!registration_is_reopenable(status), "{status} should not be reopenable");
        }
    }

    #[test]
    fn only_reopened_changes_panel_state() {
        assert!(
            ReopenRegistrationOutcome::Reopened {
                restored_count: 1,
                cleared_count: 2
            }
            .changed_state()
        );
        assert!(!ReopenRegistrationOutcome::AlreadyInRegistration.changed_state());
        assert!(
            !ReopenRegistrationOutcome::NotReopenable {
                current_status: "running".to_string()
            }
            .changed_state()
        );
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
