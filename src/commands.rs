use crate::aoe4world::search_players;
use crate::db::{bind_account, to_channel_id, to_db_id, to_message_id};
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
    audit, bracket, bracket_view, checkin, checkin_panel, panel, registration, seed_panel, seeding, set_thread,
    setup as tournament_setup, start as tournament_start, teardown,
};
use crate::{Context, Data, Error};
use chrono::{DateTime, FixedOffset, NaiveDateTime, Utc};
use regex::Regex;
use serenity::all::{
    AutocompleteChoice, ChannelId, CreateChannel, GetMessages, GuildChannel, PermissionOverwrite,
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

/// The tournament guild's commands.
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

// Creates a tournament: the invoking channel becomes
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
    let announce_channel_id = to_db_id(announce_channel.id);
    if let Some(existing) = tournament_db::get_live_tournament_by_announce_channel(pool, announce_channel_id).await? {
        ephemeral(
            ctx,
            locale.pick(
                format!(
                    "這個頻道已經有進行中的賽事 **{}**（`{}`）。請改用其他頻道，或先結束或刪除該賽事。",
                    existing.name, existing.slug
                ),
                format!(
                    "This channel already hosts the tournament **{}** (`{}`). Use another channel, or finish or delete that one first.",
                    existing.name, existing.slug
                ),
            ),
        )
        .await?;
        return Ok(());
    }

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

    let user_id = to_db_id(ctx.author().id);
    let tournament_id = tournament_db::insert_tournament(pool, &slug, &name, user_id).await?;
    tournament_db::set_tournament_channels(
        pool,
        tournament_id,
        tournament_db::TournamentChannels {
            category_id: category_id.map(to_db_id),
            announce_channel_id,
            register_channel_id: to_db_id(register.id),
            bracket_channel_id: to_db_id(bracket.id),
            matches_channel_id: to_db_id(matches.id),
            draft_channel_id: to_db_id(draft.id),
        },
    )
    .await?;
    tournament_db::add_admin(pool, tournament_id, user_id, user_id).await?;

    // Tournaments start in `registration` status immediately, with no separate
    // "open registration" command — so this is the only
    // place the panel can ever get posted.
    // The cap is `not null default 32`, so the panel can show it from the start
    // even though `/tournament setup` has not run yet.
    let cap = tournament_db::get_tournament(pool, tournament_id)
        .await?
        .map_or(32, |t| t.entrant_cap);
    let register_message_id = panel::post_initial(ctx.http(), register.id, tournament_id, &name, cap).await?;
    tournament_db::set_register_message_id(pool, tournament_id, to_db_id(register_message_id)).await?;

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

/// The overwrites for an output channel: `@everyone` may read but not
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
    let channel_id = to_db_id(ctx.channel_id());
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

    let added_by = to_db_id(ctx.author().id);
    let target = to_db_id(user.id);
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

    let removed_by = to_db_id(ctx.author().id);
    let target = to_db_id(user.id);
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
// also binds an aoe4world profile; there is no separate bind step. Only ELO is
// snapshotted here — ATR is a bulk seeding-time fetch, not a per-registrant one.
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
    let user_id = to_db_id(ctx.author().id);
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

// Tournament-independent — the player list is global, so unlike
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
    let user_id = to_db_id(ctx.author().id);
    let outcome = registration::rebind(&ctx.data().database, user_id, i64::from(in_game_name)).await?;
    // No tournament to name — the player list is global (see the note above).
    info!(
        "rebind by {} ({user_id}) to aoe4 id {in_game_name}: {outcome:?}",
        ctx.author().name
    );
    ephemeral(ctx, outcome.message(locale)).await?;
    Ok(())
}

// Tournament-independent, like `rebind` — the player list is global. Kept
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
    let user_id = to_db_id(ctx.author().id);

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
    let user_id = to_db_id(ctx.author().id);
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
// and posts the check-in panel to the register
// channel `/tournament create` made. `minutes` is purely informational —
// there is no cron closing check-in automatically; `/tournament close-checkin`
// stays a separate, explicit action.
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
        let register_channel_id = to_channel_id(tournament.register_channel_id.unwrap());
        let message_id = checkin_panel::post_initial(
            ctx.http(),
            pool,
            register_channel_id,
            tournament.id,
            &tournament.name,
            closes_at,
            true,
        )
        .await?;
        tournament_db::set_checkin_message_id(pool, tournament.id, Some(to_db_id(message_id))).await?;

        // Registration closes here, so the panel must stop inviting
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
    let user_id = to_db_id(ctx.author().id);
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
    // The order is whatever the organizers left it as — closing check-in is the
    // edge that used to overwrite a hand-made one.
    let policy = seeding::SeedPolicy::from_source(&tournament.seed_source);
    let seeded = seed_and_post_panel(ctx, &tournament, policy, locale).await?;
    ctx.say(format!("{}\n{seeded}", outcome.message(&tournament.name, locale)))
        .await?;
    Ok(())
}

/// Fetches ratings, writes the seed order under `policy` and puts the seeding
/// panel in front of the organizers, returning the line to append to the
/// caller's reply.
///
/// **Best-effort by design.** By the time this runs the tournament has already
/// advanced to `seeding`, so an aoe4world outage must not fail the command and
/// strand the lifecycle — it seeds from whatever ratings are stored, says so, and
/// points at `/tournament seed refresh`. The panel is best-effort for the
/// same reason, which is what `ensure_seed_panel` buys.
async fn seed_and_post_panel(
    ctx: Context<'_>,
    tournament: &tournament_db::Tournament,
    policy: seeding::SeedPolicy,
    locale: Locale,
) -> Result<String, Error> {
    let pool = &ctx.data().database;
    let outcome = seeding::refresh_ratings(pool, tournament, policy).await?;
    audit::log_action("seed", tournament.id, &tournament.slug, ctx.author(), &outcome);

    let message = outcome.message(&tournament.name, locale);
    match ensure_seed_panel(ctx, tournament).await {
        SeedPanelOutcome::Updated | SeedPanelOutcome::Reposted | SeedPanelOutcome::NoChannel => Ok(message),
        // Said out loud rather than swallowed: the seeding itself succeeded, and
        // an organizer who cannot see the panel needs to know it is the panel
        // that is missing, not the seeding.
        SeedPanelOutcome::Failed => Ok(format!(
            "{message}\n{}",
            locale.pick(
                "種子名單：無法張貼，請確認機器人可在賽程頻道發言。",
                "Seeding panel: could not post — check the bot can send messages in the bracket channel.",
            )
        )),
    }
}

/// What `ensure_seed_panel` did, so each caller words it its own way.
enum SeedPanelOutcome {
    Updated,
    Reposted,
    NoChannel,
    Failed,
}

/// Leaves `#{slug}-bracket` showing a current seeding panel, whatever state it
/// was in: edited when the stored message is still there, re-posted when it is
/// not.
///
/// **Never propagates.** A panel that has gone missing — a stale
/// `seed_message_id`, or one an organizer deleted — used to turn `close-checkin`
/// into an error that posted nothing, *after* the status had already moved to
/// `seeding` and the no-shows had been marked. Recovering from that is the whole
/// reason the fallback exists rather than a bare edit.
async fn ensure_seed_panel(ctx: Context<'_>, tournament: &tournament_db::Tournament) -> SeedPanelOutcome {
    let pool = &ctx.data().database;
    // Always set by `create()`; the panel has nowhere to go without it.
    let Some(bracket_channel_id) = tournament.bracket_channel_id else {
        return SeedPanelOutcome::NoChannel;
    };

    if tournament.seed_message_id.is_some() && seed_panel::refresh(ctx.http(), pool, tournament).await.is_ok() {
        return SeedPanelOutcome::Updated;
    }

    let channel_id = to_channel_id(bracket_channel_id);
    match seed_panel::post_initial(ctx.http(), pool, channel_id, tournament.id, &tournament.name).await {
        Ok(message_id) => {
            match tournament_db::set_seed_message_id(pool, tournament.id, Some(to_db_id(message_id))).await {
                Ok(()) => SeedPanelOutcome::Reposted,
                // The panel is up; only the handle to it is lost, so the next
                // call posts a second one rather than editing this.
                Err(err) => {
                    error!(
                        "failed to record the seeding panel for tournament {}: {err:?}",
                        tournament.id
                    );
                    SeedPanelOutcome::Failed
                },
            }
        },
        Err(err) => {
            error!(
                "failed to post the seeding panel for tournament {}: {err:?}",
                tournament.id
            );
            SeedPanelOutcome::Failed
        },
    }
}

// The one backward lifecycle edge, for a check-in
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

    let channel_id = to_channel_id(register_channel_id);
    let message_id = to_message_id(checkin_message_id);
    if let Err(err) = channel_id.delete_message(ctx.http(), message_id).await {
        error!(
            "failed to delete the check-in panel for tournament {}: {err:?}",
            tournament.id
        );
    }
}

/// Which rounds a preset assignment covers, as a distance back from the final. An
/// assignment covers its own round and every round after it, so `Ro8` means Ro8,
/// the semi and the final.
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

// Configuration a tournament needs before `/tournament start` will run. Always
// reports the full state, so it doubles as "what am I still missing?".
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

    // The panel displays both the cap and the start time, so it goes stale the
    // moment either is written.
    panel::refresh_now(ctx.http(), pool, &tournament).await?;

    let entries = tournament_db::list_entries_for_tournament(pool, tournament.id).await?;
    ctx.say(setup_summary(&tournament, &presets, &entries, locale)).await?;
    Ok(())
}

/// The whole configuration in one reply, plus what start is still waiting on.
/// Which rounds an assignment covers, named the way the bracket names them.
///
/// The round name comes from `bracket::round_name` rather than a second table of
/// names, so the setup panel and the bracket can never disagree. Depth 1 is the
/// final and covers nothing else, so it reads as a round rather than a range.
fn preset_scope(from_depth: i64, locale: Locale) -> String {
    if from_depth == tournament_setup::DEFAULT_DEPTH {
        return locale.pick("預設（所有輪次）", "Default preset").to_string();
    }
    let sets = 1usize << u32::try_from(from_depth - 1).unwrap_or(0).min(16);
    let name = bracket::localize_round_name(&bracket::round_name(sets, from_depth == 1), locale);
    if from_depth == 1 {
        return name;
    }
    // Latin inside Chinese takes a space, as elsewhere in this file; 八強之後 must not.
    let space = if name.is_ascii() { " " } else { "" };
    locale.pick(format!("{name}{space}之後"), format!("{name} onwards"))
}

fn setup_summary(
    tournament: &tournament_db::Tournament,
    presets: &[tournament_db::RoundPreset],
    entries: &[tournament_db::TournamentEntry],
    locale: Locale,
) -> String {
    let registered = entries.iter().filter(|e| e.status == "active").count();
    let start = tournament.scheduled_start_at.map_or_else(
        || locale.pick("未設定", "not set").to_string(),
        |at| format!("<t:{}:F>", at.timestamp()),
    );
    let base = tournament
        .draft_base_url
        .clone()
        .unwrap_or_else(crate::drafttool::base_url);
    let preset_lines = if presets.is_empty() {
        locale.pick("未設定", "not set").to_string()
    } else {
        presets
            .iter()
            .map(|p| {
                // `<url>` inside the link suppresses Discord's embed: six presets
                // would otherwise unfurl six previews under one short message.
                let link = format!(
                    "[{}](<{base}/presets/{}>)",
                    crate::ranked::escape(p.preset_name.as_deref().unwrap_or(&p.draft_preset_id)),
                    p.draft_preset_id
                );
                format!("· {}: {link} (Bo{})", preset_scope(p.from_depth, locale), p.best_of)
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
        "**{} — {}**\n{}: {registered}/{}\n{}: {start}{placeholder}\n{}:\n{preset_lines}{still_needed}",
        tournament.name,
        locale.pick("賽事設定", "setup"),
        locale.pick("已報名 / 上限", "Registered / cap"),
        tournament.entrant_cap,
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
    let depths =
        tournament_setup::depths_to_assign(&tournament_db::list_round_presets(pool, tournament.id).await?, depth);
    let preset_name = check.name().unwrap_or(&preset_id);
    for depth in &depths {
        tournament_db::upsert_round_preset(pool, tournament.id, *depth, &preset_id, preset_name, best_of).await?;
    }

    // Say so rather than let two lines appear in the summary unexplained.
    let also_default = if depths.len() > 1 {
        locale.pick(
            "\n（之前沒有預設，所以這個也設為所有輪次的預設。）",
            "\n(There was no default yet, so this is now the default for every round too.)",
        )
    } else {
        ""
    };

    let tournament = tournament_db::get_tournament(pool, tournament.id).await?.unwrap();
    let presets = tournament_db::list_round_presets(pool, tournament.id).await?;
    let entries = tournament_db::list_entries_for_tournament(pool, tournament.id).await?;
    ctx.say(format!(
        "{}{also_default}\n\n{}",
        check.message(locale),
        setup_summary(&tournament, &presets, &entries, locale)
    ))
    .await?;
    Ok(())
}

// Seeding. Only `seed` is authoritative;
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
    let channel_id = to_db_id(ctx.channel_id());
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
    let channel_id = to_channel_id(bracket_channel_id);

    // Always a fresh post rather than an edit: the point of `list` is to bring a
    // buried or deleted panel back into view, which editing in place cannot do.
    let message_id = seed_panel::post_initial(ctx.http(), pool, channel_id, tournament.id, &tournament.name).await?;
    tournament_db::set_seed_message_id(pool, tournament.id, Some(to_db_id(message_id))).await?;

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
    // Takes the *whole* field manual, not just this entrant: seeds are
    // written as one 1..n order, so there is no per-row notion of who was moved.
    tournament_db::set_seed_source(pool, tournament.id, seeding::SeedPolicy::KeepManual.as_source()).await?;

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
    // `seed set` is how you put one back. Recorded before seeding rather than
    // after, so the field and the column cannot disagree if the pass fails.
    let policy = seeding::SeedPolicy::Suggest;
    tournament_db::set_seed_source(&ctx.data().database, tournament.id, policy.as_source()).await?;
    let message = seed_and_post_panel(ctx, &tournament, policy, locale).await?;
    bracket_view::reconcile(ctx.http(), &ctx.data().database, &tournament).await?;
    ctx.say(message).await?;
    Ok(())
}

/// The panel does not belong in the channel outside `seeding` onward
/// (`seeding::seed_panel_expected`), so a reopen takes it down whether or not the
/// order it displayed survived. Best-effort, for the same reason
/// `delete_checkin_panel` is: a message someone removed by hand must not turn a
/// successful reopen into a failure.
async fn delete_seed_panel(ctx: Context<'_>, tournament: &tournament_db::Tournament) {
    let (Some(seed_message_id), Some(bracket_channel_id)) = (tournament.seed_message_id, tournament.bracket_channel_id)
    else {
        return;
    };

    let channel_id = to_channel_id(bracket_channel_id);
    let message_id = to_message_id(seed_message_id);
    if let Err(err) = channel_id.delete_message(ctx.http(), message_id).await {
        error!(
            "failed to delete the seeding panel for tournament {}: {err:?}",
            tournament.id
        );
    }
}

// Repairs a tournament's Discord side without recreating it.
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
    // `create_permission` is Discord's *edit channel permissions* endpoint,
    // which needs MANAGE_ROLES — declaring only MANAGE_CHANNELS let the command
    // run and 403 on every overwrite, reporting nothing.
    required_bot_permissions = "MANAGE_CHANNELS | MANAGE_ROLES",
    // `refresh` is taken by the home guild's ranked-board command, so the Rust
    // name differs from the one Discord shows.
    rename = "refresh",
    description_localized("zh-TW", "修復賽事頻道權限，並重新張貼遺失的面板。")
)]
pub async fn refresh_panels(ctx: Context<'_>) -> Result<(), Error> {
    // Ephemeral: this is a diagnostic for whoever ran it, not narration for the
    // channel — and a per-reader locale is only correct for a single reader.
    ctx.defer_ephemeral().await?;
    let locale = Locale::from_context(ctx);
    let Some(tournament) = resolve_tournament_by_channel(ctx).await? else {
        return Ok(());
    };
    let pool = &ctx.data().database;

    // One line per item, always, saying what happened to it. An earlier version
    // listed only repairs, so a run that fixed nothing and failed at everything
    // reported "nothing needed repairing" — the two outcomes a repair tool most
    // needs to tell apart.
    let mut lines: Vec<String> = Vec::new();

    let (applied, failed) = reapply_channel_permissions(ctx, &tournament).await;
    lines.push(if failed > 0 {
        locale.pick(
            format!("頻道權限：{failed} 個頻道套用失敗，機器人可能缺少「管理身分組」權限。"),
            format!("Channel permissions: {failed} channel(s) failed — the bot may lack Manage Roles."),
        )
    } else if applied > 0 {
        locale
            .pick("頻道權限：已重新套用。", "Channel permissions: reapplied.")
            .to_string()
    } else {
        locale
            .pick(
                "頻道權限：沒有可套用的頻道。",
                "Channel permissions: no output channels.",
            )
            .to_string()
    });

    lines.push(refresh_register_panel(ctx, &tournament, locale).await?);
    lines.push(refresh_checkin_panel(ctx, &tournament, locale).await?);
    lines.push(refresh_seed_panel(ctx, &tournament, locale).await?);

    lines.push(match bracket_view::reconcile(ctx.http(), pool, &tournament).await {
        Ok(bracket_view::ReconcileOutcome::NoChannel) => locale
            .pick(
                "賽程表：這場賽事沒有賽程頻道。",
                "Bracket: this tournament has no bracket channel.",
            )
            .to_string(),
        Ok(bracket_view::ReconcileOutcome::TooFewEntrants) => locale
            .pick(
                "賽程表：報名者不足兩人，還畫不出對戰表。",
                "Bracket: fewer than two entrants, so there is nothing to draw yet.",
            )
            .to_string(),
        Ok(outcome) if outcome.changed() => locale.pick("賽程表：已重新張貼。", "Bracket: reposted.").to_string(),
        Ok(_) => locale.pick("賽程表：已更新。", "Bracket: updated.").to_string(),
        Err(err) => {
            error!("failed to redraw the bracket for tournament {}: {err:?}", tournament.id);
            locale
                .pick(
                    "賽程表：無法張貼，請確認機器人可在賽程頻道發言。",
                    "Bracket: could not post — check the bot can send messages in the bracket channel.",
                )
                .to_string()
        },
    });

    ephemeral(ctx, lines.join("\n")).await?;
    Ok(())
}

/// The registration panel: edited if it is there, posted if it is not.
async fn refresh_register_panel(
    ctx: Context<'_>,
    tournament: &tournament_db::Tournament,
    locale: Locale,
) -> Result<String, Error> {
    let pool = &ctx.data().database;
    let Some(register_channel_id) = tournament.register_channel_id else {
        return Ok(locale
            .pick(
                "報名面板：這場賽事沒有報名頻道。",
                "Registration panel: no register channel.",
            )
            .to_string());
    };

    if tournament.register_message_id.is_some() && panel::refresh_now(ctx.http(), pool, tournament).await.is_ok() {
        return Ok(locale
            .pick("報名面板：已更新。", "Registration panel: updated.")
            .to_string());
    }

    let channel_id = to_channel_id(register_channel_id);
    match panel::post_initial(
        ctx.http(),
        channel_id,
        tournament.id,
        &tournament.name,
        tournament.entrant_cap,
    )
    .await
    {
        Ok(message_id) => {
            tournament_db::set_register_message_id(pool, tournament.id, to_db_id(message_id)).await?;
            Ok(locale
                .pick("報名面板：已重新張貼。", "Registration panel: reposted.")
                .to_string())
        },
        Err(err) => {
            error!(
                "failed to repost the registration panel for tournament {}: {err:?}",
                tournament.id
            );
            Ok(locale
                .pick("報名面板：無法張貼。", "Registration panel: could not post.")
                .to_string())
        },
    }
}

/// The check-in panel, which only belongs in the channel once check-in has opened.
async fn refresh_checkin_panel(
    ctx: Context<'_>,
    tournament: &tournament_db::Tournament,
    locale: Locale,
) -> Result<String, Error> {
    let pool = &ctx.data().database;
    if !checkin::checkin_panel_expected(&tournament.status) {
        return Ok(locale
            .pick(
                "簽到面板：尚未開始簽到。",
                "Check-in panel: check-in has not opened yet.",
            )
            .to_string());
    }
    let Some(register_channel_id) = tournament.register_channel_id else {
        return Ok(locale
            .pick(
                "簽到面板：這場賽事沒有報名頻道。",
                "Check-in panel: no register channel.",
            )
            .to_string());
    };

    if tournament.checkin_message_id.is_some() && checkin_panel::refresh_now(ctx.http(), pool, tournament).await.is_ok()
    {
        return Ok(locale
            .pick("簽到面板：已更新。", "Check-in panel: updated.")
            .to_string());
    }

    let channel_id = to_channel_id(register_channel_id);
    match checkin_panel::post_initial(
        ctx.http(),
        pool,
        channel_id,
        tournament.id,
        &tournament.name,
        tournament.checkin_closes_at,
        // Past check-in this must come back closed, not inviting presses.
        checkin::checkin_is_open(&tournament.status),
    )
    .await
    {
        Ok(message_id) => {
            tournament_db::set_checkin_message_id(pool, tournament.id, Some(to_db_id(message_id))).await?;
            Ok(locale
                .pick("簽到面板：已重新張貼。", "Check-in panel: reposted.")
                .to_string())
        },
        Err(err) => {
            error!(
                "failed to repost the check-in panel for tournament {}: {err:?}",
                tournament.id
            );
            Ok(locale
                .pick("簽到面板：無法張貼。", "Check-in panel: could not post.")
                .to_string())
        },
    }
}

/// The seeding panel, which only belongs in the channel from `seeding` onward.
async fn refresh_seed_panel(
    ctx: Context<'_>,
    tournament: &tournament_db::Tournament,
    locale: Locale,
) -> Result<String, Error> {
    if !seeding::seed_panel_expected(&tournament.status) {
        return Ok(locale
            .pick(
                "種子名單：尚未進入排種階段。",
                "Seeding panel: seeding has not started yet.",
            )
            .to_string());
    }
    Ok(match ensure_seed_panel(ctx, tournament).await {
        SeedPanelOutcome::Updated => locale.pick("種子名單：已更新。", "Seeding panel: updated."),
        SeedPanelOutcome::Reposted => locale.pick("種子名單：已重新張貼。", "Seeding panel: reposted."),
        SeedPanelOutcome::NoChannel => {
            locale.pick("種子名單：這場賽事沒有賽程頻道。", "Seeding panel: no bracket channel.")
        },
        SeedPanelOutcome::Failed => locale.pick("種子名單：無法張貼。", "Seeding panel: could not post."),
    }
    .to_string())
}

/// Re-applies the output channels' overwrites, so a tournament created before
/// the bot was granted an explicit allow starts working. Returns
/// `(applied, failed)`: best-effort per channel, since one an admin has since
/// deleted must not stop the others, and a caller that cannot see the failures
/// cannot tell a repaired tournament from a broken one.
async fn reapply_channel_permissions(ctx: Context<'_>, tournament: &tournament_db::Tournament) -> (usize, usize) {
    let Some(guild_id) = ctx.guild_id() else {
        return (0, 0);
    };
    let overwrites = read_only_overwrites(guild_id.everyone_role(), ctx.cache().current_user().id);

    let (mut applied, mut failed) = (0, 0);
    for channel_id in [
        tournament.bracket_channel_id,
        tournament.draft_channel_id,
        tournament.matches_channel_id,
    ]
    .into_iter()
    .flatten()
    {
        let channel_id = to_channel_id(channel_id);
        for overwrite in &overwrites {
            if let Err(err) = channel_id.create_permission(ctx.http(), overwrite.clone()).await {
                error!("failed to reapply permissions on channel {channel_id}: {err:?}");
                failed += 1;
            } else {
                applied += 1;
            }
        }
    }
    (applied, failed)
}

// Turns the seeded field into a bracket and opens round one. No
// confirmation: setup, status, seeds and the clock are four gates already, and
// `/tournament delete` is the only way back.
/// Starts the tournament: generates the bracket, opens round one and its threads.
#[poise::command(
    slash_command,
    guild_only,
    check = "tournament_only",
    check = "tournament_manage_only",
    // Opening a set means a private thread and a panel inside it. Declared so
    // Discord refuses the command outright rather than letting it half-run and
    // 403 per set, which is how the channel-permission bug stayed invisible.
    required_bot_permissions = "CREATE_PRIVATE_THREADS | SEND_MESSAGES_IN_THREADS",
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
        open_ready_sets(ctx, &tournament).await?;
    }

    ctx.say(outcome.message(&tournament.name, locale)).await?;
    Ok(())
}

/// Opens a thread and a draft room for every set now playable.
///
/// Best-effort per set, and a no-op for one that already has a thread: a set
/// whose thread could not be created must not stop the rest of the round from
/// opening, and an admin can retry afterwards.
async fn open_ready_sets(ctx: Context<'_>, tournament: &tournament_db::Tournament) -> Result<(), Error> {
    let pool = &ctx.data().database;
    for set in tournament_db::list_sets_for_tournament(pool, tournament.id).await? {
        if set.status != "ready" {
            continue;
        }
        if let Err(err) = set_thread::open(ctx.http(), pool, tournament, &set).await {
            error!(
                "failed to open set {} for tournament {}: {err:?}",
                set.id, tournament.id
            );
        }
    }
    Ok(())
}

// The inverse of `create`: removes the four channels it
// made and the `tournaments` row, which cascades to every tournament-scoped
// table. The announce channel and the category are left alone — the bot created
// neither — and so is `tournament_players`, which is global.
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

    let channel_id = to_db_id(ctx.channel_id());
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
        let channel_id = to_channel_id(channel_id);
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
    use super::{FromRound, parse_start_time, preset_scope, read_only_overwrites};
    use crate::locale::Locale;
    use serenity::all::{PermissionOverwriteType, Permissions, RoleId, UserId};

    #[test]
    fn a_presets_scope_reads_as_the_rounds_it_covers() {
        assert_eq!(preset_scope(0, Locale::En), "Default preset");
        assert_eq!(preset_scope(1, Locale::En), "Final", "the final covers only itself");
        assert_eq!(preset_scope(2, Locale::En), "Semifinal onwards");
        assert_eq!(preset_scope(3, Locale::En), "Quarterfinal onwards");
        assert_eq!(preset_scope(4, Locale::En), "Ro16 onwards");
        assert_eq!(preset_scope(5, Locale::En), "Ro32 onwards");
    }

    #[test]
    fn every_scope_an_organizer_can_pick_reads_back_as_the_round_they_picked() {
        // The two directions are written independently — `FromRound` maps a choice to
        // a depth, `preset_scope` maps a depth to a label — so this is what stops the
        // panel naming a round the organizer never chose.
        for (choice, label) in [
            (FromRound::Ro32, "Ro32"),
            (FromRound::Ro16, "Ro16"),
            (FromRound::Quarterfinal, "Quarterfinal"),
            (FromRound::Semifinal, "Semifinal"),
            (FromRound::Final, "Final"),
        ] {
            assert!(
                preset_scope(choice.depth(), Locale::En).starts_with(label),
                "{label} should round-trip, got {}",
                preset_scope(choice.depth(), Locale::En)
            );
        }
    }

    #[test]
    fn the_closing_rounds_are_named_in_chinese() {
        assert_eq!(preset_scope(1, Locale::ZhTw), "決賽");
        assert_eq!(preset_scope(2, Locale::ZhTw), "準決賽之後");
        assert_eq!(preset_scope(3, Locale::ZhTw), "八強之後");
        assert_eq!(preset_scope(0, Locale::ZhTw), "預設（所有輪次）");
    }

    #[test]
    fn the_earlier_rounds_keep_ro_x_and_take_a_space_before_the_chinese() {
        // `RoX` is language-neutral, so it is not translated — but Latin inside
        // Chinese takes a space, which a translated name must not.
        assert_eq!(preset_scope(4, Locale::ZhTw), "Ro16 之後");
        assert_eq!(preset_scope(5, Locale::ZhTw), "Ro32 之後");
    }

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
