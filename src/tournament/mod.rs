//! Tournament management (docs/tournament.md).

// Consumed by `/tournament start`, which generates and stores a bracket. Until that
// lands, only this module's own tests exercise it — remove the allow then.
#[allow(dead_code)]
pub(crate) mod bracket;
// `/tournament admin add|remove|list` (§8.2) — the codebase's first access control.
pub(crate) mod access;
// Chunk 7 (`/tournament create`, the admin list) is the first caller, but only of
// a fraction of this file — everything else is consumed starting with chunk 9
// (registration) through chunk 22 (result import); see the per-section notes in
// db.rs itself for which chunk consumes which table.
#[allow(dead_code)]
pub(crate) mod db;
#[allow(dead_code)]
pub(crate) mod render;
// `/tournament create`'s slug argument (§8.1).
pub(crate) mod slug;
