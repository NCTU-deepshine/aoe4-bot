-- `aoe4_id` on `tournament_entries` becomes required. `/tournament invite`
-- (chunk 32's own follow-on) now always resolves a real profile before
-- writing an entry, exactly as a first-time `/tournament register` already
-- did — so nothing since that follow-on ever writes a null one, and the
-- "invitee binds later" catch-up path it made possible is retired alongside it.
--
-- SQLite has no ALTER COLUMN, so the table is rebuilt — but unlike chunk 31's
-- rebuild, nothing holds a foreign key into `tournament_entries` itself, so
-- this needs no `pragma foreign_keys = off` and stays an ordinary transacted
-- migration. Every existing row is copied across rather than discarded; the
-- copy fails loudly, aborting the whole migration, if one still has a null
-- `aoe4_id` — which is the point, not a case to special-case around.

create table tournament_entries_new (
  tournament_id integer not null references tournaments(id) on delete cascade,
  user_id bigint not null references tournament_players(user_id),
  aoe4_id bigint not null,                  -- snapshot, not a fk
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
  invited_by bigint,                        -- null = self-registered; set = an admin put them in
  primary key (tournament_id, user_id),
  unique (tournament_id, seed)
);

insert into tournament_entries_new
select tournament_id, user_id, aoe4_id, seed, suggested_seed, display_name, elo, atr,
       atr_source, status, registered_at, checked_in_at, invited_by
from tournament_entries;

drop table tournament_entries;
alter table tournament_entries_new rename to tournament_entries;
