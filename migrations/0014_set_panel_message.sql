-- `/set redraft` (§8.7) has to strike the pinned set panel before it repoints
-- the draft, the same way it strikes the `#…-draft` announcement: the panel
-- body carries the live `/match/` link, which claims a seat, and the draft
-- tool has no `DELETE` on `/api/matches` — so an unstruck panel leaves a
-- player able to scroll back into the exact room the redraft was meant to
-- escape. Editing it needs a handle, which nothing has stored until now.
alter table tournament_sets add column panel_message_id bigint;
