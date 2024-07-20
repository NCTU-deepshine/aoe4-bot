use crate::aoe4world::{CivData, Profile};
use crate::db::Account;
use crate::Context;
use reqwest::Url;
use serenity::all::UserId;
use std::cmp::Ordering;
use std::fmt::{Display, Formatter};
use tracing::info;

pub(crate) struct RankedPlayer {
    aoe4_name: String,
    discord_display: String,
    discord_username: String,
    rank_level: String,
    global_rank: i32,
    rating: i32,
    recent_max_rating: i32,
    elo: i32,
    favorite_civ: CivData,
    games_played: i32,
    last_played: String,
}

impl Eq for RankedPlayer {}

impl PartialEq<Self> for RankedPlayer {
    fn eq(&self, other: &Self) -> bool {
        self.rating == other.rating
    }
}

impl PartialOrd<Self> for RankedPlayer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.rating.partial_cmp(&other.rating)
    }
}

impl Ord for RankedPlayer {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rating.cmp(&other.rating)
    }
}

impl Display for RankedPlayer {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({})  遊戲ID： {}\n\
        階級：{}, 全球排名：{}, 遊戲場次：{}, 愛用文明：{} (出場率 {}), 上次遊玩：{}\n\
        排名積分：{}, 近期最高積分：{}, ELO：{}",
            self.discord_display,
            self.discord_username,
            self.aoe4_name,
            self.rank_level,
            self.global_rank,
            self.games_played,
            self.favorite_civ.civilization,
            self.favorite_civ.pick_rate * 100.0_f64.round(),
            self.last_played,
            self.rating,
            self.recent_max_rating,
            self.elo
        )
    }
}

pub(crate) async fn try_create_ranked_from_account(ctx: &Context<'_>, account: Account) -> Option<RankedPlayer> {
    let user = ctx.http().get_user(UserId::new(account.user_id as u64)).await.ok()?;
    let discord_username = user.name.clone();
    let discord_global_name = user.global_name.clone();
    let discord_nickname = ctx
        .http()
        .get_guild(ctx.data().guild_id)
        .await
        .ok()?
        .member(ctx.http(), UserId::new(account.user_id as u64))
        .await
        .ok()
        .and_then(|member| member.nick.clone());
    let discord_display = discord_nickname.unwrap_or(discord_global_name.unwrap_or(discord_username.clone()));
    info!("got discord profile for {}", discord_display);

    let url = Url::parse("https://aoe4world.com/api/v0/players/")
        .unwrap()
        .join(&account.aoe4_id.to_string())
        .unwrap();
    let profile = reqwest::get(url).await.ok()?.json::<Profile>().await.ok()?;
    info!("got aoe4 world profile for {}", profile.name);

    Some(RankedPlayer {
        aoe4_name: profile.name.clone(),
        discord_display,
        discord_username,
        rank_level: profile.modes.rm_solo.rank_level,
        global_rank: profile.modes.rm_solo.rank,
        rating: profile.modes.rm_solo.rating,
        recent_max_rating: profile.modes.rm_solo.max_rating_1m,
        elo: profile.modes.rm_1v1_elo.rating,
        favorite_civ: profile.modes.rm_solo.civilizations[0].clone(),
        games_played: profile.modes.rm_solo.games_count,
        last_played: profile.modes.rm_solo.last_game_at.to_rfc3339().unwrap(),
    })
}
