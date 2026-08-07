//! Who may manage a tournament's admin list (docs/tournament.md §8.2), and who
//! may create one in the first place — the codebase's first access control.
//! The decisions (`decide`, `may_create_tournament`) are pure and
//! Discord/database-free so every case is unit-tested directly; the check
//! functions around them do the one DB lookup and/or Discord permission fetch
//! each decision actually needs.

use crate::db::to_db_id;
use crate::locale::Locale;
use crate::reply::ephemeral;
use crate::tournament::db;
use crate::{Context, Error};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Access {
    /// `tournaments.created_by` — the ultimate authority (§8.2).
    Creator,
    /// Listed in `tournament_admins`, but not the creator.
    Admin,
    /// Holds the guild's `MANAGE_GUILD` bit. Per §8.2 this grants the SAME
    /// authority as the creator (not merely admin-tier) — deliberate, so a
    /// tournament is recoverable if its creator has left the server.
    ManageGuildBypass,
    Nobody,
}

impl Access {
    /// `/tournament admin add|remove|list` (§8.2: "creator only") — a plain
    /// `Admin` may not manage the admin list; only `Creator` or the bypass may.
    pub(crate) fn may_manage_admins(self) -> bool {
        matches!(self, Access::Creator | Access::ManageGuildBypass)
    }

    /// The broader "any admin" tier (§8.2: "open/close check-in, seed, start,
    /// cancel, draft, manual report, schedule") — unlike `may_manage_admins`,
    /// a plain `Admin` is included.
    pub(crate) fn may_manage_tournament(self) -> bool {
        matches!(self, Access::Creator | Access::Admin | Access::ManageGuildBypass)
    }
}

/// Pure decision. `has_manage_guild` is checked before `is_admin` deliberately:
/// a user who is both a plain admin AND holds `MANAGE_GUILD` must be reported as
/// `ManageGuildBypass`, not `Admin` — otherwise `may_manage_admins` would
/// (incorrectly) deny someone who actually has the authority to act.
pub(crate) fn decide(user_id: i64, created_by: i64, is_admin: bool, has_manage_guild: bool) -> Access {
    if user_id == created_by {
        Access::Creator
    } else if has_manage_guild {
        Access::ManageGuildBypass
    } else if is_admin {
        Access::Admin
    } else {
        Access::Nobody
    }
}

/// Command check for `/tournament admin add|remove|list` and `/tournament delete`
/// — the creator-only tier, tighter than `tournament_manage_only`. Resolves the
/// tournament from the invoking channel — matching ANY of its five stored
/// channel ids, not a slug argument (docs/tournament.md §8.2's admin-resolution
/// gap: every reply here is ephemeral, so there's no clutter cost to letting an
/// admin manage the list from wherever they already are) — then applies `decide`.
pub(crate) async fn tournament_admin_only(ctx: Context<'_>) -> Result<bool, Error> {
    let pool = &ctx.data().database;
    let channel_id = to_db_id(ctx.channel_id());

    let Some(tournament) = db::get_tournament_by_any_channel_id(pool, channel_id).await? else {
        ephemeral(ctx, wrong_channel_message(Locale::from_context(ctx))).await?;
        return Ok(false);
    };

    let user_id = to_db_id(ctx.author().id);
    let is_admin = db::is_admin(pool, tournament.id, user_id).await?;
    let has_manage_guild = author_has_manage_guild(ctx).await?;

    if decide(user_id, tournament.created_by, is_admin, has_manage_guild).may_manage_admins() {
        return Ok(true);
    }
    ephemeral(
        ctx,
        Locale::from_context(ctx).pick(
            "只有賽事建立者（或擁有管理伺服器權限的成員）才能執行這個操作。",
            "Only the tournament's creator (or a member with Manage Guild) can do that.",
        ),
    )
    .await?;
    Ok(false)
}

/// Command check for `/tournament open-checkin|close-checkin` (and, in later
/// chunks, seed/start/cancel/draft/report/schedule) — the broader "any admin"
/// tier, unlike `tournament_admin_only`'s creator-only gate.
pub(crate) async fn tournament_manage_only(ctx: Context<'_>) -> Result<bool, Error> {
    let pool = &ctx.data().database;
    let channel_id = to_db_id(ctx.channel_id());

    let Some(tournament) = db::get_tournament_by_any_channel_id(pool, channel_id).await? else {
        ephemeral(ctx, wrong_channel_message(Locale::from_context(ctx))).await?;
        return Ok(false);
    };

    let user_id = to_db_id(ctx.author().id);
    let is_admin = db::is_admin(pool, tournament.id, user_id).await?;
    let has_manage_guild = author_has_manage_guild(ctx).await?;

    if decide(user_id, tournament.created_by, is_admin, has_manage_guild).may_manage_tournament() {
        return Ok(true);
    }
    ephemeral(
        ctx,
        Locale::from_context(ctx).pick(
            "只有賽事建立者、賽事管理員，或擁有管理伺服器權限的成員才能執行這個操作。",
            "Only the tournament's creator, an admin, or a member with Manage Guild can do that.",
        ),
    )
    .await?;
    Ok(false)
}

/// The one wording for "you're not in a tournament channel", shared by the two
/// checks here and by `commands::resolve_tournament_by_channel` — ten call sites
/// before this existed, which localizing would have turned into ten pairs.
pub(crate) fn wrong_channel_message(locale: Locale) -> &'static str {
    locale.pick(
        "這個指令必須在賽事自己的頻道中執行（公告、報名、賽表、地圖選用或對戰頻道）。",
        "This command must be run in one of the tournament's own channels \
         (its announce, register, bracket, draft or matches channel).",
    )
}

async fn author_has_manage_guild(ctx: Context<'_>) -> Result<bool, Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(false); // guild_only + tournament_only already refuse this case
    };
    let Some(member) = ctx.author_member().await else {
        return Ok(false);
    };
    let partial_guild = guild_id.to_partial_guild(ctx.http()).await?;
    Ok(partial_guild.member_permissions(&member).manage_guild())
}

/// `/tournament create` (§8.4). Deliberately an OR, not a replacement of the
/// Manage Guild path: the organizer role is a hardcoded id (`guilds.rs`) with no
/// setup command yet, so Manage Guild has to keep working on its own even if the
/// role is never assigned, misconfigured, or later deleted.
pub(crate) fn may_create_tournament(has_manage_guild: bool, has_organizer_role: bool) -> bool {
    has_manage_guild || has_organizer_role
}

/// Command check for `/tournament create`.
pub(crate) async fn create_tournament_only(ctx: Context<'_>) -> Result<bool, Error> {
    let has_manage_guild = author_has_manage_guild(ctx).await?;
    let has_organizer_role = author_has_organizer_role(ctx).await?;

    if may_create_tournament(has_manage_guild, has_organizer_role) {
        return Ok(true);
    }
    ephemeral(
        ctx,
        Locale::from_context(ctx).pick(
            "你需要管理伺服器權限或賽事主辦身分組才能建立賽事。",
            "You need Manage Guild or the tournament organizer role to create a tournament.",
        ),
    )
    .await?;
    Ok(false)
}

async fn author_has_organizer_role(ctx: Context<'_>) -> Result<bool, Error> {
    let Some(member) = ctx.author_member().await else {
        return Ok(false);
    };
    Ok(member.roles.contains(&ctx.data().guilds.tournament_organizer_role))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREATOR: i64 = 1;
    const OTHER: i64 = 2;

    #[test]
    fn the_wrong_channel_message_renders_in_both_locales() {
        let zh = wrong_channel_message(Locale::ZhTw);
        let en = wrong_channel_message(Locale::En);
        assert_ne!(zh, en);
        assert!(zh.contains("賽事自己的頻道"), "{zh}");
        assert!(en.contains("tournament's own channels"), "{en}");
    }

    #[test]
    fn the_creator_always_wins_regardless_of_the_other_flags() {
        for is_admin in [false, true] {
            for has_manage_guild in [false, true] {
                assert_eq!(decide(CREATOR, CREATOR, is_admin, has_manage_guild), Access::Creator);
            }
        }
    }

    #[test]
    fn manage_guild_bypass_outranks_plain_admin() {
        // The subtle case: a listed admin who is ALSO a Manage Guild member must
        // report as the bypass, or `may_manage_admins` would wrongly deny them.
        assert_eq!(decide(OTHER, CREATOR, true, true), Access::ManageGuildBypass);
        assert_eq!(decide(OTHER, CREATOR, false, true), Access::ManageGuildBypass);
    }

    #[test]
    fn plain_admin_without_manage_guild_is_admin_tier() {
        assert_eq!(decide(OTHER, CREATOR, true, false), Access::Admin);
    }

    #[test]
    fn nobody_without_any_of_the_three() {
        assert_eq!(decide(OTHER, CREATOR, false, false), Access::Nobody);
    }

    #[test]
    fn only_creator_and_bypass_may_manage_admins() {
        assert!(Access::Creator.may_manage_admins());
        assert!(Access::ManageGuildBypass.may_manage_admins());
        assert!(!Access::Admin.may_manage_admins());
        assert!(!Access::Nobody.may_manage_admins());
    }

    #[test]
    fn creator_admin_and_bypass_may_manage_the_tournament() {
        assert!(Access::Creator.may_manage_tournament());
        assert!(Access::Admin.may_manage_tournament());
        assert!(Access::ManageGuildBypass.may_manage_tournament());
        assert!(!Access::Nobody.may_manage_tournament());
    }

    #[test]
    fn creating_a_tournament_needs_manage_guild_or_the_organizer_role() {
        assert!(may_create_tournament(true, true));
        assert!(may_create_tournament(true, false));
        assert!(may_create_tournament(false, true));
        assert!(!may_create_tournament(false, false));
    }
}
