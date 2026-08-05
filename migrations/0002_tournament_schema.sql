-- 1. the event. deliberately thin: rules live per-round, not here.
create table if not exists tournaments (
  id integer primary key autoincrement,
  slug text not null unique,
  name text not null,
  status text not null default 'registration'
    check (status in ('registration','checkin','seeding','running','completed','canceled')),
  draft_base_url text,
  -- discord wiring; see §8.1. announce_channel_id is the channel /tournament create ran in.
  announce_channel_id bigint,
  category_id bigint,
  register_channel_id bigint,
  register_message_id bigint,
  bracket_channel_id bigint,
  matches_channel_id bigint,
  draft_channel_id bigint,                  -- public; spectator links, see §8.1
  checkin_message_id bigint,
  checkin_closes_at timestamp,
  created_by bigint not null,               -- authority over the admin list, see §8.2
  created_at timestamp not null default (datetime('now')),
  started_at timestamp,
  completed_at timestamp
);

-- 2. stages: the extension point for swiss / group / double-elim.
--    single elimination today = exactly one stage with format 'single_elim'.
create table if not exists tournament_stages (
  id integer primary key autoincrement,
  tournament_id integer not null references tournaments(id) on delete cascade,
  ordinal integer not null,
  name text not null,                       -- 'Main Bracket', 'Group Stage', 'Playoffs'
  format text not null default 'single_elim'
    check (format in ('single_elim','double_elim','swiss','group','round_robin')),
  config text,                              -- json: format-specific knobs
  status text not null default 'pending'
    check (status in ('pending','running','completed')),
  unique (tournament_id, ordinal)
);

-- 3. rounds: where per-round rules live.
create table if not exists tournament_rounds (
  id integer primary key autoincrement,
  stage_id integer not null references tournament_stages(id) on delete cascade,
  ordinal integer not null,
  name text not null,                       -- 'Ro16', 'Quarterfinal', 'Final', 'Swiss R1'
  best_of integer not null check (best_of % 2 = 1),   -- odd only; see §7 on completion
  bracket text                              -- double elimination, later
    check (bracket in ('winners','losers','grand_final')),
  draft_preset_id text,                     -- the tool's preset ObjectId; must be a PUBLIC preset (§3.3)
  rules text,                               -- json: other per-round overrides
  unique (stage_id, ordinal, bracket)
);

-- 4. tournament players: one MAIN aoe4 profile per discord user, and one user per
--    profile. deliberately separate from `accounts`, which is the ranked board's
--    table and allows a user several profiles — see the notes. bound at sign-up
--    (§8.5) and reused by every later tournament.
create table if not exists tournament_players (
  user_id bigint primary key,               -- discord user; one main profile each
  aoe4_id bigint not null unique,           -- and one user per profile
  display_name text not null,               -- snapshot; aoe4world names change
  bound_at timestamp not null default (datetime('now')),
  updated_at timestamp
);

-- 5. entrants. keyed by discord user: everything the bot does with an entrant is a
--    discord action (mentions, thread membership, buttons). the profile is resolved
--    through tournament_players, and snapshotted here so a later rebind cannot
--    rewrite history.
create table if not exists tournament_entries (
  tournament_id integer not null references tournaments(id) on delete cascade,
  user_id bigint not null references tournament_players(user_id),
  aoe4_id bigint not null,                  -- snapshot, not a fk; see above
  seed integer,                             -- FINAL seed; the organizer may override
  suggested_seed integer,                   -- what the bot computed, kept for audit
  display_name text not null,               -- snapshot; aoe4world names change
  elo integer,                              -- snapshot of rm_1v1_elo.rating
  atr real,                                 -- snapshot of esports tournament elo
  atr_source text check (atr_source in ('esports','manual')),
  status text not null default 'active'
    check (status in ('active','eliminated','withdrawn','no_show')),
  registered_at timestamp not null default (datetime('now')),
  checked_in_at timestamp,                  -- null = did not check in; see §8.3
  primary key (tournament_id, user_id),
  unique (tournament_id, seed)
);

-- 6. a set = one Bo_N meeting between two entrants.
--    loser_advances_to_* is unused by single elimination but present so
--    double elimination needs no migration.
create table if not exists tournament_sets (
  id integer primary key autoincrement,
  tournament_id integer not null references tournaments(id) on delete cascade,
  round_id integer not null references tournament_rounds(id) on delete cascade,
  position integer not null,                -- index within the round, 1-based, top to bottom
  slot1_user_id bigint references tournament_players(user_id),  -- null until fed by a previous round
  slot2_user_id bigint references tournament_players(user_id),
  slot1_wins integer not null default 0,
  slot2_wins integer not null default 0,
  winner_user_id bigint references tournament_players(user_id),
  status text not null default 'pending'
    check (status in ('pending','ready','drafting','in_progress','completed','bye','walkover')),
  draft_external_id text,                   -- the tool's 24-hex id; overwritten by a redraft (§8.7).
                                              -- the room link is derived: draft_base_url .. '/match/' .. this
  draft_synced_at timestamp,
  draft_announce_message_id bigint,         -- post in the draft channel; null = not announced yet (§8.7)
  redraft_count integer not null default 0, -- guards /set redraft; also a signal something went wrong
  thread_id bigint,                         -- the set's private thread; see §8.7
  winner_advances_to_set_id integer references tournament_sets(id),
  winner_advances_to_slot integer check (winner_advances_to_slot in (1,2)),
  loser_advances_to_set_id integer references tournament_sets(id),
  loser_advances_to_slot integer check (loser_advances_to_slot in (1,2)),
  scheduled_at timestamp,
  completed_at timestamp,
  unique (round_id, position)
);

-- 7. games: a projection of the draft's GAME_RESULT steps, or a manual override.
create table if not exists tournament_games (
  id integer primary key autoincrement,
  set_id integer not null references tournament_sets(id) on delete cascade,
  game_number integer not null,
  map text,                                 -- draft-tool map id, kebab-case
  slot1_civ text,                           -- draft-tool civ id, kebab-case
  slot2_civ text,
  winner_user_id bigint references tournament_players(user_id),
  status text not null default 'pending'
    check (status in ('pending','in_progress','completed','void')),
  source text not null default 'draft_import'
    check (source in ('draft_import','manual')),
  reported_by bigint,                       -- discord user_id, when source = 'manual'
  reported_at timestamp,
  unique (set_id, game_number)
);

-- 8. per-tournament admins, beyond the creator (docs/tournament.md §8.8).
create table if not exists tournament_admins (
  tournament_id integer not null references tournaments(id) on delete cascade,
  user_id bigint not null,                  -- discord user_id
  added_by bigint not null,
  added_at timestamp not null default (datetime('now')),
  primary key (tournament_id, user_id)
);

-- 9. message ids for a bracket render split across several messages (docs/tournament.md §8.8).
create table if not exists tournament_bracket_messages (
  tournament_id integer not null references tournaments(id) on delete cascade,
  ordinal integer not null,                 -- which chunk of a split bracket
  message_id bigint not null,
  primary key (tournament_id, ordinal)
);
