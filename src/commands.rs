use crate::aoe4world::search_players;
use crate::db::{bind_account, to_db_id};
use crate::guilds::home_only;
use crate::ranked::try_create_ranked_without_account;
use crate::refresh::do_refresh;
use crate::{Context, Data, Error};
use regex::Regex;
use serenity::all::{AutocompleteChoice, ChannelId, GetMessages};
use serenity::json::json;
use tracing::{error, info};

static INTERACTION_CHANNEL_ID: ChannelId = ChannelId::new(1263524546582020254);

pub(crate) type Command = poise::Command<Data, Error>;

/// The home guild's commands
pub(crate) fn home() -> Vec<Command> {
    vec![rebuild(), bind(), id(), name(), refresh(), check()]
}

/// The tournament guild's commands. Empty until §8.4's arrive — the list exists now
/// so registration is already split, and so a later chunk adding a tournament
/// command has an obvious place to put it that is not the home guild.
pub(crate) fn tournament() -> Vec<Command> {
    Vec::new()
}

#[poise::command(
    slash_command,
    guild_only,
    check = "home_only",
    subcommands("id", "name"),
    subcommand_required
)]
pub async fn bind(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, guild_only, check = "home_only")]
pub async fn id(ctx: Context<'_>, aoe4_id: i32) -> Result<(), Error> {
    info!("attempting to bind id {}", aoe4_id);
    let user_id = ctx.author().id;
    info!("binding discord user {} with aoe4 player {}", user_id, aoe4_id);
    let message = bind_account(&ctx.data().database, to_db_id(user_id), i64::from(aoe4_id))
        .await
        .inspect_err(|_error| {
            error!("database insert failed");
        })?;
    ctx.say(message).await?;
    Ok(())
}

#[poise::command(slash_command, guild_only, check = "home_only")]
pub async fn rebuild(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let channel = ctx.guild_channel().await.unwrap();

    let regex1 = Regex::new(r"綁定discord帳號 `(?<user_id>[0-9]+)` 與世紀帝國四帳號 `(?<aoe4_id>[0-9]+)`").unwrap();
    let regex2 =
        Regex::new(r"Bound discord user `(?<user_id>[0-9]+)` to aoe4 world profile `(?<aoe4_id>[0-9]+)`").unwrap();

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
            if let Some(cap) = regex1.captures(content) {
                let user_id = cap["user_id"].parse::<i64>().unwrap();
                let aoe4_id = cap["aoe4_id"].parse::<i64>().unwrap();
                let msg = bind_account(&ctx.data().database, user_id, aoe4_id).await?;
                info!(msg);
            }
            if let Some(cap) = regex2.captures(content) {
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
    let mut players = match search_players(username).await {
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

#[poise::command(slash_command, guild_only, check = "home_only")]
pub async fn name(
    ctx: Context<'_>,
    #[description = "遊戲ID"]
    #[autocomplete = "auto_complete_id"]
    aoe4_id: i32,
) -> Result<(), Error> {
    info!("attempting to bind id {}", aoe4_id);
    let user_id = ctx.author().id;
    info!("binding discord user {} with aoe4 player {}", user_id, aoe4_id);
    let message = bind_account(&ctx.data().database, to_db_id(user_id), i64::from(aoe4_id))
        .await
        .inspect_err(|_error| {
            error!("database insert failed");
        })?;
    ctx.say(message).await?;
    Ok(())
}

#[poise::command(slash_command, guild_only, check = "home_only", rename = "查分")]
pub async fn check(
    ctx: Context<'_>,
    #[description = "遊戲ID"]
    #[autocomplete = "auto_complete_id"]
    aoe4_id: i32,
) -> Result<(), Error> {
    info!("attempting to check id {}", aoe4_id);
    ctx.defer().await?;
    let Some(player) = try_create_ranked_without_account(aoe4_id).await else {
        info!("no ranked data for aoe4 id {}", aoe4_id);
        ctx.say("查不到這位玩家的單挑積分資料").await?;
        return Ok(());
    };
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

#[poise::command(slash_command, guild_only, check = "home_only")]
pub async fn refresh(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    do_refresh(ctx.http(), ctx.data()).await?;
    ctx.say("刷新完成").await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use regex::Regex;

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

    #[test]
    fn test_regex2() {
        let regex =
            Regex::new(r"Bound discord user `(?<user_id>[0-9]+)` to aoe4 world profile `(?<aoe4_id>[0-9]+)`").unwrap();
        let hay = "Bound discord user `380688858729414658` to aoe4 world profile `3763401`";
        let result = regex.captures(hay);
        assert!(result.is_some());
        let cap = result.unwrap();
        assert_eq!("380688858729414658", &cap["user_id"]);
        assert_eq!("3763401", &cap["aoe4_id"]);
    }
}
