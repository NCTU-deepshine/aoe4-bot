-- Every seeded invite has written a real, compacted `seed` immediately since
-- chunk 32 — including chunk 35's pin resolution — rather than only at close,
-- as originally specified. A tournament still in `registration` or `checkin`
-- may be carrying exactly such a premature value, prematurely compacting a
-- pin before it's even known whether more entrants arrive to fill the gap in
-- front of it. `seed` is now written only at close (`refresh_ratings`), so any
-- value already sitting on an unclosed tournament is cleared here, letting the
-- corrected code compute it fresh, once, at the real close.
--
-- `manual_seed` (the pin itself) and `suggested_seed` (already only ever
-- written at close) are untouched.
update tournament_entries
set seed = null
where tournament_id in (select id from tournaments where status in ('registration', 'checkin'))
  and seed is not null;
