use crate::aoe4world::SearchResult;
use crate::db::{
    add_reminder, bind_account, delete_reminder, list_all, list_reminder_needed, reminder_update_last_reminded,
    select_account,
};
use crate::ranked::{RankedPlayer, try_create_ranked_from_account, try_create_ranked_without_account};
use chrono::Utc;
use poise::futures_util::StreamExt;
use poise::futures_util::stream;
use rand::Rng;
use regex::Regex;
use reqwest::Url;
use serenity::all::{
    AutocompleteChoice, ChannelId, CreateMessage, EmojiId, GetMessages, Http, Message, Reaction, ReactionType, Ready,
    UserId,
};
use serenity::async_trait;
use serenity::json::json;
use serenity::model::id::GuildId;
use serenity::prelude::*;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use std::collections::HashMap;
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
        i64::from(aoe4_id),
    )
    .await
    .inspect_err(|_error| {
        error!("database insert failed");
    })?;
    ctx.say(message).await?;
    Ok(())
}

#[poise::command(slash_command)]
pub async fn rebuild(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let channel = ctx.guild_channel().await.unwrap();

    let regex = Regex::new(r"綁定discord帳號 `(?<user_id>[0-9]+)` 與世紀帝國四帳號 `(?<aoe4_id>[0-9]+)`").unwrap();

    let mut latest_message = channel.last_message_id.unwrap();
    let limit = 50;
    let mut messages = channel
        .messages(ctx.http(), GetMessages::new().before(latest_message).limit(limit))
        .await?;
    loop {
        info!("loading first batch, size {}", messages.len());
        for message in messages.iter() {
            let content = &message.content;
            latest_message = message.id;
            if let Some(cap) = regex.captures(content) {
                let user_id = cap["user_id"].parse::<i64>().unwrap();
                let aoe4_id = cap["aoe4_id"].parse::<i64>().unwrap();
                let msg = bind_account(&ctx.data().database, user_id, aoe4_id).await?;
                info!(msg);
            }
        }
        if messages.len() < limit as usize {
            break;
        }
        messages = channel
            .messages(ctx.http(), GetMessages::new().before(latest_message).limit(limit))
            .await?;
    }

    ctx.say("重建完成").await?;
    Ok(())
}

async fn auto_complete_id(_ctx: Context<'_>, username: &str) -> impl Iterator<Item = AutocompleteChoice> {
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
        i64::from(aoe4_id),
    )
    .await
    .inspect_err(|_error| {
        error!("database insert failed");
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
                    .inspect_err(|_error| {
                        error!("setting reminder failed");
                    })?
            } else {
                delete_reminder(&ctx.data().database, user_id)
                    .await
                    .inspect_err(|_error| {
                        error!("deleting reminder failed");
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

    let accounts = list_all(&data.database).await.inspect_err(|_error| {
        error!("database query failed");
    })?;
    let players = stream::iter(accounts)
        .filter_map(|account| try_create_ranked_from_account(http, data, account))
        .collect::<Vec<RankedPlayer>>()
        .await;
    let mut unique_players = players
        .into_iter()
        .fold(HashMap::new(), |mut acc, player| {
            acc.entry(String::from(player.discord_username()))
                .or_insert_with(Vec::new)
                .push(player);
            acc
        })
        .into_values()
        .filter_map(|mut list| {
            list.sort();
            let sorted = list;
            sorted.into_iter().reduce(|mut acc, player| {
                acc.append_alt(player);
                acc
            })
        })
        .collect::<Vec<RankedPlayer>>();
    info!("finish ranked player collection");

    unique_players.sort();
    let sorted_players = unique_players;
    info!("collected and sorted {} players", sorted_players.len());

    info!("clearing all existing messages in the channel");
    let messages = http
        .get_messages(RANK_CHANNEL_ID, None, None)
        .await
        .inspect_err(|_error| {
            error!("getting message from discord channel failed");
        })?;

    for message_id in messages.iter().map(|message| message.id) {
        http.delete_message(RANK_CHANNEL_ID, message_id, None)
            .await
            .inspect_err(|_error| {
                error!("deleting existing messages from discord failed");
            })?;
    }

    let mut buffer = String::new();
    for (i, player) in sorted_players.iter().enumerate() {
        let text = format!("第{}名  {}\n_ _\n", i + 1, player);
        buffer = buffer + &text;

        if i % 5 == 4 {
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
        if user
            .direct_message(
                &http,
                CreateMessage::new().content(format!("溫馨提醒：已經耍廢{}天囉 該爬天梯了！", days)),
            )
            .await
            .is_ok()
        {
            reminder_update_last_reminded(&data.database, reminder.user_id).await
        }
    }

    Ok(())
}

struct Emperor;

impl Emperor {
    fn select_emoji() -> ReactionType {
        let num = rand::rng().random_range(0..100);
        if num == 0 {
            ReactionType::from('🍄')
        } else if num <= 10 {
            ReactionType::from('🐷')
        } else {
            ReactionType::from(EmojiId::new(1299285258457448522))
        }
    }
}

#[async_trait]
impl EventHandler for Emperor {
    async fn message(&self, ctx: poise::serenity_prelude::Context, new_message: Message) {
        let emperor = UserId::new(453010726311821322);
        let knockgod = UserId::new(364796522396647424);
        let baltune = UserId::new(202510973519527937);
        let racoon = UserId::new(302663000463114242);
        let author = new_message.author.id;
        let content = &new_message.content;
        let mut blocked = false;
        if author == emperor
            || content.contains("天子")
            || content.contains("唱歌")
            || new_message.mentions_user_id(emperor)
        {
            blocked = Self::detect_blocked(new_message.react(&ctx.http, Emperor::select_emoji()).await);
        }
        if content.contains("那可")
            || content.contains("納可")
            || content.contains("knock")
            || new_message.mentions_user_id(knockgod)
        {
            blocked = Self::detect_blocked(
                new_message
                    .react(&ctx.http, ReactionType::from(EmojiId::new(1264746593366839431)))
                    .await,
            );
        }
        if content.contains("平等院") || content.contains("海門城堡") {
            blocked = Self::detect_blocked(
                new_message
                    .react(&ctx.http, ReactionType::from(EmojiId::new(1338936646615306250)))
                    .await,
            );
        }
        if content.contains("balt")
            || content.contains("Balt")
            || content.contains("包吞")
            || new_message.mentions_user_id(baltune)
        {
            blocked = Self::detect_blocked(
                new_message
                    .react(&ctx.http, ReactionType::from(EmojiId::new(1264326708962525225)))
                    .await,
            );
        }
        if content.contains("城主")
            || content.contains("成主")
            || (content.contains("all") && content.contains("in"))
            || content.contains("快攻")
            || content.contains("試煉")
            || content.contains("喝水")
            || content.contains("諸葛弩")
            || content.contains("議會廳")
            || content.contains("競技場")
            || content.contains("勝利塔")
            || content.contains("衝車")
            || content.contains("搓車")
        {
            blocked = Self::detect_blocked(new_message.react(&ctx.http, ReactionType::from('🦧')).await);
        }
        if content.contains("象") {
            blocked = Self::detect_blocked(new_message.react(&ctx.http, ReactionType::from('🐘')).await);
        }
        if author == racoon {
            blocked = Self::detect_blocked(new_message.react(&ctx.http, ReactionType::from('🦝')).await);
        }

        if blocked {
            let num = rand::rng().random_range(0..10);
            if num == 0 {
                let channel = ctx
                    .http
                    .get_channel(new_message.channel_id)
                    .await
                    .unwrap()
                    .guild()
                    .unwrap();
                channel.say(ctx.http, "<:emoji_93:1299285258457448522>").await.unwrap();
            }
        }
    }

    async fn ready(&self, _: poise::serenity_prelude::Context, ready: Ready) {
        info!("{} emperor bot is connected!", ready.user.name);
    }
}

impl Emperor {
    fn detect_blocked(result: serenity::Result<Reaction>) -> bool {
        match result {
            Ok(_) => false,
            Err(error) => {
                if let serenity::Error::Http(HttpError::UnsuccessfulRequest(error_response)) = error
                    && error_response.error.message == "Reaction blocked"
                {
                    // handle blocked reaction
                    return true;
                }
                false
            },
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    info!("starting app");

    // Get the discord token set in `Secrets.toml`
    let token = std::env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN must be set");
    // Get the guild_id set in `Secrets.toml`
    let guild_id: GuildId = std::env::var("GUILD_ID")
        .expect("GUILD_ID must be set")
        .parse()
        .expect("GUILD_ID must be a valid integer");

    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&db_url)
        .await
        .expect("failed to connect to DATABASE_URL");

    // Run the schema migration
    pool.execute(include_str!("../schema.sql"))
        .await
        .expect("failed to run migrations");

    let pool_cloned = pool.clone();
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![rebuild(), bind(), id(), name(), refresh(), check(), remind()],
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id).await?;
                Ok(Data {
                    database: pool_cloned,
                    guild_id,
                })
            })
        })
        .build();

    info!("prepared frameworks");

    let mut client = Client::builder(
        &token,
        GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT,
    )
    .framework(framework)
    .event_handler(Emperor)
    .await
    .expect("Err creating client");
    info!("prepared client");

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

    info!("starting serenity client");
    client.start().await.unwrap();
}

#[cfg(test)]
mod tests {
    use regex::Regex;
    use serenity::all::GatewayIntents;
    #[test]
    fn test_intents() {
        let intents = GatewayIntents::non_privileged();
        assert!(intents.guild_emojis_and_stickers());
        assert!(intents.guild_message_reactions());
        assert!(intents.guild_message_typing());
    }

    #[test]
    fn test_contains() {
        assert!(String::from("比那明居天子").contains("天子"))
    }

    #[test]
    fn test_regex() {
        let regex = Regex::new(r"綁定discord帳號 `(?<user_id>[0-9]+)` 與世紀帝國四帳號 `(?<aoe4_id>[0-9]+)`").unwrap();
        let hay = "綁定discord帳號 `182108123174010880` 與世紀帝國四帳號 `199837`";
        let result = regex.captures(hay);
        assert!(result.is_some());
        let cap = result.unwrap();
        assert_eq!("182108123174010880", &cap["user_id"]);
        assert_eq!("199837", &cap["aoe4_id"]);
    }
}
