//! 归一化领域模型。
//!
//! `ncm-api-rs` 返回原始 JSON（`serde_json::Value`），本模块负责把歌单 / 歌曲
//! 载荷转换为后续「本地筛选、整理、重建歌单」流程直接使用的强类型模型，
//! 也是本地 JSON 缓存的数据格式。**这里不包含任何网络请求或加密逻辑。**

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// 从歌曲详情 JSON（`/api/v3/song/detail` 响应中 `songs[]` 的某一项）构建。
    pub fn from_value(v: &Value) -> Song {
        let publish = {
            let t = get_i64(v, "publishTime");
            (t > 0).then_some(t)
        };
        Song {
            id: get_u64(v, "id"),
            name: get_str(v, "name"),
            artists: v["ar"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|a| a["name"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            album: v["al"]["name"].as_str().map(str::to_string),
            duration_ms: get_u64(v, "dt"),
            publish_time_ms: publish,
            fee: get_i64(v, "fee"),
            popularity: v["pop"].as_f64(),
            alias: v["alia"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|a| a.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            has_mv: get_i64(v, "mv") > 0,
            available: true,
        }
    }

    /// 用已知 id 构造一个「已失效/查不到详情」的占位歌曲。
    pub fn unavailable(id: u64) -> Song {
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

    /// 发行年份（未知返回 None）。
    pub fn publish_year(&self) -> Option<i32> {
        let ms = self.publish_time_ms?;
        if ms <= 0 {
            return None;
        }
        let days = (ms / 1000).div_euclid(86_400);
        let (y, _, _) = civil_from_days(days);
        Some(y)
    }
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

impl PlaylistInfo {
    /// 从歌单详情 JSON（`/api/v6/playlist/detail` 响应中 `playlist` 字段）构建。
    pub fn from_value(v: &Value) -> PlaylistInfo {
        let track_ids = v["trackIds"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t["id"].as_i64())
                    .map(|id| id as u64)
                    .collect()
            })
            .unwrap_or_default();

        let mut info = PlaylistInfo {
            id: get_u64(v, "id"),
            name: get_str(v, "name"),
            description: v["description"].as_str().map(str::to_string),
            track_count: get_i64(v, "trackCount").max(0) as usize,
            play_count: v["playCount"].as_i64().filter(|c| *c >= 0).map(|c| c as u64),
            tags: v["tags"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| t.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            cover_url: v["coverImgUrl"].as_str().map(str::to_string),
            creator_name: v["creator"]["nickname"].as_str().map(str::to_string),
            creator_id: v["creator"]["userId"].as_i64().map(|id| id as u64),
            special_type: get_i64(v, "specialType"),
            track_ids,
        };
        // 正常情况下会返回全部 trackIds；极端情况下兜底用响应内嵌 tracks。
        if info.track_ids.is_empty() {
            info.track_ids = v["tracks"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| t["id"].as_i64())
                        .map(|id| id as u64)
                        .collect()
                })
                .unwrap_or_default();
        }
        info
    }
}

/// 一次「抓取 + 整理」产出的本地缓存结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistDump {
    pub meta: PlaylistInfo,
    pub songs: Vec<Song>,
    pub fetched_at_ms: u64,
}

// ---------------------------------------------------------------------------
// JSON 取值小助手（对缺失字段给出安全默认值）
// ---------------------------------------------------------------------------

fn get_u64(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_i64()).map(|x| x.max(0) as u64).unwrap_or(0)
}

fn get_i64(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(0)
}

fn get_str(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn year_conversion() {
        // 1970-01-01
        assert_eq!(civil_from_days(0).0, 1970);
        // 2024-01-01 00:00:00 UTC = 1704067200s / 86400 = 19723 天
        assert_eq!(civil_from_days(19_723).0, 2024);
    }

    #[test]
    fn song_from_value() {
        let v = json!({
            "id": 3399839173_i64,
            "name": "甲乙丙丁",
            "ar": [{"id": 1, "name": "李佳薇"}],
            "al": {"id": 9, "name": "某专辑"},
            "alia": ["副标题"],
            "dt": 210461,
            "fee": 1,
            "mv": 123,
            "pop": 88.5,
            "publishTime": 1704067200000_i64
        });
        let s = Song::from_value(&v);
        assert_eq!(s.id, 3399839173);
        assert_eq!(s.name, "甲乙丙丁");
        assert_eq!(s.artists, vec!["李佳薇".to_string()]);
        assert_eq!(s.album.as_deref(), Some("某专辑"));
        assert_eq!(s.duration_ms, 210461);
        assert_eq!(s.fee, 1);
        assert!(s.has_mv);
        assert!(s.available);
        assert_eq!(s.publish_year(), Some(2024));
    }

    #[test]
    fn song_from_value_missing_fields_is_safe() {
        let s = Song::from_value(&json!({"id": 5}));
        assert_eq!(s.id, 5);
        assert_eq!(s.name, "");
        assert!(s.artists.is_empty());
        assert_eq!(s.duration_ms, 0);
        assert_eq!(s.fee, 0);
        assert_eq!(s.publish_year(), None);
    }

    #[test]
    fn playlist_from_value_reads_track_ids() {
        let v = json!({
            "id": 3778678,
            "name": "热歌榜",
            "description": "desc",
            "trackCount": 200,
            "playCount": 1411376_i64,
            "tags": [],
            "creator": {"nickname": "网易云音乐", "userId": 1},
            "specialType": 10,
            "trackIds": [{"id": 1}, {"id": 2}],
            "tracks": []
        });
        let p = PlaylistInfo::from_value(&v);
        assert_eq!(p.id, 3778678);
        assert_eq!(p.name, "热歌榜");
        assert_eq!(p.track_count, 200);
        assert_eq!(p.creator_name.as_deref(), Some("网易云音乐"));
        assert_eq!(p.track_ids, vec![1, 2]);
    }

    #[test]
    fn dedup_key_lowercases() {
        let mut s = Song::unavailable(1);
        s.name = "Hello".into();
        s.artists = vec!["A".into()];
        assert_eq!(s.dedup_key(), ("a".to_string(), "hello".to_string()));
        assert!(!s.available);
    }
}
