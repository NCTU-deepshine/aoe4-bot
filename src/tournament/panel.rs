//! The registration panel (docs/tournament.md §8.5): a persistent message in
//! `#{slug}-register`, edited in place as entrants come and go. `render` is the
//! pure part — content plus the two buttons — golden-string tested here with no
//! Discord involved; `post_initial` and `refresh` are the thin Discord/DB glue
//! that `commands::create` and `tournament::registration`'s callers use.

use crate::Error;
use crate::tournament::action::Action;
use crate::tournament::db::{self, Tournament, TournamentEntry};
use crate::tournament::throttle::EditThrottle;
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
pub(crate) fn render(tournament_id: i64, name: &str, entries: &[TournamentEntry]) -> (String, Vec<CreateActionRow>) {
    let active: Vec<&TournamentEntry> = entries.iter().filter(|e| e.status == "active").collect();

    let roster = if active.is_empty() {
        "*No one has registered yet.*".to_string()
    } else {
        let mut names: Vec<&str> = active.iter().map(|e| e.display_name.as_str()).collect();
        if names.len() > ROSTER_DISPLAY_CAP {
            let remaining = names.len() - ROSTER_DISPLAY_CAP;
            names.truncate(ROSTER_DISPLAY_CAP);
            format!("{} · … and {remaining} more", names.join(" · "))
        } else {
            names.join(" · ")
        }
    };

    // No round/best_of line here (unlike the design doc's mock) — rounds don't
    // exist until chunk 12 generates the bracket, so there's nothing true to say
    // about format yet. "Single elimination" itself is a fixed design decision
    // (§1), not per-round data, so it stays.
    let content = format!(
        "**{name} — registration is OPEN**\nSingle elimination · check-in required before start\n\n\
         **Registered ({})**\n{roster}",
        active.len()
    );

    let components = vec![CreateActionRow::Buttons(vec![
        CreateButton::new(Action::Register.custom_id(tournament_id))
            .label("Register")
            .style(ButtonStyle::Primary),
        CreateButton::new(Action::Withdraw.custom_id(tournament_id))
            .label("Withdraw")
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
) -> Result<MessageId, Error> {
    let (content, components) = render(tournament_id, name, &[]);
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
    let (content, components) = render(tournament.id, &tournament.name, &entries);

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
    fn renders_a_placeholder_when_nobody_has_registered() {
        let (content, _) = render(1, "Relic Cup", &[]);
        assert!(content.contains("Registered (0)"));
        assert!(content.contains("No one has registered yet."));
    }

    #[test]
    fn renders_active_entrants_and_excludes_withdrawn_ones() {
        let entries = vec![
            entry(1, "MarineLorD", "active"),
            entry(2, "Beasty", "withdrawn"),
            entry(3, "Anotand", "active"),
        ];
        let (content, _) = render(1, "Relic Cup", &entries);
        assert!(content.contains("Registered (2)"));
        assert!(content.contains("MarineLorD"));
        assert!(content.contains("Anotand"));
        assert!(!content.contains("Beasty"));
    }

    #[test]
    fn truncates_the_roster_beyond_the_display_cap() {
        let entries: Vec<TournamentEntry> = (1..=12).map(|i| entry(i, &format!("Player{i}"), "active")).collect();
        let (content, _) = render(1, "Relic Cup", &entries);
        assert!(content.contains("Registered (12)"));
        assert!(content.contains("… and 2 more"));
        assert!(!content.contains("Player11"));
    }

    #[test]
    fn buttons_carry_the_tournament_id_in_their_custom_id() {
        let (_, components) = render(42, "Relic Cup", &[]);
        let CreateActionRow::Buttons(buttons) = &components[0] else {
            panic!("expected a button row");
        };
        assert_eq!(buttons.len(), 2);
        assert_eq!(
            buttons[0],
            CreateButton::new(Action::Register.custom_id(42))
                .label("Register")
                .style(ButtonStyle::Primary)
        );
        assert_eq!(
            buttons[1],
            CreateButton::new(Action::Withdraw.custom_id(42))
                .label("Withdraw")
                .style(ButtonStyle::Danger)
        );
    }
}
