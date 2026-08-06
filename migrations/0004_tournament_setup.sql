alter table tournaments add column entrant_cap integer not null default 32;

alter table tournaments add column scheduled_start_at timestamp;

create table if not exists tournament_round_presets (
  tournament_id integer not null references tournaments(id) on delete cascade,
  from_depth integer not null check (from_depth >= 0),
  draft_preset_id text not null,
  best_of integer not null check (best_of % 2 = 1),
  assigned_at timestamp not null default (datetime('now')),
  primary key (tournament_id, from_depth)
);
