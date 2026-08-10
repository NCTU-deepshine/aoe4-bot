-- no-transaction

-- With 0010, every entry lands bound; nothing writes a player row with no
-- profile any more either, so `tournament_players.aoe4_id` becomes `not null`
-- too. The two known rows predating this (an organizer's direct-invite
-- placeholders, from before invites resolved a profile) were deleted by hand
-- first, so this insert has nothing left to fail loudly on.
--
-- Unlike 0010's rebuild of `tournament_entries`, this table has real
-- dependents: `tournament_entries`, `tournament_sets` (twice) and
-- `tournament_games` all hold live foreign keys into
-- `tournament_players(user_id)`. `drop table` below would violate those, so
-- `pragma foreign_keys = off` is needed — which is a no-op inside a
-- transaction, hence this migration's `-- no-transaction` header. The rebuild
-- creates the new table under its own name, copies into it, then drops the
-- old table and renames the new one into place, rather than renaming the old
-- table out of the way first: with `legacy_alter_table` off, that rename
-- would rewrite the `references` clauses in all three dependants to point at
-- the renamed-away table, which then gets dropped out from under them.

pragma foreign_keys = off;

create table tournament_players_new (
  user_id bigint primary key,               -- discord user; one main profile each
  aoe4_id bigint not null unique,           -- and one user per profile
  display_name text not null,               -- from aoe4world
  bound_at timestamp not null default (datetime('now')),
  updated_at timestamp
);

insert into tournament_players_new (user_id, aoe4_id, display_name, bound_at, updated_at)
select user_id, aoe4_id, display_name, bound_at, updated_at
from tournament_players;

drop table tournament_players;

alter table tournament_players_new rename to tournament_players;

pragma foreign_keys = on;
