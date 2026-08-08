//! Tournament management.

// Consumed by `/tournament start`, which generates and stores a bracket. Until that
// lands, only this module's own tests exercise it — remove the allow then.
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
// The interaction dispatcher's own `EventHandler` — kept
// separate from `Emperor`, which is home-guild meme/reaction logic with no
// tournament knowledge; registered as a second handler in `main.rs`.
pub(crate) mod dispatch;
// The registration panel: rendering plus the Discord/DB glue
// `commands::create` and `dispatch::Dispatcher` call into.
pub(crate) mod panel;
#[allow(dead_code)]
pub(crate) mod render;
// `/tournament register|rebind|withdraw`'s business logic.
pub(crate) mod registration;
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
// `/tournament delete`'s guards — pure, like `access::decide`.
pub(crate) mod teardown;
// The panel-edit throttle, so a burst of button presses coalesces into one
// edit. Consumed by the registration panel (`panel::refresh`).
pub(crate) mod throttle;
