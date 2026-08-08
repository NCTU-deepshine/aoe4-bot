//! The check-in panel: a persistent message in
//! `#{slug}-register`, alongside the registration panel, edited in place as
//! entrants check in. `render` is the pure part — content plus the one button —
//! golden-string tested here with no Discord involved; `post_initial`,
//! `refresh` and `close` are the thin Discord/DB glue `commands.rs` and
//! `dispatch.rs` use.

use crate::Error;
use crate::db::{to_channel_id, to_message_id};
use crate::tournament::action::Action;
use crate::tournament::checkin::checkin_counts;
use crate::tournament::db::{self, Tournament, TournamentEntry};
use crate::tournament::throttle::EditThrottle;
use chrono::{DateTime, Utc};
use serenity::all::{
    ButtonStyle, CacheHttp, ChannelId, CreateActionRow, CreateButton, CreateMessage, EditMessage, MessageId,
};
use sqlx::SqlitePool;
use std::time::Instant;

/// Pure. `open` disables the button once check-in has closed, so a stale panel
/// stops inviting presses instead of silently failing them: components are
/// disabled when the phase changes.
pub(crate) fn render(
    tournament_id: i64,
    name: &str,
    entries: &[TournamentEntry],
    closes_at: Option<DateTime<Utc>>,
    open: bool,
) -> (String, Vec<CreateActionRow>) {
    let (checked_in, total) = checkin_counts(entries);
    // Bilingual for the same reason as the registration panel: shared
    // message, re-rendered by whoever presses the button.
    let heading = if open {
        "簽到開放中 / check-in is OPEN"
    } else {
        "簽到已結束 / check-in is CLOSED"
    };

    let closes_line = match (open, closes_at) {
        (true, Some(closes_at)) => format!("<t:{0}:R> 截止 / Closes <t:{0}:R>.\n", closes_at.timestamp()),
        _ => String::new(),
    };
    let footer = if open {
        "\n(或使用 / or use `/tournament checkin`)"
    } else {
        ""
    };

    let content =
        format!("**{name} — {heading}**\n{closes_line}\n**{checked_in}/{total} 已簽到 / checked in**{footer}");

    let components = vec![CreateActionRow::Buttons(vec![
        CreateButton::new(Action::Checkin.custom_id(tournament_id))
            .label("簽到 / Check In")
            .style(ButtonStyle::Success)
            .disabled(!open),
    ])];

    (content, components)
}

/// Posts the panel with the entrants registered so far — unlike the
/// registration panel (posted at tournament creation, with nobody registered
/// yet), check-in opens after registration has been running for a while.
///
/// `open` is a parameter rather than an assumption because `/tournament refresh`
/// also posts this panel, and a tournament past check-in must get it back in its
/// closed form instead of a live button nobody may press.
pub(crate) async fn post_initial(
    http: impl CacheHttp,
    pool: &SqlitePool,
    channel_id: ChannelId,
    tournament_id: i64,
    name: &str,
    closes_at: Option<DateTime<Utc>>,
    open: bool,
) -> Result<MessageId, Error> {
    let entries = db::list_entries_for_tournament(pool, tournament_id).await?;
    let (content, components) = render(tournament_id, name, &entries, closes_at, open);
    let message = channel_id
        .send_message(http, CreateMessage::new().content(content).components(components))
        .await?;
    Ok(message.id)
}

/// Re-fetches entries and, if the panel exists and the throttle allows it,
/// edits it in place. A no-op if `checkin_message_id`/`register_channel_id` is
/// unset. Callers should only invoke this after an outcome that actually
/// changed check-in state (see `CheckinOutcome::changed_state`) — an idempotent
/// press doesn't need a fresh edit.
pub(crate) async fn refresh(
    http: impl CacheHttp,
    pool: &SqlitePool,
    throttle: &EditThrottle,
    tournament: &Tournament,
) -> Result<(), Error> {
    let (Some(checkin_message_id), Some(register_channel_id)) =
        (tournament.checkin_message_id, tournament.register_channel_id)
    else {
        return Ok(());
    };

    let message_id = to_message_id(checkin_message_id);
    if !throttle.try_begin_edit(message_id, Instant::now()) {
        return Ok(());
    }

    edit(http, pool, register_channel_id, message_id, tournament, true).await
}

/// The unconditional final edit when check-in closes — bypasses the throttle,
/// since this fires exactly
/// once, and renders with the button disabled.
pub(crate) async fn close(http: impl CacheHttp, pool: &SqlitePool, tournament: &Tournament) -> Result<(), Error> {
    let (Some(checkin_message_id), Some(register_channel_id)) =
        (tournament.checkin_message_id, tournament.register_channel_id)
    else {
        return Ok(());
    };

    let message_id = to_message_id(checkin_message_id);
    edit(http, pool, register_channel_id, message_id, tournament, false).await
}

/// An unconditional edit that keeps the panel's current open/closed state,
/// unlike `close` which forces it shut. For `/tournament refresh`, which repairs
/// a panel without changing what phase the tournament is in.
pub(crate) async fn refresh_now(http: impl CacheHttp, pool: &SqlitePool, tournament: &Tournament) -> Result<(), Error> {
    let (Some(checkin_message_id), Some(register_channel_id)) =
        (tournament.checkin_message_id, tournament.register_channel_id)
    else {
        return Ok(());
    };

    let message_id = to_message_id(checkin_message_id);
    let open = crate::tournament::checkin::checkin_is_open(&tournament.status);
    edit(http, pool, register_channel_id, message_id, tournament, open).await
}

async fn edit(
    http: impl CacheHttp,
    pool: &SqlitePool,
    register_channel_id: i64,
    message_id: MessageId,
    tournament: &Tournament,
    open: bool,
) -> Result<(), Error> {
    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
    let (content, components) = render(
        tournament.id,
        &tournament.name,
        &entries,
        tournament.checkin_closes_at,
        open,
    );

    let channel_id = to_channel_id(register_channel_id);
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

    fn entry(user_id: i64, status: &str, checked_in_at: Option<DateTime<Utc>>) -> TournamentEntry {
        TournamentEntry {
            tournament_id: 1,
            user_id,
            aoe4_id: Some(user_id),
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
    fn renders_open_with_counts_and_the_slash_command_hint() {
        let entries = vec![entry(1, "active", Some(Utc::now())), entry(2, "active", None)];
        let (content, _) = render(1, "Relic Cup", &entries, None, true);
        // Bilingual for the same reason as the registration panel.
        assert!(content.contains("簽到開放中"));
        assert!(content.contains("check-in is OPEN"));
        assert!(content.contains("1/2 已簽到 / checked in"));
        assert!(content.contains("/tournament checkin"));
    }

    #[test]
    fn renders_a_closes_at_line_when_open_and_set() {
        let closes_at = Utc::now();
        let (content, _) = render(1, "Relic Cup", &[], Some(closes_at), true);
        assert!(content.contains(&format!("<t:{}:R>", closes_at.timestamp())));
    }

    #[test]
    fn omits_the_closes_at_line_once_closed() {
        let closes_at = Utc::now();
        let (content, _) = render(1, "Relic Cup", &[], Some(closes_at), false);
        assert!(!content.contains(":R>"));
        assert!(content.contains("簽到已結束"));
        assert!(content.contains("check-in is CLOSED"));
        assert!(!content.contains("/tournament checkin"));
    }

    #[test]
    fn the_button_is_disabled_once_closed() {
        let (_, components) = render(42, "Relic Cup", &[], None, false);
        let CreateActionRow::Buttons(buttons) = &components[0] else {
            panic!("expected a button row");
        };
        assert_eq!(
            buttons[0],
            CreateButton::new(Action::Checkin.custom_id(42))
                .label("簽到 / Check In")
                .style(ButtonStyle::Success)
                .disabled(true)
        );
    }

    #[test]
    fn the_button_carries_the_tournament_id_and_is_enabled_while_open() {
        let (_, components) = render(42, "Relic Cup", &[], None, true);
        let CreateActionRow::Buttons(buttons) = &components[0] else {
            panic!("expected a button row");
        };
        assert_eq!(
            buttons[0],
            CreateButton::new(Action::Checkin.custom_id(42))
                .label("簽到 / Check In")
                .style(ButtonStyle::Success)
                .disabled(false)
        );
    }
}
