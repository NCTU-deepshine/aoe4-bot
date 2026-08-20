//! Set threads: one private thread per set on
//! `#{slug}-matches`, holding the draft room link and the seat instruction.
//!
//! Why a thread rather than DMs: its membership already *is* the right audience —
//! both players plus every admin — so it scopes the room link with no DM
//! machinery and no closed-DM fallback, and leaves a record both players can
//! scroll back to mid-series. Admins can see the link and could in principle
//! claim a seat; they are trusted, and this is far better than a public channel.
//!
//! `thread_name` and `render_panel` are the pure parts, tested here. Everything
//! else is the Discord/DB glue `commands.rs` drives.

use crate::Error;
use crate::db::{to_channel_id, to_message_id, to_user_id};
use crate::drafttool::{self, DraftError};
use crate::ranked::escape;
use crate::tournament::action::Action;
use crate::tournament::bracket;
use crate::tournament::completion::{Settlement, Tally};
use crate::tournament::db::{self, Tournament, TournamentRound, TournamentSet};
use crate::tournament::render;
use serenity::all::{
    ButtonStyle, CacheHttp, ChannelType, CreateActionRow, CreateAllowedMentions, CreateButton, CreateMessage,
    CreateThread, EditMessage, EditThread,
};
use sqlx::SqlitePool;
use tracing::{error, info};

/// Discord's hard limit on a thread name.
const NAME_LIMIT: usize = 100;

/// Display cells allowed per name, leaving room for the `R1M1 · ` prefix and the
/// ` vs ` between them. A CJK name spends two cells per character, so measuring
/// in cells is what keeps the limit honest, using the bracket's width helper.
const NAME_WIDTH: usize = 30;

/// `R1M1 · MarineLorD vs Beasty`, inside Discord's 100-character limit.
pub(crate) fn thread_name(round_ordinal: i64, position: i64, one: &str, two: &str) -> String {
    prefixed_thread_name(&format!("R{round_ordinal}M{position}"), one, two)
}

/// `3rd Place · MarineLorD vs Beasty` — its own label rather than a fabricated
/// round number: the match plays alongside the round before it, not as a round
/// of its own, so `R{ordinal}M{position}` would misname it.
pub(crate) fn third_place_thread_name(position: i64, one: &str, two: &str) -> String {
    prefixed_thread_name(&format!("3rd Place M{position}"), one, two)
}

/// The width-fitting and length-limiting both thread-name functions share.
fn prefixed_thread_name(prefix: &str, one: &str, two: &str) -> String {
    let name = format!(
        "{prefix} · {} vs {}",
        render::fit(one, NAME_WIDTH).trim_end(),
        render::fit(two, NAME_WIDTH).trim_end()
    );

    // Cells bound characters for everything except zero-width marks, which cost a
    // character and no cells — so the limit is enforced again in characters.
    name.chars().take(NAME_LIMIT).collect()
}

/// The pinned control panel. Bilingual, like the other panels: one
/// message with several readers, none of whom interacted to summon it.
///
/// `room` is `None` before a draft has been created for this set — creation is
/// deliberately deferred to the first press of the button below, not minted
/// eagerly when the thread opens.
pub(crate) fn render_panel(
    set: &SetHeading,
    one: &Player,
    two: &Player,
    room: Option<&Room>,
) -> (String, Vec<CreateActionRow>) {
    // Names are player-editable aoe4world strings, so they are escaped where they sit
    // in markdown and `sanitize`d where they sit inside a code span — a backtick
    // cannot be escaped inside one, only replaced.
    let header = format!(
        "**{} · Match {} — Bo{}**   <@{}>  <@{}>\n`{}` {}  vs  `{}` {}\n",
        set.round_name,
        set.position,
        set.best_of,
        one.user_id,
        two.user_id,
        one.seed,
        escape(&one.name),
        two.seed,
        escape(&two.name)
    );

    let Some(room) = room else {
        return (
            format!(
                "{header}\nDraft 房間尚未建立，請按下方按鈕建立。\n\
                 **No draft room yet — press the button below to create one.**\n"
            ),
            // Call-admin here too: nothing has failed yet, so there is nobody to
            // name outright, but a player stuck on this state still needs a way
            // to reach one.
            vec![CreateActionRow::Buttons(vec![
                CreateButton::new(Action::Redraft.custom_id(set.id))
                    .label("➕ 建立 Draft / Create draft")
                    .style(ButtonStyle::Primary),
                CreateButton::new(Action::CallAdmin.custom_id(set.id))
                    .label("呼叫管理員 / Call an organizer")
                    .style(ButtonStyle::Secondary),
            ])],
        );
    };

    // The room link, not the watch link: a seat is claimed from `/match/`, and
    // `/watch/` deliberately cannot claim one.
    //
    // Each player is addressed by their own *in-game* name, not a second mention:
    // the header already pinged them, and a Discord mention is no help when you
    // are looking for somebody in the game's lobby browser.
    let body = format!(
        "{header}\nDraft 房間 / Draft room: {}\n\
         **{} 選 Player 1**，並在遊戲中開房\n\
         **{} 選 Player 2**\n\
         **{} takes seat Player 1** and hosts the lobby in game\n\
         **{} takes seat Player 2**\n\
         Draft 有任何問題，可以按下方按鈕重新產生。\n\
         If anything is wrong with the draft, use the button below to regenerate it.\n",
        room.match_url,
        game_name(one, "（名稱未驗證）"),
        game_name(two, "（名稱未驗證）"),
        game_name(one, " (name unverified)"),
        game_name(two, " (name unverified)"),
    );

    // The call-admin button is what a player has instead of knowing who to ask:
    // nothing on this panel names an organizer. Set complete is safe to press
    // early — it syncs and reports "still in progress" rather than acting on
    // an undecided draft — so it carries no confirmation of its own.
    let components = vec![CreateActionRow::Buttons(vec![
        CreateButton::new_link(&room.watch_url).label("觀戰 / Watch draft"),
        CreateButton::new(Action::Redraft.custom_id(set.id))
            .label("🔄 重新產生 Draft / Regenerate draft")
            .style(ButtonStyle::Secondary),
        CreateButton::new(Action::CallAdmin.custom_id(set.id))
            .label("呼叫管理員 / Call an organizer")
            .style(ButtonStyle::Secondary),
        CreateButton::new(Action::SetDone.custom_id(set.id))
            .label("✅ 回報完成 / Set complete")
            .style(ButtonStyle::Success),
    ])];

    (body, components)
}

/// A player's in-game name, as the seat instruction addresses them by it.
///
/// An invited entrant's name is whatever the organizer typed, and this line is
/// the one place it is read as an instruction — search the lobby browser for
/// this — so an unverified one says so rather than failing silently in game.
/// Inside a code span, hence `sanitize`: a backtick cannot be escaped there.
fn game_name(player: &Player, unverified_note: &str) -> String {
    let name = render::sanitize(&player.name);
    if player.verified {
        format!("`{name}`")
    } else {
        format!("`{name}`{unverified_note}")
    }
}

/// The public post in `#…-draft`: one per set, carrying the spectator link
/// and nothing that can claim a seat.
///
/// **Posted when the room is created, not when both seats fill.** Detecting the
/// latter meant polling an undocumented endpoint on a schedule this bot does not
/// have, so the room is announced empty and the accepted cost is that a
/// reader who edits `/watch/` to `/match/` reaches it. `/set redraft` is the remedy.
///
/// Bilingual round name, because a public channel has many readers and so no one
/// reader's locale to follow. No mentions: names only.
pub(crate) fn render_announcement(
    set: &SetHeading,
    one: &Player,
    two: &Player,
    room: &Room,
) -> (String, Vec<CreateActionRow>) {
    let body = format!(
        "**{} · Match {} — Bo{}**\n`{}` {}  vs  `{}` {}\n",
        bracket::round_name_bilingual(&set.round_name),
        set.position,
        set.best_of,
        one.seed,
        escape(&one.name),
        two.seed,
        escape(&two.name),
    );

    // The url is a button, never body text: a link button needs no permission beyond
    // sending the message, where a url in the body renders as a link only with
    // EMBED_LINKS — which the bot's own overwrite on this channel does not grant
    // (`read_only_overwrites` allows SEND_MESSAGES and nothing else).
    //
    // The watch link, never the room link: `/watch/` has no seat control, which is
    // the whole reason a public channel gets a link at all.
    let components = vec![CreateActionRow::Buttons(vec![
        CreateButton::new_link(&room.watch_url).label("觀戰 / Watch draft"),
    ])];

    (body, components)
}

/// What a redraft leaves behind on the pinned set panel, once the room it
/// pointed at is abandoned: the header, so the thread still reads as this
/// match, and a note that the link above no longer goes anywhere. Sent with no
/// components — an empty button list is what actually kills the dead
/// `/match/` link, which a struck label alone would not.
pub(crate) fn render_superseded_panel(set: &SetHeading, one: &Player, two: &Player) -> String {
    let header = format!(
        "**{} · Match {} — Bo{}**   <@{}>  <@{}>\n`{}` {}  vs  `{}` {}\n",
        set.round_name,
        set.position,
        set.best_of,
        one.user_id,
        two.user_id,
        one.seed,
        escape(&one.name),
        two.seed,
        escape(&two.name)
    );
    format!(
        "{header}\n~~Draft 房間~~ 已被重新產生，此連結已失效。\n\
         ~~Draft room~~ has been regenerated — this link is dead.\n"
    )
}

/// The `#…-draft` counterpart of `render_superseded_panel`: struck for the same
/// reason, with the same empty-components treatment.
pub(crate) fn render_superseded_announcement(set: &SetHeading, one: &Player, two: &Player) -> String {
    format!(
        "**{} · Match {} — Bo{}**\n`{}` {}  vs  `{}` {}\n\
         ~~觀戰連結~~ 該場對戰已重新產生 Draft。\n\
         ~~Watch link~~ — this set's draft was regenerated.\n",
        bracket::round_name_bilingual(&set.round_name),
        set.position,
        set.best_of,
        one.seed,
        escape(&one.name),
        two.seed,
        escape(&two.name),
    )
}

/// What the pinned panel becomes once the set is decided: the same header,
/// the result in place of the seat instruction, and no components — nothing
/// on it stays actionable once a winner is set. `close` edits the live panel
/// to this on every settlement path, not only an imported one.
pub(crate) fn render_completed_panel(
    set: &SetHeading,
    winner: &Player,
    loser: &Player,
    tally: &Tally,
    settlement: Settlement,
) -> String {
    let header = format!(
        "**{} · Match {} — Bo{}**   <@{}>  <@{}>\n`{}` {}  vs  `{}` {}\n",
        set.round_name,
        set.position,
        set.best_of,
        winner.user_id,
        loser.user_id,
        winner.seed,
        escape(&winner.name),
        loser.seed,
        escape(&loser.name)
    );
    let score = format!("{}-{}", tally.slot1_wins, tally.slot2_wins);
    // Same reasoning as `render_result`: a 3rd place win is a podium finish, not
    // an advance.
    let result = match (settlement, set.is_third_place) {
        (Settlement::Played, false) => format!(
            "🏁 **{}** 以 {score} 擊敗 **{}**，已晉級。\n\
             🏁 **{}** beat **{}** {score} and advances.",
            escape(&winner.name),
            escape(&loser.name),
            escape(&winner.name),
            escape(&loser.name)
        ),
        (Settlement::Played, true) => format!(
            "🏁 **{}** 以 {score} 擊敗 **{}**，獲得季軍。\n\
             🏁 **{}** beat **{}** {score} and takes 3rd place.",
            escape(&winner.name),
            escape(&loser.name),
            escape(&winner.name),
            escape(&loser.name)
        ),
        (Settlement::Walkover, false) => format!(
            "🏁 已將此對戰判給 **{}**（{score}）——**{}** 未完賽。\n\
             🏁 Awarded to **{}** ({score}) — **{}** didn't play it out.",
            escape(&winner.name),
            escape(&loser.name),
            escape(&winner.name),
            escape(&loser.name)
        ),
        (Settlement::Walkover, true) => format!(
            "🏁 已將季軍判給 **{}**（{score}）——**{}** 未完賽。\n\
             🏁 Awarded 3rd place to **{}** ({score}) — **{}** didn't play it out.",
            escape(&winner.name),
            escape(&loser.name),
            escape(&winner.name),
            escape(&loser.name)
        ),
    };
    format!("{header}\n{result}\n")
}

/// The `#…-draft` counterpart, edited alongside the panel. Touches the header
/// line only — `one`/`two` stay slot-ordered, the same as the original post,
/// so the second line keeps its shape and only the score is new.
pub(crate) fn render_announcement_result(set: &SetHeading, one: &Player, two: &Player, tally: &Tally) -> String {
    format!(
        "**{} · Match {} — Bo{} · {}-{}**\n`{}` {}  vs  `{}` {}\n",
        bracket::round_name_bilingual(&set.round_name),
        set.position,
        set.best_of,
        tally.slot1_wins,
        tally.slot2_wins,
        one.seed,
        escape(&one.name),
        two.seed,
        escape(&two.name),
    )
}

/// The visible trail a redraft leaves in the thread, naming who triggered it —
/// per §4, the readable record is this notice plus `redraft_count`, not a
/// history table.
///
/// Mentioned once, in the Chinese line; the English line names them by in-game
/// name instead, same as the seat instruction. `actor` is `None` for an admin
/// who is neither player — there is no in-game name to fall back to, so the
/// English line keeps the mention too.
pub(crate) fn render_redraft_notice(actor_user_id: i64, actor: Option<&Player>, count: i64) -> String {
    let actor_name = match actor {
        Some(player) if player.verified => game_name(player, ""),
        _ => format!("<@{actor_user_id}>"),
    };
    format!(
        "🔄 <@{actor_user_id}> 重新產生了這場對戰的 Draft（第 {count} 次）。\n\
         {actor_name} regenerated this set's draft (redraft #{count})."
    )
}

/// Which match this is. Grouped so the panel takes what identifies the set in
/// one argument rather than four loose numbers and a name.
pub(crate) struct SetHeading {
    pub id: i64,
    pub round_name: String,
    pub position: i64,
    pub best_of: i64,
    /// Whether this is the 3rd place match rather than a set on the path to the
    /// final — presentation-only (unlike `redraft`/`completion`'s termination
    /// logic, which stays structural): it decides whether the result reads as an
    /// "advance" or a podium finish.
    pub is_third_place: bool,
}

/// A set's occupant, as the panel needs them.
pub(crate) struct Player {
    pub user_id: i64,
    pub seed: i64,
    pub name: String,
    /// Whether `name` came from aoe4world or from the organizer who invited
    /// them. The panel hands it to their opponent as the string to search for in
    /// the lobby browser, so a guess has to be labelled as one.
    pub verified: bool,
}

/// The two links a draft room has: one to play in, one to watch.
pub(crate) struct Room {
    pub match_url: String,
    pub watch_url: String,
}

impl Room {
    fn for_draft(tournament: &Tournament, draft_id: &str) -> Self {
        let base = tournament.draft_base_url.clone().unwrap_or_else(drafttool::base_url);
        Self {
            match_url: format!("{base}/match/{draft_id}"),
            watch_url: format!("{base}/watch/{draft_id}"),
        }
    }
}

/// Opens a set: thread, members, pinned panel.
///
/// The draft room is deliberately not minted here — creation is deferred to the
/// first press of the panel's `➕ Create draft` button, so a set that never gets
/// played never spends a room on the draft tool.
///
/// A no-op once the set has a thread, so the caller can hand it every ready set
/// without tracking which are new — completing a set reopens the same path as
/// later rounds fill.
pub(crate) async fn open(
    http: impl CacheHttp,
    pool: &SqlitePool,
    tournament: &Tournament,
    set: &TournamentSet,
) -> Result<(), Error> {
    let (Some(matches_channel_id), None) = (tournament.matches_channel_id, set.thread_id) else {
        return Ok(());
    };
    let (Some(slot1), Some(slot2)) = (set.slot1_user_id, set.slot2_user_id) else {
        return Ok(());
    };
    let Some(round) = db::get_round(pool, set.round_id).await? else {
        return Ok(());
    };

    // Two point lookups rather than the whole field: a set knows exactly whose
    // names it needs, and a 32-entrant list to find two of them is waste.
    let one = player(pool, tournament.id, slot1).await?;
    let two = player(pool, tournament.id, slot2).await?;
    let is_third_place = round.name == bracket::THIRD_PLACE;

    let channel_id = to_channel_id(matches_channel_id);
    let name = if is_third_place {
        third_place_thread_name(set.position, &one.name, &two.name)
    } else {
        thread_name(round.ordinal, set.position, &one.name, &two.name)
    };
    let thread = channel_id
        .create_thread(&http, CreateThread::new(name).kind(ChannelType::PrivateThread))
        .await?;
    db::set_thread(pool, set.id, crate::db::to_db_id(thread.id)).await?;

    let admins: Vec<i64> = db::list_admins(pool, tournament.id)
        .await
        .unwrap_or_default()
        .iter()
        .map(|admin| admin.user_id)
        .collect();
    add_members(&http, thread.id, &[slot1, slot2], &admins).await;

    let heading = SetHeading {
        id: set.id,
        round_name: round.name.clone(),
        position: set.position,
        best_of: round.best_of,
        is_third_place,
    };
    let (content, components) = render_panel(&heading, &one, &two, None);
    let message = thread
        .id
        .send_message(&http, CreateMessage::new().content(content).components(components))
        .await?;
    // Best-effort: a panel that failed to pin is still the panel.
    if let Err(err) = message.pin(&http).await {
        error!("failed to pin the set panel for set {}: {err:?}", set.id);
    }
    // The handle the create/redraft button needs to edit this panel in place.
    if let Err(err) = db::set_panel_message(pool, set.id, crate::db::to_db_id(message.id)).await {
        error!("failed to record the panel for set {}: {err:?}", set.id);
    }

    Ok(())
}

/// Opens every set that is playable and has no thread yet.
///
/// One opener for both callers — `/tournament start` at the beginning, and each
/// result as it lands — because "which sets are new" is a question neither needs
/// to answer: `open` is a no-op on a set that already has a thread, so handing it
/// everything is correct by construction.
///
/// Best-effort per set, and returns nothing: one set that cannot open must not
/// stop the rest, and neither caller has anything to do about a failure beyond
/// the log.
pub(crate) async fn open_ready(http: &impl CacheHttp, pool: &SqlitePool, tournament: &Tournament) {
    let sets = match db::list_sets_for_tournament(pool, tournament.id).await {
        Ok(sets) => sets,
        Err(err) => {
            error!("failed to list sets for tournament {}: {err:?}", tournament.id);
            return;
        },
    };

    for set in sets.iter().filter(|set| set.status == "ready") {
        if let Err(err) = open(http, pool, tournament, set).await {
            error!(
                "failed to open set {} for tournament {}: {err:?}",
                set.id, tournament.id
            );
        }
    }
}

/// The result line posted into the thread when a set is decided.
///
/// Pure, and bilingual for the same reason the panel is: the thread's readers are
/// two players and every admin, none of whom asked for this message. Names are
/// escaped — they are player-editable and land in markdown here.
pub(crate) fn render_result(
    set: &SetHeading,
    winner: &Player,
    loser: &Player,
    tally: &Tally,
    settlement: Settlement,
) -> String {
    let (winner_name, loser_name) = (escape(&winner.name), escape(&loser.name));
    let score = format!("{}-{}", tally.slot1_wins, tally.slot2_wins);
    let round = bracket::round_name_bilingual(&set.round_name);
    // A walkover says so rather than reading as a win nobody watched: the two
    // players know it was not played out, and the record should agree with them.
    // A 3rd place win reads as a podium finish, never as "advancing" — there is
    // nothing after it to advance into.
    let verdict = match (settlement, set.is_third_place) {
        (Settlement::Played, false) => format!(
            "**{winner_name}** (#{}) 獲勝，晉級下一輪。 / **{winner_name}** (#{}) wins and advances.\n\
             感謝 **{loser_name}** (#{}) 的參賽。 / Thanks for playing, **{loser_name}** (#{}).",
            winner.seed, winner.seed, loser.seed, loser.seed
        ),
        (Settlement::Played, true) => format!(
            "**{winner_name}** (#{}) 獲得季軍。 / **{winner_name}** (#{}) takes 3rd place.\n\
             感謝 **{loser_name}** (#{}) 的參賽。 / Thanks for playing, **{loser_name}** (#{}).",
            winner.seed, winner.seed, loser.seed, loser.seed
        ),
        (Settlement::Walkover, false) => format!(
            "由管理員判給 **{winner_name}** (#{}) 晉級，**{loser_name}** 未完賽。 / \
             Awarded to **{winner_name}** (#{}) — **{loser_name}** didn't play it out.",
            winner.seed, winner.seed
        ),
        (Settlement::Walkover, true) => format!(
            "由管理員判給 **{winner_name}** (#{}) 季軍，**{loser_name}** 未完賽。 / \
             Awarded 3rd place to **{winner_name}** (#{}) — **{loser_name}** didn't play it out.",
            winner.seed, winner.seed
        ),
    };
    format!(
        "🏁 **{round} · Match {} — {score}**\n{verdict}\n\
         本討論串已封存。 / This thread is now closed.",
        set.position
    )
}

/// Posts the result and shuts the thread down: archived and locked, so it stops
/// counting against the guild's cap on active threads while staying readable.
///
/// Every step is best-effort. The completion transaction has already committed by
/// the time this runs, so a thread that refuses to archive is a cosmetic problem,
/// and failing the caller over it would report a set as unfinished when it is not.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn close(
    http: &impl CacheHttp,
    pool: &SqlitePool,
    tournament: &Tournament,
    set: &TournamentSet,
    winner: &Player,
    loser: &Player,
    tally: &Tally,
    settlement: Settlement,
) {
    let Some(thread_id) = set.thread_id else {
        return; // a set decided before its thread ever opened
    };
    let thread_id = to_channel_id(thread_id);

    let Some(round) = db::get_round(pool, set.round_id).await.ok().flatten() else {
        error!("set {} has no round, so its thread cannot be closed", set.id);
        return;
    };
    let heading = SetHeading {
        id: set.id,
        round_name: round.name.clone(),
        position: set.position,
        best_of: round.best_of,
        is_third_place: round.name == bracket::THIRD_PLACE,
    };

    // Posted before the lock, not after: a locked thread is a bad place to try to
    // write, and the result is the more important half of the two.
    if let Err(err) = thread_id
        .send_message(
            http,
            CreateMessage::new()
                .content(render_result(&heading, winner, loser, tally, settlement))
                // Names reach this message; nothing in it should ping.
                .allowed_mentions(CreateAllowedMentions::new().empty_users().empty_roles()),
        )
        .await
    {
        error!("failed to post the result in set {}'s thread: {err:?}", set.id);
    }

    // Before the lock, same reasoning: an edit into a locked thread is the one
    // Discord call in this function that a closed thread would actually break.
    if let Some(panel_id) = set.panel_message_id {
        let content = render_completed_panel(&heading, winner, loser, tally, settlement);
        if let Err(err) = thread_id
            .edit_message(
                http,
                to_message_id(panel_id),
                EditMessage::new().content(content).components(vec![]),
            )
            .await
        {
            error!("failed to strike the panel for set {}: {err:?}", set.id);
        }
    }

    // Slot-ordered, not winner/loser: the announcement's second line must keep
    // the order it was first posted in, and only its header line changes.
    if let (Some(channel_id), Some(announce_id)) = (tournament.draft_channel_id, set.draft_announce_message_id) {
        let (one, two) = if Some(winner.user_id) == set.slot1_user_id {
            (winner, loser)
        } else {
            (loser, winner)
        };
        let content = render_announcement_result(&heading, one, two, tally);
        if let Err(err) = to_channel_id(channel_id)
            .edit_message(http, to_message_id(announce_id), EditMessage::new().content(content))
            .await
        {
            error!("failed to edit the announcement for set {}: {err:?}", set.id);
        }
    }

    if let Err(err) = thread_id
        .edit_thread(http, EditThread::new().archived(true).locked(true))
        .await
    {
        error!("failed to archive set {}'s thread: {err:?}", set.id);
    }
}

/// Posts the set's spectator announcement in `#…-draft` and records its id.
///
/// Returns nothing rather than a `Result`, so a caller cannot propagate a failed
/// spectator post into a failed set. One post per set comes for free: `open` is a
/// no-op once the set has a thread, so the room — and therefore this — happens
/// once. `draft_announce_message_id` is a handle for editing or replacing that
/// post later, not a guard against a second call. `pub(crate)` because a redraft
/// announces its fresh room through this same path.
pub(crate) async fn announce(
    http: &impl CacheHttp,
    pool: &SqlitePool,
    tournament: &Tournament,
    set: &SetHeading,
    one: &Player,
    two: &Player,
    room: &Room,
) {
    let Some(draft_channel_id) = tournament.draft_channel_id else {
        // A live tournament without one means `/tournament create` half-failed.
        // Said out loud: a silently-403ing best-effort post is how a permission
        // bug once survived two deploys.
        error!(
            "tournament {} has no draft channel, so set {} is unannounced",
            tournament.id, set.id
        );
        return;
    };

    let (content, components) = render_announcement(set, one, two, room);
    let message = to_channel_id(draft_channel_id)
        .send_message(
            http,
            CreateMessage::new()
                .content(content)
                .components(components)
                // Not belt-and-braces: `escape` leaves `<` and `@` alone, so an
                // aoe4world display name of `<@123>` renders as a live mention of a
                // stranger — in a public channel. An empty builder sends
                // `parse: [], users: [], roles: []`, which pings nobody whatever a
                // name contains.
                .allowed_mentions(CreateAllowedMentions::new()),
        )
        .await;

    match message {
        // The watch url is in the log because this line is the only way an
        // organizer recovers the post by hand.
        Err(err) => error!(
            "failed to announce set {} in channel {draft_channel_id}: {err:?} — watch link: {}",
            set.id, room.watch_url
        ),
        Ok(message) => {
            if let Err(err) = db::set_draft_announce_message(pool, set.id, crate::db::to_db_id(message.id)).await {
                error!("failed to record the announcement for set {}: {err:?}", set.id);
            }
        },
    }
}

/// A slot's occupant. An entry that has vanished falls back to the raw id
/// rather than failing the thread — the set still needs to open.
pub(crate) async fn player(pool: &SqlitePool, tournament_id: i64, user_id: i64) -> Result<Player, Error> {
    let entry = db::get_entry(pool, tournament_id, user_id).await?;
    Ok(Player {
        user_id,
        seed: entry.as_ref().and_then(|e| e.seed).unwrap_or_default(),
        // An entry can no longer be unbound, so the only way this reads
        // unverified any more is a vanished entry — the fallback raw id,
        // verified by nobody.
        verified: entry.is_some(),
        name: entry.map_or_else(|| user_id.to_string(), |e| e.display_name),
    })
}

/// Both players plus every current admin. Best-effort per member: one who has
/// left the guild must not cost everyone else their thread.
async fn add_members(http: &impl CacheHttp, thread_id: serenity::all::ChannelId, players: &[i64], admins: &[i64]) {
    let members = players.iter().copied().chain(admins.iter().copied());

    for user_id in members {
        if let Err(err) = http
            .http()
            .add_thread_channel_member(thread_id, to_user_id(user_id))
            .await
        {
            error!("failed to add {user_id} to thread {thread_id}: {err:?}");
        }
    }
}

/// Creates the draft room and stores the pointer, or reports why it could not.
///
/// Deliberately does not fail the caller: a set with a thread and no room is
/// recoverable by an admin, and a set with neither is not.
///
/// `pub(crate)` so `redraft::run` can reuse it for a set's first draft, where
/// there is no old room to strike and `mint_room`'s split ordering is moot.
pub(crate) async fn create_room(
    pool: &SqlitePool,
    tournament: &Tournament,
    round: &TournamentRound,
    set_id: i64,
) -> Option<Room> {
    let (draft_id, room) = mint_room(tournament, round, set_id).await?;
    if let Err(err) = db::set_draft_pointer(pool, set_id, &draft_id).await {
        error!("failed to store the draft pointer for set {set_id}: {err:?}");
        return None;
    }
    Some(room)
}

/// Asks the draft tool for a fresh room, or reports why it could not — the half
/// of `create_room` that talks to the tool, with no DB write of its own.
///
/// `pub(crate)` and split out so a redraft can mint the replacement room before
/// touching anything: `set_draft_pointer` nulls the announcement handle, so a
/// redraft must not repoint until it has both a new room to announce and the old
/// one's links struck (§8.7's ordering).
pub(crate) async fn mint_room(tournament: &Tournament, round: &TournamentRound, set_id: i64) -> Option<(String, Room)> {
    let preset_id = round.draft_preset_id.as_ref()?;
    match drafttool::create_match(preset_id).await {
        Ok(created) => {
            info!("minted draft {} for set {set_id}", created.id);
            let room = Room::for_draft(tournament, &created.id);
            Some((created.id, room))
        },
        Err(err) => {
            // `PresetRejected`'s issues are the only account of which rule the
            // preset broke, and we cannot check it beforehand.
            error!("failed to create a draft for set {set_id}: {err}");
            if let DraftError::PresetRejected { issues } = &err {
                error!("the draft tool rejected preset {preset_id}: {issues:?}");
            }
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(user_id: i64, seed: i64, name: &str) -> Player {
        Player {
            user_id,
            seed,
            name: name.to_string(),
            verified: true,
        }
    }

    fn invitee(user_id: i64, seed: i64, name: &str) -> Player {
        Player {
            verified: false,
            ..player(user_id, seed, name)
        }
    }

    fn heading(id: i64, round_name: &str, position: i64, best_of: i64) -> SetHeading {
        SetHeading {
            id,
            round_name: round_name.to_string(),
            position,
            best_of,
            is_third_place: round_name == bracket::THIRD_PLACE,
        }
    }

    fn room() -> Room {
        Room {
            match_url: "https://draft.example/match/65f1".to_string(),
            watch_url: "https://draft.example/watch/65f1".to_string(),
        }
    }

    #[test]
    fn a_thread_name_reads_as_round_and_match() {
        assert_eq!(thread_name(1, 1, "MarineLorD", "Beasty"), "R1M1 · MarineLorD vs Beasty");
    }

    #[test]
    fn a_third_place_thread_is_named_by_its_own_label_not_a_fabricated_round() {
        // A 4-entrant bracket has no round after the final, so `R{ordinal}M{position}`
        // would misname this match — it plays alongside the semifinal, not as a
        // round of its own.
        assert_eq!(
            third_place_thread_name(1, "MarineLorD", "Beasty"),
            "3rd Place M1 · MarineLorD vs Beasty"
        );
        assert!(
            third_place_thread_name(1, "A", "B").starts_with("3rd Place"),
            "no fabricated round number"
        );
    }

    #[test]
    fn a_third_place_thread_name_stays_within_the_cjk_width_limit() {
        let name = third_place_thread_name(1, &"賽".repeat(200), &"程".repeat(200));
        assert!(
            name.chars().count() <= NAME_LIMIT,
            "{} chars: {name}",
            name.chars().count()
        );
        assert!(name.contains("賽"), "{name}");
    }

    #[test]
    fn a_thread_name_stays_within_discords_limit_for_long_names() {
        let name = thread_name(10, 10, &"a".repeat(200), &"b".repeat(200));
        assert!(
            name.chars().count() <= NAME_LIMIT,
            "{} chars: {name}",
            name.chars().count()
        );
    }

    #[test]
    fn a_thread_name_stays_within_the_limit_for_double_width_names() {
        // CJK costs two display cells per character, which is why the budget is
        // measured in cells — counting chars would let these through at twice
        // the intended length.
        let name = thread_name(1, 1, &"賽".repeat(200), &"程".repeat(200));
        assert!(
            name.chars().count() <= NAME_LIMIT,
            "{} chars: {name}",
            name.chars().count()
        );
        assert!(name.contains("賽"), "{name}");
    }

    #[test]
    fn the_panel_pings_each_player_once_and_says_which_seat_each_takes() {
        let (content, _) = render_panel(
            &heading(1, "Round 1", 1, 3),
            &player(7, 1, "MarineLorD"),
            &player(9, 8, "Beasty"),
            Some(&room()),
        );
        // One ping each, in the header — the seat lines address them by
        // in-game name instead of mentioning them again.
        assert_eq!(content.matches("<@7>").count(), 1, "{content}");
        assert_eq!(content.matches("<@9>").count(), 1, "{content}");
        assert!(content.contains("`MarineLorD` takes seat Player 1"), "{content}");
        assert!(content.contains("`Beasty` takes seat Player 2"), "{content}");
    }

    #[test]
    fn a_verified_name_is_handed_over_without_qualification() {
        let (content, _) = render_panel(
            &heading(1, "Round 1", 1, 3),
            &player(7, 1, "MarineLorD"),
            &player(9, 8, "Beasty"),
            Some(&room()),
        );
        assert!(content.contains("`Beasty` takes seat Player 2"), "{content}");
        assert!(!content.to_lowercase().contains("unverified"), "{content}");
    }

    #[test]
    fn an_invitees_name_is_marked_where_it_is_read_as_an_instruction() {
        // The seat line is what a player types into the lobby browser. An
        // organizer's guess going in unmarked is how two people fail to find
        // each other with no idea why.
        let (content, _) = render_panel(
            &heading(1, "Round 1", 1, 3),
            &player(7, 1, "MarineLorD"),
            &invitee(9, 8, "Beasty"),
            Some(&room()),
        );
        assert!(
            content.contains("`Beasty` (name unverified) takes seat Player 2"),
            "{content}"
        );
        assert!(content.contains("`Beasty`（名稱未驗證） 選 Player 2"), "{content}");
        // Only the unverified side is marked.
        assert!(content.contains("`MarineLorD` takes seat Player 1"), "{content}");
        assert!(!content.contains("MarineLorD (name unverified)"), "{content}");
    }

    #[test]
    fn the_panel_carries_the_room_link_so_a_seat_can_be_claimed() {
        let (content, components) = render_panel(
            &heading(1, "Final", 1, 5),
            &player(7, 1, "A"),
            &player(9, 2, "B"),
            Some(&room()),
        );
        // `/watch/` cannot claim a seat, so the body must carry `/match/`.
        assert!(content.contains("/match/65f1"), "{content}");
        assert!(
            !content.contains("/watch/65f1"),
            "the body links the room, not the spectator view"
        );
        assert_eq!(components.len(), 1, "one row");
    }

    #[test]
    fn the_panel_names_the_round_and_the_series_length() {
        let (content, _) = render_panel(
            &heading(1, "Semifinal", 2, 5),
            &player(7, 1, "A"),
            &player(9, 4, "B"),
            Some(&room()),
        );
        assert!(content.contains("Semifinal · Match 2 — Bo5"), "{content}");
    }

    #[test]
    fn player_one_is_told_to_host_the_lobby_and_each_is_addressed_by_their_own_game_name() {
        // A Discord mention is no use in the game's lobby browser, so each
        // seat line addresses that player by their own aoe4world name.
        let (content, _) = render_panel(
            &heading(1, "Round 1", 1, 3),
            &player(7, 1, "MarineLorD"),
            &player(9, 8, "Beasty"),
            Some(&room()),
        );
        assert!(
            content.contains("`MarineLorD` takes seat Player 1** and hosts the lobby in game"),
            "{content}"
        );
        assert!(content.contains("`Beasty` takes seat Player 2**"), "{content}");
        assert!(content.contains("開房"), "the Chinese half says it too: {content}");
    }

    /// The buttons on a row, by label — the assertion that says what a player
    /// can actually press.
    fn labels(components: &[CreateActionRow]) -> String {
        format!("{components:?}")
    }

    #[test]
    fn both_panel_states_offer_a_call_admin_button_but_only_the_working_one_offers_set_done() {
        // A player stuck before creation still needs a way to reach an admin, so
        // call-admin is on both states — set-done is the one thing that makes no
        // sense before there is a draft to have played.
        let set = heading(77, "Round 1", 1, 3);
        let (_, with_room) = render_panel(&set, &player(7, 1, "A"), &player(9, 8, "B"), Some(&room()));
        let (_, without) = render_panel(&set, &player(7, 1, "A"), &player(9, 8, "B"), None);

        assert!(labels(&with_room).contains("calladmin:77"), "{}", labels(&with_room));
        assert!(labels(&with_room).contains("Watch draft"), "{}", labels(&with_room));
        assert!(labels(&with_room).contains("redraft:77"), "{}", labels(&with_room));
        assert!(labels(&with_room).contains("setdone:77"), "{}", labels(&with_room));
        assert!(labels(&without).contains("calladmin:77"), "{}", labels(&without));
        assert!(labels(&without).contains("redraft:77"), "{}", labels(&without));
        assert!(
            !labels(&without).contains("setdone"),
            "no set-complete button without a room: {}",
            labels(&without)
        );
    }

    #[test]
    fn a_set_with_no_draft_yet_invites_the_button_press_rather_than_reporting_a_failure() {
        // Creation is deferred to the button, not attempted eagerly, so a fresh
        // set reads as "not created yet" rather than "something went wrong".
        let (content, components) = render_panel(
            &heading(1, "Round 1", 1, 3),
            &player(7, 1, "A"),
            &player(9, 8, "B"),
            None,
        );
        assert!(content.contains("No draft room yet"), "{content}");
        assert!(!content.contains("could not be created"), "{content}");
        assert!(content.contains("<@7>"), "the players are still named: {content}");
        assert!(labels(&components).contains("Create draft"), "{components:?}");
        assert!(labels(&components).contains("redraft:1"), "{components:?}");
    }

    // The public draft-channel post.

    #[test]
    fn an_announcement_names_the_round_the_match_and_the_series() {
        let (content, _) = render_announcement(
            &heading(1, "Quarterfinal", 2, 5),
            &player(7, 1, "A"),
            &player(9, 4, "B"),
            &room(),
        );
        // Bilingual, because a public channel has many readers.
        assert!(content.contains("**八強 / Quarterfinal · Match 2 — Bo5**"), "{content}");
    }

    #[test]
    fn an_announcement_shows_each_players_seed_and_name() {
        let (content, _) = render_announcement(
            &heading(1, "Ro16", 1, 3),
            &player(7, 1, "MarineLorD"),
            &player(9, 8, "Beasty"),
            &room(),
        );
        assert!(content.contains("`1` MarineLorD  vs  `8` Beasty"), "{content}");
    }

    #[test]
    fn the_public_post_carries_the_watch_link_and_never_the_room_link() {
        // `Room` holds both urls, so this proves the seat-claiming one cannot
        // escape into a public channel. The counterpart of
        // `the_panel_carries_the_room_link_so_a_seat_can_be_claimed`: between them
        // they state the whole `/match/` vs `/watch/` split.
        let (content, components) = render_announcement(
            &heading(1, "Final", 1, 5),
            &player(7, 1, "A"),
            &player(9, 2, "B"),
            &room(),
        );
        let buttons = labels(&components);
        assert!(buttons.contains("/watch/65f1"), "{buttons}");
        assert!(!buttons.contains("/match/65f1"), "{buttons}");
        assert!(!content.contains("/match/"), "{content}");
    }

    #[test]
    fn the_public_post_never_pings_anybody() {
        // The invariant that makes a public channel safe to post entrant names in.
        let (content, _) = render_announcement(
            &heading(1, "Final", 1, 5),
            &player(7, 1, "A"),
            &player(9, 2, "B"),
            &room(),
        );
        assert!(!content.contains("<@"), "{content}");
    }

    #[test]
    fn the_header_carries_the_round_the_match_and_the_series_and_nothing_else() {
        let (content, _) = render_announcement(
            &heading(1, "Final", 1, 5),
            &player(7, 1, "A"),
            &player(9, 2, "B"),
            &room(),
        );
        assert!(content.starts_with("**決賽 / Final · Match 1 — Bo5**\n"), "{content}");
    }

    #[test]
    fn a_name_containing_markdown_is_escaped_in_the_public_post() {
        // Entrant names are remote aoe4world data, and this post is outside a code
        // fence — unlike the bracket, which is fenced.
        let (content, _) = render_announcement(
            &heading(1, "Final", 1, 5),
            &player(7, 1, "*Bea*sty_"),
            &player(9, 2, "B"),
            &room(),
        );
        assert!(content.contains(r"\*Bea\*sty\_"), "{content}");
    }

    #[test]
    fn a_display_name_cannot_smuggle_a_mention_into_the_public_post() {
        // `escape` leaves `<` and `@` alone, so the rendered text still reads
        // `<@42>` — this documents that the renderer is *not* what protects the
        // channel. The empty `allowed_mentions` at the send site is, and that is not
        // observable from a string, so this test exists to point at it.
        let (content, _) = render_announcement(
            &heading(1, "Final", 1, 5),
            &player(7, 1, "<@42>"),
            &player(9, 2, "B"),
            &room(),
        );
        assert!(content.contains("<@42>"), "the name is not rewritten: {content}");
    }

    #[test]
    fn the_announcement_offers_only_link_buttons() {
        // Nothing on a public message should route to a handler.
        let (_, components) = render_announcement(
            &heading(77, "Final", 1, 5),
            &player(7, 1, "A"),
            &player(9, 2, "B"),
            &room(),
        );
        let buttons = labels(&components);
        assert_eq!(components.len(), 1, "one row");
        assert!(!buttons.contains("custom_id"), "{buttons}");
        assert!(!buttons.contains("calladmin"), "{buttons}");
    }

    #[test]
    fn a_backtick_in_a_name_cannot_break_the_panels_code_span() {
        // The seat line's game name sits inside inline code, where a markdown escape
        // does nothing — a raw backtick would close the span and mangle the rest of
        // the line, so it is replaced instead.
        let (content, _) = render_panel(
            &heading(1, "Round 1", 1, 3),
            &player(7, 1, "a`b"),
            &player(9, 8, "B"),
            Some(&room()),
        );
        assert!(content.contains("`a'b` takes seat Player 1"), "{content}");
        assert!(!content.contains("a`b"), "no raw backtick survives: {content}");
    }

    #[test]
    fn a_name_containing_markdown_is_escaped_in_the_panel_header() {
        let (content, _) = render_panel(
            &heading(1, "Round 1", 1, 3),
            &player(7, 1, "*Bea*sty_"),
            &player(9, 8, "B"),
            Some(&room()),
        );
        assert!(content.contains(r"\*Bea\*sty\_"), "{content}");
    }

    fn tally(slot1_wins: i64, slot2_wins: i64) -> Tally {
        Tally { slot1_wins, slot2_wins }
    }

    #[test]
    fn the_result_names_the_winner_the_score_and_both_seeds_in_both_languages() {
        let content = render_result(
            &heading(1, "Quarterfinal", 3, 3),
            &player(7, 1, "MarineLorD"),
            &player(9, 8, "Beasty"),
            &tally(2, 1),
            Settlement::Played,
        );
        assert!(content.contains("2-1"), "{content}");
        assert!(
            content.contains("MarineLorD") && content.contains("Beasty"),
            "{content}"
        );
        assert!(content.contains("#1") && content.contains("#8"), "{content}");
        // Bilingual, and the round name doubled where a translation exists.
        assert!(
            content.contains("八強") && content.contains("Quarterfinal"),
            "{content}"
        );
        assert!(content.contains("wins and advances"), "{content}");
        assert!(content.contains("Match 3"), "{content}");
    }

    #[test]
    fn an_awarded_set_says_so_in_the_thread_it_closes() {
        let awarded = render_result(
            &heading(1, "Semifinal", 2, 3),
            &player(7, 1, "MarineLorD"),
            &player(9, 4, "Beasty"),
            &tally(1, 0),
            Settlement::Walkover,
        );
        // Neither player watched this one finish; the thread should not claim
        // otherwise.
        assert!(!awarded.contains("wins and advances"), "{awarded}");
        assert!(awarded.contains("Awarded to"), "{awarded}");
        assert!(awarded.contains("判給"), "{awarded}");
        // The score and both names survive either way.
        assert!(awarded.contains("1-0") && awarded.contains("MarineLorD") && awarded.contains("Beasty"));
        assert!(awarded.contains("This thread is now closed"));
    }

    #[test]
    fn a_3rd_place_win_reads_as_a_podium_finish_rather_than_an_advance() {
        let content = render_result(
            &heading(1, bracket::THIRD_PLACE, 1, 3),
            &player(7, 1, "MarineLorD"),
            &player(9, 8, "Beasty"),
            &tally(2, 1),
            Settlement::Played,
        );
        assert!(!content.contains("advances"), "{content}");
        assert!(!content.contains("晉級"), "{content}");
        assert!(content.contains("3rd place"), "{content}");
        assert!(content.contains("季軍"), "{content}");
        assert!(
            content.contains("MarineLorD") && content.contains("Beasty"),
            "{content}"
        );
    }

    #[test]
    fn an_awarded_3rd_place_match_names_it_as_such() {
        let content = render_result(
            &heading(1, bracket::THIRD_PLACE, 1, 3),
            &player(7, 1, "MarineLorD"),
            &player(9, 8, "Beasty"),
            &tally(1, 0),
            Settlement::Walkover,
        );
        assert!(!content.contains("advances"), "{content}");
        assert!(content.contains("Awarded 3rd place"), "{content}");
        assert!(content.contains("判給") && content.contains("季軍"), "{content}");
    }

    #[test]
    fn a_name_carrying_markdown_is_escaped_in_the_result() {
        // Mention syntax is a separate defence: `escape` deliberately leaves `<`
        // and `@` alone, and `close` sends with empty `allowed_mentions`, so a
        // name like `<@1234>` renders as text and pings nobody.
        let content = render_result(
            &heading(1, "Final", 1, 5),
            &player(7, 1, "*Bea*sty_"),
            &player(9, 2, "B"),
            &tally(3, 0),
            Settlement::Played,
        );
        assert!(content.contains(r"\*Bea\*sty\_"), "{content}");
    }

    // What `close` edits the panel and the announcement to, once a set is decided.

    #[test]
    fn a_completed_panel_shows_the_score_and_carries_no_buttons() {
        let content = render_completed_panel(
            &heading(1, "Round 1", 1, 3),
            &player(7, 1, "MarineLorD"),
            &player(9, 8, "Beasty"),
            &tally(2, 1),
            Settlement::Played,
        );
        assert!(content.contains("2-1"), "{content}");
        assert!(
            content.contains("MarineLorD") && content.contains("Beasty"),
            "{content}"
        );
        assert!(content.contains("beat"), "{content}");
        assert!(content.contains("擊敗"), "{content}");
    }

    #[test]
    fn a_completed_panel_names_an_award_rather_than_a_win() {
        let content = render_completed_panel(
            &heading(1, "Round 1", 1, 3),
            &player(7, 1, "MarineLorD"),
            &player(9, 8, "Beasty"),
            &tally(1, 0),
            Settlement::Walkover,
        );
        assert!(!content.contains("beat"), "{content}");
        assert!(content.contains("Awarded to"), "{content}");
        assert!(content.contains("判給"), "{content}");
    }

    #[test]
    fn a_completed_3rd_place_panel_says_so_rather_than_claiming_an_advance() {
        let content = render_completed_panel(
            &heading(1, bracket::THIRD_PLACE, 1, 3),
            &player(7, 1, "MarineLorD"),
            &player(9, 8, "Beasty"),
            &tally(2, 1),
            Settlement::Played,
        );
        assert!(!content.contains("advances"), "{content}");
        assert!(!content.contains("晉級"), "{content}");
        assert!(content.contains("3rd place"), "{content}");
        assert!(content.contains("季軍"), "{content}");
    }

    #[test]
    fn a_completed_panels_name_is_escaped() {
        let content = render_completed_panel(
            &heading(1, "Round 1", 1, 3),
            &player(7, 1, "*Bea*sty_"),
            &player(9, 8, "B"),
            &tally(2, 0),
            Settlement::Played,
        );
        assert!(content.contains(r"\*Bea\*sty\_"), "{content}");
    }

    #[test]
    fn the_completed_announcement_touches_the_header_line_only() {
        // `one`/`two` stay slot-ordered — the second line keeps the exact shape
        // it was first posted with, whichever side happened to win.
        let (original, _) = render_announcement(
            &heading(1, "Quarterfinal", 2, 3),
            &player(7, 1, "MarineLorD"),
            &player(9, 8, "Beasty"),
            &room(),
        );
        let second_line = original.lines().nth(1).unwrap();

        let result = render_announcement_result(
            &heading(1, "Quarterfinal", 2, 3),
            &player(7, 1, "MarineLorD"),
            &player(9, 8, "Beasty"),
            &tally(2, 1),
        );
        assert_eq!(
            result.lines().nth(1).unwrap(),
            second_line,
            "the second line must not change"
        );
        assert!(result.contains("2-1"), "{result}");
        assert_ne!(
            result.lines().next().unwrap(),
            original.lines().next().unwrap(),
            "only the header changes"
        );
    }

    // What a redraft leaves in place of the abandoned room.

    #[test]
    fn a_superseded_panel_names_the_match_and_carries_no_live_link() {
        let content = render_superseded_panel(&heading(1, "Round 1", 1, 3), &player(7, 1, "A"), &player(9, 8, "B"));
        assert!(content.contains("Round 1 · Match 1 — Bo3"), "{content}");
        assert!(content.contains("regenerated"), "{content}");
        assert!(content.contains("重新產生"), "{content}");
        assert!(!content.contains("/match/"), "{content}");
    }

    #[test]
    fn a_superseded_announcement_names_the_match_and_carries_no_live_link() {
        let content =
            render_superseded_announcement(&heading(1, "Final", 1, 5), &player(7, 1, "A"), &player(9, 2, "B"));
        assert!(content.contains("決賽 / Final · Match 1 — Bo5"), "{content}");
        assert!(content.contains("regenerated"), "{content}");
        assert!(!content.contains("/watch/"), "{content}");
    }

    #[test]
    fn the_redraft_notice_mentions_the_player_actor_once_and_names_them_by_game_name_in_english() {
        let actor = player(7, 1, "MarineLorD");
        let content = render_redraft_notice(7, Some(&actor), 2);
        assert_eq!(content.matches("<@7>").count(), 1, "{content}");
        assert!(content.contains("重新產生"), "{content}");
        assert!(content.contains("`MarineLorD` regenerated"), "{content}");
        assert!(content.contains('2'), "{content}");
    }

    #[test]
    fn the_redraft_notice_falls_back_to_a_mention_for_an_admin_actor() {
        // An admin who is neither player has no in-game name to address them by.
        let content = render_redraft_notice(42, None, 1);
        assert_eq!(content.matches("<@42>").count(), 2, "{content}");
        assert!(content.contains("regenerated"), "{content}");
    }
}
