//! Tournament management.

// Pure bracket generation: sizes, seed order by reflection, byes and the
// advancement links `/tournament start` persists. The allow is for `Set::is_bye`
// alone — `start` reads the two slots directly instead.
#[allow(dead_code)]
pub(crate) mod bracket;
// The bracket as Discord sees it: a preview from the first two
// entrants, becoming the real thing in place once the event starts.
pub(crate) mod bracket_view;
// `/tournament admin add|remove|list` — the codebase's first access control.
pub(crate) mod access;
// The interaction dispatcher's custom_id parsing: consumed
// immediately by `dispatch::Dispatcher`.
pub(crate) mod action;
// One log line per tournament action, shared by the slash-command and button
// surfaces so a destructive one leaves the same record either way.
pub(crate) mod audit;
// Row types and queries for every tournament table — see the per-section notes
// in db.rs itself for what each one holds.
#[allow(dead_code)]
pub(crate) mod db;
// `/tournament open-checkin|checkin|close-checkin`'s business logic, plus
// `reopen-registration`'s backward edge.
pub(crate) mod checkin;
// The check-in panel: rendering plus the Discord/DB glue
// `commands.rs` and `dispatch::Dispatcher` call into.
pub(crate) mod checkin_panel;
// Deciding a set from its games: eliminating the loser, advancing the winner and
// opening whatever that makes playable. Shared by every way of reporting a
// result, so a set decided by hand and one decided by import behave alike.
pub(crate) mod completion;
// The interaction dispatcher's own `EventHandler` — kept
// separate from `Emperor`, which is home-guild meme/reaction logic with no
// tournament knowledge; registered as a second handler in `main.rs`.
pub(crate) mod dispatch;
// `/tournament invite|uninvite`: the organizers' own door into the field, for an
// entrant who has no aoe4world profile and never signed themselves up.
pub(crate) mod invite;
// Syncing a set against its draft: fetch, map onto our slots, upsert its
// games, settle through `completion`. Callable but uncalled — `/set done` and
// the background poll each add a caller.
#[allow(dead_code)]
pub(crate) mod import;
// The registration panel: rendering plus the Discord/DB glue
// `commands::create` and `dispatch::Dispatcher` call into.
pub(crate) mod panel;
// Whether a stored panel message still exists — the shared probe and outcome
// type every panel's `ensure()` uses.
pub(crate) mod panel_check;
#[allow(dead_code)]
pub(crate) mod render;
// An organizer's own record of a game, for a set played outside the draft tool
// or a draft that was abandoned. The fallback, not the primary path.
pub(crate) mod report;
// `/tournament register|rebind|withdraw`'s business logic.
pub(crate) mod registration;
// `/set redraft`: abandons a set's current draft room for a fresh one from the
// same preset — the remedy for a mis-seated draft or one that stalled.
pub(crate) mod redraft;
// `/tournament create`'s slug argument.
pub(crate) mod slug;
// What must be configured before a tournament can start, and
// which draft preset — and so which best_of — each round uses.
pub(crate) mod setup;
// Ratings and suggested seeding: the pure tiering plus the one
// aoe4world path that snapshots ATR and ELO onto each entry.
pub(crate) mod seeding;
// The seeding panel: the seeded field, rendered into
// `#{slug}-bracket` and edited in place as an organizer overrides seeds.
pub(crate) mod seed_panel;
// Set threads: a private thread per set, its draft room and
// the pinned panel telling each player which seat to take.
pub(crate) mod set_thread;
// `/tournament start`: the gates, then the generated
// bracket persisted and round one opened.
pub(crate) mod start;
// On boot, confirm every live tournament's panels still exist and recreate
// whichever an organizer deleted.
pub(crate) mod startup;
// `/tournament delete`'s guards — pure, like `access::decide`.
pub(crate) mod teardown;
// The panel-edit throttle, so a burst of button presses coalesces into one
// edit. Consumed by the registration panel (`panel::refresh`).
pub(crate) mod throttle;
