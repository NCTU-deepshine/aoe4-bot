//! Tournament management (docs/tournament.md).

// Consumed by `/tournament start`, which generates and stores a bracket. Until that
// lands, only this module's own tests exercise it — remove the allow then.
#[allow(dead_code)]
pub(crate) mod bracket;
#[allow(dead_code)]
pub(crate) mod render;
