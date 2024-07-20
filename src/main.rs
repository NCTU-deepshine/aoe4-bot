use crate::db::{bind_account, list_all};
use crate::ranked::{try_create_ranked_from_account, RankedPlayer};
use anyhow::Context as _;
use poise::futures_util::stream;
use poise::futures_util::StreamExt;
use serenity::all::{ChannelId, ExecuteWebhook};
use serenity::model::id::GuildId;
use serenity::prelude::*;
use shuttle_runtime::SecretStore;
use sqlx::{Executor, PgPool};
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::{error, info};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

mod aoe4world;
mod db;
mod ranked;

static RANK_CHANNEL_ID: ChannelId = ChannelId::new(1263079883937153105);

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
    info!("attempting to bind {}", aoe4_id);
    let user_id = ctx.author().id;
    info!("binding discord user {} with aoe4 player {}", user_id, aoe4_id);
    let message = bind_account(
        &ctx.data().database,
        i64::try_from(u64::from(user_id)).unwrap(),
        i64::try_from(aoe4_id).unwrap(),
    )
    .await
    .map_err(|error| {
        error!("database insert failed");
        error
    })?;
    ctx.say(message).await?;
    Ok(())
}

#[poise::command(slash_command, guild_cooldown = 600)]
pub async fn refresh(ctx: Context<'_>) -> Result<(), Error> {
    info!("attempting to refresh");
    ctx.say("refresh triggered").await?;

    let accounts = list_all(&ctx.data().database).await.map_err(|error| {
        error!("database query failed");
        error
    })?;
    let mut players = stream::iter(accounts)
        .filter_map(|account| try_create_ranked_from_account(&ctx, account))
        .collect::<Vec<RankedPlayer>>()
        .await;
    players.sort();
    let sorted_players = players;
    info!("collected and sorted {} players", sorted_players.len());

    info!("clearing all existing messages in the channel");
    let messages = ctx
        .http()
        .get_messages(RANK_CHANNEL_ID, None, None)
        .await
        .map_err(|error| {
            error!("getting message from discord channel failed");
            error
        })?;

    for message_id in messages.iter().map(|message| message.id) {
        ctx.http()
            .delete_message(RANK_CHANNEL_ID, message_id, None)
            .await
            .map_err(|error| {
                error!("deleting existing messages from discord failed");
                error
            })?;
    }

    let mut buffer = String::new();
    for (i, player) in sorted_players.iter().enumerate() {
        let text = format!("第{}名  {}\n\n", i + 1, player);
        buffer = buffer + &text;

        if i % 10 == 9 {
            send_rankings(&ctx, &buffer).await?;
            buffer = String::new();
        }
    }

    if !buffer.is_empty() {
        send_rankings(&ctx, &buffer).await?;
    }

    ctx.say("refresh done").await?;
    Ok(())
}

async fn send_rankings(ctx: &Context<'_>, content: &String) -> Result<(), Error> {
    info!("attempt to write: {}", content);
    ctx.http()
        .get_channel(RANK_CHANNEL_ID)
        .await?
        .guild()
        .unwrap()
        .say(ctx.http(), content)
        .await?;
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

    let sched = JobScheduler::new().await.unwrap();
    sched.add(
        Job::new_async("0 * * * * *", move |_uuid, _l| Box::pin ({
            let token_cloned = token.clone();
            async move {
                let http = serenity::http::Http::new(&token_cloned);
                info!("refresh triggered by cron");
                let webhook = serenity::model::webhook::Webhook::from_url(&http, "https://discord.com/api/webhooks/1264316263052738613/rEh_lKsHiCdTnYg6f52uUBt9y0WWBd2oghOURMUtTdbGkeP_4p4ckTWI-L5KTQ75QwOl").await.unwrap();
                webhook.execute(&http, false, ExecuteWebhook::new().content("/refresh")).await.unwrap();
            }
        })).unwrap()
    ).await.unwrap();
    sched.start().await.unwrap();

    Ok(client.into())
}
