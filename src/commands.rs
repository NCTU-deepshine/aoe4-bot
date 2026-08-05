use crate::aoe4world::search_players;
use crate::db::{bind_account, to_db_id};
use crate::guilds::{home_only, tournament_only};
use crate::ranked::try_create_ranked_without_account;
use crate::refresh::do_refresh;
use crate::reply::ephemeral;
use crate::tournament::access::{create_tournament_only, tournament_admin_only, tournament_manage_only};
use crate::tournament::db as tournament_db;
use crate::tournament::slug::{slugify, validate_slug};
use crate::tournament::{checkin, checkin_panel, panel, registration};
use crate::{Context, Data, Error};
use regex::Regex;
use serenity::all::{
    AutocompleteChoice, ChannelId, CreateChannel, GetMessages, GuildChannel, PermissionOverwrite,
    PermissionOverwriteType, Permissions, User,
};
use serenity::json::json;
use tracing::{error, info};

static INTERACTION_CHANNEL_ID: ChannelId = ChannelId::new(1263524546582020254);

pub(crate) type Command = poise::Command<Data, Error>;

/// The home guild's commands
pub(crate) fn home() -> Vec<Command> {
    vec![rebuild(), bind(), id(), name(), refresh(), check()]
}

/// The tournament guild's commands (§8.4).
pub(crate) fn tournament() -> Vec<Command> {
    vec![tournament_root()]
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

// `tournament_root` rather than `tournament`, which is already the name of the
// `crate::tournament` module and of this file's own list-returning `tournament()`
// above; `rename` keeps the Discord-visible command `/tournament` regardless.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    rename = "tournament",
    subcommands(
        "create",
        "admin",
        "register",
        "rebind",
        "withdraw",
        "open_checkin",
        "check_in",
        "close_checkin"
    ),
    subcommand_required
)]
pub async fn tournament_root(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

// Creates a tournament (docs/tournament.md §8.1): the invoking channel becomes
// its announce channel, and `#{slug}-register|bracket|draft|matches` are created
// alongside it in the invoking channel's existing category (or uncategorized, if
// the invoking channel has none). The creator is registered as the first admin.
/// Creates a tournament: makes its channels and registers you as the first admin.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "create_tournament_only",
    required_bot_permissions = "MANAGE_CHANNELS"
)]
pub async fn create(
    ctx: Context<'_>,
    #[description = "Display name (defaults to this channel's category name)"] name: Option<String>,
    #[description = "Channel prefix (defaults to a slug derived from the name)"] slug: Option<String>,
) -> Result<(), Error> {
    ctx.defer().await?;

    // guild_only + tournament_only already guarantee a real guild text channel,
    // the same shortcut rebuild() takes on the same guarantee.
    let announce_channel = ctx.guild_channel().await.unwrap();
    let category_id = announce_channel.parent_id;

    let name = match name.filter(|n| !n.trim().is_empty()) {
        Some(name) => name,
        None => match category_name(ctx, category_id).await? {
            Some(name) => name,
            None => {
                ephemeral(
                    ctx,
                    "This channel has no category to name the tournament after — please provide a name.",
                )
                .await?;
                return Ok(());
            },
        },
    };

    let slug = match slug.filter(|s| !s.trim().is_empty()) {
        Some(slug) => slug,
        None => match slugify(&name) {
            Some(slug) => slug,
            None => {
                ephemeral(
                    ctx,
                    "Couldn't derive a slug from that name — please provide one explicitly.",
                )
                .await?;
                return Ok(());
            },
        },
    };

    if let Err(err) = validate_slug(&slug) {
        ephemeral(ctx, err.message()).await?;
        return Ok(());
    }

    let pool = &ctx.data().database;
    if tournament_db::get_tournament_by_slug(pool, &slug).await?.is_some() {
        ephemeral(ctx, format!("A tournament with slug `{slug}` already exists.")).await?;
        return Ok(());
    }

    let read_only_to_everyone = PermissionOverwrite {
        allow: Permissions::empty(),
        deny: Permissions::SEND_MESSAGES,
        kind: PermissionOverwriteType::Role(ctx.guild_id().unwrap().everyone_role()),
    };

    let register = create_tournament_channel(ctx, &format!("{slug}-register"), category_id, vec![]).await?;
    let bracket = create_tournament_channel(
        ctx,
        &format!("{slug}-bracket"),
        category_id,
        vec![read_only_to_everyone.clone()],
    )
    .await?;
    let draft = create_tournament_channel(
        ctx,
        &format!("{slug}-draft"),
        category_id,
        vec![read_only_to_everyone.clone()],
    )
    .await?;
    let matches = create_tournament_channel(
        ctx,
        &format!("{slug}-matches"),
        category_id,
        vec![read_only_to_everyone],
    )
    .await?;

    let user_id = i64::try_from(ctx.author().id.get()).unwrap();
    let tournament_id = tournament_db::insert_tournament(pool, &slug, &name, user_id).await?;
    tournament_db::set_tournament_channels(
        pool,
        tournament_id,
        tournament_db::TournamentChannels {
            category_id: category_id.map(|id| i64::try_from(id.get()).unwrap()),
            announce_channel_id: i64::try_from(announce_channel.id.get()).unwrap(),
            register_channel_id: i64::try_from(register.id.get()).unwrap(),
            bracket_channel_id: i64::try_from(bracket.id.get()).unwrap(),
            matches_channel_id: i64::try_from(matches.id.get()).unwrap(),
            draft_channel_id: i64::try_from(draft.id.get()).unwrap(),
        },
    )
    .await?;
    tournament_db::add_admin(pool, tournament_id, user_id, user_id).await?;

    // Tournaments start in `registration` status immediately, with no separate
    // "open registration" command (docs/tournament.md §8.3) — so this is the only
    // place the panel can ever get posted.
    let register_message_id = panel::post_initial(ctx.http(), register.id, tournament_id, &name).await?;
    tournament_db::set_register_message_id(pool, tournament_id, i64::try_from(register_message_id.get()).unwrap())
        .await?;

    let category_note = if category_id.is_none() {
        "\n(This channel has no category, so the new channels are uncategorized.)"
    } else {
        ""
    };
    ctx.say(format!(
        "Created **{name}** (`{slug}`): <#{}> <#{}> <#{}> <#{}>{category_note}",
        register.id, bracket.id, draft.id, matches.id,
    ))
    .await?;
    Ok(())
}

async fn category_name(ctx: Context<'_>, category_id: Option<ChannelId>) -> Result<Option<String>, Error> {
    let Some(category_id) = category_id else {
        return Ok(None);
    };
    Ok(ctx
        .http()
        .get_channel(category_id)
        .await?
        .guild()
        .map(|channel| channel.name))
}

async fn create_tournament_channel(
    ctx: Context<'_>,
    name: &str,
    category_id: Option<ChannelId>,
    overwrites: Vec<PermissionOverwrite>,
) -> Result<GuildChannel, Error> {
    let mut builder = CreateChannel::new(name).permissions(overwrites);
    if let Some(category_id) = category_id {
        builder = builder.category(category_id);
    }
    Ok(ctx.guild_id().unwrap().create_channel(ctx.http(), builder).await?)
}

#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    subcommands("add", "remove", "list"),
    subcommand_required
)]
pub async fn admin(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Resolves the tournament for `/tournament admin *` from the invoking channel —
/// the same lookup `tournament_admin_only` already did to authorize the call, but
/// poise checks don't hand their result forward to the command body.
async fn resolve_tournament_by_channel(ctx: Context<'_>) -> Result<Option<tournament_db::Tournament>, Error> {
    let channel_id = i64::try_from(ctx.channel_id().get()).unwrap();
    Ok(tournament_db::get_tournament_by_any_channel_id(&ctx.data().database, channel_id).await?)
}

#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_admin_only"
)]
pub async fn add(ctx: Context<'_>, user: User) -> Result<(), Error> {
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        ephemeral(ctx, "This command must be run in one of the tournament's own channels.").await?;
        return Ok(());
    };

    let added_by = i64::try_from(ctx.author().id.get()).unwrap();
    let target = i64::try_from(user.id.get()).unwrap();
    tournament_db::add_admin(&ctx.data().database, tournament.id, target, added_by).await?;
    ephemeral(
        ctx,
        format!("Added {} as an admin for **{}**.", user.name, tournament.name),
    )
    .await?;
    Ok(())
}

#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_admin_only"
)]
pub async fn remove(ctx: Context<'_>, user: User) -> Result<(), Error> {
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        ephemeral(ctx, "This command must be run in one of the tournament's own channels.").await?;
        return Ok(());
    };

    let target = i64::try_from(user.id.get()).unwrap();
    tournament_db::remove_admin(&ctx.data().database, tournament.id, target).await?;
    ephemeral(
        ctx,
        format!("Removed {} as an admin for **{}**.", user.name, tournament.name),
    )
    .await?;
    Ok(())
}

#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_admin_only"
)]
pub async fn list(ctx: Context<'_>) -> Result<(), Error> {
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        ephemeral(ctx, "This command must be run in one of the tournament's own channels.").await?;
        return Ok(());
    };

    let admins = tournament_db::list_admins(&ctx.data().database, tournament.id).await?;
    let body = if admins.is_empty() {
        "No admins yet.".to_string()
    } else {
        admins
            .iter()
            .map(|a| format!("<@{}>", a.user_id))
            .collect::<Vec<_>>()
            .join("\n")
    };
    ephemeral(ctx, format!("Admins for **{}**:\n{body}", tournament.name)).await?;
    Ok(())
}

// Registers for the tournament resolved from the invoking channel (any of its
// five stored channels — see `resolve_tournament_by_channel`). A first sign-up
// also binds an aoe4world profile; there is no separate bind step
// (docs/tournament.md §8.5). Only ELO is snapshotted here — ATR is a bulk
// seeding-time fetch (chunk 11, §6 "Reuse"), not a per-registrant one.
/// Registers you for the tournament. Give `aoe4_id` only on your first ever sign-up.
#[poise::command(slash_command, guild_only, check = "tournament_only")]
pub async fn register(
    ctx: Context<'_>,
    #[description = "Your aoe4world profile — required on your first ever sign-up only"]
    #[autocomplete = "auto_complete_id"]
    aoe4_id: Option<i32>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        ephemeral(ctx, "This command must be run in one of the tournament's own channels.").await?;
        return Ok(());
    };

    let pool = &ctx.data().database;
    let user_id = i64::try_from(ctx.author().id.get()).unwrap();
    let outcome = registration::register(pool, &tournament, user_id, aoe4_id.map(i64::from)).await?;
    ephemeral(ctx, outcome.message(&tournament.name)).await?;

    if outcome.changed_state() {
        panel::refresh(ctx.http(), pool, &ctx.data().panel_throttle, &tournament).await?;
    }
    Ok(())
}

// Tournament-independent — the player list is global (§4), so unlike
// register/withdraw this resolves no tournament by channel.
/// Changes which aoe4world profile is bound to your Discord account.
#[poise::command(slash_command, guild_only, check = "tournament_only")]
pub async fn rebind(
    ctx: Context<'_>,
    #[description = "The aoe4world profile to bind instead"]
    #[autocomplete = "auto_complete_id"]
    aoe4_id: i32,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let user_id = i64::try_from(ctx.author().id.get()).unwrap();
    let outcome = registration::rebind(&ctx.data().database, user_id, i64::from(aoe4_id)).await?;
    ephemeral(ctx, outcome.message()).await?;
    Ok(())
}

/// Withdraws you from the tournament, before it has started.
#[poise::command(slash_command, guild_only, check = "tournament_only")]
pub async fn withdraw(ctx: Context<'_>) -> Result<(), Error> {
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        ephemeral(ctx, "This command must be run in one of the tournament's own channels.").await?;
        return Ok(());
    };

    let pool = &ctx.data().database;
    let user_id = i64::try_from(ctx.author().id.get()).unwrap();
    let outcome = registration::withdraw(pool, &tournament, user_id).await?;
    ephemeral(ctx, outcome.message(&tournament.name)).await?;

    if outcome.changed_state() {
        panel::refresh(ctx.http(), pool, &ctx.data().panel_throttle, &tournament).await?;
    }
    Ok(())
}

// Opens check-in for the tournament resolved from the invoking channel
// (docs/tournament.md §8.3) and posts the check-in panel to the register
// channel `/tournament create` made. `minutes` is purely informational —
// there is no cron closing check-in automatically; `/tournament close-checkin`
// stays a separate, explicit action (§11 follow-ups).
/// Opens check-in and posts its panel. `minutes`, if given, is shown as when it closes.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_manage_only",
    rename = "open-checkin"
)]
pub async fn open_checkin(
    ctx: Context<'_>,
    #[description = "Minutes until check-in closes — informational only; closing is still a separate command"]
    minutes: Option<i64>,
) -> Result<(), Error> {
    ctx.defer().await?;
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        ephemeral(ctx, "This command must be run in one of the tournament's own channels.").await?;
        return Ok(());
    };

    let pool = &ctx.data().database;
    let outcome = checkin::open(pool, &tournament, minutes).await?;

    if let checkin::OpenCheckinOutcome::Opened { closes_at } = outcome {
        // Always set by `create()` when the tournament was made.
        let register_channel_id = ChannelId::new(u64::try_from(tournament.register_channel_id.unwrap()).unwrap());
        let message_id = checkin_panel::post_initial(
            ctx.http(),
            pool,
            register_channel_id,
            tournament.id,
            &tournament.name,
            closes_at,
        )
        .await?;
        tournament_db::set_checkin_message_id(pool, tournament.id, i64::try_from(message_id.get()).unwrap()).await?;
    }

    ctx.say(outcome.message(&tournament.name)).await?;
    Ok(())
}

// `check_in` in Rust to avoid colliding with the `checkin` module import;
// `rename` keeps the Discord-visible command `/tournament checkin`.
/// Checks you in, once check-in has opened.
#[poise::command(slash_command, guild_only, check = "tournament_only", rename = "checkin")]
pub async fn check_in(ctx: Context<'_>) -> Result<(), Error> {
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        ephemeral(ctx, "This command must be run in one of the tournament's own channels.").await?;
        return Ok(());
    };

    let pool = &ctx.data().database;
    let user_id = i64::try_from(ctx.author().id.get()).unwrap();
    let outcome = checkin::checkin(pool, &tournament, user_id).await?;
    ephemeral(ctx, outcome.message(&tournament.name)).await?;

    if outcome.changed_state() {
        checkin_panel::refresh(ctx.http(), pool, &ctx.data().panel_throttle, &tournament).await?;
    }
    Ok(())
}

/// Closes check-in: marks no-shows and moves the tournament into seeding.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_manage_only",
    rename = "close-checkin"
)]
pub async fn close_checkin(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        ephemeral(ctx, "This command must be run in one of the tournament's own channels.").await?;
        return Ok(());
    };

    let pool = &ctx.data().database;
    let outcome = checkin::close(pool, &tournament).await?;

    if matches!(outcome, checkin::CloseCheckinOutcome::Closed { .. }) {
        checkin_panel::close(ctx.http(), pool, &tournament).await?;
    }

    ctx.say(outcome.message(&tournament.name)).await?;
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
