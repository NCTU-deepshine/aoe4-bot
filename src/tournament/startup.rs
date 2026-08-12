//! On boot, confirm every live tournament's registration, check-in, seed and
//! bracket panels still exist, and recreate whichever an organizer deleted.
//! `docs/tournament.md:1335-1336`: *"Panel message ids live in the DB, so a
//! boot-time reconciliation should confirm each still exists and recreate it
//! if an organizer deleted it."*
//!
//! Per-set panels and the `#…-draft` announcement are not covered here — a
//! set panel lives inside an already-open thread, and there is no "repost
//! into an existing thread" entry point today; that would be new `set_thread`
//! machinery, not reuse. Nor is channel-permission reapplication
//! (`commands::reapply_channel_permissions`): nothing deletes a permission
//! overwrite on restart, and it needs `ctx.guild_id()`/`ctx.cache()`, which
//! this boot path doesn't have.
//!
//! Best-effort per tournament and per panel, the same contract as
//! `set_thread::open_ready` — one tournament that can't be checked must not
//! stop the rest, and this runs with nobody watching, so the log is the only
//! record.

use crate::tournament::db;
use crate::tournament::panel_check::PanelOutcome;
use crate::tournament::{bracket_view, checkin_panel, panel, seed_panel};
use serenity::all::CacheHttp;
use sqlx::SqlitePool;
use tracing::{error, info};

pub(crate) async fn reconcile_all(http: impl CacheHttp, pool: &SqlitePool) {
    let tournaments = match db::list_live_tournaments(pool).await {
        Ok(tournaments) => tournaments,
        Err(err) => {
            error!("failed to list live tournaments for boot reconciliation: {err:?}");
            return;
        },
    };

    for tournament in &tournaments {
        report(
            tournament.id,
            "registration",
            panel::ensure(&http, pool, tournament).await,
        );
        report(
            tournament.id,
            "check-in",
            checkin_panel::ensure(&http, pool, tournament).await,
        );
        report(tournament.id, "seed", seed_panel::ensure(&http, pool, tournament).await);

        match bracket_view::reconcile(&http, pool, tournament).await {
            Ok(outcome) if outcome.changed() => {
                info!("bracket reconciled for tournament {}: {outcome:?}", tournament.id);
            },
            Ok(_) => {},
            Err(err) => error!(
                "failed to reconcile the bracket for tournament {}: {err:?}",
                tournament.id
            ),
        }
    }
}

/// One line per panel — a repost is worth an `info!`, a check that couldn't
/// even be completed is worth an `error!`, and everything else (already there,
/// not configured, not expected yet) is unremarkable and stays quiet, so the
/// log reads as a repair list rather than a status dump.
fn report(tournament_id: i64, panel: &str, outcome: PanelOutcome) {
    match outcome {
        PanelOutcome::Reposted => info!("reposted the {panel} panel for tournament {tournament_id}"),
        PanelOutcome::Failed => error!("could not confirm or repair the {panel} panel for tournament {tournament_id}"),
        PanelOutcome::Present | PanelOutcome::NotConfigured | PanelOutcome::NotExpected => {},
    }
}
