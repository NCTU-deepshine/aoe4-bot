use serde::Deserialize;
use serenity::all::Timestamp;

#[derive(Deserialize, Debug)]
pub(crate) struct Profile {
    pub name: String,
    pub modes: Modes,
}

#[derive(Deserialize, Debug)]
pub(crate) struct Modes {
    pub rm_solo: RankedData,
    pub rm_1v1_elo: RankedEloData,
}

#[derive(Deserialize, Debug)]
pub(crate) struct RankedData {
    pub rank: i32,
    pub rank_level: String,
    pub rating: i32,
    pub max_rating_1m: i32,
    pub games_count: i32,
    pub civilizations: Vec<CivData>,
    pub last_game_at: Timestamp,
}

#[derive(Deserialize, Clone, Debug)]
pub(crate) struct CivData {
    pub civilization: String,
    pub pick_rate: f64,
}

#[derive(Deserialize, Debug)]
pub(crate) struct RankedEloData {
    pub rating: i32,
}
