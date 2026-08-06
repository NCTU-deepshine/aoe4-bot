//! Tournament management (docs/tournament.md).

// Consumed by `/tournament start`, which generates and stores a bracket. Until that
// lands, only this module's own tests exercise it — remove the allow then.
#[allow(dead_code)]
pub(crate) mod bracket;
// The bracket as Discord sees it (chunk 29, §8.6): a preview from the first two
// entrants, becoming the real thing in place once the event starts.
pub(crate) mod bracket_view;
// `/tournament admin add|remove|list` (§8.2) — the codebase's first access control.
pub(crate) mod access;
// The interaction dispatcher's custom_id parsing (chunk 8, §8.5): consumed
// immediately by `dispatch::Dispatcher`.
pub(crate) mod action;
// One log line per tournament action, shared by the slash-command and button
// surfaces so a destructive one leaves the same record either way.
pub(crate) mod audit;
// Chunk 7 (`/tournament create`, the admin list) is the first caller, but only of
// a fraction of this file — everything else is consumed starting with chunk 9
// (registration) through chunk 22 (result import); see the per-section notes in
// db.rs itself for which chunk consumes which table.
#[allow(dead_code)]
pub(crate) mod db;
// `/tournament open-checkin|checkin|close-checkin`'s business logic (chunk 10,
// §8.3, §8.5), plus `reopen-registration`'s backward edge (chunk 25).
pub(crate) mod checkin;
// The check-in panel (chunk 10, §8.5): rendering plus the Discord/DB glue
// `commands.rs` and `dispatch::Dispatcher` call into.
pub(crate) mod checkin_panel;
// The interaction dispatcher's own `EventHandler` (chunk 8, §8.5) — kept
// separate from `Emperor`, which is home-guild meme/reaction logic with no
// tournament knowledge; registered as a second handler in `main.rs`.
pub(crate) mod dispatch;
// The registration panel (chunk 9, §8.5): rendering plus the Discord/DB glue
// `commands::create` and `dispatch::Dispatcher` call into.
pub(crate) mod panel;
#[allow(dead_code)]
pub(crate) mod render;
// `/tournament register|rebind|withdraw`'s business logic (chunk 9, §8.5, §4).
pub(crate) mod registration;
// `/tournament create`'s slug argument (§8.1).
pub(crate) mod slug;
// What must be configured before a tournament can start (chunk 27, §8.3), and
// which draft preset — and so which best_of — each round uses (§3.3).
pub(crate) mod setup;
// Ratings and suggested seeding (chunk 11, §6): the pure tiering plus the one
// aoe4world path that snapshots ATR and ELO onto each entry.
pub(crate) mod seeding;
// The seeding panel (chunk 11, §8.5): the seeded field, rendered into
// `#{slug}-bracket` and edited in place as an organizer overrides seeds.
pub(crate) mod seed_panel;
// `/tournament delete`'s guards (chunk 26, §8.4) — pure, like `access::decide`.
pub(crate) mod teardown;
// The panel-edit throttle (§8.5, "Edits must be throttled"). Consumed by
// chunk 9's registration panel (`panel::refresh`).
pub(crate) mod throttle;
