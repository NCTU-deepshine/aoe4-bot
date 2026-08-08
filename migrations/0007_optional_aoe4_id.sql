-- An entrant may have no aoe4world profile: an organizer can put a Discord member
-- straight into a field, and such a player has nothing to look up. `aoe4_id` stays
-- `unique` while becoming nullable, which is exactly the constraint wanted —
-- SQLite treats nulls as distinct, so any number of entrants may be unbound while
-- one Discord user per real profile is still enforced.
--
-- The constraint must stay a column-level `unique`: the error it produces,
-- "UNIQUE constraint failed: tournament_players.aoe4_id", is string-matched by
-- `registration::is_aoe4_id_conflict` to word a contested profile.
--
-- SQLite has no ALTER COLUMN, so both tables are recreated. **Every tournament,
-- entrant, bracket, set and game is discarded** rather than copied across — a
-- deliberate choice while no event that matters has been run. Copying them would
-- mean dropping a table three others hold foreign keys into, which needs
-- `pragma foreign_keys = off`, which is a no-op inside a transaction and so forces
-- the whole migration to run untransacted. Emptying the tables first means the
-- foreign keys are satisfied at every step, this stays an ordinary atomic
-- migration, and nothing has to turn enforcement off and remember to turn it back
-- on. `accounts`, which belongs to the ranked board and not to this feature, is
-- untouched.
--
-- The Discord side is not cleaned up: channels, threads and panels for a wiped
-- tournament survive with nothing behind them, and have to be deleted by hand.

delete from tournaments;
delete from tournament_players;

drop table tournament_entries;

create table tournament_entries (
  tournament_id integer not null references tournaments(id) on delete cascade,
  user_id bigint not null references tournament_players(user_id),
  aoe4_id bigint,                           -- snapshot, not a fk. null for an unbound entrant
  seed integer,                             -- FINAL seed; the organizer may override
  suggested_seed integer,                   -- what the bot computed, kept for audit
  display_name text not null,               -- snapshot; aoe4world names change
  elo integer,                              -- snapshot of rm_1v1_elo.rating
  atr real,                                 -- snapshot of esports tournament elo
  atr_source text check (atr_source in ('esports','manual')),
  status text not null default 'active'
    check (status in ('active','eliminated','withdrawn','no_show')),
  registered_at timestamp not null default (datetime('now')),
  checked_in_at timestamp,                  -- null = did not check in
  primary key (tournament_id, user_id),
  unique (tournament_id, seed)
);

drop table tournament_players;

create table tournament_players (
  user_id bigint primary key,               -- discord user; one main profile each
  aoe4_id bigint unique,                    -- and one user per profile. null = no aoe4world profile at all
  display_name text not null,               -- from aoe4world, or an organizer's assertion for an unbound entrant
  bound_at timestamp not null default (datetime('now')),
  updated_at timestamp
);
