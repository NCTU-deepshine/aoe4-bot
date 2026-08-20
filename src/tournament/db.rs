//! Row types and queries for the tournament schema.
//!
//! One section per table, in the same order as `migrations/0002_tournament_schema.sql`.
//! Scope is deliberately general-purpose reads/writes, not the business logic built
//! on top (bracket persistence, permission decisions, draft-preset validation, the
//! re-import upsert) — those stay in the module that needs them.

use crate::tournament::bracket::Slot;
use chrono::{DateTime, Utc};
use sqlx::{AssertSqlSafe, FromRow, SqlitePool};
use tracing::error;

fn log_db_error(err: &sqlx::Error) {
    // Debug, not Display: `sqlx::Error`'s Display drops the constraint name and
    // the SQLite extended code, which is usually the whole answer.
    error!("database operation failed with error {err:?}");
}

// 1. tournaments

// Most fields below are written by `insert_tournament` / `set_tournament_channels`
// and read back by the registration replies and the panels, which is why the row
// is wider than anything that creates a tournament needs.
#[derive(FromRow)]
pub(crate) struct Tournament {
    pub id: i64,
    pub slug: String,
    pub name: String,
    pub status: String,
    pub draft_base_url: Option<String>,
    pub announce_channel_id: Option<i64>,
    pub category_id: Option<i64>,
    pub register_channel_id: Option<i64>,
    pub register_message_id: Option<i64>,
    pub bracket_channel_id: Option<i64>,
    pub matches_channel_id: Option<i64>,
    pub draft_channel_id: Option<i64>,
    pub checkin_message_id: Option<i64>,
    pub seed_message_id: Option<i64>,
    pub checkin_closes_at: Option<DateTime<Utc>>,
    pub entrant_cap: i64,
    pub scheduled_start_at: Option<DateTime<Utc>>,
    pub seed_source: String,
    /// `open | invite_only`. Which door into the field is open, as opposed to
    /// `status`, which says whether any door is.
    pub registration_mode: String,
    pub created_by: i64,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

pub(crate) async fn insert_tournament(
    pool: &SqlitePool,
    slug: &str,
    name: &str,
    created_by: i64,
) -> Result<i64, sqlx::Error> {
    // The start time defaults a week out, and is set here rather than as a column
    // default because SQLite rejects a non-constant default on `alter table add
    // column`. Same statement as created_at's own default means one clock, so
    // `scheduled_start_at == created_at + 7 days` holds exactly — which is how
    // `setup::start_time_is_default` spots an untouched placeholder without a
    // column recording whether anyone edited it.
    let result = sqlx::query(
        r"
        insert into tournaments (slug, name, created_by, scheduled_start_at)
        values (?1, ?2, ?3, datetime('now', '+7 days'))
        ",
    )
    .bind(slug)
    .bind(name)
    .bind(created_by)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(result.last_insert_rowid())
}

pub(crate) async fn get_tournament(pool: &SqlitePool, id: i64) -> Result<Option<Tournament>, sqlx::Error> {
    sqlx::query_as(
        r"
        select id, slug, name, status, draft_base_url, announce_channel_id, category_id,
               register_channel_id, register_message_id, bracket_channel_id, matches_channel_id,
               draft_channel_id, checkin_message_id, seed_message_id, checkin_closes_at,
               entrant_cap, scheduled_start_at, seed_source, registration_mode, created_by, created_at,
               started_at, completed_at
        from tournaments
        where id = ?1
        ",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .inspect_err(log_db_error)
}

pub(crate) async fn get_tournament_by_slug(pool: &SqlitePool, slug: &str) -> Result<Option<Tournament>, sqlx::Error> {
    sqlx::query_as(
        r"
        select id, slug, name, status, draft_base_url, announce_channel_id, category_id,
               register_channel_id, register_message_id, bracket_channel_id, matches_channel_id,
               draft_channel_id, checkin_message_id, seed_message_id, checkin_closes_at,
               entrant_cap, scheduled_start_at, seed_source, registration_mode, created_by, created_at,
               started_at, completed_at
        from tournaments
        where slug = ?1
        ",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
    .inspect_err(log_db_error)
}

pub(crate) async fn update_tournament_status(pool: &SqlitePool, id: i64, status: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournaments set status = ?1 where id = ?2")
        .bind(status)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// `/tournament delete`. One statement is enough: every
/// tournament-scoped table cascades off this row — entries, admins, stages,
/// rounds, sets, games and bracket messages — which is invisible here, hence the
/// note. `tournament_players` is deliberately not among them: the Discord↔aoe4world
/// binding is global and outlives any one tournament.
pub(crate) async fn delete_tournament(pool: &SqlitePool, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(r"delete from tournaments where id = ?1")
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// The channel ids `/tournament create` allocates, written once Discord confirms
/// they exist. A carrier rather than six positional args, matching
/// `NewGame`'s precedent (`insert_game`).
pub(crate) struct TournamentChannels {
    pub category_id: Option<i64>,
    pub announce_channel_id: i64,
    pub register_channel_id: i64,
    pub bracket_channel_id: i64,
    pub matches_channel_id: i64,
    pub draft_channel_id: i64,
}

pub(crate) async fn set_tournament_channels(
    pool: &SqlitePool,
    id: i64,
    channels: TournamentChannels,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        update tournaments
        set
            category_id = ?1,
            announce_channel_id = ?2,
            register_channel_id = ?3,
            bracket_channel_id = ?4,
            matches_channel_id = ?5,
            draft_channel_id = ?6
        where id = ?7
        ",
    )
    .bind(channels.category_id)
    .bind(channels.announce_channel_id)
    .bind(channels.register_channel_id)
    .bind(channels.bracket_channel_id)
    .bind(channels.matches_channel_id)
    .bind(channels.draft_channel_id)
    .bind(id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

/// The registration panel's message id — set once, right
/// after `/tournament create` posts the panel to the register channel it just made.
pub(crate) async fn set_register_message_id(
    pool: &SqlitePool,
    id: i64,
    register_message_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournaments set register_message_id = ?1 where id = ?2")
        .bind(register_message_id)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// The check-in panel's message id — set right after
/// `/tournament open-checkin` posts the panel to the register channel, and back
/// to `None` by `/tournament reopen-registration`, which deletes that message.
pub(crate) async fn set_checkin_message_id(
    pool: &SqlitePool,
    id: i64,
    checkin_message_id: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournaments set checkin_message_id = ?1 where id = ?2")
        .bind(checkin_message_id)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// The seeding panel's message id — written once by `/tournament create` and
/// again by any repost, since the panel is a fixture of the bracket channel for
/// the whole event and nothing deletes it.
///
/// Still `Option`, because a first post that failed leaves no id and
/// `commands::ensure_seed_panel` is what puts one back.
pub(crate) async fn set_seed_message_id(
    pool: &SqlitePool,
    id: i64,
    seed_message_id: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournaments set seed_message_id = ?1 where id = ?2")
        .bind(seed_message_id)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// When check-in closes on its own — informational
/// only today; nothing polls this to auto-close.
pub(crate) async fn set_checkin_closes_at(
    pool: &SqlitePool,
    id: i64,
    closes_at: Option<DateTime<Utc>>,
) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournaments set checkin_closes_at = ?1 where id = ?2")
        .bind(closes_at)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// The maximum size of the field. Enforced at registration rather than at
/// start, so an over-full field never happens in the first place.
pub(crate) async fn set_entrant_cap(pool: &SqlitePool, id: i64, entrant_cap: i64) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournaments set entrant_cap = ?1 where id = ?2")
        .bind(entrant_cap)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// When the event is meant to begin. Stored UTC; the command parses a local wall
/// time and Discord renders it back per reader.
pub(crate) async fn set_scheduled_start_at(
    pool: &SqlitePool,
    id: i64,
    scheduled_start_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournaments set scheduled_start_at = ?1 where id = ?2")
        .bind(scheduled_start_at)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// Whether the field's order is the bot's suggestion or the organizers' own.
/// `seeding::SeedPolicy` is the reader; `seed set` and `seed refresh` are the
/// only writers.
pub(crate) async fn set_seed_source(pool: &SqlitePool, id: i64, seed_source: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournaments set seed_source = ?1 where id = ?2")
        .bind(seed_source)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// Whether the public may sign themselves up. Read only through
/// `registration::RegistrationState`, so the gate and the panel cannot form
/// different opinions of it; `/tournament setup` is the only writer.
pub(crate) async fn set_registration_mode(
    pool: &SqlitePool,
    id: i64,
    registration_mode: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournaments set registration_mode = ?1 where id = ?2")
        .bind(registration_mode)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// Entrants occupying a slot. `withdrawn` and `no_show` rows persist but are
/// not in the field, so withdrawing genuinely frees a place against the cap.
pub(crate) async fn count_active_entries(pool: &SqlitePool, tournament_id: i64) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        r"
        select count(*)
        from tournament_entries
        where tournament_id = ?1
          and status = 'active'
        ",
    )
    .bind(tournament_id)
    .fetch_one(pool)
    .await
    .inspect_err(log_db_error)
}

// 10. tournament_round_presets — which draft preset a round uses, and therefore
//     how long its sets are. Keyed by depth back from the final; see
//     `tournament::setup::preset_for_depth` for how one is resolved.

#[derive(FromRow)]
pub(crate) struct RoundPreset {
    pub tournament_id: i64,
    pub from_depth: i64,
    pub draft_preset_id: String,
    /// The preset's name on the tool when it was assigned, for display only. Goes
    /// stale on a rename, which is why the panel links the id's live page.
    pub preset_name: Option<String>,
    pub best_of: i64,
    pub assigned_at: DateTime<Utc>,
}

/// Upsert: re-assigning the same depth replaces it, which is how an organizer
/// corrects a preset without a separate clear step.
pub(crate) async fn upsert_round_preset(
    pool: &SqlitePool,
    tournament_id: i64,
    from_depth: i64,
    draft_preset_id: &str,
    preset_name: &str,
    best_of: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        insert into tournament_round_presets (tournament_id, from_depth, draft_preset_id, preset_name, best_of)
        values (?1, ?2, ?3, ?4, ?5)
        on conflict (tournament_id, from_depth) do update set
            draft_preset_id = excluded.draft_preset_id,
            preset_name = excluded.preset_name,
            best_of = excluded.best_of,
            assigned_at = datetime('now')
        ",
    )
    .bind(tournament_id)
    .bind(from_depth)
    .bind(draft_preset_id)
    .bind(preset_name)
    .bind(best_of)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn list_round_presets(pool: &SqlitePool, tournament_id: i64) -> Result<Vec<RoundPreset>, sqlx::Error> {
    sqlx::query_as(
        r"
        select tournament_id, from_depth, draft_preset_id, preset_name, best_of, assigned_at
        from tournament_round_presets
        where tournament_id = ?1
        order by from_depth
        ",
    )
    .bind(tournament_id)
    .fetch_all(pool)
    .await
    .inspect_err(log_db_error)
}

/// Resolves a tournament from ANY of its five stored channel ids — the announce
/// channel (`/tournament create`'s invoking channel) or any of the four it
/// created. Used by `/tournament admin add|remove|list`'s resolution, which has
/// no slug argument to go on (see `tournament::access::tournament_admin_only`).
/// The statuses a tournament is still live in — `completed` and `canceled` are
/// history. Listed rather than negated so a new status has to be classified
/// deliberately instead of defaulting to live.
const LIVE_STATUSES: &str = "'registration', 'checkin', 'seeding', 'running'";

pub(crate) async fn get_tournament_by_any_channel_id(
    pool: &SqlitePool,
    channel_id: i64,
) -> Result<Option<Tournament>, sqlx::Error> {
    sqlx::query_as(AssertSqlSafe(format!(
        r"
        select id, slug, name, status, draft_base_url, announce_channel_id, category_id,
               register_channel_id, register_message_id, bracket_channel_id, matches_channel_id,
               draft_channel_id, checkin_message_id, seed_message_id, checkin_closes_at,
               entrant_cap, scheduled_start_at, seed_source, registration_mode, created_by, created_at,
               started_at, completed_at
        from tournaments
        where announce_channel_id = ?1
           or register_channel_id = ?1
           or bracket_channel_id = ?1
           or draft_channel_id = ?1
           or matches_channel_id = ?1
        -- A finished tournament keeps its channel ids, so an announce channel
        -- reused for the next event matches two rows. Prefer the live one, then
        -- the newer, rather than leaving the choice to row order.
        order by case when status in ({LIVE_STATUSES}) then 0 else 1 end, id desc
        limit 1
        "
    )))
    .bind(channel_id)
    .fetch_optional(pool)
    .await
    .inspect_err(log_db_error)
}

/// Enough to name the tournament holding a channel. The guard below reports it
/// and does nothing else with it, so it does not read the whole row.
#[derive(FromRow)]
pub(crate) struct TournamentLabel {
    pub name: String,
    pub slug: String,
}

/// A live tournament already announcing in `channel_id`, if there is one.
///
/// `create` refuses in that case: two live tournaments sharing an announce
/// channel makes every command run there ambiguous, and the organizer cannot
/// see which one they hit.
pub(crate) async fn get_live_tournament_by_announce_channel(
    pool: &SqlitePool,
    channel_id: i64,
) -> Result<Option<TournamentLabel>, sqlx::Error> {
    sqlx::query_as(AssertSqlSafe(format!(
        r"
        select name, slug
        from tournaments
        where announce_channel_id = ?1
          and status in ({LIVE_STATUSES})
        order by id desc
        limit 1
        "
    )))
    .bind(channel_id)
    .fetch_optional(pool)
    .await
    .inspect_err(log_db_error)
}

/// Every tournament worth reconciling panels for at boot — `completed` and
/// `canceled` events keep their message ids as a record, not something to
/// repost over on every restart.
pub(crate) async fn list_live_tournaments(pool: &SqlitePool) -> Result<Vec<Tournament>, sqlx::Error> {
    sqlx::query_as(AssertSqlSafe(format!(
        r"
        select id, slug, name, status, draft_base_url, announce_channel_id, category_id,
               register_channel_id, register_message_id, bracket_channel_id, matches_channel_id,
               draft_channel_id, checkin_message_id, seed_message_id, checkin_closes_at,
               entrant_cap, scheduled_start_at, seed_source, registration_mode, created_by, created_at,
               started_at, completed_at
        from tournaments
        where status in ({LIVE_STATUSES})
        "
    )))
    .fetch_all(pool)
    .await
    .inspect_err(log_db_error)
}

// 2. tournament_stages

#[derive(FromRow)]
pub(crate) struct TournamentStage {
    pub id: i64,
    pub tournament_id: i64,
    pub ordinal: i64,
    pub name: String,
    pub format: String,
    pub config: Option<String>,
    pub status: String,
}

pub(crate) async fn insert_stage(
    pool: &SqlitePool,
    tournament_id: i64,
    ordinal: i64,
    name: &str,
    format: &str,
) -> Result<i64, sqlx::Error> {
    let result = sqlx::query(
        r"
        insert into tournament_stages (tournament_id, ordinal, name, format)
        values (?1, ?2, ?3, ?4)
        ",
    )
    .bind(tournament_id)
    .bind(ordinal)
    .bind(name)
    .bind(format)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(result.last_insert_rowid())
}

pub(crate) async fn list_stages_for_tournament(
    pool: &SqlitePool,
    tournament_id: i64,
) -> Result<Vec<TournamentStage>, sqlx::Error> {
    sqlx::query_as(
        r"
        select id, tournament_id, ordinal, name, format, config, status
        from tournament_stages
        where tournament_id = ?1
        order by ordinal
        ",
    )
    .bind(tournament_id)
    .fetch_all(pool)
    .await
    .inspect_err(log_db_error)
}

pub(crate) async fn update_stage_status(pool: &SqlitePool, id: i64, status: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournament_stages set status = ?1 where id = ?2")
        .bind(status)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

// 3. tournament_rounds

#[derive(FromRow)]
pub(crate) struct TournamentRound {
    pub id: i64,
    pub stage_id: i64,
    pub ordinal: i64,
    pub name: String,
    pub best_of: i64,
    pub bracket: Option<String>,
    pub draft_preset_id: Option<String>,
    pub rules: Option<String>,
}

pub(crate) async fn insert_round(
    pool: &SqlitePool,
    stage_id: i64,
    ordinal: i64,
    name: &str,
    best_of: i64,
    bracket: Option<&str>,
) -> Result<i64, sqlx::Error> {
    let result = sqlx::query(
        r"
        insert into tournament_rounds (stage_id, ordinal, name, best_of, bracket)
        values (?1, ?2, ?3, ?4, ?5)
        ",
    )
    .bind(stage_id)
    .bind(ordinal)
    .bind(name)
    .bind(best_of)
    .bind(bracket)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(result.last_insert_rowid())
}

pub(crate) async fn list_rounds_for_stage(
    pool: &SqlitePool,
    stage_id: i64,
) -> Result<Vec<TournamentRound>, sqlx::Error> {
    sqlx::query_as(
        r"
        select id, stage_id, ordinal, name, best_of, bracket, draft_preset_id, rules
        from tournament_rounds
        where stage_id = ?1
        order by ordinal
        ",
    )
    .bind(stage_id)
    .fetch_all(pool)
    .await
    .inspect_err(log_db_error)
}

pub(crate) async fn get_round(pool: &SqlitePool, id: i64) -> Result<Option<TournamentRound>, sqlx::Error> {
    sqlx::query_as(
        r"
        select id, stage_id, ordinal, name, best_of, bracket, draft_preset_id, rules
        from tournament_rounds
        where id = ?1
        ",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .inspect_err(log_db_error)
}

// 4. tournament_players

#[derive(FromRow)]
pub(crate) struct TournamentPlayer {
    pub user_id: i64,
    pub aoe4_id: i64,
    pub display_name: String,
    pub bound_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
}

pub(crate) async fn get_player(pool: &SqlitePool, user_id: i64) -> Result<Option<TournamentPlayer>, sqlx::Error> {
    sqlx::query_as(
        r"
        select user_id, aoe4_id, display_name, bound_at, updated_at
        from tournament_players
        where user_id = ?1
        ",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .inspect_err(log_db_error)
}

pub(crate) async fn get_player_by_aoe4_id(
    pool: &SqlitePool,
    aoe4_id: i64,
) -> Result<Option<TournamentPlayer>, sqlx::Error> {
    sqlx::query_as(
        r"
        select user_id, aoe4_id, display_name, bound_at, updated_at
        from tournament_players
        where aoe4_id = ?1
        ",
    )
    .bind(aoe4_id)
    .fetch_optional(pool)
    .await
    .inspect_err(log_db_error)
}

/// Every entry a player has ever had, in any tournament and whatever its status.
///
/// Counts `withdrawn` rows too, deliberately: entries are never deleted, and
/// `tournament_entries.user_id` references this player row with no `on delete
/// cascade`, so a withdrawn entry blocks a delete exactly as an active one does.
/// Counting only the live ones would report success and then hit a raw FK error.
pub(crate) async fn count_entries_for_player(pool: &SqlitePool, user_id: i64) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(r"select count(*) from tournament_entries where user_id = ?1")
        .bind(user_id)
        .fetch_one(pool)
        .await
        .inspect_err(log_db_error)
}

/// Drops the global Discord↔aoe4world binding, freeing the `aoe4_id` for someone
/// else to claim. Callers must check `count_entries_for_player` first — the
/// foreign keys from entries, sets and games have no cascade, so this fails
/// rather than orphaning them. `accounts` is a separate table and is untouched
/// — the two are deliberately not linked.
pub(crate) async fn delete_player(pool: &SqlitePool, user_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(r"delete from tournament_players where user_id = ?1")
        .bind(user_id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// Binds a profile, creating the player row first if there was none.
///
/// `display_name` only ever lands from this call on the fresh-insert branch —
/// the column is `not null`, so an insert needs something, and it is
/// immediately superseded by `set_player_display_name`'s own update on every
/// caller that already has a row. The on-conflict branch leaves the name
/// alone entirely, which is what keeps every existing caller's behavior
/// exactly as it was before this could ever insert anything.
pub(crate) async fn upsert_player_binding(
    pool: &SqlitePool,
    user_id: i64,
    aoe4_id: i64,
    display_name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        insert into tournament_players (user_id, aoe4_id, display_name)
        values (?1, ?2, ?3)
        on conflict(user_id) do update
        set
            aoe4_id = excluded.aoe4_id,
            updated_at = datetime('now')
        ",
    )
    .bind(user_id)
    .bind(aoe4_id)
    .bind(display_name)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

/// Unlike `aoe4_id`/`elo`/`atr`, a name carries no game-result attribution, so it is
/// not frozen on existing entries the way those are — this writes through
/// to every entry the player has in a tournament that has not yet completed or been
/// canceled, so brackets and threads always show the current name. A transaction
/// because the two tables must agree: an entry with a name `tournament_players`
/// no longer has would be a silent regression the next time it's read.
pub(crate) async fn set_player_display_name(
    pool: &SqlitePool,
    user_id: i64,
    display_name: &str,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await.inspect_err(log_db_error)?;

    sqlx::query(
        r"
        update tournament_players
        set
            display_name = ?1,
            updated_at = datetime('now')
        where user_id = ?2
        ",
    )
    .bind(display_name)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .inspect_err(log_db_error)?;

    sqlx::query(
        r"
        update tournament_entries
        set display_name = ?1
        where user_id = ?2
          and tournament_id in (
              select id from tournaments where status not in ('completed', 'canceled')
          )
        ",
    )
    .bind(display_name)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .inspect_err(log_db_error)?;

    tx.commit().await.inspect_err(log_db_error)?;
    Ok(())
}

/// A first sign-up: writes `tournament_players` and the
/// entry together, atomically — neither survives if the other fails. Only called
/// when the caller has already confirmed no `tournament_players` row exists for
/// `user_id`; a concurrent sign-up racing for the same `aoe4_id` still surfaces as
/// a genuine UNIQUE constraint failure on `tournament_players.aoe4_id`, which the
/// caller (`tournament::registration`) maps to a friendly message rather than
/// treating as an unexpected error. `elo` is the profile's `rm_1v1_elo` rating,
/// already in hand from the same aoe4world fetch that resolved `display_name` —
/// snapshotted now and refreshed again at seeding.
pub(crate) async fn register_new_player_and_entry(
    pool: &SqlitePool,
    tournament_id: i64,
    user_id: i64,
    aoe4_id: i64,
    display_name: &str,
    elo: Option<i64>,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await.inspect_err(log_db_error)?;

    sqlx::query(
        r"
        insert into tournament_players (user_id, aoe4_id, display_name)
        values (?1, ?2, ?3)
        ",
    )
    .bind(user_id)
    .bind(aoe4_id)
    .bind(display_name)
    .execute(&mut *tx)
    .await
    .inspect_err(log_db_error)?;

    sqlx::query(
        r"
        insert into tournament_entries (tournament_id, user_id, aoe4_id, display_name, elo)
        values (?1, ?2, ?3, ?4, ?5)
        ",
    )
    .bind(tournament_id)
    .bind(user_id)
    .bind(aoe4_id)
    .bind(display_name)
    .bind(elo)
    .execute(&mut *tx)
    .await
    .inspect_err(log_db_error)?;

    tx.commit().await.inspect_err(log_db_error)?;
    Ok(())
}

/// An admin putting someone in the field: writes the player row and the entry
/// together, the way a first sign-up does, but with no profile on either.
///
/// Both statements are upserts, which is what makes re-inviting the same person
/// a name correction rather than a second verb — and what brings an uninvited
/// entry back to `active`. The player row is left alone if it already exists:
/// an invitee who has played before keeps whatever profile they bound then, and
/// only their entry says they were invited to this one.
///
/// The caller has already refused a self-registered entry, so the `status =
/// 'active'` in the update can only ever revive an entry an admin created.
/// Writes (or reactivates) the entry `/tournament invite` creates.
///
/// No player-row write here any more: by the time `invite`'s own resolution
/// reaches this call, the player row is already guaranteed to exist with the
/// right binding — `claim_profile` created or updated it for a fresh claim,
/// and every other branch only ever reaches here from one that already did.
pub(crate) async fn upsert_invited_entry(
    pool: &SqlitePool,
    tournament_id: i64,
    user_id: i64,
    aoe4_id: i64,
    display_name: &str,
    invited_by: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        insert into tournament_entries (tournament_id, user_id, aoe4_id, display_name, invited_by)
        values (?1, ?2, ?3, ?4, ?5)
        on conflict(tournament_id, user_id) do update
        set
            aoe4_id = excluded.aoe4_id,
            display_name = excluded.display_name,
            invited_by = excluded.invited_by,
            status = 'active'
        ",
    )
    .bind(tournament_id)
    .bind(user_id)
    .bind(aoe4_id)
    .bind(display_name)
    .bind(invited_by)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

/// Whether `user_id` holds an entry in any `running` tournament — the guard for
/// `/tournament rebind`: a rebind is refused while the user has an entry in a
/// running tournament, since the profile is snapshotted onto entries and sets
/// already reference the player. Deliberately
/// global, not scoped to one tournament, and deliberately narrower than
/// registration/withdrawal's "has the tournament started" gate — a `completed` or
/// `canceled` tournament's entry is frozen history, not something a rebind could
/// disturb.
pub(crate) async fn has_running_tournament_entry(pool: &SqlitePool, user_id: i64) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        r"
        select exists(
            select 1
            from tournament_entries e
            join tournaments t on t.id = e.tournament_id
            where e.user_id = ?1
              and t.status = 'running'
        )
        ",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .inspect_err(log_db_error)
}

// 5. tournament_entries

#[derive(FromRow)]
pub(crate) struct TournamentEntry {
    pub tournament_id: i64,
    pub user_id: i64,
    /// Snapshotted from the player row at sign-up. `not null` because every
    /// entry is bound the moment it is written, whether by `register` or by
    /// `invite` resolving one immediately.
    pub aoe4_id: i64,
    /// The admin who put them in the field, or `None` for a self-registered
    /// entrant. Being invited is a fact about one tournament, not about a player.
    pub invited_by: Option<i64>,
    pub seed: Option<i64>,
    pub suggested_seed: Option<i64>,
    /// The seat an organizer pinned this entrant to, or `None` for an entrant
    /// left to the default tiering. `seed` is always the resolution of every
    /// pin in the field — this column is the pin itself.
    pub manual_seed: Option<i64>,
    pub display_name: String,
    pub elo: Option<i64>,
    pub atr: Option<f64>,
    pub atr_source: Option<String>,
    pub status: String,
    pub registered_at: DateTime<Utc>,
    pub checked_in_at: Option<DateTime<Utc>>,
}

/// `elo` is snapshotted at sign-up so the bracket preview has something real to
/// order by before seeding runs. ATR is not: it is one bulk request for the
/// whole field, so it stays a seeding-time fetch rather than a per-entrant one.
pub(crate) async fn insert_entry(
    pool: &SqlitePool,
    tournament_id: i64,
    user_id: i64,
    aoe4_id: i64,
    display_name: &str,
    elo: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        insert into tournament_entries (tournament_id, user_id, aoe4_id, display_name, elo)
        values (?1, ?2, ?3, ?4, ?5)
        ",
    )
    .bind(tournament_id)
    .bind(user_id)
    .bind(aoe4_id)
    .bind(display_name)
    .bind(elo)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn get_entry(
    pool: &SqlitePool,
    tournament_id: i64,
    user_id: i64,
) -> Result<Option<TournamentEntry>, sqlx::Error> {
    sqlx::query_as(
        r"
        select tournament_id, user_id, aoe4_id, invited_by, seed, suggested_seed, manual_seed, display_name, elo,
               atr, atr_source, status, registered_at, checked_in_at
        from tournament_entries
        where tournament_id = ?1
          and user_id = ?2
        ",
    )
    .bind(tournament_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .inspect_err(log_db_error)
}

pub(crate) async fn list_entries_for_tournament(
    pool: &SqlitePool,
    tournament_id: i64,
) -> Result<Vec<TournamentEntry>, sqlx::Error> {
    sqlx::query_as(
        r"
        select tournament_id, user_id, aoe4_id, invited_by, seed, suggested_seed, manual_seed, display_name, elo,
               atr, atr_source, status, registered_at, checked_in_at
        from tournament_entries
        where tournament_id = ?1
        ",
    )
    .bind(tournament_id)
    .fetch_all(pool)
    .await
    .inspect_err(log_db_error)
}

pub(crate) async fn update_entry_status(
    pool: &SqlitePool,
    tournament_id: i64,
    user_id: i64,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        update tournament_entries
        set status = ?1
        where tournament_id = ?2
          and user_id = ?3
        ",
    )
    .bind(status)
    .bind(tournament_id)
    .bind(user_id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn set_entry_checked_in(
    pool: &SqlitePool,
    tournament_id: i64,
    user_id: i64,
    checked_in_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        update tournament_entries
        set checked_in_at = ?1
        where tournament_id = ?2
          and user_id = ?3
        ",
    )
    .bind(checked_in_at)
    .bind(tournament_id)
    .bind(user_id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

/// `/tournament close-checkin`'s no-show sweep: every
/// `active` entry that never checked in becomes `no_show` in one statement.
/// Already-`withdrawn`/`no_show` entries are untouched. Returns how many rows
/// changed, for the closing reply.
///
/// An invited entry is exempt, because check-in was never asked of it. Stamping
/// `checked_in_at` at invite time would spare it here too and is the tempting
/// version, but it is a lie the rest of the system reads back: a reopen clears
/// that column, and the check-in counter would report a field nobody confirmed.
pub(crate) async fn mark_no_shows(pool: &SqlitePool, tournament_id: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r"
        update tournament_entries
        set status = 'no_show'
        where tournament_id = ?1
          and status = 'active'
          and checked_in_at is null
          and invited_by is null
        ",
    )
    .bind(tournament_id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(result.rows_affected())
}

/// `mark_no_shows`'s exact inverse, for `/tournament reopen-registration`. Only
/// `no_show` is touched, and only `mark_no_shows` ever writes that status, so
/// every row this restores was `active` before —
/// `withdrawn` and `eliminated` entries are deliberately left alone.
pub(crate) async fn revert_no_shows(pool: &SqlitePool, tournament_id: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r"
        update tournament_entries
        set status = 'active'
        where tournament_id = ?1
          and status = 'no_show'
        ",
    )
    .bind(tournament_id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(result.rows_affected())
}

/// Wipes the check-in round for `/tournament reopen-registration`.
///
/// Seeds are `clear_seeds`'s job, not this one's: a reopen only discards them
/// when the order was the bot's own suggestion, and a manual order survives the
/// rewind.
pub(crate) async fn clear_checkins(pool: &SqlitePool, tournament_id: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r"
        update tournament_entries
        set checked_in_at = null
        where tournament_id = ?1
          and checked_in_at is not null
        ",
    )
    .bind(tournament_id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(result.rows_affected())
}

/// Drops a suggested seed order that a reopen has invalidated. The
/// `unique (tournament_id, seed)` index tolerates repeated nulls, so this needs
/// no ordering pass of its own.
pub(crate) async fn clear_seeds(pool: &SqlitePool, tournament_id: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r"
        update tournament_entries
        set seed = null,
            suggested_seed = null
        where tournament_id = ?1
          and (seed is not null or suggested_seed is not null)
        ",
    )
    .bind(tournament_id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(result.rows_affected())
}

/// Just the ELO, unlike `set_entry_ratings`, which would blank an `atr` the
/// seeding pass had already written.
pub(crate) async fn set_entry_elo(
    pool: &SqlitePool,
    tournament_id: i64,
    user_id: i64,
    elo: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        update tournament_entries
        set elo = ?1
        where tournament_id = ?2
          and user_id = ?3
        ",
    )
    .bind(elo)
    .bind(tournament_id)
    .bind(user_id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn set_entry_ratings(
    pool: &SqlitePool,
    tournament_id: i64,
    user_id: i64,
    elo: Option<i64>,
    atr: Option<f64>,
    atr_source: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        update tournament_entries
        set
            elo = ?1,
            atr = ?2,
            atr_source = ?3
        where tournament_id = ?4
          and user_id = ?5
        ",
    )
    .bind(elo)
    .bind(atr)
    .bind(atr_source)
    .bind(tournament_id)
    .bind(user_id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

/// Writes `ordered_user_ids` as seeds 1..n in one transaction.
///
/// **Every seed is nulled first, and that is load-bearing rather than tidy:**
/// `unique (tournament_id, seed)` is enforced per row as the statement runs, so
/// shifting a field down by one would collide on the very first row without a
/// clear pass. Writing the whole order rather than the changed rows also
/// guarantees the result is contiguous, which is what starting a tournament
/// requires of a finalized field.
///
/// `also_suggested` separates the two callers: `close-checkin` records what the
/// tiering proposed, an organizer's override touches only `seed`, so the
/// suggestion stays on the panel to compare against.
pub(crate) async fn set_seed_order(
    pool: &SqlitePool,
    tournament_id: i64,
    ordered_user_ids: &[i64],
    also_suggested: bool,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await.inspect_err(log_db_error)?;

    sqlx::query(r"update tournament_entries set seed = null where tournament_id = ?1")
        .bind(tournament_id)
        .execute(&mut *tx)
        .await
        .inspect_err(log_db_error)?;

    for (index, user_id) in ordered_user_ids.iter().enumerate() {
        let seed = i64::try_from(index + 1).unwrap_or(i64::MAX);
        let sql = if also_suggested {
            r"
            update tournament_entries
            set
                seed = ?1,
                suggested_seed = ?1
            where tournament_id = ?2
              and user_id = ?3
            "
        } else {
            r"
            update tournament_entries
            set seed = ?1
            where tournament_id = ?2
              and user_id = ?3
            "
        };
        sqlx::query(sql)
            .bind(seed)
            .bind(tournament_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .inspect_err(log_db_error)?;
    }

    tx.commit().await.inspect_err(log_db_error)?;
    Ok(())
}

/// Pins `user_id` to `seat`, evicting anyone else already pinned there — a
/// newer pin always wins. Returns who it evicted, if anyone, so the caller can
/// name them in the reply.
///
/// One transaction, evicted row nulled before the new pin is written, so the
/// unique index on `(tournament_id, manual_seed)` is never contended —
/// `set_seed_order`'s own clear-first pass is the same trick for the same reason.
pub(crate) async fn set_manual_seed(
    pool: &SqlitePool,
    tournament_id: i64,
    user_id: i64,
    seat: i64,
) -> Result<Option<i64>, sqlx::Error> {
    let mut tx = pool.begin().await.inspect_err(log_db_error)?;

    let displaced: Option<i64> = sqlx::query_scalar(
        r"
        select user_id
        from tournament_entries
        where tournament_id = ?1
          and manual_seed = ?2
          and user_id <> ?3
        ",
    )
    .bind(tournament_id)
    .bind(seat)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .inspect_err(log_db_error)?;

    sqlx::query(
        r"
        update tournament_entries
        set manual_seed = null
        where tournament_id = ?1
          and manual_seed = ?2
          and user_id <> ?3
        ",
    )
    .bind(tournament_id)
    .bind(seat)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .inspect_err(log_db_error)?;

    sqlx::query(
        r"
        update tournament_entries
        set manual_seed = ?1
        where tournament_id = ?2
          and user_id = ?3
        ",
    )
    .bind(seat)
    .bind(tournament_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .inspect_err(log_db_error)?;

    tx.commit().await.inspect_err(log_db_error)?;
    Ok(displaced)
}

/// Drops every pin in the tournament — what `/tournament seed refresh` means by
/// "take the suggestion back."
pub(crate) async fn clear_manual_seeds(pool: &SqlitePool, tournament_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournament_entries set manual_seed = null where tournament_id = ?1")
        .bind(tournament_id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn set_entry_seed(
    pool: &SqlitePool,
    tournament_id: i64,
    user_id: i64,
    seed: Option<i64>,
    suggested_seed: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        update tournament_entries
        set
            seed = ?1,
            suggested_seed = ?2
        where tournament_id = ?3
          and user_id = ?4
        ",
    )
    .bind(seed)
    .bind(suggested_seed)
    .bind(tournament_id)
    .bind(user_id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

// 6. tournament_sets

#[derive(FromRow)]
pub(crate) struct TournamentSet {
    pub id: i64,
    pub tournament_id: i64,
    pub round_id: i64,
    pub position: i64,
    pub slot1_user_id: Option<i64>,
    pub slot2_user_id: Option<i64>,
    pub slot1_wins: i64,
    pub slot2_wins: i64,
    pub winner_user_id: Option<i64>,
    pub status: String,
    pub draft_external_id: Option<String>,
    pub draft_synced_at: Option<DateTime<Utc>>,
    pub draft_announce_message_id: Option<i64>,
    pub redraft_count: i64,
    pub thread_id: Option<i64>,
    /// The pinned set panel, so a redraft can strike the abandoned room's
    /// live seat-claim link before replacing it — see `0014_set_panel_message.sql`.
    pub panel_message_id: Option<i64>,
    pub winner_advances_to_set_id: Option<i64>,
    pub winner_advances_to_slot: Option<i64>,
    pub loser_advances_to_set_id: Option<i64>,
    pub loser_advances_to_slot: Option<i64>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

const TOURNAMENT_SET_COLUMNS: &str = r"
    id, tournament_id, round_id, position, slot1_user_id, slot2_user_id, slot1_wins,
    slot2_wins, winner_user_id, status, draft_external_id, draft_synced_at,
    draft_announce_message_id, redraft_count, thread_id, panel_message_id,
    winner_advances_to_set_id, winner_advances_to_slot, loser_advances_to_set_id,
    loser_advances_to_slot, scheduled_at, completed_at
";

pub(crate) async fn insert_set(
    pool: &SqlitePool,
    tournament_id: i64,
    round_id: i64,
    position: i64,
    slot1_user_id: Option<i64>,
    slot2_user_id: Option<i64>,
    status: &str,
) -> Result<i64, sqlx::Error> {
    let result = sqlx::query(
        r"
        insert into tournament_sets (tournament_id, round_id, position, slot1_user_id, slot2_user_id, status)
        values (?1, ?2, ?3, ?4, ?5, ?6)
        ",
    )
    .bind(tournament_id)
    .bind(round_id)
    .bind(position)
    .bind(slot1_user_id)
    .bind(slot2_user_id)
    .bind(status)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(result.last_insert_rowid())
}

/// Writes a whole generated bracket — stage, rounds, sets and their advancement
/// links — in **one transaction**, so a tournament is never left half-bracketed.
///
/// Assembled here rather than from `insert_stage`/`insert_round`/`insert_set`,
/// which each take a `&SqlitePool` and would therefore each commit on their own.
///
/// Two passes over the sets are unavoidable: a set's `winner_advances_to_set_id`
/// points at a row in the *next* round, whose id does not exist until that round
/// has been inserted. So every set is written first, ids collected by
/// (round, position), then the links filled in.
///
/// `seed_to_user` maps a seed to the entrant holding it; a seed with no entrant
/// is a bye slot and is simply absent.
///
/// `per_round` holds the resolved preset for each round in `bracket.rounds` order, so
/// each round records the preset its drafts are created from — without it
/// `set_thread::create_room` has no preset and no set ever gets a room. **Must be
/// exactly `bracket.rounds.len()` long** — when a 3rd place round exists, the
/// caller (`start::start`) is responsible for giving it one more entry (the
/// semifinal's own preset) before calling this, since the 3rd place round itself
/// resolves no preset of its own.
pub(crate) async fn insert_bracket(
    pool: &SqlitePool,
    tournament_id: i64,
    bracket: &crate::tournament::bracket::Bracket,
    seed_to_user: &std::collections::HashMap<u32, i64>,
    per_round: &[&RoundPreset],
) -> Result<(), sqlx::Error> {
    use crate::tournament::bracket::Slot;

    // A short slice would write nulls and silently disable draft creation.
    debug_assert_eq!(per_round.len(), bracket.rounds.len());

    let mut tx = pool.begin().await.inspect_err(log_db_error)?;

    let stage = sqlx::query(
        r"
        insert into tournament_stages (tournament_id, ordinal, name, format)
        values (?1, 1, 'Main Bracket', 'single_elim')
        ",
    )
    .bind(tournament_id)
    .execute(&mut *tx)
    .await
    .inspect_err(log_db_error)?
    .last_insert_rowid();

    // (round ordinal, position) -> set id, for the linking pass below.
    let mut set_ids: std::collections::HashMap<(usize, usize), i64> = std::collections::HashMap::new();

    // Zipped rather than indexed by `round.ordinal - 1`: a 3rd place round's
    // ordinal is `round_count + 1`, one past `per_round`'s own rounds, and the
    // caller is the one responsible for extending `per_round` to match (with the
    // semifinal's own preset) before calling this. Zipping means a caller that
    // forgets loses the round entirely to the `debug_assert_eq!` above rather than
    // silently indexing past the real presets into a NULL draft_preset_id.
    for (round, preset) in bracket.rounds.iter().zip(per_round.iter()) {
        let round_id = sqlx::query(
            r"
            insert into tournament_rounds (stage_id, ordinal, name, best_of, draft_preset_id)
            values (?1, ?2, ?3, ?4, ?5)
            ",
        )
        .bind(stage)
        .bind(i64::try_from(round.ordinal).unwrap())
        .bind(&round.name)
        .bind(i64::from(round.best_of))
        .bind(preset.draft_preset_id.as_str())
        .execute(&mut *tx)
        .await
        .inspect_err(log_db_error)?
        .last_insert_rowid();

        for set in &round.sets {
            let slot1 = set.slot1.and_then(|seed| seed_to_user.get(&seed)).copied();
            let slot2 = set.slot2.and_then(|seed| seed_to_user.get(&seed)).copied();
            let set_id = sqlx::query(
                r"
                insert into tournament_sets (tournament_id, round_id, position, slot1_user_id, slot2_user_id, status)
                values (?1, ?2, ?3, ?4, ?5, 'pending')
                ",
            )
            .bind(tournament_id)
            .bind(round_id)
            .bind(i64::try_from(set.position).unwrap())
            .bind(slot1)
            .bind(slot2)
            .execute(&mut *tx)
            .await
            .inspect_err(log_db_error)?
            .last_insert_rowid();
            set_ids.insert((round.ordinal, set.position), set_id);
        }
    }

    // Winner and loser links are written independently — never behind a shared
    // `continue` — so a set with one but not the other (every set has a winner
    // link except the final and the 3rd place match; only the two semifinal sets
    // have a loser link) never has the other silently skipped alongside it.
    for round in &bracket.rounds {
        for set in &round.sets {
            let set_id = set_ids[&(round.ordinal, set.position)];

            if let Some(advancement) = set.winner_advances_to
                && let Some(&target) = set_ids.get(&(advancement.round, advancement.position))
            {
                let slot = match advancement.slot {
                    Slot::One => 1,
                    Slot::Two => 2,
                };
                sqlx::query(
                    r"
                    update tournament_sets
                    set
                        winner_advances_to_set_id = ?1,
                        winner_advances_to_slot = ?2
                    where id = ?3
                    ",
                )
                .bind(target)
                .bind(slot)
                .bind(set_id)
                .execute(&mut *tx)
                .await
                .inspect_err(log_db_error)?;
            }

            if let Some(advancement) = set.loser_advances_to
                && let Some(&target) = set_ids.get(&(advancement.round, advancement.position))
            {
                let slot = match advancement.slot {
                    Slot::One => 1,
                    Slot::Two => 2,
                };
                sqlx::query(
                    r"
                    update tournament_sets
                    set
                        loser_advances_to_set_id = ?1,
                        loser_advances_to_slot = ?2
                    where id = ?3
                    ",
                )
                .bind(target)
                .bind(slot)
                .bind(set_id)
                .execute(&mut *tx)
                .await
                .inspect_err(log_db_error)?;
            }
        }
    }

    tx.commit().await.inspect_err(log_db_error)?;
    Ok(())
}

/// When the event actually began, as opposed to when it was scheduled to.
pub(crate) async fn set_tournament_started_at(
    pool: &SqlitePool,
    id: i64,
    started_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournaments set started_at = ?1 where id = ?2")
        .bind(started_at)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// Every set in a tournament, ordered so a caller can walk the bracket forwards.
pub(crate) async fn list_sets_for_tournament(
    pool: &SqlitePool,
    tournament_id: i64,
) -> Result<Vec<TournamentSet>, sqlx::Error> {
    sqlx::query_as(AssertSqlSafe(format!(
        r"
        select {TOURNAMENT_SET_COLUMNS}
        from tournament_sets
        where tournament_id = ?1
        -- Ordered via a correlated subquery rather than a join: the shared
        -- column list is unqualified, and both tables have an `id`.
        order by (select ordinal from tournament_rounds where id = round_id), position
        "
    )))
    .bind(tournament_id)
    .fetch_all(pool)
    .await
    .inspect_err(log_db_error)
}

/// The set whose private thread this is, which is how every `/set` command finds
/// its subject: an organizer types the result in the thread they are already
/// reading, and never a set id.
pub(crate) async fn get_set_by_thread(pool: &SqlitePool, thread_id: i64) -> Result<Option<TournamentSet>, sqlx::Error> {
    sqlx::query_as(AssertSqlSafe(format!(
        r"
        select {TOURNAMENT_SET_COLUMNS}
        from tournament_sets
        where thread_id = ?1
        "
    )))
    .bind(thread_id)
    .fetch_optional(pool)
    .await
    .inspect_err(log_db_error)
}

pub(crate) async fn get_set(pool: &SqlitePool, id: i64) -> Result<Option<TournamentSet>, sqlx::Error> {
    sqlx::query_as(AssertSqlSafe(format!(
        r"
        select {TOURNAMENT_SET_COLUMNS}
        from tournament_sets
        where id = ?1
        "
    )))
    .bind(id)
    .fetch_optional(pool)
    .await
    .inspect_err(log_db_error)
}

pub(crate) async fn get_set_by_position(
    pool: &SqlitePool,
    round_id: i64,
    position: i64,
) -> Result<Option<TournamentSet>, sqlx::Error> {
    sqlx::query_as(AssertSqlSafe(format!(
        r"
        select {TOURNAMENT_SET_COLUMNS}
        from tournament_sets
        where round_id = ?1
          and position = ?2
        "
    )))
    .bind(round_id)
    .bind(position)
    .fetch_optional(pool)
    .await
    .inspect_err(log_db_error)
}

pub(crate) async fn list_sets_for_round(pool: &SqlitePool, round_id: i64) -> Result<Vec<TournamentSet>, sqlx::Error> {
    sqlx::query_as(AssertSqlSafe(format!(
        r"
        select {TOURNAMENT_SET_COLUMNS}
        from tournament_sets
        where round_id = ?1
        order by position
        "
    )))
    .bind(round_id)
    .fetch_all(pool)
    .await
    .inspect_err(log_db_error)
}

pub(crate) async fn set_advancement(
    pool: &SqlitePool,
    id: i64,
    winner_advances_to_set_id: Option<i64>,
    winner_advances_to_slot: Option<i64>,
    loser_advances_to_set_id: Option<i64>,
    loser_advances_to_slot: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        update tournament_sets
        set
            winner_advances_to_set_id = ?1,
            winner_advances_to_slot = ?2,
            loser_advances_to_set_id = ?3,
            loser_advances_to_slot = ?4
        where id = ?5
        ",
    )
    .bind(winner_advances_to_set_id)
    .bind(winner_advances_to_slot)
    .bind(loser_advances_to_set_id)
    .bind(loser_advances_to_slot)
    .bind(id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn update_set_status(pool: &SqlitePool, id: i64, status: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournament_sets set status = ?1 where id = ?2")
        .bind(status)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// Which column a winner lands in is decided by `Slot`, not by the caller building
/// SQL — the column name is never data, so this stays two static queries.
pub(crate) async fn set_slot(pool: &SqlitePool, id: i64, slot: Slot, user_id: i64) -> Result<(), sqlx::Error> {
    let sql = match slot {
        Slot::One => r"update tournament_sets set slot1_user_id = ?1 where id = ?2",
        Slot::Two => r"update tournament_sets set slot2_user_id = ?1 where id = ?2",
    };
    sqlx::query(sql)
        .bind(user_id)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn set_thread(pool: &SqlitePool, id: i64, thread_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournament_sets set thread_id = ?1 where id = ?2")
        .bind(thread_id)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// Used for both the first draft and a redraft: a redraft
/// overwrites the pointer, so the sync/announcement state from the superseded room
/// must not survive alongside it. The room link is not stored — it is
/// `draft_base_url` (on `tournaments`) plus `/match/` plus this id.
pub(crate) async fn set_draft_pointer(pool: &SqlitePool, id: i64, draft_external_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        update tournament_sets
        set
            draft_external_id = ?1,
            draft_synced_at = null,
            draft_announce_message_id = null
        where id = ?2
        ",
    )
    .bind(draft_external_id)
    .bind(id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn increment_redraft_count(pool: &SqlitePool, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournament_sets set redraft_count = redraft_count + 1 where id = ?1")
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn set_draft_synced_at(
    pool: &SqlitePool,
    id: i64,
    synced_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournament_sets set draft_synced_at = ?1 where id = ?2")
        .bind(synced_at)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn set_draft_announce_message(pool: &SqlitePool, id: i64, message_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournament_sets set draft_announce_message_id = ?1 where id = ?2")
        .bind(message_id)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// The pinned set panel's message id, recorded on open and re-recorded after a
/// redraft posts a fresh panel. Deliberately not cleared by `set_draft_pointer`:
/// a redraft strikes and replaces this message explicitly rather than losing
/// track of it.
pub(crate) async fn set_panel_message(pool: &SqlitePool, id: i64, message_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournament_sets set panel_message_id = ?1 where id = ?2")
        .bind(message_id)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn record_set_result(
    pool: &SqlitePool,
    id: i64,
    slot1_wins: i64,
    slot2_wins: i64,
    winner_user_id: Option<i64>,
    status: &str,
    completed_at: Option<DateTime<Utc>>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        update tournament_sets
        set
            slot1_wins = ?1,
            slot2_wins = ?2,
            winner_user_id = ?3,
            status = ?4,
            completed_at = ?5
        where id = ?6
        ",
    )
    .bind(slot1_wins)
    .bind(slot2_wins)
    .bind(winner_user_id)
    .bind(status)
    .bind(completed_at)
    .bind(id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

/// What `complete_set_and_advance` writes. A carrier rather than six positional
/// args, matching `TournamentChannels` and `NewGame`.
pub(crate) struct SetResult {
    pub set_id: i64,
    pub tournament_id: i64,
    pub slot1_wins: i64,
    pub slot2_wins: i64,
    pub winner_user_id: i64,
    pub loser_user_id: i64,
    /// `completed` for a set that was played out, `walkover` for one an organizer
    /// handed over. Terminal either way; the distinction is for the record.
    pub status: &'static str,
}

/// What the settlement changed beyond the set itself.
pub(crate) struct Advanced {
    /// False when the set was already decided — nothing was written at all.
    pub completed: bool,
    /// Whether the *winner's* target became ready — never the loser's target
    /// (the 3rd place match), so this keeps meaning "the next set in the main
    /// bracket is now open" and completing a semifinal never claims to have
    /// opened a set it only half-fed.
    pub target_became_ready: bool,
    pub tournament_completed: bool,
    /// True when the set just settled was the 3rd place match — the other set
    /// with no winner target, distinguished from the final by whether some other
    /// set names it as a *loser* target.
    pub is_third_place: bool,
}

/// Completing a set, **in one transaction**: the set is decided, the loser is
/// eliminated, the winner is written into the next set (and the loser into the
/// 3rd place match, when this was a semifinal), and either target opens once it
/// has both players.
///
/// Assembled inline for the same reason `insert_bracket` is — `record_set_result`,
/// `update_entry_status`, `set_slot` and `update_set_status` each take a
/// `&SqlitePool` and would each commit on their own, so a failure halfway would
/// leave a set decided and its winner nowhere, which nothing downstream can detect.
///
/// **The first statement is the lock.** Excluding every status a set never leaves
/// means a second caller writes nothing and reports `completed: false`, rather
/// than advancing the same winner twice — the set row is the only thing
/// serialising two settlements of one set, whether that is a button pressed twice
/// or a result landing at the same moment as an organizer's award.
pub(crate) async fn complete_set_and_advance(pool: &SqlitePool, result: SetResult) -> Result<Advanced, sqlx::Error> {
    let mut tx = pool.begin().await.inspect_err(log_db_error)?;

    let decided = sqlx::query(
        r"
        update tournament_sets
        set
            slot1_wins = ?1,
            slot2_wins = ?2,
            winner_user_id = ?3,
            status = ?4,
            completed_at = ?5
        where id = ?6
          and status not in ('completed', 'walkover', 'bye')
        ",
    )
    .bind(result.slot1_wins)
    .bind(result.slot2_wins)
    .bind(result.winner_user_id)
    .bind(result.status)
    .bind(Utc::now())
    .bind(result.set_id)
    .execute(&mut *tx)
    .await
    .inspect_err(log_db_error)?
    .rows_affected();

    if decided == 0 {
        tx.rollback().await.inspect_err(log_db_error)?;
        return Ok(Advanced {
            completed: false,
            target_became_ready: false,
            tournament_completed: false,
            is_third_place: false,
        });
    }

    sqlx::query(
        r"
        update tournament_entries
        set status = 'eliminated'
        where tournament_id = ?1
          and user_id = ?2
        ",
    )
    .bind(result.tournament_id)
    .bind(result.loser_user_id)
    .execute(&mut *tx)
    .await
    .inspect_err(log_db_error)?;

    let advancement: Option<(i64, i64)> = sqlx::query_as(
        r"
        select winner_advances_to_set_id, winner_advances_to_slot
        from tournament_sets
        where id = ?1
          and winner_advances_to_set_id is not null
          and winner_advances_to_slot is not null
        ",
    )
    .bind(result.set_id)
    .fetch_optional(&mut *tx)
    .await
    .inspect_err(log_db_error)?;

    let (mut target_became_ready, mut tournament_completed, mut is_third_place) = (false, false, false);
    match advancement {
        Some((target, slot)) => {
            // The column name is chosen here, never bound — two static queries
            // rather than one interpolated one, as `set_slot` does.
            let sql = if slot == 1 {
                r"update tournament_sets set slot1_user_id = ?1 where id = ?2"
            } else {
                r"update tournament_sets set slot2_user_id = ?1 where id = ?2"
            };
            sqlx::query(sql)
                .bind(result.winner_user_id)
                .bind(target)
                .execute(&mut *tx)
                .await
                .inspect_err(log_db_error)?;

            // Readiness is both slots being filled, not the round being over: the
            // other half of the bracket may still be playing.
            target_became_ready = sqlx::query(
                r"
                update tournament_sets
                set status = 'ready'
                where id = ?1
                  and status = 'pending'
                  and slot1_user_id is not null
                  and slot2_user_id is not null
                ",
            )
            .bind(target)
            .execute(&mut *tx)
            .await
            .inspect_err(log_db_error)?
            .rows_affected()
                > 0;
        },
        // No winner target means either the final or the 3rd place match — the
        // only two rootless sets. Distinguished structurally: a set that some
        // *other* set names as a loser target is the 3rd place match; if nothing
        // does, it's the final, and the event ends with it. Order-independent —
        // whichever of the two is decided first, only the final flips the
        // tournament's status.
        None => {
            let is_loser_target: bool =
                sqlx::query_scalar(r"select exists(select 1 from tournament_sets where loser_advances_to_set_id = ?1)")
                    .bind(result.set_id)
                    .fetch_one(&mut *tx)
                    .await
                    .inspect_err(log_db_error)?;

            if is_loser_target {
                is_third_place = true;
            } else {
                sqlx::query(
                    r"
                    update tournaments
                    set
                        status = 'completed',
                        completed_at = ?1
                    where id = ?2
                    ",
                )
                .bind(Utc::now())
                .bind(result.tournament_id)
                .execute(&mut *tx)
                .await
                .inspect_err(log_db_error)?;
                tournament_completed = true;
            }
        },
    }

    // The loser's own advancement, symmetric to the winner's above but never
    // touching `target_became_ready` — its target is the 3rd place match, not the
    // next set in the main bracket.
    let loser_advancement: Option<(i64, i64)> = sqlx::query_as(
        r"
        select loser_advances_to_set_id, loser_advances_to_slot
        from tournament_sets
        where id = ?1
          and loser_advances_to_set_id is not null
          and loser_advances_to_slot is not null
        ",
    )
    .bind(result.set_id)
    .fetch_optional(&mut *tx)
    .await
    .inspect_err(log_db_error)?;

    if let Some((target, slot)) = loser_advancement {
        let sql = if slot == 1 {
            r"update tournament_sets set slot1_user_id = ?1 where id = ?2"
        } else {
            r"update tournament_sets set slot2_user_id = ?1 where id = ?2"
        };
        sqlx::query(sql)
            .bind(result.loser_user_id)
            .bind(target)
            .execute(&mut *tx)
            .await
            .inspect_err(log_db_error)?;

        sqlx::query(
            r"
            update tournament_sets
            set status = 'ready'
            where id = ?1
              and status = 'pending'
              and slot1_user_id is not null
              and slot2_user_id is not null
            ",
        )
        .bind(target)
        .execute(&mut *tx)
        .await
        .inspect_err(log_db_error)?;
    }

    tx.commit().await.inspect_err(log_db_error)?;
    Ok(Advanced {
        completed: true,
        target_became_ready,
        tournament_completed,
        is_third_place,
    })
}

pub(crate) async fn set_scheduled_at(
    pool: &SqlitePool,
    id: i64,
    scheduled_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournament_sets set scheduled_at = ?1 where id = ?2")
        .bind(scheduled_at)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

// 7. tournament_games

#[derive(FromRow, Clone)]
pub(crate) struct TournamentGame {
    pub id: i64,
    pub set_id: i64,
    pub game_number: i64,
    pub map: Option<String>,
    pub slot1_civ: Option<String>,
    pub slot2_civ: Option<String>,
    pub winner_user_id: Option<i64>,
    pub status: String,
    pub source: String,
    pub reported_by: Option<i64>,
    pub reported_at: Option<DateTime<Utc>>,
}

/// A plain data carrier rather than positional arguments — `insert_game` would
/// otherwise take eleven parameters.
pub(crate) struct NewGame {
    pub set_id: i64,
    pub game_number: i64,
    pub map: Option<String>,
    pub slot1_civ: Option<String>,
    pub slot2_civ: Option<String>,
    pub winner_user_id: Option<i64>,
    pub status: String,
    pub source: String,
    pub reported_by: Option<i64>,
    pub reported_at: Option<DateTime<Utc>>,
}

pub(crate) async fn insert_game(pool: &SqlitePool, new: NewGame) -> Result<i64, sqlx::Error> {
    let result = sqlx::query(
        r"
        insert into tournament_games (
            set_id, game_number, map, slot1_civ, slot2_civ, winner_user_id, status, source,
            reported_by, reported_at
        )
        values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        ",
    )
    .bind(new.set_id)
    .bind(new.game_number)
    .bind(new.map)
    .bind(new.slot1_civ)
    .bind(new.slot2_civ)
    .bind(new.winner_user_id)
    .bind(new.status)
    .bind(new.source)
    .bind(new.reported_by)
    .bind(new.reported_at)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(result.last_insert_rowid())
}

/// Writes an organizer's own record of one game, replacing whatever was there.
///
/// An upsert rather than an insert-or-update pair, because correcting a game is
/// the ordinary case and `unique (set_id, game_number)` would otherwise reject
/// it. Not `update_game_result`, which cannot set `source`, `reported_by` or
/// `reported_at` — a correction routed through that would leave a row still
/// claiming to have come from the draft tool.
///
/// A carrier rather than positional arguments, like `NewGame` next to it.
pub(crate) struct ManualGame {
    pub set_id: i64,
    pub game_number: i64,
    pub winner_user_id: i64,
    pub reported_by: i64,
    pub map: Option<String>,
    pub slot1_civ: Option<String>,
    pub slot2_civ: Option<String>,
}

pub(crate) async fn record_manual_game(pool: &SqlitePool, game: ManualGame) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        insert into tournament_games (
            set_id, game_number, map, slot1_civ, slot2_civ, winner_user_id, status, source,
            reported_by, reported_at
        )
        values (?1, ?2, ?3, ?4, ?5, ?6, 'completed', 'manual', ?7, ?8)
        on conflict (set_id, game_number) do update
        set
            map = excluded.map,
            slot1_civ = excluded.slot1_civ,
            slot2_civ = excluded.slot2_civ,
            winner_user_id = excluded.winner_user_id,
            status = excluded.status,
            source = excluded.source,
            reported_by = excluded.reported_by,
            reported_at = excluded.reported_at
        ",
    )
    .bind(game.set_id)
    .bind(game.game_number)
    .bind(game.map)
    .bind(game.slot1_civ)
    .bind(game.slot2_civ)
    .bind(game.winner_user_id)
    .bind(game.reported_by)
    .bind(Utc::now())
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

/// Writes one game from a synced draft. The guarded sibling of
/// `record_manual_game`: the same upsert shape, but the `where` clause on the
/// conflict update is the entire mechanism that makes re-import safe — a row
/// an organizer already corrected (`source = 'manual'`) simply does not match
/// it, so the insert is skipped rather than overwriting their correction.
pub(crate) async fn upsert_draft_import_game(pool: &SqlitePool, new: NewGame) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        insert into tournament_games (
            set_id, game_number, map, slot1_civ, slot2_civ, winner_user_id, status, source
        )
        values (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'draft_import')
        on conflict (set_id, game_number) do update
        set
            map = excluded.map,
            slot1_civ = excluded.slot1_civ,
            slot2_civ = excluded.slot2_civ,
            winner_user_id = excluded.winner_user_id,
            status = excluded.status
        where tournament_games.source = 'draft_import'
        ",
    )
    .bind(new.set_id)
    .bind(new.game_number)
    .bind(new.map)
    .bind(new.slot1_civ)
    .bind(new.slot2_civ)
    .bind(new.winner_user_id)
    .bind(new.status)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn get_game(
    pool: &SqlitePool,
    set_id: i64,
    game_number: i64,
) -> Result<Option<TournamentGame>, sqlx::Error> {
    sqlx::query_as(
        r"
        select id, set_id, game_number, map, slot1_civ, slot2_civ, winner_user_id, status,
               source, reported_by, reported_at
        from tournament_games
        where set_id = ?1
          and game_number = ?2
        ",
    )
    .bind(set_id)
    .bind(game_number)
    .fetch_optional(pool)
    .await
    .inspect_err(log_db_error)
}

pub(crate) async fn list_games_for_set(pool: &SqlitePool, set_id: i64) -> Result<Vec<TournamentGame>, sqlx::Error> {
    sqlx::query_as(
        r"
        select id, set_id, game_number, map, slot1_civ, slot2_civ, winner_user_id, status,
               source, reported_by, reported_at
        from tournament_games
        where set_id = ?1
        order by game_number
        ",
    )
    .bind(set_id)
    .fetch_all(pool)
    .await
    .inspect_err(log_db_error)
}

pub(crate) async fn update_game_result(
    pool: &SqlitePool,
    id: i64,
    winner_user_id: Option<i64>,
    status: &str,
    map: Option<&str>,
    slot1_civ: Option<&str>,
    slot2_civ: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        update tournament_games
        set
            winner_user_id = ?1,
            status = ?2,
            map = ?3,
            slot1_civ = ?4,
            slot2_civ = ?5
        where id = ?6
        ",
    )
    .bind(winner_user_id)
    .bind(status)
    .bind(map)
    .bind(slot1_civ)
    .bind(slot2_civ)
    .bind(id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

/// `source = 'manual'` rows survive: regenerating a draft discards the imported
/// record of a game, but never an organizer's own correction.
pub(crate) async fn void_games_for_set(pool: &SqlitePool, set_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        update tournament_games
        set status = 'void'
        where set_id = ?1
          and source = 'draft_import'
        ",
    )
    .bind(set_id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

// 8. tournament_admins

#[derive(FromRow)]
pub(crate) struct TournamentAdmin {
    pub tournament_id: i64,
    pub user_id: i64,
    pub added_by: i64,
    pub added_at: DateTime<Utc>,
}

pub(crate) async fn add_admin(
    pool: &SqlitePool,
    tournament_id: i64,
    user_id: i64,
    added_by: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        insert into tournament_admins (tournament_id, user_id, added_by)
        values (?1, ?2, ?3)
        on conflict (tournament_id, user_id) do nothing
        ",
    )
    .bind(tournament_id)
    .bind(user_id)
    .bind(added_by)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn remove_admin(pool: &SqlitePool, tournament_id: i64, user_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        delete from tournament_admins
        where tournament_id = ?1
          and user_id = ?2
        ",
    )
    .bind(tournament_id)
    .bind(user_id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn list_admins(pool: &SqlitePool, tournament_id: i64) -> Result<Vec<TournamentAdmin>, sqlx::Error> {
    sqlx::query_as(
        r"
        select tournament_id, user_id, added_by, added_at
        from tournament_admins
        where tournament_id = ?1
        ",
    )
    .bind(tournament_id)
    .fetch_all(pool)
    .await
    .inspect_err(log_db_error)
}

pub(crate) async fn is_admin(pool: &SqlitePool, tournament_id: i64, user_id: i64) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        r"
        select exists(
            select 1
            from tournament_admins
            where tournament_id = ?1
              and user_id = ?2
        )
        ",
    )
    .bind(tournament_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .inspect_err(log_db_error)
}

// 9. tournament_bracket_messages

#[derive(FromRow)]
pub(crate) struct TournamentBracketMessage {
    pub tournament_id: i64,
    pub ordinal: i64,
    pub message_id: i64,
}

pub(crate) async fn upsert_bracket_message(
    pool: &SqlitePool,
    tournament_id: i64,
    ordinal: i64,
    message_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        insert into tournament_bracket_messages (tournament_id, ordinal, message_id)
        values (?1, ?2, ?3)
        on conflict (tournament_id, ordinal) do update set message_id = excluded.message_id
        ",
    )
    .bind(tournament_id)
    .bind(ordinal)
    .bind(message_id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

/// Drops every chunk from `ordinal` onwards.
///
/// The bracket's message count is not fixed: it follows the field size, which
/// jumps at powers of two — 8 entrants render to one message, 9 to three. When
/// the field shrinks the surplus has to go, or a stale tail of a bigger bracket
/// sits below the current one.
pub(crate) async fn delete_bracket_messages_from(
    pool: &SqlitePool,
    tournament_id: i64,
    ordinal: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        delete from tournament_bracket_messages
        where tournament_id = ?1
          and ordinal >= ?2
        ",
    )
    .bind(tournament_id)
    .bind(ordinal)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn list_bracket_messages(
    pool: &SqlitePool,
    tournament_id: i64,
) -> Result<Vec<TournamentBracketMessage>, sqlx::Error> {
    sqlx::query_as(
        r"
        select tournament_id, ordinal, message_id
        from tournament_bracket_messages
        where tournament_id = ?1
        order by ordinal
        ",
    )
    .bind(tournament_id)
    .fetch_all(pool)
    .await
    .inspect_err(log_db_error)
}
