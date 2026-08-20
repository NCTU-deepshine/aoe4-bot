//! `/set redraft`, and the same button on the set panel — labelled `➕ Create
//! draft` before a set has one, `🔄 Regenerate draft` after. Creation is
//! deferred to that first press rather than minted when the thread opens, and
//! this module is the one place both a set's first draft and its remedy for a
//! draft that went wrong live, since the panel only instructs a seat — it
//! never checks who actually took it (§8.7).
//!
//! `refuse` is pure, like `report::refuse`, and pins the guard order with a
//! test rather than leaving it to be read out of `run`. `run` is the effectful
//! half, and branches on whether the set already has a draft: a first
//! creation has no old room to strike, so it can go straight through
//! `set_thread::create_room` and edit the panel in place; a regenerate does
//! the Discord and DB work in the order §8.7 requires, because
//! `db::set_draft_pointer` nulls the announcement handle, so anything that
//! still needs the *old* handle — striking the stale announcement and panel —
//! has to run before it, and nothing may be destroyed before the replacement
//! room exists to take its place.

use crate::Error;
use crate::db::{to_channel_id, to_db_id, to_message_id};
use crate::locale::Locale;
use crate::tournament::bracket;
use crate::tournament::completion;
use crate::tournament::db::{self, Tournament, TournamentSet};
use crate::tournament::set_thread::{self, SetHeading};
use serenity::all::{CacheHttp, CreateAllowedMentions, CreateMessage, EditMessage};
use sqlx::SqlitePool;
use tracing::error;

/// A player may redraft a set this many times on their own; past it, only an
/// admin can. Two covers the realistic cases — wrong seats, then a fumble —
/// without letting frustration strand a pile of undeletable rooms on someone
/// else's server (§12).
pub(crate) const FREE_REDRAFTS: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedraftOutcome {
    /// The set's first draft, minted on the first press of the button that
    /// otherwise reads `🔄 Regenerate draft`.
    Created,
    Redrafted {
        count: i64,
    },
    /// A finished set is not redraftable; corrections go through `/set report`.
    AlreadyComplete,
    /// A slot is still empty, or the set was settled as a bye.
    NotPlayable,
    /// Neither of the two players, nor an admin.
    NotYours,
    /// The round has no `draft_preset_id` — there is nothing to mint from.
    NoPreset,
    /// Past `FREE_REDRAFTS`, and the caller is not an admin.
    RateLimited {
        count: i64,
    },
    /// The draft tool refused the mint. `existing` says whether this was a
    /// regenerate (so the prior draft is untouched) or a first creation (so
    /// there was nothing to leave untouched) — the two need different wording.
    RoomFailed {
        existing: bool,
    },
}

impl RedraftOutcome {
    pub(crate) fn message(self, locale: Locale) -> String {
        match self {
            RedraftOutcome::Created => locale.pick("Draft 已建立。".to_string(), "Draft created.".to_string()),
            RedraftOutcome::Redrafted { count } => locale.pick(
                format!("已重新產生 Draft（第 {count} 次）。"),
                format!("Draft regenerated (redraft #{count})."),
            ),
            RedraftOutcome::AlreadyComplete => locale.pick(
                "這場對戰已經結束，無法重新產生 Draft。若結果有誤，請用 /set report 更正。".to_string(),
                "That set is already finished, so its draft can't be regenerated. If the result \
                 is wrong, correct it with /set report."
                    .to_string(),
            ),
            RedraftOutcome::NotPlayable => locale.pick(
                "這場對戰還沒有兩位選手，沒有 Draft 可以重新產生。".to_string(),
                "That set doesn't have both players yet, so there's no draft to regenerate.".to_string(),
            ),
            RedraftOutcome::NotYours => locale.pick(
                "只有這場對戰的選手，或管理員，才能重新產生 Draft。".to_string(),
                "Only the two players in this set, or an admin, can regenerate its draft.".to_string(),
            ),
            RedraftOutcome::NoPreset => locale.pick(
                "這個輪次沒有設定 Draft 預設集，無法重新產生。".to_string(),
                "This round has no draft preset configured, so there's nothing to regenerate from.".to_string(),
            ),
            RedraftOutcome::RateLimited { count } => locale.pick(
                format!("這場對戰已經重新產生 {count} 次，之後只有管理員可以再重新產生。"),
                format!(
                    "This set has already been redrafted {count} times — past that, only an \
                     admin can regenerate it."
                ),
            ),
            RedraftOutcome::RoomFailed { existing: true } => locale.pick(
                "無法建立新的 Draft 房間，原本的 Draft 未受影響。請稍後再試。".to_string(),
                "Couldn't create a new draft room — the existing draft is untouched. Try again \
                 shortly."
                    .to_string(),
            ),
            RedraftOutcome::RoomFailed { existing: false } => locale.pick(
                "無法建立 Draft 房間，請稍後再試。".to_string(),
                "Couldn't create a draft room. Try again shortly.".to_string(),
            ),
        }
    }
}

/// Why a redraft would be refused, if it would be. Checked in the order a
/// caller would actually hit them: a finished set first, since nothing else
/// matters once it is — exactly `report::refuse`'s ordering, for the same
/// reason.
pub(crate) fn refuse(set: &TournamentSet, has_preset: bool, is_player: bool, is_admin: bool) -> Option<RedraftOutcome> {
    if completion::is_decided(&set.status) {
        return Some(RedraftOutcome::AlreadyComplete);
    }
    if set.slot1_user_id.is_none() || set.slot2_user_id.is_none() {
        return Some(RedraftOutcome::NotPlayable);
    }
    if !is_player && !is_admin {
        return Some(RedraftOutcome::NotYours);
    }
    if !has_preset {
        return Some(RedraftOutcome::NoPreset);
    }
    if !is_admin && set.redraft_count >= FREE_REDRAFTS {
        return Some(RedraftOutcome::RateLimited {
            count: set.redraft_count,
        });
    }
    None
}

/// Creates `set`'s first draft, or abandons its current one for a fresh one
/// from the same preset — whichever the button/command means by whether the
/// set already has a draft.
///
/// `actor_user_id` need not be a player — a button or command from an admin who
/// is neither of the two names calls in with `is_admin = true` and an id that
/// only ends up in the notice.
pub(crate) async fn run(
    http: impl CacheHttp,
    pool: &SqlitePool,
    tournament: &Tournament,
    set: &TournamentSet,
    actor_user_id: i64,
    is_admin: bool,
) -> Result<RedraftOutcome, Error> {
    let Some(round) = db::get_round(pool, set.round_id).await? else {
        return Ok(RedraftOutcome::NotPlayable);
    };
    let is_player = set.slot1_user_id == Some(actor_user_id) || set.slot2_user_id == Some(actor_user_id);
    if let Some(refusal) = refuse(set, round.draft_preset_id.is_some(), is_player, is_admin) {
        return Ok(refusal);
    }

    // Both slots are `Some` — `refuse` already returned `NotPlayable` otherwise.
    let one = set_thread::player(pool, tournament.id, set.slot1_user_id.unwrap_or_default()).await?;
    let two = set_thread::player(pool, tournament.id, set.slot2_user_id.unwrap_or_default()).await?;
    let heading = SetHeading {
        id: set.id,
        round_name: round.name.clone(),
        position: set.position,
        best_of: round.best_of,
        is_third_place: round.name == bracket::THIRD_PLACE,
    };

    if set.draft_external_id.is_none() {
        // A first draft has no old room to strike and no announcement to
        // supersede, so it goes straight through the same path `open` used to
        // take eagerly, and edits the existing pinned panel in place.
        let Some(room) = set_thread::create_room(pool, tournament, &round, set.id).await else {
            return Ok(RedraftOutcome::RoomFailed { existing: false });
        };

        if let (Some(thread_id), Some(panel_id)) = (set.thread_id.map(to_channel_id), set.panel_message_id) {
            let (content, components) = set_thread::render_panel(&heading, &one, &two, Some(&room));
            if let Err(err) = thread_id
                .edit_message(
                    &http,
                    to_message_id(panel_id),
                    EditMessage::new().content(content).components(components),
                )
                .await
            {
                error!("failed to update the panel for set {}: {err:?}", set.id);
            }
        }

        set_thread::announce(&http, pool, tournament, &heading, &one, &two, &room).await;
        return Ok(RedraftOutcome::Created);
    }

    // Minted first and checked before anything else moves: a failed mint must
    // leave the existing draft exactly as it was.
    let Some((draft_id, room)) = set_thread::mint_room(tournament, &round, set.id).await else {
        return Ok(RedraftOutcome::RoomFailed { existing: true });
    };
    // `None` for an admin who is neither player — the notice falls back to a
    // mention for them, since there is no in-game name to address them by.
    let actor = if one.user_id == actor_user_id {
        Some(&one)
    } else if two.user_id == actor_user_id {
        Some(&two)
    } else {
        None
    };

    // Strike the old links before repointing: `set_draft_pointer` nulls the
    // announcement handle below, so this is the last chance to reach it.
    if let (Some(draft_channel_id), Some(old_announce_id)) =
        (tournament.draft_channel_id, set.draft_announce_message_id)
    {
        let content = set_thread::render_superseded_announcement(&heading, &one, &two);
        if let Err(err) = to_channel_id(draft_channel_id)
            .edit_message(
                &http,
                to_message_id(old_announce_id),
                EditMessage::new().content(content).components(vec![]),
            )
            .await
        {
            error!("failed to strike the stale announcement for set {}: {err:?}", set.id);
        }
    }
    let thread_id = set.thread_id.map(to_channel_id);
    if let (Some(thread_id), Some(old_panel_id)) = (thread_id, set.panel_message_id) {
        let content = set_thread::render_superseded_panel(&heading, &one, &two);
        if let Err(err) = thread_id
            .edit_message(
                &http,
                to_message_id(old_panel_id),
                EditMessage::new().content(content).components(vec![]),
            )
            .await
        {
            error!("failed to strike the stale panel for set {}: {err:?}", set.id);
        }
    }

    db::set_draft_pointer(pool, set.id, &draft_id).await?;
    db::increment_redraft_count(pool, set.id).await?;
    // Guard 2 (§8.7): the imported record of a game played on the abandoned
    // draft is discarded, but never an organizer's own correction.
    db::void_games_for_set(pool, set.id).await?;
    let count = set.redraft_count + 1;

    if let Some(thread_id) = thread_id {
        if let Err(err) = thread_id
            .send_message(
                &http,
                CreateMessage::new()
                    .content(set_thread::render_redraft_notice(actor_user_id, actor, count))
                    .allowed_mentions(CreateAllowedMentions::new()),
            )
            .await
        {
            error!("failed to post the redraft notice for set {}: {err:?}", set.id);
        }

        let (content, components) = set_thread::render_panel(&heading, &one, &two, Some(&room));
        match thread_id
            .send_message(&http, CreateMessage::new().content(content).components(components))
            .await
        {
            Ok(message) => {
                if let Err(err) = message.pin(&http).await {
                    error!("failed to pin the redrawn panel for set {}: {err:?}", set.id);
                }
                if let Err(err) = db::set_panel_message(pool, set.id, to_db_id(message.id)).await {
                    error!("failed to record the redrawn panel for set {}: {err:?}", set.id);
                }
            },
            Err(err) => error!("failed to post the redrawn panel for set {}: {err:?}", set.id),
        }
    }

    set_thread::announce(&http, pool, tournament, &heading, &one, &two, &room).await;

    Ok(RedraftOutcome::Redrafted { count })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(status: &str, slot1: Option<i64>, slot2: Option<i64>, redraft_count: i64) -> TournamentSet {
        TournamentSet {
            id: 1,
            tournament_id: 1,
            round_id: 1,
            position: 1,
            slot1_user_id: slot1,
            slot2_user_id: slot2,
            slot1_wins: 0,
            slot2_wins: 0,
            winner_user_id: None,
            status: status.to_string(),
            draft_external_id: None,
            draft_synced_at: None,
            draft_announce_message_id: None,
            redraft_count,
            thread_id: Some(555),
            panel_message_id: Some(777),
            winner_advances_to_set_id: None,
            winner_advances_to_slot: None,
            loser_advances_to_set_id: None,
            loser_advances_to_slot: None,
            scheduled_at: None,
            completed_at: None,
        }
    }

    fn playable(redraft_count: i64) -> TournamentSet {
        set("ready", Some(10), Some(20), redraft_count)
    }

    #[test]
    fn a_finished_set_is_refused_before_anything_else_is_looked_at() {
        // Even a set with no preset and a stranger for an actor: completion
        // outranks every other guard.
        let done = set("completed", Some(10), Some(20), 0);
        assert_eq!(
            refuse(&done, false, false, false),
            Some(RedraftOutcome::AlreadyComplete)
        );

        let bye = set("bye", Some(10), None, 0);
        assert_eq!(refuse(&bye, true, true, true), Some(RedraftOutcome::AlreadyComplete));
    }

    #[test]
    fn a_set_missing_a_player_has_no_draft_to_regenerate() {
        assert_eq!(
            refuse(&set("pending", Some(10), None, 0), true, true, true),
            Some(RedraftOutcome::NotPlayable)
        );
        assert_eq!(
            refuse(&set("pending", None, None, 0), true, true, true),
            Some(RedraftOutcome::NotPlayable)
        );
    }

    #[test]
    fn a_stranger_is_refused_but_either_player_or_an_admin_gets_through() {
        assert_eq!(refuse(&playable(0), true, false, false), Some(RedraftOutcome::NotYours));
        assert_eq!(refuse(&playable(0), true, true, false), None);
        assert_eq!(refuse(&playable(0), true, false, true), None);
    }

    #[test]
    fn a_round_with_no_preset_has_nothing_to_regenerate_from() {
        assert_eq!(refuse(&playable(0), false, true, false), Some(RedraftOutcome::NoPreset));
    }

    #[test]
    fn a_player_is_rate_limited_at_the_threshold_but_an_admin_is_never() {
        assert_eq!(refuse(&playable(FREE_REDRAFTS - 1), true, true, false), None);
        assert_eq!(
            refuse(&playable(FREE_REDRAFTS), true, true, false),
            Some(RedraftOutcome::RateLimited { count: FREE_REDRAFTS })
        );
        assert_eq!(refuse(&playable(FREE_REDRAFTS + 5), true, true, true), None);
    }

    #[test]
    fn every_refusal_renders_in_both_locales() {
        let outcomes = [
            RedraftOutcome::Created,
            RedraftOutcome::Redrafted { count: 1 },
            RedraftOutcome::AlreadyComplete,
            RedraftOutcome::NotPlayable,
            RedraftOutcome::NotYours,
            RedraftOutcome::NoPreset,
            RedraftOutcome::RateLimited { count: FREE_REDRAFTS },
            RedraftOutcome::RoomFailed { existing: true },
            RedraftOutcome::RoomFailed { existing: false },
        ];
        for outcome in outcomes {
            assert!(!outcome.message(Locale::ZhTw).is_empty());
            assert!(!outcome.message(Locale::En).is_empty());
        }
    }
}
