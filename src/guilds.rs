use crate::locale::Locale;
use crate::reply::ephemeral;
use crate::{Context, Error};
use serenity::model::id::{GuildId, RoleId};

/// Which feature set something belongs to (docs/tournament.md §8.0).
///
/// The two sets must not leak into each other: no tournament commands in the home
/// guild, and none of the ranked board or the message reactions in the tournament
/// guild.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Feature {
    /// What the bot did first: the ranked board, `/查分`, the reactions.
    Home,
    /// Tournaments.
    Tournament,
}

/// Where each feature set lives, plus the one role this codebase currently
/// hardcodes alongside them. All of it is known and fixed for now, so this is
/// configuration rather than a per-guild table.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Guilds {
    pub(crate) home: GuildId,
    pub(crate) tournament: GuildId,
    /// Who may run `/tournament create` (docs/tournament.md §8.4), besides
    /// anyone with `MANAGE_GUILD` (that bypass always applies — see
    /// `tournament::access` — so the tournament stays creatable even if this
    /// role is never assigned, misconfigured, or later deleted). Hardcoded, with
    /// no env override (unlike the guild ids below): a scratch guild used for
    /// local testing would need its own different role id anyway, and a local
    /// admin already has `MANAGE_GUILD` on their own scratch server, so an
    /// override would buy nothing there. Real configurability is a later
    /// improvement (a proper per-guild setting), not an env var.
    pub(crate) tournament_organizer_role: RoleId,
}

// Both guilds, in the source. A guild id is an identifier, not a credential — it is
// in every message link and visible to every member — so it belongs here alongside
// the channel ids this codebase already hardcodes, rather than in the deployment's
// environment. There are exactly two, and both are known (docs/tournament.md §8.0).
const HOME_GUILD: GuildId = GuildId::new(1262320259252097034);
const TOURNAMENT_GUILD: GuildId = GuildId::new(1154585078811340850);
const TOURNAMENT_ORGANIZER_ROLE: RoleId = RoleId::new(1477224039817678940);

impl Guilds {
    /// The guild constants above, unless the environment names something else.
    ///
    /// The overrides let a local run point at a scratch server, and they mean this
    /// change cannot break the deployed bot: if `GUILD_ID` is still set in production
    /// it keeps governing, so a wrong constant would be corrected rather than shipped.
    pub(crate) fn configured() -> Self {
        let guilds = Self {
            home: from_env("GUILD_ID", HOME_GUILD),
            tournament: from_env("TOURNAMENT_GUILD_ID", TOURNAMENT_GUILD),
            tournament_organizer_role: TOURNAMENT_ORGANIZER_ROLE,
        };
        ensure_distinct(guilds.home, guilds.tournament);
        guilds
    }

    pub(crate) fn guild_for(self, feature: Feature) -> GuildId {
        match feature {
            Feature::Home => self.home,
            Feature::Tournament => self.tournament,
        }
    }

    /// Whether `feature` may be used in `guild`, where `None` means a DM.
    ///
    /// Pure, so the rule is testable without a Discord connection — and it is the
    /// same rule for commands and for event handlers.
    pub(crate) fn allows(self, feature: Feature, guild: Option<GuildId>) -> bool {
        guild == Some(self.guild_for(feature))
    }
}

fn ensure_distinct(home: GuildId, tournament: GuildId) {
    if home == tournament {
        panic!(
            "the home and tournament guilds must be different (docs/tournament.md §8.0); check GUILD_ID and TOURNAMENT_GUILD_ID"
        );
    }
}

fn from_env(name: &str, default: GuildId) -> GuildId {
    resolve(std::env::var(name).ok().as_deref(), default, name)
}

/// Pure so the override can be tested without touching the process environment,
/// which is global and shared by every test in the binary.
fn resolve(configured: Option<&str>, default: GuildId, name: &str) -> GuildId {
    match configured.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be a valid guild id")),
        None => default,
    }
}

/// Command check for the home guild's commands.
///
/// Registration is already per guild, so this only fires for an invocation that
/// should not have been possible — a registration left behind by an older deploy,
/// or the bot sitting in a third guild. Defence in depth, not the mechanism.
pub(crate) async fn home_only(ctx: Context<'_>) -> Result<bool, Error> {
    allowed_here(ctx, Feature::Home).await
}

/// Command check for the tournament guild's commands. Mirrors `home_only`
/// exactly (docs/tournament.md §8.0) — defence in depth, not the mechanism,
/// since registration is already per guild.
pub(crate) async fn tournament_only(ctx: Context<'_>) -> Result<bool, Error> {
    allowed_here(ctx, Feature::Tournament).await
}

async fn allowed_here(ctx: Context<'_>, feature: Feature) -> Result<bool, Error> {
    if ctx.data().guilds.allows(feature, ctx.guild_id()) {
        return Ok(true);
    }
    // Answer before refusing. A check that returns false without replying leaves
    // the interaction unacknowledged, and Discord shows "the application did not
    // respond" — see the CommandCheckFailed arm in errors.rs.
    ephemeral(
        ctx,
        Locale::from_context(ctx).pick(
            "這個指令不能在這個伺服器使用。",
            "This command can't be used in this server.",
        ),
    )
    .await?;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::{Feature, Guilds, HOME_GUILD, TOURNAMENT_GUILD, ensure_distinct, resolve};
    use serenity::model::id::{GuildId, RoleId};

    const HOME: GuildId = GuildId::new(1);
    const TOURNAMENT: GuildId = GuildId::new(2);
    const ELSEWHERE: GuildId = GuildId::new(3);
    const ORGANIZER_ROLE: RoleId = RoleId::new(9);

    fn two_guilds() -> Guilds {
        Guilds {
            home: HOME,
            tournament: TOURNAMENT,
            tournament_organizer_role: ORGANIZER_ROLE,
        }
    }

    #[test]
    fn each_feature_is_allowed_only_in_its_own_guild() {
        let guilds = two_guilds();

        assert!(guilds.allows(Feature::Home, Some(HOME)));
        assert!(!guilds.allows(Feature::Home, Some(TOURNAMENT)));

        assert!(guilds.allows(Feature::Tournament, Some(TOURNAMENT)));
        assert!(!guilds.allows(Feature::Tournament, Some(HOME)));
    }

    #[test]
    fn nothing_is_allowed_in_an_unrelated_guild_or_a_dm() {
        let guilds = two_guilds();

        for feature in [Feature::Home, Feature::Tournament] {
            assert!(!guilds.allows(feature, Some(ELSEWHERE)));
            assert!(!guilds.allows(feature, None), "a DM has no guild to match");
        }
    }

    #[test]
    fn an_unset_or_blank_override_leaves_the_known_guild() {
        for default in [HOME_GUILD, TOURNAMENT_GUILD] {
            assert_eq!(resolve(None, default, "GUILD_ID"), default);
            assert_eq!(resolve(Some(""), default, "GUILD_ID"), default);
            assert_eq!(resolve(Some("   "), default, "GUILD_ID"), default);
        }
    }

    #[test]
    fn an_override_wins_over_the_known_guild() {
        assert_eq!(resolve(Some("42"), HOME_GUILD, "GUILD_ID"), GuildId::new(42));
        assert_eq!(resolve(Some(" 42 "), HOME_GUILD, "GUILD_ID"), GuildId::new(42));
    }

    #[test]
    fn distinct_guilds_are_accepted() {
        // Covers the real constants too: startup would panic if they ever converged,
        // and the whole split is pointless if they did.
        ensure_distinct(HOME_GUILD, TOURNAMENT_GUILD);
        ensure_distinct(HOME, TOURNAMENT);
    }

    #[test]
    #[should_panic(expected = "must be different")]
    fn one_guild_playing_both_roles_is_rejected() {
        // Only reachable through an override. Rejected at startup rather than handled,
        // so no later code has to ask whether the two guilds might be the same one.
        ensure_distinct(HOME, HOME);
    }
}
