//! 网易云接口的原始 DTO 与归一化后的领域模型。
//!
//! 归一化后的 [`Song`] 与 [`PlaylistInfo`] 是本软件后续「本地播放器式筛选、
//! 整理、重建歌单」流程的统一数据源，也是本地 JSON 缓存的数据格式。

use serde::{Deserialize, Serialize};

/// 归一化后的单曲（后续筛选逻辑的数据基础）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Song {
    pub id: u64,
    pub name: String,
    /// 歌手名列表
    pub artists: Vec<String>,
    pub album: Option<String>,
    /// 时长（毫秒）
    pub duration_ms: u64,
    /// 发行时间（Unix 毫秒），未知为 None
    pub publish_time_ms: Option<i64>,
    /// 版权/收费状态：0=免费可播 1=仅 VIP 4=付费单曲 8=…（网易 `fee` 字段）
    pub fee: i64,
    /// 热度 0~100
    pub popularity: Option<f64>,
    /// 别名（副标题等）
    pub alias: Vec<String>,
    pub has_mv: bool,
    /// 是否仍能获取到详情；false 表示该曲目已从网易曲库下架/失效
    pub available: bool,
}

impl Song {
    /// 形如「歌手A/歌手B - 歌名」的展示字符串。
    pub fn display(&self) -> String {
        let artists = if self.artists.is_empty() {
            "未知歌手".to_string()
        } else {
            self.artists.join("/")
        };
        format!("{artists} - {}", self.name)
    }

    /// 以 (歌手, 歌名) 为去重键（不区分大小写）。
    pub fn dedup_key(&self) -> (String, String) {
        (
            self.artists.join("/").to_lowercase(),
            self.name.to_lowercase(),
        )
    }

    /// 是否仅限 VIP。
    pub fn is_vip_only(&self) -> bool {
        self.fee == 1
    }

    /// 发行年份（本地时区换算，未知返回 None）。
    pub fn publish_year(&self) -> Option<i32> {
        let ms = self.publish_time_ms?;
        if ms <= 0 {
            return None;
        }
        let secs = ms / 1000;
        Some(
            chrono_like_year(secs),
        )
    }
}

/// 直接换算 Unix 秒 -> 年份（避免额外引入 chrono 依赖）。
fn chrono_like_year(secs: i64) -> i32 {
    // 近似算法仅用于展示，误差可忽略（采用公历 400 年周期推算）。
    let days = secs.div_euclid(86_400);
    let (y, _, _) = civil_from_days(days);
    y
}

/// 由天数(自 1970-01-01)推算公历 (y, m, d)。Howard Hinnant 的 civil_from_days 算法。
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y } as i32, m, d)
}

/// 归一化后的歌单元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistInfo {
    pub id: u64,
    pub name: String,
    pub description: Option<String>,
    /// 歌单内曲目总数（含已下架）
    pub track_count: usize,
    pub play_count: Option<u64>,
    pub tags: Vec<String>,
    pub cover_url: Option<String>,
    pub creator_name: Option<String>,
    pub creator_id: Option<u64>,
    /// 0=普通歌单 10=排行榜 等
    pub special_type: i64,
    /// 歌单里所有曲目 id（含可能已失效的）
    pub track_ids: Vec<u64>,
}

/// 一次「抓取 + 整理」产出的本地缓存结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistDump {
    pub meta: PlaylistInfo,
    pub songs: Vec<Song>,
    pub fetched_at_ms: u64,
}

// ---------------------------------------------------------------------------
// 以下为网易云接口的原始响应 DTO，仅用于反序列化，字段与线上 JSON 一一对应。
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct ApiPlaylistDetail {
    pub code: i64,
    #[serde(default)]
    pub playlist: Option<ApiPlaylist>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiPlaylist {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "trackCount")]
    pub track_count: Option<i64>,
    #[serde(default, rename = "playCount")]
    pub play_count: Option<i64>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, rename = "coverImgUrl")]
    pub cover_img_url: Option<String>,
    #[serde(default)]
    pub creator: Option<ApiCreator>,
    #[serde(default, rename = "specialType")]
    pub special_type: i64,
    #[serde(default, rename = "trackIds")]
    pub track_ids: Vec<ApiTrackId>,
    #[serde(default)]
    pub tracks: Vec<ApiTrack>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiCreator {
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default, rename = "userId")]
    pub user_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiTrackId {
    pub id: i64,
}

// 仅用于反序列化；部分字段暂未使用属正常（保留 API 完整形状）。
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct ApiSongDetail {
    #[serde(default)]
    pub code: i64,
    #[serde(default)]
    pub songs: Vec<ApiTrack>,
}

/// 歌曲详情（字段名与线上一致：ar=艺术家 al=专辑 dt=时长 fee=收费 pop=热度…）。
#[derive(Debug, Deserialize)]
pub(crate) struct ApiTrack {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub ar: Vec<ApiArtist>,
    #[serde(default)]
    pub al: Option<ApiAlbum>,
    #[serde(default)]
    pub alia: Vec<String>,
    #[serde(default)]
    pub dt: i64,
    #[serde(default)]
    pub fee: i64,
    #[serde(default)]
    pub mv: i64,
    #[serde(default)]
    pub pop: Option<f64>,
    #[serde(default, rename = "publishTime")]
    pub publish_time: Option<i64>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct ApiArtist {
    pub id: i64,
    #[serde(default)]
    pub name: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct ApiAlbum {
    pub id: i64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "picUrl")]
    pub pic_url: Option<String>,
}

/// 登录用户的公开信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub user_id: u64,
    pub nickname: String,
}

impl PlaylistInfo {
    pub(crate) fn from_api(raw: &ApiPlaylist) -> Self {
        PlaylistInfo {
            id: raw.id as u64,
            name: raw.name.clone(),
            description: raw.description.clone(),
            track_count: raw.track_count.unwrap_or(0).max(0) as usize,
            play_count: raw.play_count.and_then(|c| (c >= 0).then_some(c as u64)),
            tags: raw.tags.clone(),
            cover_url: raw.cover_img_url.clone(),
            creator_name: raw.creator.as_ref().and_then(|c| c.nickname.clone()),
            creator_id: raw.creator.as_ref().and_then(|c| c.user_id).map(|v| v as u64),
            special_type: raw.special_type,
            track_ids: raw.track_ids.iter().map(|t| t.id as u64).collect(),
        }
    }
}

impl Song {
    pub(crate) fn from_api(raw: &ApiTrack) -> Self {
        let publish = raw.publish_time.filter(|v| *v > 0);
        Song {
            id: raw.id as u64,
            name: raw.name.clone(),
            artists: raw.ar.iter().map(|a| a.name.clone()).collect(),
            album: raw.al.as_ref().and_then(|a| a.name.clone()),
            duration_ms: raw.dt.max(0) as u64,
            publish_time_ms: publish,
            fee: raw.fee,
            popularity: raw.pop,
            alias: raw.alia.clone(),
            has_mv: raw.mv > 0,
            available: true,
        }
    }

    /// 用已知 id 构造一个「已失效/查不到详情」的占位歌曲。
    pub(crate) fn unavailable(id: u64) -> Self {
        Song {
            id,
            name: "（已下架或不可见）".to_string(),
            artists: Vec::new(),
            album: None,
            duration_ms: 0,
            publish_time_ms: None,
            fee: 0,
            popularity: None,
            alias: Vec::new(),
            has_mv: false,
            available: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn year_conversion() {
        // 2024-01-01 00:00:00 UTC
        assert_eq!(chrono_like_year(1_704_067_200), 2024);
        // 1970-01-01
        assert_eq!(chrono_like_year(0), 1970);
        // 2000-03-01
        assert_eq!(chrono_like_year(951_782_400), 2000);
    }

    #[test]
    fn dedup_key_lowercases() {
        let mut s = Song::unavailable(1);
        s.name = "Hello".into();
        s.artists = vec!["A".into()];
        assert_eq!(s.dedup_key(), ("a".to_string(), "hello".to_string()));
    }
}
