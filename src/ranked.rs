use crate::aoe4world::{CivData, Profile};
use crate::db::Account;
use crate::Data;
use chrono::{DateTime, Utc};
use reqwest::Url;
use serenity::all::{Http, UserId};
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
    last_played: DateTime<Utc>,
}

impl RankedPlayer {
    pub(crate) fn last_played(&self) -> String {
        let days = Utc::now().signed_duration_since(self.last_played).num_days();
        if days == 0 {
            "最近".to_string()
        } else {
            format!("{}天前", days)
        }
    }

    pub(crate) fn rank_level(&self) -> String {
        match self.rank_level.as_str() {
            "conqueror_3" => "征服者3".to_string(),
            "conqueror_2" => "征服者2".to_string(),
            "conqueror_1" => "征服者1".to_string(),
            "diamond_3" => "鑽石3".to_string(),
            "diamond_2" => "鑽石2".to_string(),
            "diamond_1" => "鑽石1".to_string(),
            "platinum_3" => "白金3".to_string(),
            "platinum_2" => "白金2".to_string(),
            "platinum_1" => "白金1".to_string(),
            "gold_3" => "黃金3".to_string(),
            "gold_2" => "黃金2".to_string(),
            "gold_1" => "黃金1".to_string(),
            "silver_3" => "白銀3".to_string(),
            "silver_2" => "白銀2".to_string(),
            "silver_1" => "白銀1".to_string(),
            "bronze_3" => "青銅3".to_string(),
            "bronze_2" => "青銅2".to_string(),
            "bronze_1" => "青銅1".to_string(),
            _ => self.rank_level.clone(),
        }
    }
}

impl Eq for RankedPlayer {}

impl PartialEq<Self> for RankedPlayer {
    fn eq(&self, other: &Self) -> bool {
        self.global_rank == other.global_rank
    }
}

impl PartialOrd<Self> for RankedPlayer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.global_rank.partial_cmp(&other.global_rank)
    }
}

impl Ord for RankedPlayer {
    fn cmp(&self, other: &Self) -> Ordering {
        self.global_rank.cmp(&other.global_rank)
    }
}

impl Display for RankedPlayer {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({})\n\
            遊戲ID: {}\n\
            階級: {}\n\
            全球排名: {}, 遊戲場次：{}\n\
            愛用文明: {} (出場率 {}%), 上次遊玩: {}\n\
            排名積分: {}, 近期最高積分: {}, ELO: {}",
            self.discord_display,
            self.discord_username,
            self.aoe4_name,
            self.rank_level(),
            self.global_rank,
            self.games_played,
            self.favorite_civ.civilization(),
            self.favorite_civ.pick_rate.round(),
            self.last_played(),
            self.rating,
            self.recent_max_rating,
            self.elo
        )
    }
}

pub(crate) async fn try_create_ranked_from_account(http: &Http, data: &Data, account: Account) -> Option<RankedPlayer> {
    info!(
        "try create ranked from account, discord {}, aoe4 {}",
        account.user_id, account.aoe4_id
    );
    let user = http.get_user(UserId::new(account.user_id as u64)).await.ok()?;
    let discord_username = user.name.clone();
    let discord_global_name = user.global_name.clone();
    let discord_nickname = http
        .get_guild(data.guild_id)
        .await
        .ok()?
        .member(http, UserId::new(account.user_id as u64))
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
        favorite_civ: profile
            .modes
            .rm_solo
            .civilizations
            .first()
            .unwrap_or(&CivData {
                civilization: "未知".to_string(),
                pick_rate: 0.0,
            })
            .clone(),
        games_played: profile.modes.rm_solo.games_count,
        last_played: profile.modes.rm_solo.last_game_at,
    })
}

#[tokio::test]
async fn test() {
    let account = Account {
        user_id: 720955323183267840,
        aoe4_id: 13753974,
    };
    let url = Url::parse("https://aoe4world.com/api/v0/players/")
        .unwrap()
        .join(&account.aoe4_id.to_string())
        .unwrap();
    let profile = reqwest::get(url).await.unwrap().json::<Profile>().await.unwrap();
    info!("got aoe4 world profile for {}", profile.name);
}
