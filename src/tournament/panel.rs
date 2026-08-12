//! The registration panel: a persistent message in
//! `#{slug}-register`, edited in place as entrants come and go. `render` is the
//! pure part — content plus the two buttons — golden-string tested here with no
//! Discord involved; `post_initial` and `refresh` are the thin Discord/DB glue
//! that `commands::create` and `tournament::registration`'s callers use.

use crate::Error;
use crate::db::{to_channel_id, to_db_id, to_message_id};
use crate::tournament::action::Action;
use crate::tournament::db::{self, Tournament, TournamentEntry};
use crate::tournament::panel_check::{self, PanelOutcome};
use crate::tournament::registration::RegistrationState;
use crate::tournament::throttle::EditThrottle;
use chrono::{DateTime, Utc};
use serenity::all::{
    ButtonStyle, CacheHttp, ChannelId, CreateActionRow, CreateButton, CreateEmbed, CreateMessage, EditMessage,
    MessageId,
};
use sqlx::SqlitePool;
use std::time::{Duration, Instant};
use tracing::error;

/// "Edits must be throttled" — coalesce to at most one edit every few
/// seconds, shared between the slash-command and button paths via one
/// `Arc<EditThrottle>` (see `main.rs`).
pub(crate) const PANEL_EDIT_MIN_INTERVAL: Duration = Duration::from_secs(3);

/// Names shown before the roster is truncated with "... and N more" — mirrors the
/// existing `.take(10)` cap on `commands::auto_complete_id`'s results.
const ROSTER_DISPLAY_CAP: usize = 10;

/// Pure. Filters `entries` down to `active` internally — a withdrawn entry's row
/// persists, but it must never show up in the roster, and callers should not
/// have to remember to filter it out themselves.
/// The heading, which becomes the embed's title.
///
/// Kept out of the message's `content` deliberately. An ephemeral reply to one of
/// these buttons is rendered by Discord as a reply to this message, and that
/// preview flattens `content` — and only `content` — onto a single line. With the
/// panel in an embed there is nothing to flatten.
pub(crate) fn render_title(name: &str, state: RegistrationState) -> String {
    let heading = match state {
        RegistrationState::Open => "報名進行中 / registration is OPEN",
        RegistrationState::InviteOnly => "邀請制 / registration is INVITE-ONLY",
        RegistrationState::Closed => "報名已結束 / registration is CLOSED",
    };
    format!("{name} — {heading}")
}

/// Pure, and the part worth golden-testing: everything below the title.
pub(crate) fn render(
    entries: &[TournamentEntry],
    entrant_cap: i64,
    scheduled_start_at: Option<DateTime<Utc>>,
    state: RegistrationState,
) -> String {
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

    // The one line that differs per state. Invite-only says so rather than
    // teaching a command that will refuse, and a closed panel says nothing at
    // all — it is a record by then.
    let first_time = match state {
        RegistrationState::Open => {
            "第一次報名？請用 `/tournament register` 並輸入你的遊戲名稱。\n\
             First time? Use `/tournament register` and type your in-game name.\n\n"
        },
        RegistrationState::InviteOnly => {
            "本賽事為邀請制，參賽名單由主辦方決定。\n\
             This event is invite-only — the organizers finalize the roster.\n\n"
        },
        RegistrationState::Closed => "\n",
    };

    let starts = scheduled_start_at.map_or_else(String::new, |at| {
        format!("開賽 / Starts <t:{0}:F> (<t:{0}:R>)\n\n", at.timestamp())
    });

    // No round/best_of line here (unlike the design doc's mock) — rounds don't
    // exist until the bracket is generated, so there's nothing true to say about
    // format yet. "Single elimination" itself is a fixed design decision, not
    // per-round data, so it stays.
    //
    // Bilingual rather than per-reader: one message, many readers, and it
    // re-renders on every button press — picking any one of their languages would
    // make it flip. Only the chrome doubles; the roster appears once.
    format!(
        "\
         單淘汰 · 開賽前需簽到\n\
         Single elimination · check-in required before start\n\
         {first_time}\
         {starts}**已報名 / Registered ({}/{entrant_cap})**\n{roster}",
        active.len()
    )
}

pub(crate) fn render_components(tournament_id: i64, state: RegistrationState) -> Vec<CreateActionRow> {
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(Action::Register.custom_id(tournament_id))
            .label("報名 / Register")
            .style(ButtonStyle::Primary)
            .disabled(!state.accepts_signups()),
        // The two move independently only in invite-only: shutting the public
        // door does not lock the invited in. Once registration has closed
        // outright the panel is a historical record and both go together;
        // `/tournament withdraw` is still there.
        CreateButton::new(Action::Withdraw.custom_id(tournament_id))
            .label("退賽 / Withdraw")
            .style(ButtonStyle::Danger)
            .disabled(!state.accepts_withdrawals()),
    ])]
}

/// Title and body as one embed. Thin by design — the two functions above hold
/// everything worth testing.
fn embed(name: &str, body: String, state: RegistrationState) -> CreateEmbed {
    CreateEmbed::new().title(render_title(name, state)).description(body)
}

/// Posts the panel with an empty roster — from `commands::create`, and from
/// `/tournament refresh` when the original post is gone.
///
/// **Takes the state rather than assuming an open one.** It used to hardcode it,
/// which made the repair path post an OPEN panel over a tournament that had long
/// since moved to `checkin` — the one panel guaranteed to be wrong is the one
/// nobody was watching closely enough to have kept.
pub(crate) async fn post_initial(
    http: impl CacheHttp,
    channel_id: ChannelId,
    tournament_id: i64,
    name: &str,
    entrant_cap: i64,
    state: RegistrationState,
) -> Result<MessageId, Error> {
    // No start time yet: `create` runs before `/tournament setup` can.
    let body = render(&[], entrant_cap, None, state);
    let message = channel_id
        .send_message(
            http,
            CreateMessage::new()
                .embed(embed(name, body, state))
                .components(render_components(tournament_id, state)),
        )
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

    let message_id = to_message_id(register_message_id);
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

    let message_id = to_message_id(register_message_id);
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
    let state = RegistrationState::of(tournament);
    let body = render(&entries, tournament.entrant_cap, tournament.scheduled_start_at, state);

    let channel_id = to_channel_id(register_channel_id);
    channel_id
        .edit_message(
            http,
            message_id,
            EditMessage::new()
                // The panel was plain text before; an explicit empty content
                // clears it on an already-posted message.
                .content("")
                .embed(embed(&tournament.name, body, state))
                .components(render_components(tournament.id, state)),
        )
        .await?;
    Ok(())
}

/// Confirms the registration panel still exists — a real probe, not an edit's
/// success — and recreates it if it doesn't. Shared by `/tournament refresh`
/// and boot-time reconciliation, so both get the same, precise answer to
/// "is it actually gone" rather than misreading a 403 or a rate limit as a
/// deletion. Never propagates: a panel this can't confirm or can't
/// repair is left exactly as it was.
pub(crate) async fn ensure(http: impl CacheHttp, pool: &SqlitePool, tournament: &Tournament) -> PanelOutcome {
    let Some(register_channel_id) = tournament.register_channel_id else {
        return PanelOutcome::NotConfigured;
    };
    let channel_id = to_channel_id(register_channel_id);

    let present = match tournament.register_message_id {
        None => false,
        Some(id) => match panel_check::message_exists(&http, channel_id, to_message_id(id)).await {
            Ok(exists) => exists,
            Err(err) => {
                error!(
                    "could not confirm the registration panel for tournament {}: {err:?}",
                    tournament.id
                );
                return PanelOutcome::Failed;
            },
        },
    };

    if present {
        return match refresh_now(&http, pool, tournament).await {
            Ok(()) => PanelOutcome::Present,
            Err(err) => {
                error!(
                    "confirmed but failed to refresh the registration panel for tournament {}: {err:?}",
                    tournament.id
                );
                PanelOutcome::Failed
            },
        };
    }

    match post_initial(
        &http,
        channel_id,
        tournament.id,
        &tournament.name,
        tournament.entrant_cap,
        RegistrationState::of(tournament),
    )
    .await
    {
        Ok(message_id) => match db::set_register_message_id(pool, tournament.id, to_db_id(message_id)).await {
            Ok(()) => PanelOutcome::Reposted,
            Err(err) => {
                error!(
                    "failed to record the reposted registration panel for tournament {}: {err:?}",
                    tournament.id
                );
                PanelOutcome::Failed
            },
        },
        Err(err) => {
            error!(
                "failed to repost the registration panel for tournament {}: {err:?}",
                tournament.id
            );
            PanelOutcome::Failed
        },
    }
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
            invited_by: None,
            seed: None,
            suggested_seed: None,
            manual_seed: None,
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
        // A shared message, re-rendered by whoever presses a button, so it
        // carries both languages instead of flipping between them.
        let title = render_title("Relic Cup", RegistrationState::Open);
        assert!(title.contains("報名進行中"), "{title}");
        assert!(title.contains("registration is OPEN"), "{title}");

        let content = render(&[entry(1, "MarineLorD", "active")], 32, None, RegistrationState::Open);
        assert!(content.contains("單淘汰"), "{content}");
        assert!(content.contains("Single elimination"), "{content}");
        assert!(content.contains("已報名 / Registered (1/32)"), "{content}");
    }

    #[test]
    fn shows_the_cap_and_the_start_time_when_one_is_set() {
        // The cap is only fair if entrants can watch the field fill up — a
        // sign-up past it is refused.
        let at = Utc::now();
        let content = render(&[entry(1, "A", "active")], 8, Some(at), RegistrationState::Open);
        assert!(content.contains("Registered (1/8)"), "{content}");
        assert!(content.contains(&format!("<t:{}:F>", at.timestamp())), "{content}");
    }

    #[test]
    fn omits_the_start_line_entirely_when_unscheduled() {
        let content = render(&[], 32, None, RegistrationState::Open);
        assert!(!content.contains("Starts"), "{content}");
    }

    #[test]
    fn tells_first_timers_what_to_do_before_they_press_the_button() {
        // The Register button cannot serve a first-timer — it carries no name —
        // so the panel has to say so up front rather than let them hit the
        // refusal and go hunting for a command.
        let content = render(&[], 32, None, RegistrationState::Open);
        assert!(content.contains("第一次報名？"), "{content}");
        assert!(content.contains("First time?"), "{content}");
        assert!(content.contains("/tournament register"));
    }

    #[test]
    fn renders_a_placeholder_when_nobody_has_registered() {
        let content = render(&[], 32, None, RegistrationState::Open);
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
        let content = render(&entries, 32, None, RegistrationState::Open);
        assert!(content.contains("Registered (2/32)"));
        assert!(content.contains("MarineLorD"));
        assert!(content.contains("Anotand"));
        assert!(!content.contains("Beasty"));
    }

    #[test]
    fn truncates_the_roster_beyond_the_display_cap() {
        let entries: Vec<TournamentEntry> = (1..=12).map(|i| entry(i, &format!("Player{i}"), "active")).collect();
        let content = render(&entries, 32, None, RegistrationState::Open);
        assert!(content.contains("Registered (12/32)"));
        assert!(content.contains("…等 2 人 / and 2 more"));
        assert!(!content.contains("Player11"));
    }

    #[test]
    fn the_panel_follows_a_cap_change() {
        // `/tournament setup` writes the cap, and the panel is where entrants see
        // it — so a stale panel understates or overstates how full the field is.
        let entries = vec![entry(1, "A", "active")];
        assert!(render(&entries, 32, None, RegistrationState::Open).contains("Registered (1/32)"));
        assert!(render(&entries, 8, None, RegistrationState::Open).contains("Registered (1/8)"));
    }

    #[test]
    fn the_title_is_short_enough_to_read_as_a_reply_preview() {
        // Why the panel is an embed at all: Discord renders an ephemeral reply to
        // one of these buttons as a reply to this message, flattening its
        // `content` onto one line. The body lives in the embed, so only this
        // title could ever appear there.
        let title = render_title("Test Bot Cup", RegistrationState::Open);
        assert!(title.len() < 80, "{} chars: {title}", title.len());
        assert!(!title.contains('\n'), "a title is one line by construction");
    }

    #[test]
    fn a_closed_panel_says_so_and_stops_inviting_presses() {
        let content = render(&[entry(1, "A", "active")], 32, None, RegistrationState::Closed);
        let components = render_components(1, RegistrationState::Closed);
        let title = render_title("Relic Cup", RegistrationState::Closed);
        assert!(title.contains("報名已結束"), "{title}");
        assert!(title.contains("registration is CLOSED"), "{title}");
        // The first-timer hint would send someone to a command that refuses.
        assert!(!content.contains("First time?"), "{content}");

        let CreateActionRow::Buttons(buttons) = &components[0] else {
            panic!("expected a button row");
        };
        assert!(
            buttons.iter().all(|b| b == &b.clone().disabled(true)),
            "both buttons should be disabled"
        );
    }

    #[test]
    fn an_invite_only_panel_explains_itself_instead_of_teaching_a_command() {
        let entries = vec![entry(1, "MarineLorD", "active")];
        let content = render(&entries, 8, None, RegistrationState::InviteOnly);
        let title = render_title("Relic Cup", RegistrationState::InviteOnly);

        assert!(title.contains("邀請制"), "{title}");
        assert!(title.contains("INVITE-ONLY"), "{title}");
        assert!(content.contains("本賽事為邀請制"), "{content}");
        assert!(content.contains("the organizers finalize the roster"), "{content}");
        // Naming the command would send someone to a refusal.
        assert!(!content.contains("First time?"), "{content}");
        // An invited field is a field: the roster and the counter are unchanged.
        assert!(content.contains("Registered (1/8)"), "{content}");
        assert!(content.contains("MarineLorD"), "{content}");
    }

    #[test]
    fn invite_only_disables_register_and_leaves_withdraw_live() {
        // The first state where the two buttons part company. Shutting the public
        // door is not the same as locking the invited in.
        let CreateActionRow::Buttons(buttons) = &render_components(1, RegistrationState::InviteOnly)[0] else {
            panic!("expected a button row");
        };
        assert_eq!(buttons[0], buttons[0].clone().disabled(true), "Register is shut");
        assert_eq!(buttons[1], buttons[1].clone().disabled(false), "Withdraw stays live");
    }

    #[test]
    fn the_three_titles_are_distinct() {
        // The panel is the only thing telling a reader which door is open, so two
        // states rendering alike is the failure worth pinning.
        let titles: Vec<String> = [
            RegistrationState::Open,
            RegistrationState::InviteOnly,
            RegistrationState::Closed,
        ]
        .iter()
        .map(|state| render_title("Relic Cup", *state))
        .collect();
        assert_ne!(titles[0], titles[1]);
        assert_ne!(titles[1], titles[2]);
        assert_ne!(titles[0], titles[2]);
    }

    #[test]
    fn the_roster_survives_closing() {
        // The panel becomes a record of who is in the field, so it must still
        // list them.
        let entries = vec![entry(1, "MarineLorD", "active"), entry(2, "Beasty", "active")];
        let content = render(&entries, 32, None, RegistrationState::Closed);
        assert!(
            content.contains("MarineLorD") && content.contains("Beasty"),
            "{content}"
        );
        assert!(content.contains("Registered (2/32)"), "{content}");
    }

    #[test]
    fn buttons_carry_the_tournament_id_in_their_custom_id() {
        let components = render_components(42, RegistrationState::Open);
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
