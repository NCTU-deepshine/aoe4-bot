//! Registration and profile binding business logic (docs/tournament.md §8.5, §4).
//! Discord- and HTTP-free except for the one first-sign-up path that resolves an
//! aoe4world profile — every other branch is a plain DB read/write, which is what
//! keeps this unit-testable without live network (no live network in tests, §10).
//! `commands.rs` (the slash commands) and `dispatch.rs` (the register/withdraw
//! buttons) both call into this module rather than duplicating any of it.

use crate::aoe4world;
use crate::locale::Locale;
use crate::tournament::db::{self, Tournament, TournamentEntry};
use sqlx::SqlitePool;

/// Blocks registration and withdrawal alike once a tournament has moved past the
/// point either could still matter — the only literal lifecycle statements in the
/// design are "registering after start... rejected" (chunk 12's gate) and
/// "withdrawal... before start only" (§8.4), and both read the same way.
/// Deliberately broader than `db::has_running_tournament_entry`, which is
/// `/tournament rebind`'s guard and only cares about `running` specifically
/// (§4 notes) — a rebind can't disturb a `completed`/`canceled` tournament's
/// frozen entry, but registering/withdrawing from one makes no sense either way.
pub(crate) fn tournament_has_started(status: &str) -> bool {
    matches!(status, "running" | "completed" | "canceled")
}

/// 1-based rank of `user_id`'s entry by `registered_at` (ties broken by `user_id`
/// for determinism) — not a raw row count, which would drift after a
/// withdraw-then-rejoin cycle if other entrants joined in the meantime.
pub(crate) fn entrant_number(entries: &[TournamentEntry], user_id: i64) -> i64 {
    let mut ordered: Vec<&TournamentEntry> = entries.iter().collect();
    ordered.sort_by_key(|e| (e.registered_at, e.user_id));
    ordered
        .iter()
        .position(|e| e.user_id == user_id)
        .map(|pos| i64::try_from(pos + 1).unwrap_or(0))
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RegisterOutcome {
    /// A genuinely new entry for this tournament — either a first-ever sign-up
    /// (with a freshly resolved `elo`) or a returning player using an existing
    /// binding (`elo` is `None`, since no profile is re-fetched on a repeat
    /// sign-up).
    Registered {
        display_name: String,
        elo: Option<i64>,
        entrant_number: i64,
    },
    AlreadyRegistered {
        display_name: String,
        entrant_number: i64,
    },
    /// A withdrawn entry flipped back to `active` — entries are never deleted, so
    /// this is not the same as `AlreadyRegistered`.
    Reactivated {
        display_name: String,
        entrant_number: i64,
    },
    NeedsProfileArgument,
    AlreadyBoundToDifferentProfile {
        display_name: String,
    },
    ProfileClaimedByAnother {
        other_user_id: i64,
        other_display_name: String,
    },
    /// The transaction's own `UNIQUE(aoe4_id)` failure: a genuine race between two
    /// concurrent first sign-ups for the same profile, caught after the pre-check
    /// already passed for both. Nobody to name here — the pre-check is what
    /// supplies `ProfileClaimedByAnother`'s detail in the common case.
    ProfileClaimRace,
    LookupFailed,
    RegistrationClosed,
}

impl RegisterOutcome {
    pub(crate) fn message(&self, tournament_name: &str, locale: Locale) -> String {
        match self {
            RegisterOutcome::Registered {
                display_name,
                elo,
                entrant_number,
            } => {
                let elo_suffix = elo.map(|e| format!(" (ELO {e})")).unwrap_or_default();
                locale.pick(
                    format!("以 **{display_name}**{elo_suffix} 報名成功，你是第 {entrant_number} 位參賽者。"),
                    format!("Registered as **{display_name}**{elo_suffix}. You are entrant #{entrant_number}."),
                )
            },
            RegisterOutcome::AlreadyRegistered {
                display_name,
                entrant_number,
            } => locale.pick(
                format!("你已經報名過 **{tournament_name}**，身分是 **{display_name}**（第 {entrant_number} 位參賽者）。"),
                format!(
                    "You're already registered for **{tournament_name}** as **{display_name}** \
                     (entrant #{entrant_number})."
                ),
            ),
            RegisterOutcome::Reactivated {
                display_name,
                entrant_number,
            } => locale.pick(
                format!(
                    "歡迎回來 — 你重新報名了 **{tournament_name}**，身分是 **{display_name}**\
                     （第 {entrant_number} 位）。"
                ),
                format!(
                    "Welcome back — you're registered again for **{tournament_name}** as **{display_name}** \
                     (entrant #{entrant_number})."
                ),
            ),
            RegisterOutcome::NeedsProfileArgument => locale.pick(
                "你還沒有綁定任何帳號 — 第一次報名請用 `/tournament register aoe4_id:<profile>` 指定綁定aoe4帳號進行報名。".to_string(),
                "You haven't registered a profile yet — please use `/tournament register aoe4_id:<profile>` only for the first time to bind your aoe4 account and sign up."
                    .to_string(),
            ),
            RegisterOutcome::AlreadyBoundToDifferentProfile { display_name } => locale.pick(
                format!("你已經綁定 **{display_name}**。想更換帳號請用 `/tournament rebind`。"),
                format!(
                    "You're already bound to **{display_name}**. Use `/tournament rebind` if you want to change \
                     your profile."
                ),
            ),
            RegisterOutcome::ProfileClaimedByAnother {
                other_user_id,
                other_display_name,
            } => locale.pick(
                format!(
                    "這個 aoe4 帳號已經綁定給 <@{other_user_id}>（**{other_display_name}**）。如果有誤請找管理員。"
                ),
                format!(
                    "That aoe4 profile is already registered to <@{other_user_id}> \
                     (**{other_display_name}**). If this is a mistake, ask an admin."
                ),
            ),
            RegisterOutcome::ProfileClaimRace => locale.pick(
                "這個 aoe4 帳號剛剛被別人綁走了 — 請換一個帳號再試一次。".to_string(),
                "That aoe4 profile was just claimed by someone else — try again with a different profile."
                    .to_string(),
            ),
            RegisterOutcome::LookupFailed => locale.pick(
                "找不到這個 aoe4 帳號 — 請確認 id 後再試一次。".to_string(),
                "Couldn't find that aoe4 profile — double-check the id and try again.".to_string(),
            ),
            RegisterOutcome::RegistrationClosed => locale.pick(
                format!("**{tournament_name}** 的報名已經結束。"),
                format!("Registration is closed for **{tournament_name}**."),
            ),
        }
    }

    /// Whether this outcome actually changed the entry set — the caller's signal
    /// for whether the registration panel needs a (throttled) refresh.
    pub(crate) fn changed_state(&self) -> bool {
        matches!(
            self,
            RegisterOutcome::Registered { .. } | RegisterOutcome::Reactivated { .. }
        )
    }
}

pub(crate) async fn register(
    pool: &SqlitePool,
    tournament: &Tournament,
    user_id: i64,
    aoe4_id: Option<i64>,
) -> Result<RegisterOutcome, sqlx::Error> {
    if tournament_has_started(&tournament.status) {
        return Ok(RegisterOutcome::RegistrationClosed);
    }

    if let Some(entry) = db::get_entry(pool, tournament.id, user_id).await? {
        return if entry.status == "withdrawn" {
            db::update_entry_status(pool, tournament.id, user_id, "active").await?;
            let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
            Ok(RegisterOutcome::Reactivated {
                entrant_number: entrant_number(&entries, user_id),
                display_name: entry.display_name,
            })
        } else {
            let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
            Ok(RegisterOutcome::AlreadyRegistered {
                entrant_number: entrant_number(&entries, user_id),
                display_name: entry.display_name,
            })
        };
    }

    match db::get_player(pool, user_id).await? {
        Some(player) => {
            if let Some(given) = aoe4_id
                && given != player.aoe4_id
            {
                return Ok(RegisterOutcome::AlreadyBoundToDifferentProfile {
                    display_name: player.display_name,
                });
            }
            db::insert_entry(pool, tournament.id, user_id, player.aoe4_id, &player.display_name).await?;
            let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
            Ok(RegisterOutcome::Registered {
                entrant_number: entrant_number(&entries, user_id),
                display_name: player.display_name,
                elo: None,
            })
        },
        None => {
            let Some(aoe4_id) = aoe4_id else {
                return Ok(RegisterOutcome::NeedsProfileArgument);
            };

            if let Some(other) = db::get_player_by_aoe4_id(pool, aoe4_id).await?
                && other.user_id != user_id
            {
                return Ok(RegisterOutcome::ProfileClaimedByAnother {
                    other_user_id: other.user_id,
                    other_display_name: other.display_name,
                });
            }

            let Some(profile) = aoe4world::fetch_profile(aoe4_id).await else {
                return Ok(RegisterOutcome::LookupFailed);
            };
            let elo = profile.modes.rm_1v1_elo.map(|data| i64::from(data.rating));

            match db::register_new_player_and_entry(pool, tournament.id, user_id, aoe4_id, &profile.name, elo).await {
                Ok(()) => {
                    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
                    Ok(RegisterOutcome::Registered {
                        entrant_number: entrant_number(&entries, user_id),
                        display_name: profile.name,
                        elo,
                    })
                },
                Err(err) if is_aoe4_id_conflict(&err) => Ok(RegisterOutcome::ProfileClaimRace),
                Err(err) => Err(err),
            }
        },
    }
}

/// Distinguishes the transaction's own `UNIQUE(tournament_players.aoe4_id)`
/// failure from any other database error, the same string-matching approach
/// `integration_tests.rs`'s `reproduce_conflict_error` already relies on.
fn is_aoe4_id_conflict(err: &sqlx::Error) -> bool {
    err.to_string().contains("tournament_players.aoe4_id")
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WithdrawOutcome {
    Success,
    NotRegistered,
    AlreadyWithdrawn,
    TournamentAlreadyStarted,
}

impl WithdrawOutcome {
    pub(crate) fn message(&self, tournament_name: &str, locale: Locale) -> String {
        match self {
            WithdrawOutcome::Success => locale.pick(
                format!("你已退出 **{tournament_name}**。"),
                format!("You've withdrawn from **{tournament_name}**."),
            ),
            WithdrawOutcome::NotRegistered => locale.pick(
                format!("你並沒有報名 **{tournament_name}**。"),
                format!("You're not registered for **{tournament_name}**."),
            ),
            WithdrawOutcome::AlreadyWithdrawn => locale.pick(
                format!("你原本便已經退出 **{tournament_name}** 了。"),
                format!("You're already withdrawn from **{tournament_name}**."),
            ),
            WithdrawOutcome::TournamentAlreadyStarted => locale.pick(
                format!("**{tournament_name}** 已經開賽 — 無法再退賽。需要退出請聯絡管理員。"),
                format!(
                    "**{tournament_name}** has already started — withdrawal is no longer possible. Contact an \
                     admin if you need to drop out."
                ),
            ),
        }
    }

    pub(crate) fn changed_state(&self) -> bool {
        matches!(self, WithdrawOutcome::Success)
    }
}

pub(crate) async fn withdraw(
    pool: &SqlitePool,
    tournament: &Tournament,
    user_id: i64,
) -> Result<WithdrawOutcome, sqlx::Error> {
    if tournament_has_started(&tournament.status) {
        return Ok(WithdrawOutcome::TournamentAlreadyStarted);
    }
    let Some(entry) = db::get_entry(pool, tournament.id, user_id).await? else {
        return Ok(WithdrawOutcome::NotRegistered);
    };
    if entry.status == "withdrawn" {
        return Ok(WithdrawOutcome::AlreadyWithdrawn);
    }
    db::update_entry_status(pool, tournament.id, user_id, "withdrawn").await?;
    Ok(WithdrawOutcome::Success)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RebindOutcome {
    Success { display_name: String, elo: Option<i64> },
    NoExistingProfile,
    ProfileClaimedByAnother { other_user_id: i64 },
    RefusedRunningTournament,
    LookupFailed,
}

impl RebindOutcome {
    /// Tournament-independent — the player list is global (§4), so unlike
    /// register/withdraw's messages this needs no tournament name.
    pub(crate) fn message(&self, locale: Locale) -> String {
        match self {
            RebindOutcome::Success { display_name, elo } => {
                let elo_suffix = elo.map(|e| format!(" (ELO {e})")).unwrap_or_default();
                locale.pick(
                    format!("已改綁到 **{display_name}**{elo_suffix}。"),
                    format!("Rebound to **{display_name}**{elo_suffix}."),
                )
            },
            RebindOutcome::NoExistingProfile => locale.pick(
                "你還沒有在任何賽事報名過 — 請先用 `/tournament register aoe4_id:<profile>`。".to_string(),
                "You haven't registered anywhere yet — use `/tournament register aoe4_id:<profile>` first.".to_string(),
            ),
            RebindOutcome::ProfileClaimedByAnother { other_user_id } => locale.pick(
                format!("這個帳號已經綁定給 <@{other_user_id}>。"),
                format!("That profile is already bound to <@{other_user_id}>."),
            ),
            RebindOutcome::RefusedRunningTournament => locale.pick(
                "你在進行中的賽事還有參賽紀錄，無法改綁。請聯絡管理員。".to_string(),
                "You can't rebind while you have an entry in a running tournament. Ask an admin for help.".to_string(),
            ),
            RebindOutcome::LookupFailed => locale.pick(
                "找不到這個 aoe4 帳號 — 請確認 id 後再試一次。".to_string(),
                "Couldn't find that aoe4 profile — double-check the id and try again.".to_string(),
            ),
        }
    }
}

pub(crate) async fn rebind(pool: &SqlitePool, user_id: i64, aoe4_id: i64) -> Result<RebindOutcome, sqlx::Error> {
    if db::get_player(pool, user_id).await?.is_none() {
        return Ok(RebindOutcome::NoExistingProfile);
    }
    if db::has_running_tournament_entry(pool, user_id).await? {
        return Ok(RebindOutcome::RefusedRunningTournament);
    }
    if let Some(other) = db::get_player_by_aoe4_id(pool, aoe4_id).await?
        && other.user_id != user_id
    {
        return Ok(RebindOutcome::ProfileClaimedByAnother {
            other_user_id: other.user_id,
        });
    }
    let Some(profile) = aoe4world::fetch_profile(aoe4_id).await else {
        return Ok(RebindOutcome::LookupFailed);
    };
    let elo = profile.modes.rm_1v1_elo.map(|data| i64::from(data.rating));
    db::update_player_binding(pool, user_id, aoe4_id).await?;
    Ok(RebindOutcome::Success {
        display_name: profile.name,
        elo,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn entry(user_id: i64, registered_at: chrono::DateTime<Utc>) -> TournamentEntry {
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
            status: "active".to_string(),
            registered_at,
            checked_in_at: None,
        }
    }

    #[test]
    fn messages_render_in_both_locales() {
        let outcome = WithdrawOutcome::Success;
        let zh = outcome.message("Relic Cup", Locale::ZhTw);
        let en = outcome.message("Relic Cup", Locale::En);
        assert_ne!(zh, en);
        assert!(zh.contains("你已退出"), "{zh}");
        assert!(en.contains("You've withdrawn"), "{en}");
        // The tournament name is data, not text — it is not translated.
        assert!(zh.contains("Relic Cup") && en.contains("Relic Cup"));
    }

    #[test]
    fn entrant_number_ranks_by_registration_order() {
        let t0 = Utc.timestamp_opt(1_000, 0).unwrap();
        let t1 = Utc.timestamp_opt(2_000, 0).unwrap();
        let t2 = Utc.timestamp_opt(3_000, 0).unwrap();
        let entries = vec![entry(1, t0), entry(2, t1), entry(3, t2)];

        assert_eq!(entrant_number(&entries, 1), 1);
        assert_eq!(entrant_number(&entries, 2), 2);
        assert_eq!(entrant_number(&entries, 3), 3);
    }

    #[test]
    fn entrant_number_survives_out_of_order_input() {
        let t0 = Utc.timestamp_opt(1_000, 0).unwrap();
        let t1 = Utc.timestamp_opt(2_000, 0).unwrap();
        let t2 = Utc.timestamp_opt(3_000, 0).unwrap();
        // Deliberately not in registration order, unlike the list a real query
        // would return — the function must sort, not trust caller order.
        let entries = vec![entry(3, t2), entry(1, t0), entry(2, t1)];

        assert_eq!(entrant_number(&entries, 1), 1);
        assert_eq!(entrant_number(&entries, 3), 3);
    }

    #[test]
    fn entrant_number_ties_break_by_user_id() {
        let t = Utc.timestamp_opt(1_000, 0).unwrap();
        let entries = vec![entry(5, t), entry(2, t)];

        assert_eq!(entrant_number(&entries, 2), 1);
        assert_eq!(entrant_number(&entries, 5), 2);
    }

    #[test]
    fn tournament_started_statuses_are_recognized() {
        for status in ["running", "completed", "canceled"] {
            assert!(tournament_has_started(status), "{status} should count as started");
        }
        for status in ["registration", "checkin", "seeding"] {
            assert!(!tournament_has_started(status), "{status} should not count as started");
        }
    }

    #[test]
    fn only_registered_and_reactivated_change_panel_state() {
        assert!(
            RegisterOutcome::Registered {
                display_name: "A".to_string(),
                elo: None,
                entrant_number: 1
            }
            .changed_state()
        );
        assert!(
            RegisterOutcome::Reactivated {
                display_name: "A".to_string(),
                entrant_number: 1
            }
            .changed_state()
        );
        assert!(
            !RegisterOutcome::AlreadyRegistered {
                display_name: "A".to_string(),
                entrant_number: 1
            }
            .changed_state()
        );
        assert!(!RegisterOutcome::NeedsProfileArgument.changed_state());
    }
}
