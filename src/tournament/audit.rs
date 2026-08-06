//! One line per tournament action, whichever surface it came from. Until this
//! existed only failures were logged (`errors.rs`, `dispatch.rs`,
//! `db::log_db_error`) and a command that worked left nothing behind at all —
//! so after a destructive one there was no record of who ran it.
//!
//! Register, withdraw and check-in each have two surfaces (a slash command in
//! `commands.rs` and a button in `dispatch.rs`), which is why the line lives
//! here rather than being written out at each of them.

use serenity::all::User;
use std::fmt::Debug;
use tracing::info;

/// Name *and* id: the name is what makes a log line readable months later, the
/// id is what still resolves after someone renames themselves.
///
/// `outcome` is the action's own `*Outcome` enum: its `Debug` already names the
/// variant and carries the counts, so there is nothing to spell out by hand.
pub(crate) fn log_action(action: &str, tournament_id: i64, slug: &str, actor: &User, outcome: &impl Debug) {
    info!(
        "{action} on tournament {tournament_id} ({slug}) by {} ({}): {outcome:?}",
        actor.name, actor.id
    );
}
