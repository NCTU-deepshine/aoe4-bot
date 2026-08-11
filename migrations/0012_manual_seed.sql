-- A manual seed becomes a pin on a seat rather than a one-off nudge to the
-- order: `manual_seed` records the seat an organizer claimed for an entrant,
-- and `seeding::resolved_order` places every pinned entrant there and tiers
-- everyone else into what is left. `seed` stays the column the bracket and
-- every panel actually read — it is always the resolution, never the pin.
--
-- One pin per seat is the invariant the resolution relies on, so the schema
-- holds it rather than the code: the unique index tolerates repeated nulls,
-- same as `unique (tournament_id, seed)` already does.
alter table tournament_entries add column manual_seed integer;

create unique index tournament_entries_manual_seed
  on tournament_entries (tournament_id, manual_seed);
