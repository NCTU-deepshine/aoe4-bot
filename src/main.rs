use crate::db::{bind_account, list_all};
use crate::ranked::{try_create_ranked_from_account, RankedPlayer};
use anyhow::Context as _;
use poise::futures_util::stream;
use poise::futures_util::StreamExt;
use serenity::all::ChannelId;
use serenity::json::json;
use serenity::model::id::GuildId;
use serenity::prelude::*;
use shuttle_runtime::SecretStore;
use sqlx::{Executor, PgPool};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

mod aoe4world;
mod db;
mod ranked;

static RANK_CHANNEL_ID: ChannelId = ChannelId::new(1263079883937153105u64);

struct Data {
    database: PgPool,
    guild_id: GuildId,
}

#[poise::command(slash_command)]
async fn hello(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say("world!").await?;
    Ok(())
}

#[poise::command(slash_command, subcommands("id"), subcommand_required, aliases("綁定"))]
pub async fn bind(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command)]
pub async fn id(ctx: Context<'_>, aoe4_id: i32) -> Result<(), Error> {
    let user_id = ctx.author().id;
    ctx.say("bind!").await?;
    let message = bind_account(
        &ctx.data().database,
        i32::try_from(u64::from(user_id)).unwrap(),
        aoe4_id,
    )
    .await?;
    ctx.say(message).await?;
    Ok(())
}

#[poise::command(slash_command)]
pub async fn refresh(ctx: Context<'_>) -> Result<(), Error> {
    let accounts = list_all(&ctx.data().database).await?;
    let mut players = stream::iter(accounts)
        .filter_map(|account| try_create_ranked_from_account(&ctx, account))
        .collect::<Vec<RankedPlayer>>()
        .await;
    players.sort();
    let sorted_players = players;

    // clear all existing messages in the channel
    let messages = ctx.http().get_messages(RANK_CHANNEL_ID, None, None).await?;
    let message_ids = messages.iter().map(|message| message.id).collect::<Vec<_>>();
    ctx.http()
        .delete_messages(RANK_CHANNEL_ID, &json!(&message_ids), None)
        .await?;

    for (i, player) in sorted_players.iter().enumerate() {
        let text = format!("第{}名  {}", i, player);
        ctx.say(text).await?;
    }

    Ok(())
}

#[shuttle_runtime::main]
async fn serenity(
    #[shuttle_shared_db::Postgres] pool: PgPool,
    #[shuttle_runtime::Secrets] secret_store: SecretStore,
) -> shuttle_serenity::ShuttleSerenity {
    // Get the discord token set in `Secrets.toml`
    let token = secret_store
        .get("DISCORD_TOKEN")
        .context("'DISCORD_TOKEN' was not found")?;
    // Get the guild_id set in `Secrets.toml`
    let guild_id: GuildId = secret_store
        .get("GUILD_ID")
        .context("'GUILD_ID' was not found")?
        .parse()
        .unwrap();

    // Run the schema migration
    pool.execute(include_str!("../schema.sql"))
        .await
        .context("failed to run migrations")?;

    let data = Data {
        database: pool,
        guild_id,
    };

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![hello(), bind(), id(), refresh()],
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id.clone()).await?;
                Ok(data)
            })
        })
        .build();

    let client = Client::builder(&token, GatewayIntents::empty())
        .framework(framework)
        .await
        .expect("Err creating client");

    Ok(client.into())
}
