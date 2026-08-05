//! Tournament management (docs/tournament.md).

// Consumed by `/tournament start`, which generates and stores a bracket. Until that
// lands, only this module's own tests exercise it — remove the allow then.
#[allow(dead_code)]
pub(crate) mod bracket;
// `/tournament admin add|remove|list` (§8.2) — the codebase's first access control.
pub(crate) mod access;
// The interaction dispatcher's custom_id parsing (chunk 8, §8.5): consumed
// immediately by `dispatch::Dispatcher`.
pub(crate) mod action;
// Chunk 7 (`/tournament create`, the admin list) is the first caller, but only of
// a fraction of this file — everything else is consumed starting with chunk 9
// (registration) through chunk 22 (result import); see the per-section notes in
// db.rs itself for which chunk consumes which table.
#[allow(dead_code)]
pub(crate) mod db;
// The interaction dispatcher's own `EventHandler` (chunk 8, §8.5) — kept
// separate from `Emperor`, which is home-guild meme/reaction logic with no
// tournament knowledge; registered as a second handler in `main.rs`.
pub(crate) mod dispatch;
#[allow(dead_code)]
pub(crate) mod render;
// `/tournament create`'s slug argument (§8.1).
pub(crate) mod slug;
// The panel-edit throttle (§8.5, "Edits must be throttled"). Consumed starting
// with chunk 9's registration panel; only this module's own tests exercise it
// until then.
#[allow(dead_code)]
pub(crate) mod throttle;
