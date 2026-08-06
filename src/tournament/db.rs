//! Row types and queries for the tournament schema (docs/tournament.md §4, §8.8).
//!
//! One section per table, in the same order as `migrations/0002_tournament_schema.sql`.
//! Scope is deliberately general-purpose reads/writes, not the business logic later
//! chunks build on top (bracket persistence, permission decisions, draft-preset
//! validation, the re-import upsert) — those stay in the chunk that needs them.

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

// Most fields below are read starting with chunk 9 (registration replies, panels)
// and beyond — chunk 7 only writes them via `insert_tournament`/
// `set_tournament_channels`, never reads them back off a fetched row.
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

// consumed by chunk 12 (`/tournament start`, generating and publishing the bracket)
pub(crate) async fn get_tournament(pool: &SqlitePool, id: i64) -> Result<Option<Tournament>, sqlx::Error> {
    sqlx::query_as(
        r"
        select id, slug, name, status, draft_base_url, announce_channel_id, category_id,
               register_channel_id, register_message_id, bracket_channel_id, matches_channel_id,
               draft_channel_id, checkin_message_id, seed_message_id, checkin_closes_at,
               entrant_cap, scheduled_start_at, created_by, created_at,
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
               entrant_cap, scheduled_start_at, created_by, created_at,
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

// consumed starting with chunk 10 (checkin/seeding/running/... lifecycle transitions)
pub(crate) async fn update_tournament_status(pool: &SqlitePool, id: i64, status: &str) -> Result<(), sqlx::Error> {
    sqlx::query(r"update tournaments set status = ?1 where id = ?2")
        .bind(status)
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// `/tournament delete` (docs/tournament.md §8.4). One statement is enough: every
/// tournament-scoped table cascades off this row — entries, admins, stages,
/// rounds, sets, games and bracket messages — which is invisible here, hence the
/// note. `tournament_players` is deliberately not among them: the Discord↔aoe4world
/// binding is global (§4) and outlives any one tournament.
pub(crate) async fn delete_tournament(pool: &SqlitePool, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(r"delete from tournaments where id = ?1")
        .bind(id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

/// The channel ids `/tournament create` allocates, written once Discord confirms
/// they exist (§8.1). A carrier rather than six positional args, matching
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

/// The registration panel's message id (docs/tournament.md §8.5) — set once, right
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

/// The check-in panel's message id (docs/tournament.md §8.5) — set right after
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

/// The seeding panel's message id (docs/tournament.md §8.5) — set when
/// `/tournament close-checkin` posts the panel, and back to `None` by
/// `/tournament reopen-registration`, which deletes that message along with the
/// seeds it displayed.
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

/// When check-in closes on its own (docs/tournament.md §8.3) — informational
/// only today; nothing polls this to auto-close (§11 follow-ups).
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

/// The maximum size of the field (§8.3). Enforced at registration rather than at
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

/// Entrants occupying a slot. `withdrawn` and `no_show` rows persist (§4) but are
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
//     how long its sets are (§3.3). Keyed by depth back from the final; see
//     `tournament::setup::preset_for_depth` for how one is resolved.

#[derive(FromRow)]
pub(crate) struct RoundPreset {
    pub tournament_id: i64,
    pub from_depth: i64,
    pub draft_preset_id: String,
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
    best_of: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        insert into tournament_round_presets (tournament_id, from_depth, draft_preset_id, best_of)
        values (?1, ?2, ?3, ?4)
        on conflict (tournament_id, from_depth) do update set
            draft_preset_id = excluded.draft_preset_id,
            best_of = excluded.best_of,
            assigned_at = datetime('now')
        ",
    )
    .bind(tournament_id)
    .bind(from_depth)
    .bind(draft_preset_id)
    .bind(best_of)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn list_round_presets(pool: &SqlitePool, tournament_id: i64) -> Result<Vec<RoundPreset>, sqlx::Error> {
    sqlx::query_as(
        r"
        select tournament_id, from_depth, draft_preset_id, best_of, assigned_at
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
pub(crate) async fn get_tournament_by_any_channel_id(
    pool: &SqlitePool,
    channel_id: i64,
) -> Result<Option<Tournament>, sqlx::Error> {
    sqlx::query_as(
        r"
        select id, slug, name, status, draft_base_url, announce_channel_id, category_id,
               register_channel_id, register_message_id, bracket_channel_id, matches_channel_id,
               draft_channel_id, checkin_message_id, seed_message_id, checkin_closes_at,
               entrant_cap, scheduled_start_at, created_by, created_at,
               started_at, completed_at
        from tournaments
        where announce_channel_id = ?1
           or register_channel_id = ?1
           or bracket_channel_id = ?1
           or draft_channel_id = ?1
           or matches_channel_id = ?1
        ",
    )
    .bind(channel_id)
    .fetch_optional(pool)
    .await
    .inspect_err(log_db_error)
}

// 2. tournament_stages — consumed by chunk 12 (`/tournament start`, generating the bracket)

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

// 3. tournament_rounds — consumed by chunk 12 (`/tournament start`) and chunk 15 (round presets)

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

// 4. tournament_players — consumed by chunk 9 (registration, which is also binding)

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

/// A no-op if `user_id` already has a bound profile — the caller (chunk 9's
/// registration) uses this to write the player row on a first sign-up only, and
/// otherwise falls through to `insert_entry` against the row that's already there.
pub(crate) async fn insert_player_if_absent(
    pool: &SqlitePool,
    user_id: i64,
    aoe4_id: i64,
    display_name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        insert into tournament_players (user_id, aoe4_id, display_name)
        values (?1, ?2, ?3)
        on conflict (user_id) do nothing
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

/// Rebinding changes which aoe4 profile is bound, nothing else — `display_name` is a
/// separate, player-editable concern (see `set_player_display_name`).
/// Every entry a player has ever had, in any tournament and whatever its status.
///
/// Counts `withdrawn` rows too, deliberately: entries are never deleted (§4), and
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
/// (§4: the two are deliberately not linked).
pub(crate) async fn delete_player(pool: &SqlitePool, user_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(r"delete from tournament_players where user_id = ?1")
        .bind(user_id)
        .execute(pool)
        .await
        .inspect_err(log_db_error)?;
    Ok(())
}

pub(crate) async fn update_player_binding(pool: &SqlitePool, user_id: i64, aoe4_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        update tournament_players
        set
            aoe4_id = ?1,
            updated_at = datetime('now')
        where user_id = ?2
        ",
    )
    .bind(aoe4_id)
    .bind(user_id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(())
}

/// Unlike `aoe4_id`/`elo`/`atr`, a name carries no game-result attribution, so it is
/// not frozen on existing entries the way those are (§4 notes) — this writes through
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

/// A first sign-up (docs/tournament.md §8.5): writes `tournament_players` and the
/// entry together, atomically — neither survives if the other fails. Only called
/// when the caller has already confirmed no `tournament_players` row exists for
/// `user_id`; a concurrent sign-up racing for the same `aoe4_id` still surfaces as
/// a genuine UNIQUE constraint failure on `tournament_players.aoe4_id`, which the
/// caller (`tournament::registration`) maps to a friendly message rather than
/// treating as an unexpected error. `elo` is the profile's `rm_1v1_elo` rating,
/// already in hand from the same aoe4world fetch that resolved `display_name` —
/// snapshotted now and refreshed again at seeding (chunk 11).
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

/// Whether `user_id` holds an entry in any `running` tournament — the guard for
/// `/tournament rebind` (docs/tournament.md §4 notes: "a rebind is refused while
/// the user has an entry in a running tournament", since the profile is
/// snapshotted onto entries and sets already reference the player). Deliberately
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

// 5. tournament_entries — consumed starting with chunk 9 (registration) through chunk 11 (seeding)

#[derive(FromRow)]
pub(crate) struct TournamentEntry {
    pub tournament_id: i64,
    pub user_id: i64,
    pub aoe4_id: i64,
    pub seed: Option<i64>,
    pub suggested_seed: Option<i64>,
    pub display_name: String,
    pub elo: Option<i64>,
    pub atr: Option<f64>,
    pub atr_source: Option<String>,
    pub status: String,
    pub registered_at: DateTime<Utc>,
    pub checked_in_at: Option<DateTime<Utc>>,
}

/// `elo` is snapshotted at sign-up so the bracket preview has something real to
/// order by before seeding runs (§6). ATR is not: it is one bulk request for the
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
        select tournament_id, user_id, aoe4_id, seed, suggested_seed, display_name, elo, atr,
               atr_source, status, registered_at, checked_in_at
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
        select tournament_id, user_id, aoe4_id, seed, suggested_seed, display_name, elo, atr,
               atr_source, status, registered_at, checked_in_at
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

/// `/tournament close-checkin`'s no-show sweep (docs/tournament.md §8.3): every
/// `active` entry that never checked in becomes `no_show` in one statement.
/// Already-`withdrawn`/`no_show` entries are untouched. Returns how many rows
/// changed, for the closing reply.
pub(crate) async fn mark_no_shows(pool: &SqlitePool, tournament_id: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r"
        update tournament_entries
        set status = 'no_show'
        where tournament_id = ?1
          and status = 'active'
          and checked_in_at is null
        ",
    )
    .bind(tournament_id)
    .execute(pool)
    .await
    .inspect_err(log_db_error)?;
    Ok(result.rows_affected())
}

/// `mark_no_shows`'s exact inverse, for `/tournament reopen-registration`
/// (docs/tournament.md §8.3). Only `no_show` is touched, and only `mark_no_shows`
/// ever writes that status, so every row this restores was `active` before —
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

/// Wipes the whole check-in round for `/tournament reopen-registration` (§8.3).
/// The seed columns go too: nothing writes them before chunk 11, but a reopen
/// out of `seeding` is precisely when they would be stale, and the
/// `unique (tournament_id, seed)` index tolerates repeated nulls.
pub(crate) async fn clear_checkins(pool: &SqlitePool, tournament_id: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r"
        update tournament_entries
        set checked_in_at = null,
            seed = null,
            suggested_seed = null
        where tournament_id = ?1
          and (checked_in_at is not null or seed is not null or suggested_seed is not null)
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

/// Writes `ordered_user_ids` as seeds 1..n in one transaction (§6).
///
/// **Every seed is nulled first, and that is load-bearing rather than tidy:**
/// `unique (tournament_id, seed)` is enforced per row as the statement runs, so
/// shifting a field down by one would collide on the very first row without a
/// clear pass. Writing the whole order rather than the changed rows also
/// guarantees the result is contiguous, which is what chunk 12's `start`
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

// 6. tournament_sets — consumed starting with chunk 12 (bracket generation creates sets) through chunk 20

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
    draft_announce_message_id, redraft_count, thread_id, winner_advances_to_set_id,
    winner_advances_to_slot, loser_advances_to_set_id, loser_advances_to_slot,
    scheduled_at, completed_at
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
pub(crate) async fn insert_bracket(
    pool: &SqlitePool,
    tournament_id: i64,
    bracket: &crate::tournament::bracket::Bracket,
    seed_to_user: &std::collections::HashMap<u32, i64>,
) -> Result<(), sqlx::Error> {
    use crate::tournament::bracket::Slot;

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

    for round in &bracket.rounds {
        let round_id = sqlx::query(
            r"
            insert into tournament_rounds (stage_id, ordinal, name, best_of)
            values (?1, ?2, ?3, ?4)
            ",
        )
        .bind(stage)
        .bind(i64::try_from(round.ordinal).unwrap())
        .bind(&round.name)
        .bind(i64::from(round.best_of))
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

    for round in &bracket.rounds {
        for set in &round.sets {
            let Some(advancement) = set.winner_advances_to else {
                continue; // the final
            };
            let Some(target) = set_ids.get(&(advancement.round, advancement.position)) else {
                continue;
            };
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
            .bind(set_ids[&(round.ordinal, set.position)])
            .execute(&mut *tx)
            .await
            .inspect_err(log_db_error)?;
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

/// Used for both the first draft (chunk 16) and a redraft (chunk 20): a redraft
/// overwrites the pointer, so the sync/announcement state from the superseded room
/// must not survive alongside it. The room link is not stored — it is
/// `draft_base_url` (on `tournaments`) plus `/match/` plus this id (docs/tournament.md §4).
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

// 7. tournament_games — consumed starting with chunk 18 (set completion) through chunk 22 (result import)

#[derive(FromRow)]
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

/// `source = 'manual'` rows survive — chunk 20's redraft guard.
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

// 9. tournament_bracket_messages — consumed by chunk 12 (bracket publication, chunked across several messages)

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
