//! The organizers' own door into a field: `/tournament invite` puts a Discord
//! member straight in, with a name the admin supplies and no aoe4world profile
//! behind it, and `/tournament uninvite` takes them back out.
//!
//! Shaped like `registration.rs` — pure decisions plus thin database writes, no
//! Discord and no HTTP — so every branch is testable without either. The cap and
//! the seed order are deliberately reused rather than reimplemented: an invited
//! entrant occupies a place and a seed exactly like a self-registered one.

use crate::locale::Locale;
use crate::tournament::db::{self, Tournament};
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
    },
    /// The same person invited again: a corrected name, or an uninvited entry
    /// brought back. One verb covers both, so a typo needs no second one.
    Reinvited {
        display_name: String,
        seed: Option<i64>,
    },
    /// They signed themselves up, so this is not an invite to withdraw on the
    /// organizers' behalf. Whether an admin may remove such an entry at all is
    /// deliberately left open.
    AlreadySelfRegistered {
        display_name: String,
    },
    FieldFull {
        cap: i64,
    },
    SeedOutOfRange {
        field_size: i64,
    },
    NameRequired,
    InvitesClosed {
        current_status: String,
    },
}

impl InviteOutcome {
    pub(crate) fn message(&self, tournament_name: &str, locale: Locale) -> String {
        match self {
            InviteOutcome::Invited { display_name, seed } => locale.pick(
                format!(
                    "已將 **{display_name}** 加入 **{tournament_name}**{}。對方不需要報名或連結遊戲帳號。",
                    seed_clause(*seed, locale)
                ),
                format!(
                    "Added **{display_name}** to **{tournament_name}**{}. They don't need to sign up or link a \
                     game account.",
                    seed_clause(*seed, locale)
                ),
            ),
            InviteOutcome::Reinvited { display_name, seed } => locale.pick(
                format!(
                    "已更新 **{display_name}** 在 **{tournament_name}** 的邀請{}。",
                    seed_clause(*seed, locale)
                ),
                format!(
                    "Updated **{display_name}**'s invitation to **{tournament_name}**{}.",
                    seed_clause(*seed, locale)
                ),
            ),
            InviteOutcome::AlreadySelfRegistered { display_name } => locale.pick(
                format!("**{display_name}** 是自己報名的，不是受邀參賽，因此無法用邀請指令調整。"),
                format!(
                    "**{display_name}** signed up on their own rather than being invited, so the invite commands \
                     don't apply to them."
                ),
            ),
            InviteOutcome::FieldFull { cap } => locale.pick(
                format!("**{tournament_name}** 的名額已滿（上限 {cap} 人），無法再邀請。"),
                format!("**{tournament_name}** is full ({cap} entrants), so there's no place to invite them into."),
            ),
            InviteOutcome::SeedOutOfRange { field_size } => locale.pick(
                format!("種子序必須介於 1 到 {field_size} 之間。"),
                format!("The seed must be between 1 and {field_size}."),
            ),
            InviteOutcome::NameRequired => locale.pick(
                "請輸入對方的遊戲名稱 — 這是對手在遊戲大廳中要搜尋的名稱。".to_string(),
                "Give their in-game name — it's what their opponent searches for in the lobby browser.".to_string(),
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
    /// panel and the bracket preview need redrawing.
    pub(crate) fn changed_state(&self) -> bool {
        matches!(self, InviteOutcome::Invited { .. } | InviteOutcome::Reinvited { .. })
    }

    /// Whether the seeding changed too, which the seeding panel follows.
    pub(crate) fn changed_seeding(&self) -> bool {
        matches!(
            self,
            InviteOutcome::Invited { seed: Some(_), .. } | InviteOutcome::Reinvited { seed: Some(_), .. }
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

pub(crate) async fn invite(
    pool: &SqlitePool,
    tournament: &Tournament,
    user_id: i64,
    in_game_name: &str,
    invited_by: i64,
    seed: Option<i64>,
) -> Result<InviteOutcome, sqlx::Error> {
    if !may_invite(&tournament.status) {
        return Ok(InviteOutcome::InvitesClosed {
            current_status: tournament.status.clone(),
        });
    }
    let name = in_game_name.trim();
    if name.is_empty() {
        return Ok(InviteOutcome::NameRequired);
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

    // Checked against the field this invite is about to produce, and before any
    // write: an invite that half-happened and then reported a bad seed would
    // leave an admin guessing which half.
    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
    let field_size =
        i64::try_from(seeding::seedable(&entries).len()).unwrap_or(i64::MAX) + i64::from(u8::from(takes_a_place));
    if let Some(seed) = seed
        && (seed < 1 || seed > field_size)
    {
        return Ok(InviteOutcome::SeedOutOfRange { field_size });
    }

    db::invite_player_and_entry(pool, tournament.id, user_id, name, invited_by).await?;

    if let Some(seed) = seed {
        // Through `reorder`, never a direct `seed` write: the order is rewritten
        // whole, so `unique (tournament_id, seed)` is never contended and the
        // result is contiguous. `also_suggested: false` — this is the organizers'
        // placement, not the tiering's proposal.
        let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
        let order = seeding::reorder(&seeding::manual_order(&entries), user_id, seed);
        db::set_seed_order(pool, tournament.id, &order, false).await?;
        // Without this the placement is destroyed by the seeding pass at
        // close-checkin, silently — the bug chunk 30 exists to have fixed.
        db::set_seed_source(pool, tournament.id, seeding::SeedPolicy::KeepManual.as_source()).await?;
    }

    let display_name = name.to_string();
    Ok(if existing.is_some() {
        InviteOutcome::Reinvited { display_name, seed }
    } else {
        InviteOutcome::Invited { display_name, seed }
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
    // `start` refuses, so the order is compacted here. A field nobody has seeded
    // yet is left alone — writing an order into it would invent one.
    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
    if entries.iter().any(|e| e.seed.is_some()) {
        db::set_seed_order(pool, tournament.id, &seeding::manual_order(&entries), false).await?;
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
    fn messages_render_in_both_locales() {
        let outcome = InviteOutcome::Invited {
            display_name: "Beasty".to_string(),
            seed: Some(2),
        };
        let zh = outcome.message("Relic Cup", Locale::ZhTw);
        let en = outcome.message("Relic Cup", Locale::En);
        assert_ne!(zh, en);
        assert!(zh.contains("已將"), "{zh}");
        assert!(en.contains("Added"), "{en}");
        // Names and numbers are data, not text — they survive both.
        for message in [&zh, &en] {
            assert!(message.contains("Beasty") && message.contains("Relic Cup") && message.contains('2'));
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
                seed: None
            }
            .changed_state()
        );
        assert!(
            InviteOutcome::Reinvited {
                display_name: "A".to_string(),
                seed: None
            }
            .changed_state()
        );
        assert!(!InviteOutcome::NameRequired.changed_state());
        assert!(!InviteOutcome::FieldFull { cap: 8 }.changed_state());
        assert!(
            UninviteOutcome::Uninvited {
                display_name: "A".to_string()
            }
            .changed_state()
        );
        assert!(!UninviteOutcome::NotInField.changed_state());
    }

    #[test]
    fn only_a_placed_invite_redraws_the_seeding_panel() {
        assert!(
            InviteOutcome::Invited {
                display_name: "A".to_string(),
                seed: Some(1)
            }
            .changed_seeding()
        );
        // No seed given means the order was not touched, so the panel showing it
        // has nothing new to say.
        assert!(
            !InviteOutcome::Invited {
                display_name: "A".to_string(),
                seed: None
            }
            .changed_seeding()
        );
    }
}
