use crate::aoe4world::search_players;
use crate::db::{bind_account, to_db_id};
use crate::guilds::{home_only, tournament_only};
use crate::locale::Locale;
use crate::ranked::try_create_ranked_without_account;
use crate::refresh::do_refresh;
use crate::reply::ephemeral;
use crate::tournament::access::{
    create_tournament_only, tournament_admin_only, tournament_manage_only, wrong_channel_message,
};
use crate::tournament::db as tournament_db;
use crate::tournament::slug::{slugify, validate_slug};
use crate::tournament::{
    audit, bracket_view, checkin, checkin_panel, panel, registration, seed_panel, seeding, setup as tournament_setup,
    start as tournament_start, teardown,
};
use crate::{Context, Data, Error};
use chrono::{DateTime, FixedOffset, NaiveDateTime, Utc};
use regex::Regex;
use serenity::all::{
    AutocompleteChoice, ChannelId, CreateChannel, GetMessages, GuildChannel, MessageId, PermissionOverwrite,
    PermissionOverwriteType, Permissions, RoleId, User, UserId,
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

/// The value a hint choice carries. Discord requires a choice's value to match
/// the option's type, so the hint cannot be valueless; callers treat this as
/// "nothing was actually picked" rather than looking up profile id 0.
pub(crate) const NO_PROFILE_PICKED: i32 = 0;

/// The hint choice is selectable like any other, so every caller has to treat it
/// as "nothing was entered" rather than as profile id 0 — which would otherwise
/// bind someone to a profile that does not exist.
fn picked_profile(aoe4_id: i32) -> Option<i32> {
    (aoe4_id != NO_PROFILE_PICKED).then_some(aoe4_id)
}

/// Shared by every command using `auto_complete_id`, so submitting the hint says
/// the same thing whichever guild you are in.
async fn ask_for_in_game_name(ctx: Context<'_>) -> Result<(), Error> {
    ephemeral(
        ctx,
        Locale::from_context(ctx).pick(
            "請輸入你的遊戲名稱，然後從清單中選擇。",
            "Type your in-game name, then pick it from the list.",
        ),
    )
    .await
}

async fn auto_complete_id(ctx: Context<'_>, username: &str) -> impl Iterator<Item = AutocompleteChoice> {
    // Discord fires an autocomplete the moment the option is focused, before a
    // single keystroke. Searching for "" returns nothing useful, so the empty
    // dropdown used to appear at exactly the moment the user needed telling what
    // to type — and cost a request to say nothing.
    if username.trim().is_empty() {
        let hint = Locale::from_context(ctx).pick("請輸入你的遊戲名稱…", "Type your in-game name…");
        return vec![AutocompleteChoice::new(hint, json!(NO_PROFILE_PICKED))].into_iter();
    }

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
        .collect::<Vec<_>>()
        .into_iter()
}

#[poise::command(slash_command, guild_only, check = "home_only")]
pub async fn name(
    ctx: Context<'_>,
    #[description = "Type your in-game name and pick yourself from the list"]
    #[description_localized("zh-TW", "輸入你的遊戲內名稱，然後從清單中選擇自己")]
    #[autocomplete = "auto_complete_id"]
    in_game_name: i32,
) -> Result<(), Error> {
    let Some(aoe4_id) = picked_profile(in_game_name) else {
        ask_for_in_game_name(ctx).await?;
        return Ok(());
    };
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
    #[description = "Type an in-game name and pick the player from the list"]
    #[description_localized("zh-TW", "輸入遊戲內名稱，然後從清單中選擇玩家")]
    #[autocomplete = "auto_complete_id"]
    in_game_name: i32,
) -> Result<(), Error> {
    let Some(aoe4_id) = picked_profile(in_game_name) else {
        ask_for_in_game_name(ctx).await?;
        return Ok(());
    };
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
        "unbind",
        "withdraw",
        "open_checkin",
        "check_in",
        "close_checkin",
        "reopen_registration",
        "start",
        "refresh_panels",
        "setup",
        "preset",
        "seed",
        "delete"
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
    let locale = Locale::from_context(ctx);
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
                    locale.pick(
                        "這個頻道沒有分類可以用來命名賽事 — 請直接提供名稱。",
                        "This channel has no category to name the tournament after — please provide a name.",
                    ),
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
                    locale.pick(
                        "無法從這個名稱產生簡稱 — 請自行指定一個。",
                        "Couldn't derive a slug from that name — please provide one explicitly.",
                    ),
                )
                .await?;
                return Ok(());
            },
        },
    };

    if let Err(err) = validate_slug(&slug) {
        ephemeral(ctx, err.message(locale)).await?;
        return Ok(());
    }

    let pool = &ctx.data().database;
    if tournament_db::get_tournament_by_slug(pool, &slug).await?.is_some() {
        ephemeral(
            ctx,
            locale.pick(
                format!("簡稱 `{slug}` 已經有其他賽事在使用。"),
                format!("A tournament with slug `{slug}` already exists."),
            ),
        )
        .await?;
        return Ok(());
    }

    let read_only = read_only_overwrites(ctx.guild_id().unwrap().everyone_role(), ctx.cache().current_user().id);

    let register = create_tournament_channel(ctx, &format!("{slug}-register"), category_id, vec![]).await?;
    let bracket = create_tournament_channel(ctx, &format!("{slug}-bracket"), category_id, read_only.clone()).await?;
    let draft = create_tournament_channel(ctx, &format!("{slug}-draft"), category_id, read_only.clone()).await?;
    let matches = create_tournament_channel(ctx, &format!("{slug}-matches"), category_id, read_only).await?;

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
    // The cap is `not null default 32`, so the panel can show it from the start
    // even though `/tournament setup` has not run yet.
    let cap = tournament_db::get_tournament(pool, tournament_id)
        .await?
        .map_or(32, |t| t.entrant_cap);
    let register_message_id = panel::post_initial(ctx.http(), register.id, tournament_id, &name, cap).await?;
    tournament_db::set_register_message_id(pool, tournament_id, i64::try_from(register_message_id.get()).unwrap())
        .await?;

    // The record a later `/tournament delete` is audited against: what existed,
    // and which channels were ours to remove.
    info!(
        "created tournament {tournament_id} ({slug}) \"{name}\" by {} ({}) \
         with channels register={} bracket={} draft={} matches={}",
        ctx.author().name,
        ctx.author().id,
        register.id,
        bracket.id,
        draft.id,
        matches.id,
    );

    let category_note = if category_id.is_none() {
        locale.pick(
            "\n（這個頻道沒有分類，所以新頻道不屬於任何分類。）",
            "\n(This channel has no category, so the new channels are uncategorized.)",
        )
    } else {
        ""
    };
    ctx.say(locale.pick(
        format!(
            "已建立 **{name}**（`{slug}`）：<#{}> <#{}> <#{}> <#{}>{category_note}",
            register.id, bracket.id, draft.id, matches.id,
        ),
        format!(
            "Created **{name}** (`{slug}`): <#{}> <#{}> <#{}> <#{}>{category_note}",
            register.id, bracket.id, draft.id, matches.id,
        ),
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

/// The overwrites for an output channel (§8.1): `@everyone` may read but not
/// post — **and the bot may post**.
///
/// The second half is not redundant. A deny on `@everyone` applies to the bot as
/// much as to anyone else, and without an allow of its own every panel and
/// bracket post into these channels fails with 403 Missing Permissions. That is
/// exactly what happened in production: the bracket preview never worked once,
/// and the failure was only visible in the logs because the redraw is
/// best-effort.
fn read_only_overwrites(everyone: RoleId, bot: UserId) -> Vec<PermissionOverwrite> {
    vec![
        PermissionOverwrite {
            allow: Permissions::empty(),
            deny: Permissions::SEND_MESSAGES,
            kind: PermissionOverwriteType::Role(everyone),
        },
        PermissionOverwrite {
            allow: Permissions::SEND_MESSAGES,
            deny: Permissions::empty(),
            kind: PermissionOverwriteType::Member(bot),
        },
    ]
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
/// Resolves the tournament from the invoking channel, **and answers when there
/// isn't one** — every caller refused identically, so the reply lives here rather
/// than in eight copies. `None` therefore means "already handled, just return".
async fn resolve_tournament_by_channel(ctx: Context<'_>) -> Result<Option<tournament_db::Tournament>, Error> {
    let channel_id = i64::try_from(ctx.channel_id().get()).unwrap();
    let tournament = tournament_db::get_tournament_by_any_channel_id(&ctx.data().database, channel_id).await?;
    if tournament.is_none() {
        ephemeral(ctx, wrong_channel_message(Locale::from_context(ctx))).await?;
    }
    Ok(tournament)
}

#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_admin_only"
)]
pub async fn add(ctx: Context<'_>, user: User) -> Result<(), Error> {
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    let added_by = i64::try_from(ctx.author().id.get()).unwrap();
    let target = i64::try_from(user.id.get()).unwrap();
    tournament_db::add_admin(&ctx.data().database, tournament.id, target, added_by).await?;
    info!(
        "admin add on tournament {} ({}) by {} ({added_by}): added {} ({target})",
        tournament.id,
        tournament.slug,
        ctx.author().name,
        user.name
    );
    ephemeral(
        ctx,
        locale.pick(
            format!("已將 {} 加入成為 **{}** 的管理員。", user.name, tournament.name),
            format!("Added {} as an admin for **{}**.", user.name, tournament.name),
        ),
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
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    let removed_by = i64::try_from(ctx.author().id.get()).unwrap();
    let target = i64::try_from(user.id.get()).unwrap();
    tournament_db::remove_admin(&ctx.data().database, tournament.id, target).await?;
    info!(
        "admin remove on tournament {} ({}) by {} ({removed_by}): removed {} ({target})",
        tournament.id,
        tournament.slug,
        ctx.author().name,
        user.name
    );
    ephemeral(
        ctx,
        locale.pick(
            format!("已將 {} 從 **{}** 的管理員列表中移除。", user.name, tournament.name),
            format!("Removed {} as an admin for **{}**.", user.name, tournament.name),
        ),
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
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    let admins = tournament_db::list_admins(&ctx.data().database, tournament.id).await?;
    let body = if admins.is_empty() {
        locale.pick("目前沒有管理員。", "No admins yet.").to_string()
    } else {
        admins
            .iter()
            .map(|a| format!("<@{}>", a.user_id))
            .collect::<Vec<_>>()
            .join("\n")
    };
    ephemeral(
        ctx,
        locale.pick(
            format!("**{}** 的管理員：\n{body}", tournament.name),
            format!("Admins for **{}**:\n{body}", tournament.name),
        ),
    )
    .await?;
    Ok(())
}

// Registers for the tournament resolved from the invoking channel (any of its
// five stored channels — see `resolve_tournament_by_channel`). A first sign-up
// also binds an aoe4world profile; there is no separate bind step
// (docs/tournament.md §8.5). Only ELO is snapshotted here — ATR is a bulk
// seeding-time fetch (chunk 11, §6 "Reuse"), not a per-registrant one.
/// Registers you for the tournament. First time? Type your in-game name below.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    description_localized("zh-TW", "報名這場賽事。第一次報名的話，請在下面輸入你的遊戲名稱。")
)]
pub async fn register(
    ctx: Context<'_>,
    #[description = "Type your in-game name and pick yourself from the list — first sign-up only"]
    #[description_localized("zh-TW", "輸入你的遊戲名稱，然後從清單中選擇自己 — 只有第一次報名需要")]
    #[autocomplete = "auto_complete_id"]
    in_game_name: Option<i32>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    let pool = &ctx.data().database;
    let user_id = i64::try_from(ctx.author().id.get()).unwrap();
    let picked = in_game_name.and_then(picked_profile);
    let outcome = registration::register(pool, &tournament, user_id, picked.map(i64::from)).await?;
    audit::log_action("register", tournament.id, &tournament.slug, ctx.author(), &outcome);
    ephemeral(ctx, outcome.message(&tournament.name, locale)).await?;

    if outcome.changed_state() {
        if let Some(entry) = tournament_db::get_entry(pool, tournament.id, user_id).await? {
            registration::snapshot_entry_elo(pool, tournament.id, user_id, entry.aoe4_id).await;
        }
        panel::refresh(ctx.http(), pool, &ctx.data().panel_throttle, &tournament).await?;
        bracket_view::reconcile(ctx.http(), pool, &tournament).await?;
    }
    Ok(())
}

// Tournament-independent — the player list is global (§4), so unlike
// register/withdraw this resolves no tournament by channel.
/// Changes which game account your Discord account is linked to.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    description_localized("zh-TW", "更換你的 Discord 帳號所連結的遊戲帳號。")
)]
pub async fn rebind(
    ctx: Context<'_>,
    #[description = "Type the in-game name to link instead, and pick it from the list"]
    #[description_localized("zh-TW", "輸入要改連結的遊戲名稱，然後從清單中選擇")]
    #[autocomplete = "auto_complete_id"]
    in_game_name: i32,
) -> Result<(), Error> {
    let locale = Locale::from_context(ctx);
    ctx.defer_ephemeral().await?;
    let Some(in_game_name) = picked_profile(in_game_name) else {
        ask_for_in_game_name(ctx).await?;
        return Ok(());
    };
    let user_id = i64::try_from(ctx.author().id.get()).unwrap();
    let outcome = registration::rebind(&ctx.data().database, user_id, i64::from(in_game_name)).await?;
    // No tournament to name — the player list is global (see the note above).
    info!(
        "rebind by {} ({user_id}) to aoe4 id {in_game_name}: {outcome:?}",
        ctx.author().name
    );
    ephemeral(ctx, outcome.message(locale)).await?;
    Ok(())
}

// Tournament-independent, like `rebind` — the player list is global (§4). Kept
// self-service: it only ever clears the caller's own binding.
/// Unlinks your game account, so your next sign-up starts from scratch.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    description_localized("zh-TW", "解除你目前連結的遊戲帳號，下次報名時重新選擇。")
)]
pub async fn unbind(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let locale = Locale::from_context(ctx);
    let user_id = i64::try_from(ctx.author().id.get()).unwrap();

    let outcome = registration::unbind(&ctx.data().database, user_id).await?;
    info!("unbind by {} ({user_id}): {outcome:?}", ctx.author().name);
    ephemeral(ctx, outcome.message(locale)).await?;
    Ok(())
}

/// Withdraws you from the tournament, before it has started.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    description_localized("zh-TW", "在賽事開始前退出報名。")
)]
pub async fn withdraw(ctx: Context<'_>) -> Result<(), Error> {
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    let pool = &ctx.data().database;
    let user_id = i64::try_from(ctx.author().id.get()).unwrap();
    let outcome = registration::withdraw(pool, &tournament, user_id).await?;
    audit::log_action("withdraw", tournament.id, &tournament.slug, ctx.author(), &outcome);
    ephemeral(ctx, outcome.message(&tournament.name, locale)).await?;

    if outcome.changed_state() {
        panel::refresh(ctx.http(), pool, &ctx.data().panel_throttle, &tournament).await?;
        bracket_view::reconcile(ctx.http(), pool, &tournament).await?;
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
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    let pool = &ctx.data().database;
    let outcome = checkin::open(pool, &tournament, minutes).await?;
    audit::log_action("open-checkin", tournament.id, &tournament.slug, ctx.author(), &outcome);

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
        tournament_db::set_checkin_message_id(pool, tournament.id, Some(i64::try_from(message_id.get()).unwrap()))
            .await?;

        // Registration closes here (§8.3), so the panel must stop inviting
        // sign-ups the gate would now refuse. Re-read: the status has moved.
        let tournament = tournament_db::get_tournament(pool, tournament.id).await?.unwrap();
        panel::refresh_now(ctx.http(), pool, &tournament).await?;
    }

    ctx.say(outcome.message(&tournament.name, locale)).await?;
    Ok(())
}

// `check_in` in Rust to avoid colliding with the `checkin` module import;
// `rename` keeps the Discord-visible command `/tournament checkin`.
/// Checks you in, once check-in has opened.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    rename = "checkin",
    description_localized("zh-TW", "在簽到開放後完成簽到。")
)]
pub async fn check_in(ctx: Context<'_>) -> Result<(), Error> {
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    let pool = &ctx.data().database;
    let user_id = i64::try_from(ctx.author().id.get()).unwrap();
    let outcome = checkin::checkin(pool, &tournament, user_id).await?;
    audit::log_action("checkin", tournament.id, &tournament.slug, ctx.author(), &outcome);
    ephemeral(ctx, outcome.message(&tournament.name, locale)).await?;

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
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    let pool = &ctx.data().database;
    let outcome = checkin::close(pool, &tournament).await?;
    audit::log_action("close-checkin", tournament.id, &tournament.slug, ctx.author(), &outcome);

    if !matches!(outcome, checkin::CloseCheckinOutcome::Closed { .. }) {
        ctx.say(outcome.message(&tournament.name, locale)).await?;
        return Ok(());
    }

    checkin_panel::close(ctx.http(), pool, &tournament).await?;
    let seeded = seed_and_post_panel(ctx, &tournament, locale).await?;
    ctx.say(format!("{}\n{seeded}", outcome.message(&tournament.name, locale)))
        .await?;
    Ok(())
}

/// Fetches ratings, writes the suggested order and posts the seeding panel,
/// returning the line to append to the caller's reply.
///
/// **Best-effort by design.** By the time this runs the tournament has already
/// advanced to `seeding`, so an aoe4world outage must not fail the command and
/// strand the lifecycle — it seeds from whatever ratings are stored, says so, and
/// points at `/tournament seed refresh` (§6).
async fn seed_and_post_panel(
    ctx: Context<'_>,
    tournament: &tournament_db::Tournament,
    locale: Locale,
) -> Result<String, Error> {
    let pool = &ctx.data().database;
    let outcome = seeding::refresh_ratings(pool, tournament).await?;
    audit::log_action("seed", tournament.id, &tournament.slug, ctx.author(), &outcome);

    // Always set by `create()`; the panel has nowhere to go without it.
    let Some(bracket_channel_id) = tournament.bracket_channel_id else {
        return Ok(String::new());
    };
    let channel_id = ChannelId::new(u64::try_from(bracket_channel_id).unwrap());

    // A tournament reopened and re-closed already has a panel; edit rather than
    // stacking a second one in the channel.
    if tournament.seed_message_id.is_some() {
        seed_panel::refresh(ctx.http(), pool, tournament).await?;
    } else {
        let message_id =
            seed_panel::post_initial(ctx.http(), pool, channel_id, tournament.id, &tournament.name).await?;
        tournament_db::set_seed_message_id(pool, tournament.id, Some(i64::try_from(message_id.get()).unwrap())).await?;
    }

    Ok(outcome.message(&tournament.name, locale))
}

// The one backward lifecycle edge (docs/tournament.md §8.3), for a check-in
// opened too early or closed too soon. A full reset rather than a partial undo:
// the check-in panel is deleted outright, so the next `/tournament open-checkin`
// posts a fresh one instead of orphaning the old message behind a new id.
/// Reopens registration: undoes check-in and puts the tournament back in registration.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_manage_only",
    rename = "reopen-registration"
)]
pub async fn reopen_registration(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    let pool = &ctx.data().database;
    let outcome = checkin::reopen_registration(pool, &tournament).await?;
    audit::log_action(
        "reopen-registration",
        tournament.id,
        &tournament.slug,
        ctx.author(),
        &outcome,
    );

    if outcome.changed_state() {
        // `tournament` is the pre-reset snapshot, so it still carries the
        // message ids the row no longer does.
        delete_checkin_panel(ctx, &tournament).await;
        delete_seed_panel(ctx, &tournament).await;
        panel::refresh_now(ctx.http(), pool, &tournament).await?;
    }

    ctx.say(outcome.message(&tournament.name, locale)).await?;
    Ok(())
}

/// Best-effort: the database reset has already committed, and an admin who
/// deleted the panel by hand should not turn a successful reopen into a failure.
async fn delete_checkin_panel(ctx: Context<'_>, tournament: &tournament_db::Tournament) {
    let (Some(checkin_message_id), Some(register_channel_id)) =
        (tournament.checkin_message_id, tournament.register_channel_id)
    else {
        return;
    };

    let channel_id = ChannelId::new(u64::try_from(register_channel_id).unwrap());
    let message_id = MessageId::new(u64::try_from(checkin_message_id).unwrap());
    if let Err(err) = channel_id.delete_message(ctx.http(), message_id).await {
        error!(
            "failed to delete the check-in panel for tournament {}: {err:?}",
            tournament.id
        );
    }
}

/// Which rounds a preset assignment covers, as a distance back from the final
/// (docs/tournament.md §3.3). An assignment covers its own round and every round
/// after it, so `Ro8` means Ro8, the semi and the final.
///
/// Offered as a choice rather than a number because rounds do not exist until
/// start — there is nothing to autocomplete against — and nobody should have to
/// know that "the final" is depth 1.
#[derive(Debug, poise::ChoiceParameter)]
pub enum FromRound {
    #[name = "All rounds (default)"]
    #[name_localized("zh-TW", "所有輪次（預設）")]
    All,
    #[name = "Ro32 onwards"]
    #[name_localized("zh-TW", "32強之後")]
    Ro32,
    #[name = "Ro16 onwards"]
    #[name_localized("zh-TW", "16強之後")]
    Ro16,
    #[name = "Quarterfinal onwards"]
    #[name_localized("zh-TW", "八強之後")]
    Quarterfinal,
    #[name = "Semifinal onwards"]
    #[name_localized("zh-TW", "四強之後")]
    Semifinal,
    #[name = "Final only"]
    #[name_localized("zh-TW", "只有決賽")]
    Final,
}

impl FromRound {
    fn depth(&self) -> i64 {
        match self {
            FromRound::All => tournament_setup::DEFAULT_DEPTH,
            FromRound::Ro32 => 5,
            FromRound::Ro16 => 4,
            FromRound::Quarterfinal => 3,
            FromRound::Semifinal => 2,
            FromRound::Final => 1,
        }
    }
}

/// Taiwan is UTC+8 year round with no daylight saving, so a fixed offset is exact
/// and saves a `chrono-tz` dependency. Organizers type a local wall time; it is
/// stored UTC and rendered back as a Discord timestamp, which every reader sees
/// in their own zone.
const LOCAL_OFFSET_HOURS: i32 = 8;

/// Pure, so the parsing rules are testable without a Discord context.
fn parse_start_time(input: &str) -> Option<DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(input.trim(), "%Y-%m-%d %H:%M").ok()?;
    let offset = FixedOffset::east_opt(LOCAL_OFFSET_HOURS * 3600)?;
    Some(naive.and_local_timezone(offset).single()?.to_utc())
}

// Configuration a tournament needs before `/tournament start` will run
// (docs/tournament.md §8.3). Always reports the full state, so it doubles as
// "what am I still missing?".
/// Configures the tournament. Run with no options to see what's still needed.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_manage_only",
    description_localized("zh-TW", "設定賽事。不帶參數執行可以查看還缺什麼。")
)]
pub async fn setup(
    ctx: Context<'_>,
    #[description = "Maximum entrants; registration refuses sign-ups past this"]
    #[description_localized("zh-TW", "參賽人數上限；超過後就無法報名")]
    cap: Option<i64>,
    #[description = "When it starts, as YYYY-MM-DD HH:MM in UTC+8"]
    #[description_localized("zh-TW", "開賽時間，格式 YYYY-MM-DD HH:MM（UTC+8）")]
    start_time: Option<String>,
) -> Result<(), Error> {
    ctx.defer().await?;
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };
    let pool = &ctx.data().database;

    if let Some(cap) = cap {
        if cap < 2 {
            ephemeral(
                ctx,
                locale.pick("人數上限至少要 2 人。", "The cap has to be at least 2."),
            )
            .await?;
            return Ok(());
        }
        tournament_db::set_entrant_cap(pool, tournament.id, cap).await?;
    }

    if let Some(start_time) = &start_time {
        let Some(parsed) = parse_start_time(start_time) else {
            ephemeral(
                ctx,
                locale.pick(
                    "看不懂這個時間 — 請用 `YYYY-MM-DD HH:MM`，例如 `2026-08-20 19:30`。",
                    "Couldn't read that time — use `YYYY-MM-DD HH:MM`, e.g. `2026-08-20 19:30`.",
                ),
            )
            .await?;
            return Ok(());
        };
        tournament_db::set_scheduled_start_at(pool, tournament.id, parsed).await?;
    }

    // Re-read so the summary reflects what was just written.
    let tournament = tournament_db::get_tournament(pool, tournament.id).await?.unwrap();
    let presets = tournament_db::list_round_presets(pool, tournament.id).await?;
    audit::log_action(
        "setup",
        tournament.id,
        &tournament.slug,
        ctx.author(),
        &(cap, start_time),
    );

    ctx.say(setup_summary(&tournament, &presets, locale)).await?;
    Ok(())
}

/// The whole configuration in one reply, plus what start is still waiting on.
fn setup_summary(
    tournament: &tournament_db::Tournament,
    presets: &[tournament_db::RoundPreset],
    locale: Locale,
) -> String {
    let start = tournament.scheduled_start_at.map_or_else(
        || locale.pick("未設定", "not set").to_string(),
        |at| format!("<t:{}:F>", at.timestamp()),
    );
    let preset_lines = if presets.is_empty() {
        locale.pick("未設定", "not set").to_string()
    } else {
        presets
            .iter()
            .map(|p| {
                let scope = if p.from_depth == tournament_setup::DEFAULT_DEPTH {
                    locale.pick("所有輪次", "all rounds").to_string()
                } else {
                    locale.pick(
                        format!("距決賽 {} 輪之後", p.from_depth),
                        format!("{} round(s) out from the final, onwards", p.from_depth),
                    )
                };
                format!("· {scope}: `{}` (Bo{})", p.draft_preset_id, p.best_of)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // The start time always has a value, so it can never be "missing" — but an
    // untouched placeholder blocks check-in, and saying so here beats letting
    // someone discover it when open-checkin refuses.
    let placeholder = if tournament_setup::start_time_is_default(tournament) {
        locale.pick(
            "\n⚠️ 開賽時間還是預設值（建立後一週）。簽到要到開賽前一小時才會開放，請先設定正確時間。",
            "\n⚠️ The start time is still the default (a week after creation). Check-in won't open until an \
             hour before it, so set the real one.",
        )
    } else {
        ""
    };

    let missing = tournament_setup::missing(presets);
    let still_needed = if missing.is_empty() {
        locale
            .pick(
                "\n\n**設定完成，可以開賽。**",
                "\n\n**Setup complete — ready to start.**",
            )
            .to_string()
    } else {
        format!(
            "\n\n{} {}",
            locale.pick("**還需要：**", "**Still needed:**"),
            missing
                .iter()
                .map(|m| m.label_for(locale))
                .collect::<Vec<_>>()
                .join(locale.pick("、", ", "))
        )
    };

    format!(
        "**{} — {}**\n{}: {} / {}\n{}: {start}{placeholder}\n{}:\n{preset_lines}{still_needed}",
        tournament.name,
        locale.pick("賽事設定", "setup"),
        locale.pick("人數上限", "Entrant cap"),
        tournament.entrant_cap,
        locale.pick("已報名", "registered"),
        locale.pick("開賽時間", "Start time"),
        locale.pick("抽選預設", "Draft presets"),
    )
}

/// Sets the draft preset for a round onwards. Also decides that round's match length.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_manage_only",
    description_localized("zh-TW", "指定某一輪之後所使用的抽選預設，同時決定該輪的戰制。")
)]
pub async fn preset(
    ctx: Context<'_>,
    #[description = "The draft tool's preset id; it has to be public"]
    #[description_localized("zh-TW", "抽選工具的預設 id，必須是公開的")]
    preset_id: String,
    #[description = "Which rounds it covers; defaults to all of them"]
    #[description_localized("zh-TW", "適用於哪些輪次，預設是全部")]
    from_round: Option<FromRound>,
) -> Result<(), Error> {
    ctx.defer().await?;
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    let preset_id = preset_id.trim().to_string();
    let check = tournament_setup::check_preset(&preset_id).await;
    audit::log_action("preset", tournament.id, &tournament.slug, ctx.author(), &check);

    let Some(best_of) = check.best_of() else {
        ephemeral(ctx, check.message(locale)).await?;
        return Ok(());
    };

    let depth = from_round.unwrap_or(FromRound::All).depth();
    let pool = &ctx.data().database;
    tournament_db::upsert_round_preset(pool, tournament.id, depth, &preset_id, best_of).await?;

    let tournament = tournament_db::get_tournament(pool, tournament.id).await?.unwrap();
    let presets = tournament_db::list_round_presets(pool, tournament.id).await?;
    ctx.say(format!(
        "{}\n\n{}",
        check.message(locale),
        setup_summary(&tournament, &presets, locale)
    ))
    .await?;
    Ok(())
}

// Seeding (docs/tournament.md §6, §8.4). Only `seed` is authoritative;
// `suggested_seed` stays as the tiering proposed it, so the panel can show what
// an organizer overrode.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    subcommands("seed_list", "seed_set", "seed_refresh"),
    subcommand_required
)]
pub async fn seed(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Autocompletes over the tournament's own checked-in field, sorted by current
/// seed and rendered like the panel — `3 · MarineLorD · ATR 2293`.
async fn autocomplete_entrant(ctx: Context<'_>, partial: &str) -> impl Iterator<Item = AutocompleteChoice> {
    // Resolves the tournament directly rather than via
    // `resolve_tournament_by_channel`, which replies when there isn't one — an
    // autocomplete must never send a message. No field, no suggestions.
    let pool = &ctx.data().database;
    let channel_id = i64::try_from(ctx.channel_id().get()).unwrap();
    let entries = match tournament_db::get_tournament_by_any_channel_id(pool, channel_id).await {
        Ok(Some(tournament)) => tournament_db::list_entries_for_tournament(pool, tournament.id)
            .await
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    let mut field = seeding::seedable(&entries);
    field.sort_by_key(|e| (e.seed.unwrap_or(i64::MAX), e.user_id));

    let needle = partial.to_lowercase();
    field
        .iter()
        .filter(|e| e.display_name.to_lowercase().contains(&needle))
        .map(|e| {
            let seed = e.seed.map_or_else(|| "—".to_string(), |s| s.to_string());
            let atr = e.atr.map_or_else(|| "—".to_string(), |a| format!("{a:.0}"));
            let elo = e.elo.map_or_else(|| "—".to_string(), |e| e.to_string());
            AutocompleteChoice::new(
                format!("{seed} · {} · ATR {atr} · ELO {elo}", e.display_name),
                e.user_id.to_string(),
            )
        })
        // Discord accepts at most 25 choices.
        .take(25)
        .collect::<Vec<_>>()
        .into_iter()
}

/// Reposts the seeding panel, in case it was deleted or has scrolled away.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_manage_only",
    rename = "list"
)]
pub async fn seed_list(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    let pool = &ctx.data().database;
    let Some(bracket_channel_id) = tournament.bracket_channel_id else {
        return Ok(());
    };
    let channel_id = ChannelId::new(u64::try_from(bracket_channel_id).unwrap());

    // Always a fresh post rather than an edit: the point of `list` is to bring a
    // buried or deleted panel back into view, which editing in place cannot do.
    let message_id = seed_panel::post_initial(ctx.http(), pool, channel_id, tournament.id, &tournament.name).await?;
    tournament_db::set_seed_message_id(pool, tournament.id, Some(i64::try_from(message_id.get()).unwrap())).await?;

    ephemeral(
        ctx,
        locale.pick(
            format!("已在 <#{bracket_channel_id}> 重新張貼種子列表。"),
            format!("Reposted the seeding panel in <#{bracket_channel_id}>."),
        ),
    )
    .await?;
    Ok(())
}

/// Moves an entrant to a seed. Everyone between shifts along to keep 1..n intact.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_manage_only",
    rename = "set"
)]
pub async fn seed_set(
    ctx: Context<'_>,
    #[description = "The entrant to move — pick from the field"]
    #[autocomplete = "autocomplete_entrant"]
    entrant: String,
    #[description = "Their new seed; everyone between shifts along"] seed: i64,
) -> Result<(), Error> {
    ctx.defer().await?;
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    let pool = &ctx.data().database;
    let entries = tournament_db::list_entries_for_tournament(pool, tournament.id).await?;
    let field = seeding::seedable(&entries);
    // Typed rather than picked, or picked from a field that has since changed:
    // either way the lookup below reports it as not in the field.
    let target = entrant.parse::<i64>().unwrap_or(0);

    let Some(entry) = field.iter().find(|e| e.user_id == target) else {
        let outcome = seeding::SeedOutcome::NotInField;
        audit::log_action("seed set", tournament.id, &tournament.slug, ctx.author(), &outcome);
        ephemeral(ctx, outcome.message(locale)).await?;
        return Ok(());
    };
    let field_size = i64::try_from(field.len()).unwrap_or(i64::MAX);
    if seed < 1 || seed > field_size {
        let outcome = seeding::SeedOutcome::OutOfRange { field_size };
        audit::log_action("seed set", tournament.id, &tournament.slug, ctx.author(), &outcome);
        ephemeral(ctx, outcome.message(locale)).await?;
        return Ok(());
    }

    let from = entry.seed.unwrap_or(field_size);
    let display_name = entry.display_name.clone();
    // Current order by seed, so the shift is relative to what the panel shows.
    let mut ordered: Vec<&tournament_db::TournamentEntry> = field.clone();
    ordered.sort_by_key(|e| (e.seed.unwrap_or(i64::MAX), e.user_id));
    let current: Vec<i64> = ordered.iter().map(|e| e.user_id).collect();

    // `also_suggested: false` — an override must not overwrite what the tiering
    // proposed, or the panel loses the comparison.
    tournament_db::set_seed_order(pool, tournament.id, &seeding::reorder(&current, target, seed), false).await?;

    let outcome = seeding::SeedOutcome::Moved {
        display_name,
        from,
        to: seed,
    };
    audit::log_action("seed set", tournament.id, &tournament.slug, ctx.author(), &outcome);

    let tournament = tournament_db::get_tournament(pool, tournament.id).await?.unwrap();
    seed_panel::refresh(ctx.http(), pool, &tournament).await?;
    bracket_view::reconcile(ctx.http(), pool, &tournament).await?;
    ctx.say(outcome.message(locale)).await?;
    Ok(())
}

/// Re-fetches ATR and ELO for the field and recomputes the suggested seeding.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_manage_only",
    rename = "refresh"
)]
pub async fn seed_refresh(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    // Discards any override — that is the point of asking for a refresh, and
    // `seed set` is how you put one back.
    let message = seed_and_post_panel(ctx, &tournament, locale).await?;
    bracket_view::reconcile(ctx.http(), &ctx.data().database, &tournament).await?;
    ctx.say(message).await?;
    Ok(())
}

/// Best-effort, for the same reason `delete_checkin_panel` is: reopening has
/// already cleared the seeds this panel displayed, so a message someone removed
/// by hand must not turn a successful reopen into a failure.
async fn delete_seed_panel(ctx: Context<'_>, tournament: &tournament_db::Tournament) {
    let (Some(seed_message_id), Some(bracket_channel_id)) = (tournament.seed_message_id, tournament.bracket_channel_id)
    else {
        return;
    };

    let channel_id = ChannelId::new(u64::try_from(bracket_channel_id).unwrap());
    let message_id = MessageId::new(u64::try_from(seed_message_id).unwrap());
    if let Err(err) = channel_id.delete_message(ctx.http(), message_id).await {
        error!(
            "failed to delete the seeding panel for tournament {}: {err:?}",
            tournament.id
        );
    }
}

// Repairs a tournament's Discord side (§8.1, §8.5) without recreating it.
//
// Two reasons it exists. `create` shapes channel permissions once, so a
// tournament made before a permissions fix stays broken forever otherwise — and
// the bot being unable to post in its own output channels is exactly the bug
// this was written for. And a panel can be deleted, or never posted because a
// call failed, with no other way to get it back.
/// Repairs this tournament's channels and reposts any missing panel.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_manage_only",
    required_bot_permissions = "MANAGE_CHANNELS",
    // `refresh` is taken by the home guild's ranked-board command, so the Rust
    // name differs from the one Discord shows.
    rename = "refresh",
    description_localized("zh-TW", "修復賽事頻道權限，並重新張貼遺失的面板。")
)]
pub async fn refresh_panels(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };
    let pool = &ctx.data().database;
    let mut repaired: Vec<&str> = Vec::new();

    if reapply_channel_permissions(ctx, &tournament).await {
        repaired.push(locale.pick("頻道權限", "channel permissions"));
    }

    // Each panel: edit if the message is still there, repost if it is not. A
    // message someone deleted leaves a stale id, and editing that fails.
    if panel::refresh_now(ctx.http(), pool, &tournament).await.is_err()
        && let Some(register_channel_id) = tournament.register_channel_id
    {
        let channel_id = ChannelId::new(u64::try_from(register_channel_id).unwrap());
        let message_id = panel::post_initial(
            ctx.http(),
            channel_id,
            tournament.id,
            &tournament.name,
            tournament.entrant_cap,
        )
        .await?;
        tournament_db::set_register_message_id(pool, tournament.id, i64::try_from(message_id.get()).unwrap()).await?;
        repaired.push(locale.pick("報名面板", "the registration panel"));
    }

    if tournament.checkin_message_id.is_some()
        && checkin_panel::refresh_now(ctx.http(), pool, &tournament).await.is_err()
        && let Some(register_channel_id) = tournament.register_channel_id
    {
        let channel_id = ChannelId::new(u64::try_from(register_channel_id).unwrap());
        let message_id = checkin_panel::post_initial(
            ctx.http(),
            pool,
            channel_id,
            tournament.id,
            &tournament.name,
            tournament.checkin_closes_at,
        )
        .await?;
        tournament_db::set_checkin_message_id(pool, tournament.id, Some(i64::try_from(message_id.get()).unwrap()))
            .await?;
        repaired.push(locale.pick("簽到面板", "the check-in panel"));
    }

    if tournament.seed_message_id.is_some()
        && seed_panel::refresh(ctx.http(), pool, &tournament).await.is_err()
        && let Some(bracket_channel_id) = tournament.bracket_channel_id
    {
        let channel_id = ChannelId::new(u64::try_from(bracket_channel_id).unwrap());
        let message_id =
            seed_panel::post_initial(ctx.http(), pool, channel_id, tournament.id, &tournament.name).await?;
        tournament_db::set_seed_message_id(pool, tournament.id, Some(i64::try_from(message_id.get()).unwrap())).await?;
        repaired.push(locale.pick("種子名單", "the seeding panel"));
    }

    if bracket_view::reconcile(ctx.http(), pool, &tournament).await.is_ok() {
        repaired.push(locale.pick("賽程表", "the bracket"));
    }

    let summary = if repaired.is_empty() {
        locale
            .pick("沒有需要修復的項目。", "Nothing needed repairing.")
            .to_string()
    } else {
        format!(
            "{} {}",
            locale.pick("已修復：", "Repaired:"),
            repaired.join(locale.pick("、", ", "))
        )
    };
    ctx.say(summary).await?;
    Ok(())
}

/// Re-applies the output channels' overwrites, so a tournament created before
/// the bot was granted an explicit allow starts working. Best-effort per
/// channel: one an admin has since deleted must not stop the others.
async fn reapply_channel_permissions(ctx: Context<'_>, tournament: &tournament_db::Tournament) -> bool {
    let Some(guild_id) = ctx.guild_id() else {
        return false;
    };
    let overwrites = read_only_overwrites(guild_id.everyone_role(), ctx.cache().current_user().id);

    let mut applied = false;
    for channel_id in [
        tournament.bracket_channel_id,
        tournament.draft_channel_id,
        tournament.matches_channel_id,
    ]
    .into_iter()
    .flatten()
    {
        let channel_id = ChannelId::new(u64::try_from(channel_id).unwrap());
        for overwrite in &overwrites {
            if let Err(err) = channel_id.create_permission(ctx.http(), overwrite.clone()).await {
                error!("failed to reapply permissions on channel {channel_id}: {err:?}");
            } else {
                applied = true;
            }
        }
    }
    applied
}

// Turns the seeded field into a bracket and opens round one (§8.3, §5). No
// confirmation: setup, status, seeds and the clock are four gates already, and
// `/tournament cancel` is the way back.
/// Starts the tournament: generates the bracket and opens round one.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_manage_only",
    description_localized("zh-TW", "開賽：產生賽程表並開放第一輪。")
)]
pub async fn start(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    let pool = &ctx.data().database;
    let outcome = tournament_start::start(pool, &tournament).await?;
    audit::log_action("start", tournament.id, &tournament.slug, ctx.author(), &outcome);

    if matches!(outcome, tournament_start::StartOutcome::Started { .. }) {
        // The preview messages become the real bracket in place — re-read so the
        // status is `running` and the provisional label comes off.
        let tournament = tournament_db::get_tournament(pool, tournament.id).await?.unwrap();
        bracket_view::reconcile(ctx.http(), pool, &tournament).await?;
        panel::refresh_now(ctx.http(), pool, &tournament).await?;
    }

    ctx.say(outcome.message(&tournament.name, locale)).await?;
    Ok(())
}

// The inverse of `create` (docs/tournament.md §8.4): removes the four channels it
// made and the `tournaments` row, which cascades to every tournament-scoped
// table. The announce channel and the category are left alone — the bot created
// neither (§8.1) — and so is `tournament_players`, which is global (§4).
/// Deletes the tournament and the channels it created. Cannot be undone.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_admin_only",
    required_bot_permissions = "MANAGE_CHANNELS"
)]
pub async fn delete(
    ctx: Context<'_>,
    #[description = "Type the tournament's slug to confirm — this cannot be undone"] confirm: String,
) -> Result<(), Error> {
    ctx.defer().await?;
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };

    let channel_id = i64::try_from(ctx.channel_id().get()).unwrap();
    let check = teardown::check_delete(&tournament, &confirm, channel_id);
    audit::log_action("delete", tournament.id, &tournament.slug, ctx.author(), &check);
    if check != teardown::DeleteCheck::Ok {
        ephemeral(ctx, check.message(&tournament, locale)).await?;
        return Ok(());
    }

    // Channels first, then the row. The other order would leave channels nothing
    // resolves to, so the command could never be retried; this way a partial
    // failure still leaves a row reachable from the announce channel.
    let failed = delete_tournament_channels(ctx, &tournament).await;
    tournament_db::delete_tournament(&ctx.data().database, tournament.id).await?;

    // The only surviving record of the tournament, now that its row is gone —
    // the pair to the line `create` writes.
    info!(
        "deleted tournament {} ({}) \"{}\" by {} ({}), {failed} channel(s) could not be removed",
        tournament.id,
        tournament.slug,
        tournament.name,
        ctx.author().name,
        ctx.author().id,
    );

    let leftover = if failed > 0 {
        locale.pick(
            format!("（有 {failed} 個頻道無法刪除，請手動移除。）"),
            format!(" ({failed} channel(s) couldn't be deleted — remove them by hand.)"),
        )
    } else {
        String::new()
    };
    ctx.say(format!("{}{leftover}", check.message(&tournament, locale)))
        .await?;
    Ok(())
}

/// Deletes the four channels `create` made, skipping the announce channel and the
/// category. Best-effort per channel: one an admin already removed by hand must
/// not block the rest, or the row delete that follows. Returns the failure count.
async fn delete_tournament_channels(ctx: Context<'_>, tournament: &tournament_db::Tournament) -> usize {
    let created = [
        ("register", tournament.register_channel_id),
        ("bracket", tournament.bracket_channel_id),
        ("draft", tournament.draft_channel_id),
        ("matches", tournament.matches_channel_id),
    ];

    let mut failed = 0;
    for (label, channel_id) in created {
        let Some(channel_id) = channel_id else {
            continue;
        };
        let channel_id = ChannelId::new(u64::try_from(channel_id).unwrap());
        if let Err(err) = channel_id.delete(ctx.http()).await {
            error!(
                "failed to delete the {label} channel {channel_id} of tournament {}: {err:?}",
                tournament.id
            );
            failed += 1;
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::{parse_start_time, read_only_overwrites};
    use serenity::all::{PermissionOverwriteType, Permissions, RoleId, UserId};

    #[test]
    fn an_output_channel_denies_everyone_but_still_lets_the_bot_post() {
        // The shape the production 403 came from: denying @everyone denies the
        // bot too, so the allow is load-bearing rather than belt-and-braces.
        let everyone = RoleId::new(1);
        let bot = UserId::new(2);
        let overwrites = read_only_overwrites(everyone, bot);

        let deny = overwrites
            .iter()
            .find(|o| o.kind == PermissionOverwriteType::Role(everyone))
            .expect("@everyone should be denied");
        assert!(deny.deny.contains(Permissions::SEND_MESSAGES));

        let allow = overwrites
            .iter()
            .find(|o| o.kind == PermissionOverwriteType::Member(bot))
            .expect("the bot should be allowed");
        assert!(allow.allow.contains(Permissions::SEND_MESSAGES));
        assert!(
            !allow.deny.contains(Permissions::SEND_MESSAGES),
            "the bot's own overwrite must not deny what it allows"
        );
    }

    use chrono::{Datelike, Timelike};
    use regex::Regex;

    #[test]
    fn a_local_wall_time_is_stored_as_the_right_utc_instant() {
        // 19:30 in UTC+8 is 11:30 UTC the same day. Taiwan has no DST, so this
        // holds year-round and needs no timezone database.
        let parsed = parse_start_time("2026-08-20 19:30").expect("should parse");
        assert_eq!((parsed.year(), parsed.month(), parsed.day()), (2026, 8, 20));
        assert_eq!((parsed.hour(), parsed.minute()), (11, 30));
    }

    #[test]
    fn a_wall_time_before_the_offset_rolls_back_a_day_in_utc() {
        let parsed = parse_start_time("2026-08-20 07:00").expect("should parse");
        assert_eq!((parsed.day(), parsed.hour()), (19, 23));
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        assert!(parse_start_time("  2026-08-20 19:30 ").is_some());
    }

    #[test]
    fn malformed_times_are_rejected_rather_than_guessed() {
        for input in [
            "",
            "tomorrow",
            "2026-08-20",
            "20/08/2026 19:30",
            "2026-13-01 19:30",
            "2026-08-20 25:00",
        ] {
            assert!(parse_start_time(input).is_none(), "{input:?} should not parse");
        }
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
