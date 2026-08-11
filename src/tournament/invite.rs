//! The organizers' own door into a field: `/tournament invite` puts a Discord
//! member straight in against a real aoe4world profile, and
//! `/tournament uninvite` takes them back out.
//!
//! **The profile is required, not optional.** A manual invite is how an admin
//! composes a curated field, and they already know who they are putting in
//! it — so there is no "invite them unverified" path to fall back to, and
//! every successful invite ends up rated exactly like a self-registered
//! entrant.
//!
//! Shaped like `registration.rs` — pure decisions plus thin database writes, no
//! Discord and no HTTP except the one profile-claim path it shares with
//! `register` — so every other branch is testable without either. The cap and
//! the seed order are deliberately reused rather than reimplemented: an invited
//! entrant occupies a place and a seed exactly like a self-registered one.
//!
//! **A profile is never silently rebound.** `registration::binding_action` is
//! the same guard `register` uses: an unbound entrant may claim one, but an
//! admin picking a profile that conflicts with what this Discord account is
//! *already* linked to is refused outright rather than either overriding the
//! real binding or quietly keeping it — the picker's own prefill is what is
//! meant to steer an admin away from that pick in the first place.

use crate::locale::Locale;
use crate::tournament::db::{self, Tournament};
use crate::tournament::registration::BindingAction;
use crate::tournament::{registration, seeding};
use sqlx::SqlitePool;

/// Whether an admin may still compose the field.
///
/// Open through `checkin` as well as `registration`, because an invitee is
/// exempt from the sweep and so adding one late costs nothing. It closes at
/// `seeding`: from there the order is being finalized and the bracket drawn from
/// it, so a late addition belongs behind `/tournament reopen-registration`
/// rather than sliding into a field somebody has already looked at.
pub(crate) fn may_invite(status: &str) -> bool {
    matches!(status, "registration" | "checkin")
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum InviteOutcome {
    Invited {
        display_name: String,
        seed: Option<i64>,
        /// Whoever held that seat before this pin took it, if anyone.
        displaced: Option<String>,
        /// Only ever `Some` alongside a fresh claim; reusing an existing
        /// binding carries none rather than an extra fetch just to show one.
        elo: Option<i64>,
    },
    /// The same person invited again: a corrected profile, or an uninvited
    /// entry brought back. One verb covers both, so neither needs a second one.
    Reinvited {
        display_name: String,
        seed: Option<i64>,
        displaced: Option<String>,
        elo: Option<i64>,
    },
    /// They signed themselves up, so this is not an invite to withdraw on the
    /// organizers' behalf. Whether an admin may remove such an entry at all is
    /// deliberately left open.
    AlreadySelfRegistered {
        display_name: String,
    },
    /// This Discord account already carries a *different* real binding than
    /// the profile just picked. Refused outright — never overridden, and never
    /// silently kept either, since an admin acting on it should know why the
    /// name in front of them differs from what they searched.
    AlreadyBoundToDifferentProfile {
        display_name: String,
    },
    /// The picked profile is someone else's entirely, unrelated to this
    /// Discord account.
    ProfileClaimedByAnother {
        other_user_id: i64,
        other_display_name: String,
    },
    LookupFailed,
    FieldFull {
        cap: i64,
    },
    SeedOutOfRange {
        cap: i64,
    },
    InvitesClosed {
        current_status: String,
    },
}

impl InviteOutcome {
    pub(crate) fn message(&self, tournament_name: &str, locale: Locale) -> String {
        match self {
            InviteOutcome::Invited {
                display_name,
                seed,
                displaced,
                elo,
            } => invited_message(
                display_name,
                *seed,
                displaced.as_deref(),
                *elo,
                tournament_name,
                locale,
                false,
            ),
            InviteOutcome::Reinvited {
                display_name,
                seed,
                displaced,
                elo,
            } => invited_message(
                display_name,
                *seed,
                displaced.as_deref(),
                *elo,
                tournament_name,
                locale,
                true,
            ),
            InviteOutcome::AlreadySelfRegistered { display_name } => locale.pick(
                format!("**{display_name}** 是自己報名的，不是受邀參賽，因此無法用邀請指令調整。"),
                format!(
                    "**{display_name}** signed up on their own rather than being invited, so the invite commands \
                     don't apply to them."
                ),
            ),
            InviteOutcome::AlreadyBoundToDifferentProfile { display_name } => locale.pick(
                format!(
                    "這個 Discord 帳號已經連結到 **{display_name}**，邀請指令無法變更綁定。\
                     請對方自行使用 `/tournament rebind`。"
                ),
                format!(
                    "This Discord account is already linked to **{display_name}** — the invite commands can't \
                     change that. Have them run `/tournament rebind` themselves."
                ),
            ),
            InviteOutcome::ProfileClaimedByAnother {
                other_user_id,
                other_display_name,
            } => locale.pick(
                format!(
                    "這個 aoe4 帳號已經綁定給 <@{other_user_id}>（**{other_display_name}**）。如果選錯了請確認一下。"
                ),
                format!(
                    "That aoe4 profile is already registered to <@{other_user_id}> (**{other_display_name}**). \
                     Double-check the pick if this was a mistake."
                ),
            ),
            InviteOutcome::LookupFailed => locale.pick(
                "找不到這個遊戲帳號 — 請重新搜尋並選擇正確的帳號。".to_string(),
                "Couldn't find that game account — search again and pick the right one.".to_string(),
            ),
            InviteOutcome::FieldFull { cap } => locale.pick(
                format!("**{tournament_name}** 的名額已滿（上限 {cap} 人），無法再邀請。"),
                format!("**{tournament_name}** is full ({cap} entrants), so there's no place to invite them into."),
            ),
            InviteOutcome::SeedOutOfRange { cap } => locale.pick(
                format!("種子序必須介於 1 到 {cap} 之間。"),
                format!("The seed must be between 1 and {cap}."),
            ),
            InviteOutcome::InvitesClosed { current_status } => locale.pick(
                format!(
                    "**{tournament_name}** 已進入 {current_status} 階段，無法再邀請。\
                     需要加人請先用 `/tournament reopen-registration`。"
                ),
                format!(
                    "**{tournament_name}** is past inviting (currently {current_status}). Use \
                     `/tournament reopen-registration` first if someone still needs adding."
                ),
            ),
        }
    }

    /// Whether the field changed — the caller's signal for whether the roster
    /// panel, the seeding panel and the bracket preview all need redrawing.
    /// Unconditional on whether a seed was placed: a new or corrected entrant
    /// is itself something the seeding panel has to show.
    pub(crate) fn changed_state(&self) -> bool {
        matches!(self, InviteOutcome::Invited { .. } | InviteOutcome::Reinvited { .. })
    }
}

/// Shared by `Invited` and `Reinvited`, which differ only in whether this is a
/// first invite or a correction — every successful invite is linked, so there
/// is only the one wording per.
fn invited_message(
    display_name: &str,
    seed: Option<i64>,
    displaced: Option<&str>,
    elo: Option<i64>,
    tournament_name: &str,
    locale: Locale,
    reinvited: bool,
) -> String {
    let seed = seed_clause(seed, locale);
    let displaced = displaced
        .map(|name| {
            locale.pick(
                format!("，讓 **{name}** 讓出該種子"),
                format!(", displacing **{name}**"),
            )
        })
        .unwrap_or_default();
    let elo_suffix = elo.map(|e| format!(" (ELO {e})")).unwrap_or_default();
    if reinvited {
        locale.pick(
            format!(
                "已更新 **{display_name}**{elo_suffix} 在 **{tournament_name}** 的邀請{seed}{displaced}，已連結其\
                 真實遊戲帳號。"
            ),
            format!(
                "Updated **{display_name}**{elo_suffix}'s invitation to **{tournament_name}**{seed}{displaced}, \
                 linked to their real profile."
            ),
        )
    } else {
        locale.pick(
            format!(
                "已將 **{display_name}**{elo_suffix} 加入 **{tournament_name}**{seed}{displaced}，已連結其真實遊戲\
                 帳號。"
            ),
            format!(
                "Added **{display_name}**{elo_suffix} to **{tournament_name}**{seed}{displaced}, linked to their \
                 real profile."
            ),
        )
    }
}

fn seed_clause(seed: Option<i64>, locale: Locale) -> String {
    seed.map(|seed| locale.pick(format!("，種子序 {seed}"), format!(" at seed {seed}")))
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UninviteOutcome {
    Uninvited { display_name: String },
    NotInField,
    NotInvited { display_name: String },
    AlreadyOut { display_name: String },
    InvitesClosed { current_status: String },
}

impl UninviteOutcome {
    pub(crate) fn message(&self, tournament_name: &str, locale: Locale) -> String {
        match self {
            UninviteOutcome::Uninvited { display_name } => locale.pick(
                format!("已將 **{display_name}** 移出 **{tournament_name}**，其餘種子序已重新排定。"),
                format!("Removed **{display_name}** from **{tournament_name}**; the remaining seeds were renumbered."),
            ),
            UninviteOutcome::NotInField => locale.pick(
                format!("這位成員並不在 **{tournament_name}** 的參賽名單中。"),
                format!("That member isn't in **{tournament_name}**'s field."),
            ),
            UninviteOutcome::NotInvited { display_name } => locale.pick(
                format!("**{display_name}** 是自己報名的，不是受邀參賽 — 請由本人使用 `/tournament withdraw`。"),
                format!(
                    "**{display_name}** signed up on their own rather than being invited — they withdraw \
                     themselves with `/tournament withdraw`."
                ),
            ),
            UninviteOutcome::AlreadyOut { display_name } => locale.pick(
                format!("**{display_name}** 原本便已不在參賽名單中。"),
                format!("**{display_name}** is already out of the field."),
            ),
            UninviteOutcome::InvitesClosed { current_status } => locale.pick(
                format!("**{tournament_name}** 已進入 {current_status} 階段，無法再調整邀請名單。"),
                format!("**{tournament_name}** is past inviting (currently {current_status})."),
            ),
        }
    }

    pub(crate) fn changed_state(&self) -> bool {
        matches!(self, UninviteOutcome::Uninvited { .. })
    }
}

/// `profile` is the aoe4world profile id picked through the autocomplete;
/// there is no "leave it blank" path — a manual invite means the admin
/// already knows who they are inviting. Every branch resolves a real name
/// from the binding itself, so unlike `register` this needs no Discord name
/// passed in as a fallback.
pub(crate) async fn invite(
    pool: &SqlitePool,
    tournament: &Tournament,
    user_id: i64,
    profile: i64,
    invited_by: i64,
    seed: Option<i64>,
) -> Result<InviteOutcome, sqlx::Error> {
    if !may_invite(&tournament.status) {
        return Ok(InviteOutcome::InvitesClosed {
            current_status: tournament.status.clone(),
        });
    }

    let existing = db::get_entry(pool, tournament.id, user_id).await?;
    if let Some(entry) = &existing
        && entry.invited_by.is_none()
    {
        return Ok(InviteOutcome::AlreadySelfRegistered {
            display_name: entry.display_name.clone(),
        });
    }

    // Re-inviting someone already in the field corrects their name; it does not
    // take a second place. Reviving an uninvited entry does.
    let takes_a_place = existing.as_ref().is_none_or(|e| e.status != "active");
    if takes_a_place && registration::field_is_full(pool, tournament).await? {
        return Ok(InviteOutcome::FieldFull {
            cap: tournament.entrant_cap,
        });
    }

    // The seat range is the event's own size, not the field composed so far —
    // an invite-only bracket preview already draws every seat up to the cap.
    // Checked before any write: an invite that half-happened and then reported
    // a bad seed would leave an admin guessing which half.
    if let Some(seed) = seed
        && (seed < 1 || seed > tournament.entrant_cap)
    {
        return Ok(InviteOutcome::SeedOutOfRange {
            cap: tournament.entrant_cap,
        });
    }

    let player = db::get_player(pool, user_id).await?;
    let (display_name, aoe4_id, fresh_elo) =
        match registration::binding_action(player.as_ref().map(|p| p.aoe4_id), Some(profile)) {
            BindingAction::RefuseDifferent => {
                return Ok(InviteOutcome::AlreadyBoundToDifferentProfile {
                    display_name: player.expect("bound implies a player row").display_name,
                });
            },
            // `profile` is always supplied, so this is only ever reached when
            // it matches what is already bound — reuse it, no fetch.
            BindingAction::Reenter => {
                let player = player.expect("Reenter with a profile supplied implies a bound player row");
                (player.display_name, profile, None)
            },
            // A genuinely new binding. `claim_profile` creates the player row
            // itself if one is not there yet — exactly the case for a
            // first-time invitee.
            BindingAction::ClaimProfile(given) => match registration::claim_profile(pool, user_id, given).await? {
                registration::Claim::Resolved { display_name, elo } => (display_name, given, elo),
                registration::Claim::ClaimedByAnother {
                    other_user_id,
                    other_display_name,
                } => {
                    return Ok(InviteOutcome::ProfileClaimedByAnother {
                        other_user_id,
                        other_display_name,
                    });
                },
                registration::Claim::LookupFailed => return Ok(InviteOutcome::LookupFailed),
            },
        };

    db::upsert_invited_entry(pool, tournament.id, user_id, aoe4_id, &display_name, invited_by).await?;
    if let Some(elo) = fresh_elo {
        db::set_entry_elo(pool, tournament.id, user_id, elo).await?;
    }

    let displaced = if let Some(seed) = seed {
        // A pin on the seat, not a direct `seed` write: `resolved_order` is what
        // turns every pin in the field into the whole 1..n order, so
        // `unique (tournament_id, seed)` is never contended and the result is
        // contiguous. `also_suggested: false` — this is the organizers'
        // placement, not the tiering's proposal.
        let displaced_by = db::set_manual_seed(pool, tournament.id, user_id, seed).await?;
        let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
        db::set_seed_order(pool, tournament.id, &seeding::resolved_order(&entries), false).await?;
        // Without this the placement is destroyed by the seeding pass at
        // close-checkin, silently — the bug chunk 30 exists to have fixed.
        db::set_seed_source(pool, tournament.id, seeding::SeedPolicy::KeepManual.as_source()).await?;
        displaced_by.and_then(|uid| {
            entries
                .iter()
                .find(|e| e.user_id == uid)
                .map(|e| e.display_name.clone())
        })
    } else {
        None
    };

    Ok(if existing.is_some() {
        InviteOutcome::Reinvited {
            display_name,
            seed,
            displaced,
            elo: fresh_elo,
        }
    } else {
        InviteOutcome::Invited {
            display_name,
            seed,
            displaced,
            elo: fresh_elo,
        }
    })
}

pub(crate) async fn uninvite(
    pool: &SqlitePool,
    tournament: &Tournament,
    user_id: i64,
) -> Result<UninviteOutcome, sqlx::Error> {
    if !may_invite(&tournament.status) {
        return Ok(UninviteOutcome::InvitesClosed {
            current_status: tournament.status.clone(),
        });
    }
    let Some(entry) = db::get_entry(pool, tournament.id, user_id).await? else {
        return Ok(UninviteOutcome::NotInField);
    };
    if entry.invited_by.is_none() {
        return Ok(UninviteOutcome::NotInvited {
            display_name: entry.display_name,
        });
    }
    if entry.status == "withdrawn" {
        return Ok(UninviteOutcome::AlreadyOut {
            display_name: entry.display_name,
        });
    }

    // Withdrawn rather than deleted: entries are never removed, and every table
    // downstream references the row.
    db::update_entry_status(pool, tournament.id, user_id, "withdrawn").await?;

    // Removing someone from the middle of a seeded field leaves a gap that
    // `start` refuses; `resolved_order` recomputes the whole field and closes
    // it. Their own pin, if any, simply drops out along with them — it is
    // still in the column, ready to reclaim their seat if they are reinvited.
    // A field nobody has seeded yet is left alone — writing an order into it
    // would invent one.
    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
    if entries.iter().any(|e| e.seed.is_some()) {
        db::set_seed_order(pool, tournament.id, &seeding::resolved_order(&entries), false).await?;
    }

    Ok(UninviteOutcome::Uninvited {
        display_name: entry.display_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inviting_is_open_while_the_field_is_being_composed() {
        assert!(may_invite("registration"));
        // Still open during check-in: an invitee is exempt from the sweep, so a
        // late addition changes nothing about how check-in closes.
        assert!(may_invite("checkin"));
    }

    #[test]
    fn inviting_closes_once_the_order_is_being_finalized() {
        for status in ["seeding", "running", "completed", "canceled"] {
            assert!(!may_invite(status), "{status} should be past inviting");
        }
    }

    #[test]
    fn the_seed_clause_appears_only_when_a_seed_was_given() {
        for locale in [Locale::ZhTw, Locale::En] {
            assert_eq!(seed_clause(None, locale), "");
            assert!(seed_clause(Some(3), locale).contains('3'));
        }
    }

    #[test]
    fn an_invite_says_it_is_linked_and_carries_the_elo() {
        let outcome = InviteOutcome::Invited {
            display_name: "Beasty".to_string(),
            seed: Some(2),
            displaced: None,
            elo: Some(1800),
        };
        let zh = outcome.message("Relic Cup", Locale::ZhTw);
        let en = outcome.message("Relic Cup", Locale::En);
        assert_ne!(zh, en);
        assert!(zh.contains("已將") && zh.contains("已連結其真實遊戲帳號"), "{zh}");
        assert!(
            en.contains("Added") && en.contains("linked to their real profile"),
            "{en}"
        );
        // Names and numbers are data, not text — they survive both.
        for message in [&zh, &en] {
            assert!(
                message.contains("Beasty")
                    && message.contains("Relic Cup")
                    && message.contains('2')
                    && message.contains("1800")
            );
        }
    }

    #[test]
    fn a_pin_that_displaces_someone_names_them() {
        let outcome = InviteOutcome::Invited {
            display_name: "Beasty".to_string(),
            seed: Some(2),
            displaced: Some("TheViper".to_string()),
            elo: None,
        };
        let zh = outcome.message("Relic Cup", Locale::ZhTw);
        let en = outcome.message("Relic Cup", Locale::En);
        assert!(zh.contains("TheViper"), "{zh}");
        assert!(en.contains("TheViper") && en.contains("displacing"), "{en}");
    }

    #[test]
    fn a_seed_past_the_cap_names_the_cap() {
        for locale in [Locale::ZhTw, Locale::En] {
            let message = InviteOutcome::SeedOutOfRange { cap: 8 }.message("Relic Cup", locale);
            assert!(message.contains('8'), "{message}");
        }
    }

    #[test]
    fn a_reinvite_says_updated_rather_than_added() {
        let outcome = InviteOutcome::Reinvited {
            display_name: "Beasty".to_string(),
            seed: None,
            displaced: None,
            elo: None,
        };
        let zh = outcome.message("Relic Cup", Locale::ZhTw);
        let en = outcome.message("Relic Cup", Locale::En);
        assert!(zh.contains("已更新"), "{zh}");
        assert!(en.contains("Updated"), "{en}");
    }

    #[test]
    fn the_conflict_refusal_points_only_at_rebind() {
        // There is no "omit the argument" escape any more — the profile is
        // mandatory, so `/tournament rebind` is the one way out.
        for locale in [Locale::ZhTw, Locale::En] {
            let message = InviteOutcome::AlreadyBoundToDifferentProfile {
                display_name: "RealName".to_string(),
            }
            .message("Relic Cup", locale);
            assert!(message.contains("RealName"), "{message}");
            assert!(message.contains("/tournament rebind"), "{message}");
        }
    }

    #[test]
    fn refusing_a_self_registered_entry_points_at_withdraw_not_uninvite() {
        // The person can leave; an admin just cannot do it for them through this
        // door. Saying so beats a bare refusal.
        for locale in [Locale::ZhTw, Locale::En] {
            let message = UninviteOutcome::NotInvited {
                display_name: "Wam01".to_string(),
            }
            .message("Relic Cup", locale);
            assert!(message.contains("/tournament withdraw"), "{message}");
        }
    }

    #[test]
    fn only_a_completed_invite_or_removal_redraws_the_panels() {
        assert!(
            InviteOutcome::Invited {
                display_name: "A".to_string(),
                seed: None,
                displaced: None,
                elo: None,
            }
            .changed_state()
        );
        assert!(
            InviteOutcome::Reinvited {
                display_name: "A".to_string(),
                seed: None,
                displaced: None,
                elo: None,
            }
            .changed_state()
        );
        assert!(!InviteOutcome::LookupFailed.changed_state());
        assert!(!InviteOutcome::FieldFull { cap: 8 }.changed_state());
        assert!(
            UninviteOutcome::Uninvited {
                display_name: "A".to_string()
            }
            .changed_state()
        );
        assert!(!UninviteOutcome::NotInField.changed_state());
    }
}
