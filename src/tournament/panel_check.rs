//! Whether a stored panel message still exists, and the shared outcome type
//! every panel's `ensure()` reports it with.
//!
//! `missing_status` is the pure half — the same shape as
//! `Emperor::detect_blocked`, the only other place in the codebase that
//! inspects a serenity error's shape — and is what keeps `message_exists`
//! honest: a permission error, a rate limit or a network hiccup must never be
//! read as "deleted", or a boot reconciler running unattended across every
//! tournament turns a hiccup into a duplicate panel.

use crate::Error;
use serenity::all::{CacheHttp, ChannelId, HttpError, MessageId, StatusCode};

/// Which HTTP status counts as "the message is confirmed gone" — a 404
/// (Unknown Message), and only that. Pure, and the only unit-testable half of
/// this module: `serenity::ErrorResponse`/`DiscordJsonError` are
/// `#[non_exhaustive]`, so a real `serenity::Error` can't be hand-built to
/// test `is_confirmed_missing` directly.
fn missing_status(status: StatusCode) -> bool {
    status == StatusCode::NOT_FOUND
}

/// The same question asked of a real serenity error.
pub(crate) fn is_confirmed_missing(error: &serenity::Error) -> bool {
    matches!(
        error,
        serenity::Error::Http(HttpError::UnsuccessfulRequest(response)) if missing_status(response.status_code)
    )
}

/// `Ok(true)` if the message is there, `Ok(false)` if it's confirmed gone,
/// `Err` if the check itself was inconclusive — the caller's cue to leave that
/// panel alone this run rather than guess.
pub(crate) async fn message_exists(
    http: impl CacheHttp,
    channel_id: ChannelId,
    message_id: MessageId,
) -> Result<bool, Error> {
    match channel_id.message(http, message_id).await {
        Ok(_) => Ok(true),
        Err(err) if is_confirmed_missing(&err) => Ok(false),
        Err(err) => Err(err.into()),
    }
}

/// What one panel's `ensure()` found, so a caller can log or word it without
/// re-deriving the distinction between "already fine" and "just repaired" —
/// the same lesson `bracket_view::ReconcileOutcome` encodes: an edit means the
/// panel was already there, and only a repost is a repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelOutcome {
    /// Confirmed present (and, where the panel type supports it, refreshed).
    Present,
    /// Was missing, or never posted; freshly created and recorded.
    Reposted,
    /// The channel this panel lives in isn't configured.
    NotConfigured,
    /// This tournament's phase doesn't call for this panel yet.
    NotExpected,
    /// Couldn't confirm and/or couldn't repair it; left as-is.
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_404_is_confirmed_missing() {
        assert!(missing_status(StatusCode::NOT_FOUND));
        assert!(!missing_status(StatusCode::FORBIDDEN));
        assert!(!missing_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(!missing_status(StatusCode::INTERNAL_SERVER_ERROR));
    }
}
