-- the preset's name on the tool, snapshotted beside best_of so the setup panel can
-- link it by name instead of by 24-hex id. nullable: a rename on the tool leaves
-- this stale, and the link goes to the live page, so the id remains the identity.
alter table tournament_round_presets add column preset_name text;
