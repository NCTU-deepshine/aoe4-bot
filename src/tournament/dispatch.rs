//! The interaction dispatcher: one
//! `EventHandler::interaction_create` branch over `"<action>:<entity_id>"`
//! custom_ids.
//!
//! Register, Withdraw and Checkin are wired up; Redraft/SetDone
//! stay stubs until their own chunks (20, 22). The button path resolves its
//! tournament by the id encoded in the custom_id (`db::get_tournament`), unlike
//! the slash commands in `commands.rs`, which resolve it from the invoking
//! channel.

use crate::db::to_db_id;
use crate::guilds::{Feature, Guilds};
use crate::locale::Locale;
use crate::tournament::action::{self, Action};
use crate::tournament::checkin::CheckinOutcome;
use crate::tournament::registration::{RegisterOutcome, WithdrawOutcome};
use crate::tournament::throttle::EditThrottle;
use crate::tournament::{audit, bracket_view, checkin, checkin_panel, db, panel, registration};
use serenity::all::{
    ComponentInteraction, CreateInteractionResponse, CreateInteractionResponseMessage, EditInteractionResponse,
    Interaction,
};
use serenity::async_trait;
use serenity::prelude::*;
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::error;

/// How long a set thread waits before it may summon the organizers again. Far
/// longer than the panel-edit window: this one costs somebody a notification
/// rather than an API call.
const CALL_ADMIN_MIN_INTERVAL: Duration = Duration::from_secs(300);

pub(crate) struct Dispatcher {
    guilds: Guilds,
    pool: SqlitePool,
    panel_throttle: Arc<EditThrottle>,
    /// Its own window, and not shared with `panel_throttle`, so a busy
    /// registration panel can never suppress a call for help.
    help_throttle: EditThrottle,
}

impl Dispatcher {
    pub(crate) fn new(guilds: Guilds, pool: SqlitePool, panel_throttle: Arc<EditThrottle>) -> Self {
        Self {
            guilds,
            pool,
            panel_throttle,
            help_throttle: EditThrottle::new(CALL_ADMIN_MIN_INTERVAL),
        }
    }
}

#[async_trait]
impl EventHandler for Dispatcher {
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Component(component) = interaction else {
            return;
        };

        // Tournament guild only — the same rule
        // `Emperor::message` applies for the home guild, just a separate handler.
        if !self.guilds.allows(Feature::Tournament, component.guild_id) {
            return;
        }

        let Some((action, entity_id)) = action::parse_custom_id(&component.data.custom_id) else {
            // Malformed, or a button left over from an older deploy. Ignore
            // rather than panic.
            return;
        };

        if action.requires_defer() && !defer(&ctx, &component, action, entity_id).await {
            return;
        }

        match action {
            Action::Register => self.handle_register(&ctx, &component, entity_id).await,
            Action::Withdraw => self.handle_withdraw(&ctx, &component, entity_id).await,
            Action::Checkin => self.handle_checkin(&ctx, &component, entity_id).await,
            Action::CallAdmin => self.handle_call_admin(&ctx, &component, entity_id).await,
            // Neither has a handler yet.
            Action::Redraft | Action::SetDone => {},
        }
    }
}

impl Dispatcher {
    /// The button carries no `aoe4_id` argument — a first-timer pressing
    /// it gets `registration::register`'s `NeedsProfileArgument` message, naming
    /// the command to use instead.
    async fn handle_register(&self, ctx: &Context, component: &ComponentInteraction, tournament_id: i64) {
        let Some(tournament) = self.resolve_tournament(Action::Register, tournament_id).await else {
            return;
        };

        let user_id = to_db_id(component.user.id);
        let outcome = match registration::register(&self.pool, &tournament, user_id, None).await {
            Ok(outcome) => outcome,
            Err(err) => {
                error!("register button failed for tournament {tournament_id}, user {user_id}: {err:?}");
                return;
            },
        };
        audit::log_action(
            "register button",
            tournament.id,
            &tournament.slug,
            &component.user,
            &outcome,
        );
        let locale = Locale::from_discord_locale(&component.locale);

        // Deferred (Action::Register.requires_defer() == true), so the reply
        // edits the initial deferred response rather than creating a new one.
        let response = EditInteractionResponse::new().content(outcome.message(&tournament.name, locale));
        if let Err(err) = component.edit_response(&ctx.http, response).await {
            error!("failed to edit the register response for tournament {tournament_id}: {err:?}");
        }

        if matches!(
            outcome,
            RegisterOutcome::Registered { .. } | RegisterOutcome::Reactivated { .. }
        ) {
            if let Ok(Some(entry)) = db::get_entry(&self.pool, tournament.id, user_id).await {
                registration::snapshot_entry_elo(&self.pool, tournament.id, user_id, entry.aoe4_id).await;
            }
            self.refresh_panel(ctx, &tournament).await;
            self.reconcile_bracket(ctx, &tournament).await;
        }
    }

    /// A player asking for an organizer from inside a set thread.
    ///
    /// The entity is the **set**, not the tournament, because the button lives
    /// on the set's panel and the ping should say which match needs attention.
    /// Posted into the thread the button was pressed in, so it needs no stored
    /// thread id and works even if one was never recorded.
    async fn handle_call_admin(&self, ctx: &Context, component: &ComponentInteraction, set_id: i64) {
        let Ok(Some(set)) = db::get_set(&self.pool, set_id).await else {
            error!("call-admin button for unknown set {set_id}");
            return;
        };
        let Ok(Some(tournament)) = db::get_tournament(&self.pool, set.tournament_id).await else {
            error!("call-admin button for set {set_id} with no tournament");
            return;
        };
        let locale = Locale::from_discord_locale(&component.locale);

        // Answer the presser first: the ping below is a second API call, and the
        // ack window is three seconds.
        let acknowledged = ephemeral_ack(
            ctx,
            component,
            locale.pick("已通知管理員。", "The organizers have been notified."),
        )
        .await;
        if !acknowledged {
            return;
        }

        // One ping per set per window. A player waiting on an organizer will
        // press this more than once, and each press is a notification to
        // everyone running the event.
        if !self.help_throttle.try_begin_edit(component.message.id, Instant::now()) {
            return;
        }

        let admins = db::list_admins(&self.pool, tournament.id).await.unwrap_or_default();
        let mentions: Vec<String> = admins.iter().map(|admin| format!("<@{}>", admin.user_id)).collect();
        let content = format!(
            "<@{}> 需要協助 / needs an organizer{}",
            component.user.id,
            if mentions.is_empty() {
                String::new()
            } else {
                format!(" — {}", mentions.join(" "))
            }
        );

        audit::log_action("call-admin", tournament.id, &tournament.slug, &component.user, &set_id);
        if let Err(err) = component.channel_id.say(&ctx.http, content).await {
            error!("failed to post the call-admin ping for set {set_id}: {err:?}");
        }
    }

    async fn handle_withdraw(&self, ctx: &Context, component: &ComponentInteraction, tournament_id: i64) {
        let Some(tournament) = self.resolve_tournament(Action::Withdraw, tournament_id).await else {
            return;
        };

        let user_id = to_db_id(component.user.id);
        let outcome = match registration::withdraw(&self.pool, &tournament, user_id).await {
            Ok(outcome) => outcome,
            Err(err) => {
                error!("withdraw button failed for tournament {tournament_id}, user {user_id}: {err:?}");
                return;
            },
        };
        audit::log_action(
            "withdraw button",
            tournament.id,
            &tournament.slug,
            &component.user,
            &outcome,
        );
        let locale = Locale::from_discord_locale(&component.locale);

        // Never deferred (Action::Withdraw.requires_defer() == false), so the
        // reply is a fresh ephemeral message, not an edit of a deferred one.
        let response = CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .ephemeral(true)
                .content(outcome.message(&tournament.name, locale)),
        );
        if let Err(err) = component.create_response(&ctx.http, response).await {
            error!("failed to respond to a withdraw interaction for tournament {tournament_id}: {err:?}");
        }

        if matches!(outcome, WithdrawOutcome::Success) {
            self.refresh_panel(ctx, &tournament).await;
            self.reconcile_bracket(ctx, &tournament).await;
        }
    }

    async fn handle_checkin(&self, ctx: &Context, component: &ComponentInteraction, tournament_id: i64) {
        let Some(tournament) = self.resolve_tournament(Action::Checkin, tournament_id).await else {
            return;
        };

        let user_id = to_db_id(component.user.id);
        let outcome = match checkin::checkin(&self.pool, &tournament, user_id).await {
            Ok(outcome) => outcome,
            Err(err) => {
                error!("checkin button failed for tournament {tournament_id}, user {user_id}: {err:?}");
                return;
            },
        };
        audit::log_action(
            "checkin button",
            tournament.id,
            &tournament.slug,
            &component.user,
            &outcome,
        );
        let locale = Locale::from_discord_locale(&component.locale);

        // Never deferred (Action::Checkin.requires_defer() == false), so the
        // reply is a fresh ephemeral message, not an edit of a deferred one.
        let response = CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .ephemeral(true)
                .content(outcome.message(&tournament.name, locale)),
        );
        if let Err(err) = component.create_response(&ctx.http, response).await {
            error!("failed to respond to a checkin interaction for tournament {tournament_id}: {err:?}");
        }

        if matches!(outcome, CheckinOutcome::CheckedIn { .. }) {
            self.refresh_checkin_panel(ctx, &tournament).await;
        }
    }

    async fn resolve_tournament(&self, action: Action, entity_id: i64) -> Option<db::Tournament> {
        match db::get_tournament(&self.pool, entity_id).await {
            Ok(Some(tournament)) => Some(tournament),
            Ok(None) => {
                // A stale button whose entity_id no longer resolves to a
                // tournament — reachable by pressing a panel button
                // `/tournament delete` has not yet removed the channel for, and
                // not a case to panic on. Whether this
                // interaction was already deferred depends on `action`, and
                // replying the wrong way is worse than leaving it to time out,
                // so this is a log-only best effort.
                error!("a {action:?} button named tournament {entity_id}, which no longer exists");
                None
            },
            Err(err) => {
                error!("failed to resolve tournament {entity_id} for a {action:?} interaction: {err:?}");
                None
            },
        }
    }

    async fn refresh_panel(&self, ctx: &Context, tournament: &db::Tournament) {
        if let Err(err) = panel::refresh(&ctx.http, &self.pool, &self.panel_throttle, tournament).await {
            error!(
                "failed to refresh the registration panel for tournament {}: {err:?}",
                tournament.id
            );
        }
    }

    /// The draw follows the field, and the preview exists from the first two
    /// entrants, so a sign-up or withdrawal redraws it.
    async fn reconcile_bracket(&self, ctx: &Context, tournament: &db::Tournament) {
        if let Err(err) = bracket_view::reconcile(&ctx.http, &self.pool, tournament).await {
            error!("failed to redraw the bracket for tournament {}: {err:?}", tournament.id);
        }
    }

    async fn refresh_checkin_panel(&self, ctx: &Context, tournament: &db::Tournament) {
        if let Err(err) = checkin_panel::refresh(&ctx.http, &self.pool, &self.panel_throttle, tournament).await {
            error!(
                "failed to refresh the check-in panel for tournament {}: {err:?}",
                tournament.id
            );
        }
    }
}

/// Acknowledge within Discord's 3s window, ephemerally, before doing any
/// slower work. Returns whether the defer succeeded — a failure here
/// means the interaction is already unrecoverable, so the caller should not
/// continue on to whatever real work it was about to do.
/// Answers the presser immediately and privately. For the handlers that do not
/// defer: the interaction still has to be acknowledged inside three seconds, and
/// only the presser needs to see that it worked.
async fn ephemeral_ack(ctx: &Context, component: &ComponentInteraction, content: &str) -> bool {
    let response =
        CreateInteractionResponse::Message(CreateInteractionResponseMessage::new().ephemeral(true).content(content));
    if let Err(err) = component.create_response(&ctx.http, response).await {
        error!("failed to acknowledge an interaction: {err:?}");
        return false;
    }
    true
}

async fn defer(ctx: &Context, component: &ComponentInteraction, action: Action, entity_id: i64) -> bool {
    let response = CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true));
    if let Err(err) = component.create_response(&ctx.http, response).await {
        error!("failed to defer a {action:?} interaction for entity {entity_id}: {err:?}");
        return false;
    }
    true
}
