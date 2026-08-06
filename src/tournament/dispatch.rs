//! The interaction dispatcher (docs/tournament.md §8.5, §8.9): one
//! `EventHandler::interaction_create` branch over `"<action>:<entity_id>"`
//! custom_ids.
//!
//! Register, Withdraw and Checkin are wired up (chunks 9, 10); Redraft/SetDone
//! stay stubs until their own chunks (20, 22). The button path resolves its
//! tournament by the id encoded in the custom_id (`db::get_tournament`), unlike
//! the slash commands in `commands.rs`, which resolve it from the invoking
//! channel.

use crate::guilds::{Feature, Guilds};
use crate::locale::Locale;
use crate::tournament::action::{self, Action};
use crate::tournament::checkin::CheckinOutcome;
use crate::tournament::registration::{RegisterOutcome, WithdrawOutcome};
use crate::tournament::throttle::EditThrottle;
use crate::tournament::{audit, checkin, checkin_panel, db, panel, registration};
use serenity::all::{
    ComponentInteraction, CreateInteractionResponse, CreateInteractionResponseMessage, EditInteractionResponse,
    Interaction,
};
use serenity::async_trait;
use serenity::prelude::*;
use sqlx::SqlitePool;
use std::sync::Arc;
use tracing::error;

pub(crate) struct Dispatcher {
    guilds: Guilds,
    pool: SqlitePool,
    panel_throttle: Arc<EditThrottle>,
}

impl Dispatcher {
    pub(crate) fn new(guilds: Guilds, pool: SqlitePool, panel_throttle: Arc<EditThrottle>) -> Self {
        Self {
            guilds,
            pool,
            panel_throttle,
        }
    }
}

#[async_trait]
impl EventHandler for Dispatcher {
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Component(component) = interaction else {
            return;
        };

        // Tournament guild only (docs/tournament.md §8.0) — the same rule
        // `Emperor::message` applies for the home guild, just a separate handler.
        if !self.guilds.allows(Feature::Tournament, component.guild_id) {
            return;
        }

        let Some((action, entity_id)) = action::parse_custom_id(&component.data.custom_id) else {
            // Malformed, or a button left over from an older deploy. Ignore
            // rather than panic (§8.5, §10).
            return;
        };

        if action.requires_defer() && !defer(&ctx, &component, action, entity_id).await {
            return;
        }

        match action {
            Action::Register => self.handle_register(&ctx, &component, entity_id).await,
            Action::Withdraw => self.handle_withdraw(&ctx, &component, entity_id).await,
            Action::Checkin => self.handle_checkin(&ctx, &component, entity_id).await,
            Action::Redraft => {}, // chunk 20
            Action::SetDone => {}, // chunk 22
        }
    }
}

impl Dispatcher {
    /// The button carries no `aoe4_id` argument (§8.5) — a first-timer pressing
    /// it gets `registration::register`'s `NeedsProfileArgument` message, naming
    /// the command to use instead.
    async fn handle_register(&self, ctx: &Context, component: &ComponentInteraction, tournament_id: i64) {
        let Some(tournament) = self.resolve_tournament(Action::Register, tournament_id).await else {
            return;
        };

        let user_id = i64::try_from(component.user.id.get()).unwrap();
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
        }
    }

    async fn handle_withdraw(&self, ctx: &Context, component: &ComponentInteraction, tournament_id: i64) {
        let Some(tournament) = self.resolve_tournament(Action::Withdraw, tournament_id).await else {
            return;
        };

        let user_id = i64::try_from(component.user.id.get()).unwrap();
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
        }
    }

    async fn handle_checkin(&self, ctx: &Context, component: &ComponentInteraction, tournament_id: i64) {
        let Some(tournament) = self.resolve_tournament(Action::Checkin, tournament_id).await else {
            return;
        };

        let user_id = i64::try_from(component.user.id.get()).unwrap();
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
                // tournament — reachable since chunk 26, by pressing a panel
                // button `/tournament delete` has not yet removed the channel
                // for (§8.5/§10, and not a case to panic on). Whether this
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
/// slower work (§8.5). Returns whether the defer succeeded — a failure here
/// means the interaction is already unrecoverable, so the caller should not
/// continue on to whatever real work it was about to do.
async fn defer(ctx: &Context, component: &ComponentInteraction, action: Action, entity_id: i64) -> bool {
    let response = CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true));
    if let Err(err) = component.create_response(&ctx.http, response).await {
        error!("failed to defer a {action:?} interaction for entity {entity_id}: {err:?}");
        return false;
    }
    true
}
