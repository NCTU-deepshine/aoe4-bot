use crate::{Context, Error};

/// Reply so that only the invoking user sees it.
///
/// The tournament feature leans on this heavily: a
/// registration or check-in press has to tell one person "you are already
/// registered" without narrating it to the channel. Ephemeral is ignored for
/// prefix commands, which is fine — every command here is a slash command.
pub(crate) async fn ephemeral(ctx: Context<'_>, content: impl Into<String>) -> Result<(), Error> {
    ctx.send(poise::CreateReply::default().content(content).ephemeral(true))
        .await?;
    Ok(())
}
