//! 本地「播放器式」筛选规则。
//!
//! 相当于本地播放器里的条件过滤：最短时长、是否仅 VIP、关键词、是否保留
//! 已下架歌曲、去重等。当前为最小可用实现，后续 GUI 阶段可在此之上组合成
//! 更复杂的规则集（AND/OR、按歌手/年代/专辑分组等）。

use std::collections::HashSet;

use super::types::Song;

/// 一组可叠加的筛选规则。
#[derive(Debug, Clone, Default)]
pub struct SongFilter {
    /// 丢弃已下架/查不到详情的歌曲
    pub drop_unavailable: bool,
    /// 丢弃仅限 VIP 的歌曲
    pub drop_vip_only: bool,
    /// 最短时长（秒）
    pub min_seconds: Option<u64>,
    /// 关键词（匹配歌名或歌手，大小写不敏感）
    pub keyword: Option<String>,
    /// 按 (歌手, 歌名) 去重，保留首次出现
    pub dedupe: bool,
}

impl SongFilter {
    pub fn apply(&self, songs: &[Song]) -> Vec<Song> {
        let mut out = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for song in songs {
            if self.drop_unavailable && !song.available {
                continue;
            }
            if self.drop_vip_only && song.is_vip_only() {
                continue;
            }
            if let Some(min) = self.min_seconds
                && song.duration_ms / 1000 < min
            {
                continue;
            }
            if let Some(kw) = &self.keyword {
                let kw = kw.to_lowercase();
                let haystack = format!(
                    "{} {} {}",
                    song.name,
                    song.artists.join(" "),
                    song.album.clone().unwrap_or_default()
                )
                .to_lowercase();
                if !haystack.contains(&kw) {
                    continue;
                }
            }
            if self.dedupe && !seen.insert(song.dedup_key()) {
                continue;
            }
            out.push(song.clone());
        }
        out
    }
}

/// 仅按歌曲 ID 去重（推送新歌单前使用：同一首歌只加一次）。
pub fn dedupe_by_id(songs: &[Song]) -> Vec<Song> {
    let mut seen = HashSet::new();
    songs
        .iter()
        .filter(|s| seen.insert(s.id))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: u64, name: &str, artist: &str, secs: u64, fee: i64) -> Song {
        Song {
            id,
            name: name.to_string(),
            artists: vec![artist.to_string()],
            album: None,
            duration_ms: secs * 1000,
            publish_time_ms: None,
            fee,
            popularity: None,
            alias: Vec::new(),
            has_mv: false,
            available: true,
        }
    }

    #[test]
    fn dedupe_keeps_first() {
        let songs = vec![song(1, "A", "X", 10, 0), song(2, "A", "X", 20, 0)];
        let kept = SongFilter { dedupe: true, ..Default::default() }.apply(&songs);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, 1);
    }

    #[test]
    fn filters_combine() {
        let songs = vec![
            song(1, "short", "X", 5, 0),
            song(2, "vip", "X", 60, 1),
            song(3, "ok", "Y", 60, 0),
        ];
        let kept = SongFilter {
            min_seconds: Some(30),
            drop_vip_only: true,
            ..Default::default()
        }
        .apply(&songs);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, 3);
    }
}
