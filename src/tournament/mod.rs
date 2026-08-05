//! Tournament management (docs/tournament.md).

// Consumed by `/tournament start`, which generates and stores a bracket. Until that
// lands, only this module's own tests exercise it — remove the allow then.
#[allow(dead_code)]
pub(crate) mod bracket;
// Consumed starting with chunk 7 (`/tournament create`) onward; until then only
// src/integration_tests.rs's gate tests exercise it — remove the allow once chunk 7
// lands and calls these functions directly.
#[allow(dead_code)]
pub(crate) mod db;
#[allow(dead_code)]
pub(crate) mod render;
