-- The seeding panel's message handle (docs/tournament.md §8.5), alongside the
-- registration and check-in panels' own ids on the same row. Nullable and unset
-- until `/tournament close-checkin` computes the first seeding and posts it;
-- cleared again by `/tournament reopen-registration`, which deletes that message.
alter table tournaments add column seed_message_id bigint;
