//! The registration panel (docs/tournament.md §8.5): a persistent message in
//! `#{slug}-register`, edited in place as entrants come and go. `render` is the
//! pure part — content plus the two buttons — golden-string tested here with no
//! Discord involved; `post_initial` and `refresh` are the thin Discord/DB glue
//! that `commands::create` and `tournament::registration`'s callers use.

use crate::Error;
use crate::tournament::action::Action;
use crate::tournament::db::{self, Tournament, TournamentEntry};
use crate::tournament::throttle::EditThrottle;
use chrono::{DateTime, Utc};
use serenity::all::{
    ButtonStyle, CacheHttp, ChannelId, CreateActionRow, CreateButton, CreateMessage, EditMessage, MessageId,
};
use sqlx::SqlitePool;
use std::time::{Duration, Instant};

/// "Edits must be throttled" (§8.5) — coalesce to at most one edit every few
/// seconds, shared between the slash-command and button paths via one
/// `Arc<EditThrottle>` (see `main.rs`).
pub(crate) const PANEL_EDIT_MIN_INTERVAL: Duration = Duration::from_secs(3);

/// Names shown before the roster is truncated with "... and N more" — mirrors the
/// existing `.take(10)` cap on `commands::auto_complete_id`'s results.
const ROSTER_DISPLAY_CAP: usize = 10;

/// Pure. Filters `entries` down to `active` internally — a withdrawn entry's row
/// persists (§4), but it must never show up in the roster, and callers should not
/// have to remember to filter it out themselves.
pub(crate) fn render(
    tournament_id: i64,
    name: &str,
    entries: &[TournamentEntry],
    entrant_cap: i64,
    scheduled_start_at: Option<DateTime<Utc>>,
) -> (String, Vec<CreateActionRow>) {
    let active: Vec<&TournamentEntry> = entries.iter().filter(|e| e.status == "active").collect();

    let roster = if active.is_empty() {
        "*還沒有人報名。 / No one has registered yet.*".to_string()
    } else {
        let mut names: Vec<&str> = active.iter().map(|e| e.display_name.as_str()).collect();
        if names.len() > ROSTER_DISPLAY_CAP {
            let remaining = names.len() - ROSTER_DISPLAY_CAP;
            names.truncate(ROSTER_DISPLAY_CAP);
            format!("{} · …等 {remaining} 人 / and {remaining} more", names.join(" · "))
        } else {
            names.join(" · ")
        }
    };

    // Shown so entrants can see the field filling up — registration refuses a
    // sign-up past the cap (§8.3), which is confusing without a visible count.
    let starts = scheduled_start_at.map_or_else(String::new, |at| {
        format!("開賽 / Starts <t:{0}:F> (<t:{0}:R>)\n\n", at.timestamp())
    });

    // No round/best_of line here (unlike the design doc's mock) — rounds don't
    // exist until chunk 12 generates the bracket, so there's nothing true to say
    // about format yet. "Single elimination" itself is a fixed design decision
    // (§1), not per-round data, so it stays.
    //
    // Bilingual rather than per-reader (§8.10): one message, many readers, and it
    // re-renders on every button press — picking any one of their languages would
    // make it flip. Only the chrome doubles; the roster appears once.
    let content = format!(
        "**{name} — 報名進行中 / registration is OPEN**\n\
         單淘汰 · 開賽前需簽到\n\
         Single elimination · check-in required before start\n\
         第一次報名？請用 `/tournament register` 並輸入你的遊戲名稱。\n\
         First time? Use `/tournament register` and type your in-game name.\n\n\
         {starts}**已報名 / Registered ({}/{entrant_cap})**\n{roster}",
        active.len()
    );

    let components = vec![CreateActionRow::Buttons(vec![
        CreateButton::new(Action::Register.custom_id(tournament_id))
            .label("報名 / Register")
            .style(ButtonStyle::Primary),
        CreateButton::new(Action::Withdraw.custom_id(tournament_id))
            .label("退賽 / Withdraw")
            .style(ButtonStyle::Danger),
    ])];

    (content, components)
}

/// Posts the panel with an empty roster — called once, from `commands::create`,
/// since a tournament starts in `registration` status immediately with no
/// separate "open registration" command.
pub(crate) async fn post_initial(
    http: impl CacheHttp,
    channel_id: ChannelId,
    tournament_id: i64,
    name: &str,
    entrant_cap: i64,
) -> Result<MessageId, Error> {
    // No start time yet: `create` runs before `/tournament setup` can.
    let (content, components) = render(tournament_id, name, &[], entrant_cap, None);
    let message = channel_id
        .send_message(http, CreateMessage::new().content(content).components(components))
        .await?;
    Ok(message.id)
}

/// Re-fetches entries and, if the panel exists and the throttle allows it, edits
/// it in place. A no-op if `register_message_id` is unset (panel never posted).
/// Callers should only invoke this after an outcome that actually changed the
/// entry set (see `RegisterOutcome::changed_state` / `WithdrawOutcome::changed_state`)
/// — an idempotent press doesn't need a fresh edit.
pub(crate) async fn refresh(
    http: impl CacheHttp,
    pool: &SqlitePool,
    throttle: &EditThrottle,
    tournament: &Tournament,
) -> Result<(), Error> {
    let (Some(register_message_id), Some(register_channel_id)) =
        (tournament.register_message_id, tournament.register_channel_id)
    else {
        return Ok(());
    };

    let message_id = MessageId::new(u64::try_from(register_message_id).unwrap());
    if !throttle.try_begin_edit(message_id, Instant::now()) {
        return Ok(());
    }

    edit(http, pool, register_channel_id, message_id, tournament).await
}

/// The unconditional edit on a phase change — bypasses the throttle, since this
/// fires once per admin command rather than once per button press. Used by
/// `/tournament reopen-registration`, which restores no-shows to the roster and
/// so must not have its re-render coalesced away.
pub(crate) async fn refresh_now(http: impl CacheHttp, pool: &SqlitePool, tournament: &Tournament) -> Result<(), Error> {
    let (Some(register_message_id), Some(register_channel_id)) =
        (tournament.register_message_id, tournament.register_channel_id)
    else {
        return Ok(());
    };

    let message_id = MessageId::new(u64::try_from(register_message_id).unwrap());
    edit(http, pool, register_channel_id, message_id, tournament).await
}

async fn edit(
    http: impl CacheHttp,
    pool: &SqlitePool,
    register_channel_id: i64,
    message_id: MessageId,
    tournament: &Tournament,
) -> Result<(), Error> {
    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
    let (content, components) = render(
        tournament.id,
        &tournament.name,
        &entries,
        tournament.entrant_cap,
        tournament.scheduled_start_at,
    );

    let channel_id = ChannelId::new(u64::try_from(register_channel_id).unwrap());
    channel_id
        .edit_message(
            http,
            message_id,
            EditMessage::new().content(content).components(components),
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn entry(user_id: i64, display_name: &str, status: &str) -> TournamentEntry {
        TournamentEntry {
            tournament_id: 1,
            user_id,
            aoe4_id: user_id,
            seed: None,
            suggested_seed: None,
            display_name: display_name.to_string(),
            elo: None,
            atr: None,
            atr_source: None,
            status: status.to_string(),
            registered_at: Utc::now(),
            checked_in_at: None,
        }
    }

    #[test]
    fn the_panel_is_bilingual_rather_than_picking_a_reader() {
        // §8.10: a shared message re-rendered by whoever presses a button, so it
        // carries both languages instead of flipping between them.
        let (content, _) = render(1, "Relic Cup", &[entry(1, "MarineLorD", "active")], 32, None);
        assert!(content.contains("報名進行中"));
        assert!(content.contains("registration is OPEN"));
        assert!(content.contains("已報名 / Registered (1/32)"));
    }

    #[test]
    fn shows_the_cap_and_the_start_time_when_one_is_set() {
        // The cap is only fair if entrants can watch the field fill up — a
        // sign-up past it is refused (§8.3).
        let at = Utc::now();
        let (content, _) = render(1, "Relic Cup", &[entry(1, "A", "active")], 8, Some(at));
        assert!(content.contains("Registered (1/8)"), "{content}");
        assert!(content.contains(&format!("<t:{}:F>", at.timestamp())), "{content}");
    }

    #[test]
    fn omits_the_start_line_entirely_when_unscheduled() {
        let (content, _) = render(1, "Relic Cup", &[], 32, None);
        assert!(!content.contains("Starts"), "{content}");
    }

    #[test]
    fn tells_first_timers_what_to_do_before_they_press_the_button() {
        // The Register button cannot serve a first-timer — it carries no name —
        // so the panel has to say so up front rather than let them hit the
        // refusal and go hunting for a command.
        let (content, _) = render(1, "Relic Cup", &[], 32, None);
        assert!(content.contains("第一次報名？"), "{content}");
        assert!(content.contains("First time?"), "{content}");
        assert!(content.contains("/tournament register"));
    }

    #[test]
    fn renders_a_placeholder_when_nobody_has_registered() {
        let (content, _) = render(1, "Relic Cup", &[], 32, None);
        assert!(content.contains("Registered (0/32)"));
        assert!(content.contains("還沒有人報名。 / No one has registered yet."));
    }

    #[test]
    fn renders_active_entrants_and_excludes_withdrawn_ones() {
        let entries = vec![
            entry(1, "MarineLorD", "active"),
            entry(2, "Beasty", "withdrawn"),
            entry(3, "Anotand", "active"),
        ];
        let (content, _) = render(1, "Relic Cup", &entries, 32, None);
        assert!(content.contains("Registered (2/32)"));
        assert!(content.contains("MarineLorD"));
        assert!(content.contains("Anotand"));
        assert!(!content.contains("Beasty"));
    }

    #[test]
    fn truncates_the_roster_beyond_the_display_cap() {
        let entries: Vec<TournamentEntry> = (1..=12).map(|i| entry(i, &format!("Player{i}"), "active")).collect();
        let (content, _) = render(1, "Relic Cup", &entries, 32, None);
        assert!(content.contains("Registered (12/32)"));
        assert!(content.contains("…等 2 人 / and 2 more"));
        assert!(!content.contains("Player11"));
    }

    #[test]
    fn buttons_carry_the_tournament_id_in_their_custom_id() {
        let (_, components) = render(42, "Relic Cup", &[], 32, None);
        let CreateActionRow::Buttons(buttons) = &components[0] else {
            panic!("expected a button row");
        };
        assert_eq!(buttons.len(), 2);
        assert_eq!(
            buttons[0],
            CreateButton::new(Action::Register.custom_id(42))
                .label("報名 / Register")
                .style(ButtonStyle::Primary)
        );
        assert_eq!(
            buttons[1],
            CreateButton::new(Action::Withdraw.custom_id(42))
                .label("退賽 / Withdraw")
                .style(ButtonStyle::Danger)
        );
    }
}
