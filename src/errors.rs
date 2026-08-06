use crate::locale::Locale;
use crate::reply::ephemeral;
use crate::{Context, Data, Error};
use poise::FrameworkError;
use tracing::{error, info, warn};

/// Every `Err` a command returns, and every invocation poise refuses, arrives here.
///
/// Without a handler poise writes to stderr and the user is left looking at a
/// slash command that silently did nothing. The messages live here rather than in
/// each command so that a command only has to declare what it requires
/// (docs/tournament.md §8.2, §8.9).
pub(crate) async fn on_error(err: FrameworkError<'_, Data, Error>) {
    match err {
        // Our own bug, or a service we depend on being down. Log the detail; tell
        // the user only what they can act on.
        FrameworkError::Command { error, ctx, .. } => {
            error!("command `{}` failed: {error:?}", ctx.command().qualified_name);
            notify(ctx, command_failed(ctx)).await;
        },
        FrameworkError::CommandPanic { payload, ctx, .. } => {
            error!("command `{}` panicked: {payload:?}", ctx.command().qualified_name);
            notify(ctx, command_failed(ctx)).await;
        },

        // Gating. Nothing declares these requirements yet; the tournament commands
        // will, and they should not each have to word the refusal.
        FrameworkError::GuildOnly { ctx, .. } => {
            notify(
                ctx,
                Locale::from_context(ctx).pick(
                    "這個指令只能在伺服器中使用。",
                    "This command can only be used in a server.",
                ),
            )
            .await
        },
        FrameworkError::NotAnOwner { ctx, .. } | FrameworkError::MissingUserPermissions { ctx, .. } => {
            notify(
                ctx,
                Locale::from_context(ctx).pick(
                    "你沒有權限使用這個指令。",
                    "You don't have permission to use this command.",
                ),
            )
            .await
        },
        FrameworkError::MissingBotPermissions {
            missing_permissions,
            ctx,
            ..
        } => {
            error!(
                "bot lacks {missing_permissions} in guild {:?}",
                ctx.guild_id().map(|id| id.get())
            );
            notify(
                ctx,
                Locale::from_context(ctx).pick(
                    "我在這個伺服器缺少必要的權限，請聯絡管理員。",
                    "I'm missing a permission I need in this server — please contact an admin.",
                ),
            )
            .await
        },

        // A check that wants to explain itself replies before returning false, so
        // only speak up when there is a real error rather than a plain refusal —
        // otherwise the user gets told twice.
        FrameworkError::CommandCheckFailed { error, ctx, .. } => match error {
            Some(error) => {
                error!("check for `{}` errored: {error:?}", ctx.command().qualified_name);
                notify(ctx, command_failed(ctx)).await;
            },
            None => info!(
                "check refused `{}` for user {}",
                ctx.command().qualified_name,
                ctx.author().id
            ),
        },

        // Argument parsing, cooldowns, unknown interactions: poise's own wording is
        // fine and keeping it means new variants are handled rather than swallowed.
        other => {
            if let Err(err) = poise::builtins::on_error(other).await {
                error!("the error handler itself failed: {err:?}");
            }
        },
    }
}

/// The one "something broke on our side" wording, shared by the three arms that
/// mean exactly that: a returned `Err`, a panic, and a check that itself errored.
fn command_failed(ctx: Context<'_>) -> &'static str {
    Locale::from_context(ctx).pick(
        "指令執行失敗，請稍後再試。",
        "That command failed — please try again shortly.",
    )
}

/// Best-effort ephemeral notice from an error path. If even this fails there is
/// nothing left to do but say so in the log.
async fn notify(ctx: Context<'_>, content: &str) {
    if let Err(err) = ephemeral(ctx, content).await {
        warn!("could not deliver an error message to the user: {err:?}");
    }
}
