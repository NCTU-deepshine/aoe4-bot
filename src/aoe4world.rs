use chrono::{DateTime, Utc};
use serde::Deserialize;

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
    pub last_game_at: DateTime<Utc>,
}

#[derive(Deserialize, Clone, Debug)]
pub(crate) struct CivData {
    pub civilization: String,
    pub pick_rate: f64,
}

impl CivData {
    pub fn civilization(&self) -> String {
        match self.civilization.as_str() {
            "byzantines" => "拜占庭".to_string(),
            "holy_roman_empire" => "神聖羅馬帝國".to_string(),
            "delhi_sultanate" => "德里蘇丹國".to_string(),
            "french" => "法蘭西".to_string(),
            "malians" => "馬利".to_string(),
            "order_of_the_dragon" => "龍騎士團".to_string(),
            "abbasid_dynasty" => "阿拔斯王朝".to_string(),
            "english" => "英格蘭".to_string(),
            "mongols" => "蒙古".to_string(),
            "ayyubids" => "阿育布".to_string(),
            "ottomans" => "鄂圖曼".to_string(),
            "rus" => "羅斯".to_string(),
            "jeanne_darc" => "聖女貞德".to_string(),
            "japanese" => "日本".to_string(),
            "chinese" => "中國".to_string(),
            "zhu_xis_legacy" => "朱熹".to_string(),
            _ => self.civilization.clone(),
        }
    }
}

#[derive(Deserialize, Debug)]
pub(crate) struct RankedEloData {
    pub rating: i32,
}
