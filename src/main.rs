use crate::aoe4world::SearchResult;
use crate::db::{
    add_reminder, bind_account, delete_reminder, list_all, list_reminder_needed, reminder_update_last_reminded,
    select_account,
};
use crate::ranked::{try_create_ranked_from_account, try_create_ranked_without_account, RankedPlayer};
use anyhow::Context as _;
use chrono::Utc;
use poise::futures_util::stream;
use poise::futures_util::StreamExt;
use reqwest::Url;
use serenity::all::{AutocompleteChoice, ChannelId, CreateMessage, Http, UserId};
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
static INTERACTION_CHANNEL_ID: ChannelId = ChannelId::new(1263524546582020254);

struct Data {
    database: PgPool,
    guild_id: GuildId,
}

#[poise::command(slash_command)]
async fn hello(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say("world!").await?;
    Ok(())
}

#[poise::command(slash_command, subcommands("id", "name"), subcommand_required)]
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
    let mut players = match get_profiles(username).await {
        None => vec![],
        Some(profiles) => profiles.players,
    };
    players.sort();
    players
        .into_iter()
        .filter_map(|player| {
            let data = player.leaderboards.rm_solo?;
            Some(AutocompleteChoice::new(
                format!("{} - 階級: {}, 積分: {}", player.name, data.rank_level(), data.rating()),
                json!(player.profile_id),
            ))
        })
        .take(10)
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

#[poise::command(slash_command, rename = "查分")]
pub async fn check(
    ctx: Context<'_>,
    #[description = "遊戲ID"]
    #[autocomplete = "auto_complete_id"]
    aoe4_id: i32,
) -> Result<(), Error> {
    info!("attempting to check id {}", aoe4_id);
    ctx.defer().await?;
    let player = try_create_ranked_without_account(aoe4_id)
        .await
        .expect("unexpected missing ranked player");
    let info = player.info();
    ctx.http()
        .get_channel(INTERACTION_CHANNEL_ID)
        .await?
        .guild()
        .unwrap()
        .say(ctx.http(), info)
        .await?;
    ctx.say("查分成功").await?;
    Ok(())
}

#[poise::command(slash_command, rename = "提醒")]
pub async fn remind(ctx: Context<'_>, #[description = "警告天數"] days: i32) -> Result<(), Error> {
    info!(
        "attempting to set {} days reminder for {}",
        days,
        ctx.cache().current_user().name
    );
    let user_id = i64::try_from(u64::from(ctx.author().id)).unwrap();
    match select_account(&ctx.data().database, user_id).await {
        None => {
            ctx.say("需要先綁定天梯榜才能夠使用提醒功能！").await?;
            Ok(())
        },
        Some(_) => {
            let message = if days > 0 {
                add_reminder(&ctx.data().database, user_id, days)
                    .await
                    .map_err(|error| {
                        error!("setting reminder failed");
                        error
                    })?
            } else {
                delete_reminder(&ctx.data().database, user_id).await.map_err(|error| {
                    error!("deleting reminder failed");
                    error
                })?
            };
            ctx.say(message).await?;
            Ok(())
        },
    }
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

async fn send_reminders(http: &Http, data: &Data) -> Result<(), Error> {
    info!("starting to send reminders");
    let reminders = list_reminder_needed(&data.database).await;
    for reminder in reminders.iter() {
        let user = http.get_user(UserId::new(reminder.user_id as u64)).await?;
        let days = Utc::now().signed_duration_since(reminder.last_played).num_days();
        match user
            .direct_message(
                &http,
                CreateMessage::new().content(format!("溫馨提醒：已經耍廢{}天囉 該爬天梯了！", days)),
            )
            .await
        {
            Ok(_) => reminder_update_last_reminded(&data.database, reminder.user_id).await,
            Err(_) => {},
        }
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

    let pool_cloned = pool.clone();
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![hello(), bind(), id(), name(), refresh(), check(), remind()],
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
                        send_reminders(&http, &data).await.unwrap();
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
