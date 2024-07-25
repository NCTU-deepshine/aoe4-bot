use crate::aoe4world::SearchResult;
use crate::db::{bind_account, list_all};
use crate::ranked::{try_create_ranked_from_account, RankedPlayer};
use anyhow::Context as _;
use poise::futures_util::stream;
use poise::futures_util::StreamExt;
use reqwest::Url;
use serenity::all::{AutocompleteChoice, ChannelId, Http};
use serenity::json::json;
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

#[poise::command(slash_command, subcommands("id", "name"), subcommand_required, aliases("綁定"))]
pub async fn bind(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command)]
pub async fn id(ctx: Context<'_>, aoe4_id: i32) -> Result<(), Error> {
    info!("attempting to bind id {}", aoe4_id);
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

async fn auto_complete_id<'a>(_ctx: Context<'_>, username: &'a str) -> impl Iterator<Item = AutocompleteChoice> {
    info!("search aoe4 world profiles with username {}", username);
    let players = match get_profiles(username).await {
        None => vec![],
        Some(profiles) => profiles.players,
    };
    players.into_iter().filter_map(|player| {
        let data = player.leaderboards.rm_solo?;
        Some(AutocompleteChoice::new(
            format!("{} - 階級: {}, 積分: {}", player.name, data.rank_level(), data.rating),
            json!(player.profile_id),
        ))
    })
}

async fn get_profiles(username: &str) -> Option<SearchResult> {
    let mut url = Url::parse("https://aoe4world.com/api/v0/players/search").unwrap();
    url.query_pairs_mut().append_pair("query", username);
    let profiles = reqwest::get(url).await.ok()?.json::<SearchResult>().await.ok()?;
    Some(profiles)
}

#[poise::command(slash_command)]
pub async fn name(
    ctx: Context<'_>,
    #[description = "遊戲ID"]
    #[autocomplete = "auto_complete_id"]
    aoe4_id: i32,
) -> Result<(), Error> {
    info!("attempting to bind id {}", aoe4_id);
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

#[poise::command(slash_command)]
pub async fn refresh(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    do_refresh(ctx.http(), ctx.data()).await?;
    ctx.say("刷新完成").await?;
    Ok(())
}

async fn do_refresh(http: &Http, data: &Data) -> Result<(), Error> {
    info!("attempting to refresh");

    let accounts = list_all(&data.database).await.map_err(|error| {
        error!("database query failed");
        error
    })?;
    let mut players = stream::iter(accounts)
        .filter_map(|account| try_create_ranked_from_account(http, data, account))
        .collect::<Vec<RankedPlayer>>()
        .await;
    info!("finish ranked player collection");
    players.sort();
    let sorted_players = players;
    info!("collected and sorted {} players", sorted_players.len());

    info!("clearing all existing messages in the channel");
    let messages = http.get_messages(RANK_CHANNEL_ID, None, None).await.map_err(|error| {
        error!("getting message from discord channel failed");
        error
    })?;

    for message_id in messages.iter().map(|message| message.id) {
        http.delete_message(RANK_CHANNEL_ID, message_id, None)
            .await
            .map_err(|error| {
                error!("deleting existing messages from discord failed");
                error
            })?;
    }

    let mut buffer = String::new();
    for (i, player) in sorted_players.iter().enumerate() {
        let text = format!("第{}名  {}\n　\n", i + 1, player);
        buffer = buffer + &text;

        if i % 10 == 9 {
            send_rankings(http, &buffer).await?;
            buffer = String::new();
        }
    }

    if !buffer.is_empty() {
        send_rankings(http, &buffer).await?;
    }

    Ok(())
}
async fn send_rankings(http: &Http, content: &String) -> Result<(), Error> {
    http.get_channel(RANK_CHANNEL_ID)
        .await?
        .guild()
        .unwrap()
        .say(http, content)
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

    let pool_cloned = pool.clone();
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![hello(), bind(), id(), name(), refresh()],
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id.clone()).await?;
                Ok(Data {
                    database: pool_cloned,
                    guild_id,
                })
            })
        })
        .build();

    let client = Client::builder(&token, GatewayIntents::empty())
        .framework(framework)
        .await
        .expect("Err creating client");

    let sched = JobScheduler::new().await.unwrap();
    sched
        .add(
            Job::new_async("0 0 0,12 * * *", move |_uuid, _l| {
                Box::pin({
                    let token_cloned = token.clone();
                    let pool_cloned = pool.clone();
                    async move {
                        let http = Http::new(&token_cloned);
                        let data = Data {
                            database: pool_cloned,
                            guild_id,
                        };
                        info!("refresh triggered by cron");
                        do_refresh(&http, &data).await.unwrap();
                    }
                })
            })
            .unwrap(),
        )
        .await
        .unwrap();
    sched.start().await.unwrap();

    Ok(client.into())
}
