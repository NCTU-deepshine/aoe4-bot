use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::cmp::Ordering;

#[derive(Deserialize, Debug)]
pub(crate) struct Profile {
    pub name: String,
    pub modes: Modes,
}

#[derive(Deserialize, Debug)]
pub(crate) struct Modes {
    pub rm_solo: Option<RankedData>,
    pub rm_1v1_elo: Option<RankedEloData>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct RankedData {
    pub rank: i32,
    pub rank_level: String,
    pub rating: i32,
    pub max_rating_1m: i32,
    pub games_count: i32,
    pub win_rate: f64,
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
            "byzantines" => "東羅馬帝國(Imperium Romanum Orientale)".to_string(),
            "holy_roman_empire" => "神聖羅馬帝國(Heiliges Römisches Reich)".to_string(),
            "delhi_sultanate" => "德里蘇丹國(سلطنت دهلی)".to_string(),
            "french" => "法蘭西(Français)".to_string(),
            "malians" => "馬利(Manden Duguba)".to_string(),
            "order_of_the_dragon" => "龍騎士團(Societas Draconistarum)".to_string(),
            "abbasid_dynasty" => "阿拔斯王朝(الْخِلَافَة الْعَبَّاسِيَّة)".to_string(),
            "english" => "英格蘭(English)".to_string(),
            "mongols" => "蒙古(ᠶᠡᠬᠡ ᠮᠣᠩᠭᠣᠯ ᠤᠯᠤᠰ)".to_string(),
            "ayyubids" => "阿育布(ئەیووبی)".to_string(),
            "ottomans" => "鄂圖曼(دولت علیهٔ عثمانیه)".to_string(),
            "rus" => "羅斯(Русь)".to_string(),
            "jeanne_darc" => "聖女貞德(Jehanne Darc)".to_string(),
            "japanese" => "日本国".to_string(),
            "chinese" => "中國".to_string(),
            "zhu_xis_legacy" => "朱熹".to_string(),
            "knights_templar" => "聖殿騎士團(Les Chevaliers Templiers)".to_string(),
            "house_of_lancaster" => "蘭卡斯特家族(House of Lancaster)".to_string(),
            "macedonian_dynasty" => "馬其頓王朝(Μακεδονική Δυναστεία)".to_string(),
            "golden_horde" => "欽察汗國(Алтан Орд)".to_string(),
            "tughlaq_dynasty" => "圖格魯克王朝(تغلق شاهیان)".to_string(),
            "sengoku_daimyo" => "戦国大名".to_string(),
            _ => self.civilization.replace("_", " "),
        }
    }
}

#[derive(Deserialize, Debug)]
pub(crate) struct RankedEloData {
    pub rating: i32,
}

#[derive(Deserialize, Debug)]
pub(crate) struct SearchResult {
    pub players: Vec<SearchedPlayer>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct SearchedPlayer {
    pub name: String,
    pub profile_id: i32,
    pub leaderboards: LeaderBoards,
}

impl SearchedPlayer {
    pub fn rating(&self) -> i32 {
        self.leaderboards.rm_solo.as_ref().map(|x| x.rating()).unwrap_or(0)
    }
}

#[derive(Deserialize, Debug)]
pub(crate) struct LeaderBoards {
    pub rm_solo: Option<SearchedRankedData>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct SearchedRankedData {
    pub rank_level: String,
    pub rating: Option<i32>,
}

impl SearchedRankedData {
    pub(crate) fn rating(&self) -> i32 {
        self.rating.unwrap_or(0)
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

impl Eq for SearchedPlayer {}

impl PartialEq<Self> for SearchedPlayer {
    fn eq(&self, other: &Self) -> bool {
        self.profile_id == other.profile_id
    }
}

impl PartialOrd<Self> for SearchedPlayer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        other.rating().partial_cmp(&self.rating())
    }
}

impl Ord for SearchedPlayer {
    fn cmp(&self, other: &Self) -> Ordering {
        other.rating().cmp(&self.rating())
    }
}
