//! The interaction dispatcher (docs/tournament.md §8.5, §8.9): one
//! `EventHandler::interaction_create` branch over `"<action>:<entity_id>"`
//! custom_ids.
//!
//! No panels exist yet (chunks 9, 10, 20, 22 add them), so every action below
//! is a stub: this chunk only proves the id round-trips to the right action
//! and entity, that a stale or malformed one is ignored rather than panicking,
//! and that a handler needing an HTTP call defers within Discord's 3s window.

use crate::guilds::{Feature, Guilds};
use crate::tournament::action::{self, Action};
use serenity::all::{ComponentInteraction, CreateInteractionResponse, CreateInteractionResponseMessage, Interaction};
use serenity::async_trait;
use serenity::prelude::*;
use tracing::error;

pub(crate) struct Dispatcher {
    guilds: Guilds,
}

impl Dispatcher {
    pub(crate) fn new(guilds: Guilds) -> Self {
        Self { guilds }
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
            Action::Register => {}, // chunk 9
            Action::Withdraw => {}, // chunk 9
            Action::Checkin => {},  // chunk 10
            Action::Redraft => {},  // chunk 20
            Action::SetDone => {},  // chunk 22
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
