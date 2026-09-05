//! 网易云音乐 API 对接层（基于 `ncm-api-rs` crate 封装）。
//!
//! 底层 HTTP/加密（weapi/eapi/linuxapi）、Cookie 注入、登录与歌单读写等都由
//! [`ncm_api_rs`]（SPlayer-Dev，NeteaseCloudMusicApi Enhanced 的 Rust 移植）
//! 负责。本模块在上层提供：
//! - 与业务模型（`types`）匹配的强类型方法；
//! - 登录态 Cookie 的本地持久化（`data/cookies.txt`，关键为 `MUSIC_U`）；
//! - 歌单链接 / ID / 短链的解析。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ncm_api_rs::{create_client, ApiClient as RawClient, ApiResponse, Query};

use super::error::{Error, Result};
use super::types::{
    AccountInfo, ApiPlaylistDetail, ApiSongDetail, PlaylistInfo, Song,
};

const WEB_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                      (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
/// 批量取歌曲详情时每个请求的曲目数。
const SONG_BATCH: usize = 500;
/// 添加/删除歌曲时每个请求的曲目数。
const ADD_BATCH: usize = 300;

/// 一个登录二维码会话。
#[derive(Debug, Clone)]
pub struct QrLogin {
    pub unikey: String,
    /// 可渲染为二维码的登录地址。
    pub url: String,
}

/// 二维码轮询结果。
#[derive(Debug)]
pub enum QrPollStatus {
    /// 扫码并确认成功，附带账号信息
    Success(AccountInfo),
    /// 等待扫码
    Waiting,
    /// 已扫码，等待手机端确认
    Scanned,
    /// 二维码过期
    Expired,
    /// 超时
    Timeout,
    /// 其它错误
    Error { code: i64, message: String },
}

/// 添加歌曲的结果统计。
#[derive(Debug, Clone, Default)]
pub struct TrackAddReport {
    pub added: usize,
    /// 因已在歌单中而跳过的数量
    pub skipped: usize,
}

/// 网易云音乐 API 客户端（薄封装 `ncm_api_rs::ApiClient`）。
pub struct NcmClient {
    raw: RawClient,
    /// 仅用于解析分享短链的重定向（普通读取都走 crate）。
    resolver: reqwest::Client,
    cookies: HashMap<String, String>,
    cookie_file: Option<PathBuf>,
    /// 反风控用 `X-Real-IP`（机房/海外 IP 建议填国内住宅 IP）。
    real_ip: Option<String>,
}

impl NcmClient {
    pub fn new() -> Result<Self> {
        let raw = create_client(None);
        let resolver = reqwest::Client::builder()
            .user_agent(WEB_UA)
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            raw,
            resolver,
            cookies: HashMap::new(),
            cookie_file: None,
            real_ip: None,
        })
    }

    /// 设置 `X-Real-IP` 请求头以规避风控（国内住宅 IP 效果最好）。
    pub fn set_real_ip(&mut self, ip: impl Into<String>) {
        self.real_ip = Some(ip.into());
    }

    // ------------------------------------------------------------------
    // Cookie 管理（登录态持久化）
    // ------------------------------------------------------------------

    /// 从本地文件加载登录 Cookie（内容为 `k=v; k2=v2` 形式）。
    pub fn load_cookies(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            self.cookies.extend(parse_cookie_str(&content));
        }
        self.cookie_file = Some(path.to_path_buf());
        Ok(())
    }

    /// 把当前 Cookie 写回 cookie_file。
    pub fn save_cookies(&self) -> Result<()> {
        if let Some(path) = &self.cookie_file {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, self.cookie_header())?;
        }
        Ok(())
    }

    /// 当前 Cookie 头字符串（可能为空）。
    pub fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// 是否存在 MUSIC_U 登录凭据（不代表一定仍有效）。
    pub fn has_login_cookie(&self) -> bool {
        self.cookies.contains_key("MUSIC_U")
    }

    /// 构造带当前 Cookie / real_ip 的基础查询参数。
    fn base_query(&self) -> Query {
        let mut q = Query::new();
        let cookies = self.cookie_header();
        if !cookies.is_empty() {
            q = q.cookie(&cookies);
        }
        q.real_ip = self.real_ip.clone();
        q
    }

    /// 把一次响应的 Set-Cookie 合并进本地 Cookie 表。
    fn capture_cookies(&mut self, r: &ApiResponse) {
        for header in &r.cookie {
            for (k, v) in parse_cookie_str(header) {
                if !is_cookie_attribute(&k) {
                    self.cookies.insert(k, v);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // 歌单读取（公开歌单无需登录）
    // ------------------------------------------------------------------

    /// 从「链接 / 分享短链 / 纯数字 ID」解析出歌单 ID。
    pub async fn resolve_playlist_input(&self, input: &str) -> Result<u64> {
        let s = input.trim();
        if s.parse::<u64>().is_ok() {
            return parse_playlist_id(s);
        }
        if s.starts_with("http://") || s.starts_with("https://") {
            if let Ok(id) = parse_playlist_id(s) {
                return Ok(id);
            }
            // 短链等：跟随重定向后从最终 URL 解析
            let resp = self.resolver.get(s).send().await?;
            let final_url = resp.url().to_string();
            return parse_playlist_id(&final_url);
        }
        parse_playlist_id(s)
    }

    /// 获取歌单元数据与全部曲目 ID。
    pub async fn playlist_meta(&self, id: u64) -> Result<PlaylistInfo> {
        let q = self.base_query().param("id", &id.to_string());
        let r = self.raw.playlist_detail(&q).await?;
        let detail: ApiPlaylistDetail = serde_json::from_value(r.body)?;
        if detail.code != 200 {
            return Err(Error::Api {
                code: detail.code,
                message: "获取歌单详情失败".into(),
            });
        }
        let raw_pl = detail.playlist.ok_or_else(|| {
            Error::Msg("歌单不存在、已被删除或为私密不可见".to_string())
        })?;
        let mut info = PlaylistInfo::from_api(&raw_pl);
        // 正常情况下会返回全部 trackIds；极端情况下兜底用响应内嵌 tracks。
        if info.track_ids.is_empty() {
            info.track_ids = raw_pl.tracks.iter().map(|t| t.id as u64).collect();
        }
        Ok(info)
    }

    /// 批量获取歌曲详情（自动按 500/批 分页；已下架的会被标记 available=false）。
    pub async fn song_details(&self, ids: &[u64]) -> Result<Vec<Song>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(SONG_BATCH) {
            let joined = chunk
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let q = self.base_query().param("ids", &joined);
            let r = self.raw.song_detail(&q).await?;
            let detail: ApiSongDetail = serde_json::from_value(r.body)?;
            let mut found: HashMap<u64, Song> = HashMap::new();
            for t in &detail.songs {
                found.insert(t.id as u64, Song::from_api(t));
            }
            for id in chunk {
                if let Some(song) = found.remove(id) {
                    out.push(song);
                } else {
                    out.push(Song::unavailable(*id));
                }
            }
        }
        Ok(out)
    }

    /// 一步完成：输入链接/ID → 歌单元数据 + 全部歌曲。
    pub async fn fetch_playlist(&self, input: &str) -> Result<(PlaylistInfo, Vec<Song>)> {
        let id = self.resolve_playlist_input(input).await?;
        let meta = self.playlist_meta(id).await?;
        let songs = self.song_details(&meta.track_ids).await?;
        Ok((meta, songs))
    }

    // ------------------------------------------------------------------
    // 登录（二维码）
    // ------------------------------------------------------------------

    /// 发起二维码登录：返回用于生成二维码的 URL 及后续轮询用的 unikey。
    pub async fn qr_login_begin(&mut self) -> Result<QrLogin> {
        let q = self.base_query();
        let r = self.raw.login_qr_key(&q).await?;
        self.capture_cookies(&r);
        let unikey = r.body["data"]["unikey"]
            .as_str()
            .or_else(|| r.body["unikey"].as_str())
            .ok_or_else(|| Error::Msg("登录接口未返回 unikey".to_string()))?
            .to_string();

        // 生成扫码 URL（与 Node 版一致）
        let q2 = self.base_query().param("key", &unikey);
        let r2 = self.raw.login_qr_create(&q2).await?;
        let url = r2.body["data"]["qrurl"]
            .as_str()
            .unwrap_or(&format!("https://music.163.com/login?codekey={unikey}"))
            .to_string();
        Ok(QrLogin { unikey, url })
    }

    /// 轮询二维码状态，直到成功 / 过期 / 超时。
    ///
    /// 轮询间隔约 1.5s。成功后自动保存 Cookie 并返回账号信息。
    pub async fn qr_login_poll(
        &mut self,
        unikey: &str,
        timeout: Duration,
    ) -> Result<QrPollStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            let q = self.base_query().param("key", unikey);
            let r = self.raw.login_qr_check(&q).await?;
            // 803 成功时的 MUSIC_U / __csrf 等登录 Cookie 在这里返回
            self.capture_cookies(&r);
            let code = r.body["code"].as_i64().unwrap_or(0);
            let message = r.body["message"]
                .as_str()
                .or_else(|| r.body["msg"].as_str())
                .unwrap_or("")
                .to_string();
            match code {
                803 => {
                    self.save_cookies()?;
                    let info = self.account_info().await?;
                    return Ok(QrPollStatus::Success(info));
                }
                // 802 已扫码待确认；801 等待扫码 —— 继续轮询
                802 | 801 => {}
                800 => return Ok(QrPollStatus::Expired),
                other => return Ok(QrPollStatus::Error { code: other, message }),
            }
            if Instant::now() >= deadline {
                return Ok(QrPollStatus::Timeout);
            }
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
    }

    /// 查询当前登录状态（凭 Cookie），未登录返回 Err。
    pub async fn account_info(&self) -> Result<AccountInfo> {
        let q = self.base_query();
        let r = self.raw.user_account(&q).await?;
        let nickname = r.body["profile"]["nickname"]
            .as_str()
            .unwrap_or("未知用户")
            .to_string();
        let user_id = r.body["profile"]["userId"].as_u64().unwrap_or(0);
        if user_id == 0 {
            return Err(Error::NotLoggedIn);
        }
        Ok(AccountInfo { user_id, nickname })
    }

    /// 确保已登录：有 Cookie 则校验；否则引导二维码登录。
    pub async fn ensure_logged_in(&mut self) -> Result<AccountInfo> {
        // Cookie 有效则直接返回；失效则落入下方扫码登录。
        if self.has_login_cookie()
            && let Ok(info) = self.account_info().await
        {
            return Ok(info);
        }
        let qr = self.qr_login_begin().await?;
        println!("请使用网易云音乐 App 扫码登录（90 秒内有效）：");
        println!("  {}\n", qr.url);
        match self
            .qr_login_poll(&qr.unikey, Duration::from_secs(90))
            .await?
        {
            QrPollStatus::Success(info) => Ok(info),
            QrPollStatus::Expired => Err(Error::msg("二维码已过期，请重试")),
            QrPollStatus::Timeout => Err(Error::msg("等待扫码超时")),
            QrPollStatus::Error { code, message } => Err(Error::Api { code, message }),
            QrPollStatus::Waiting | QrPollStatus::Scanned => {
                Err(Error::msg("扫码流程未完成"))
            }
        }
    }

    // ------------------------------------------------------------------
    // 歌单写操作（需要登录）
    // ------------------------------------------------------------------

    /// 创建歌单，返回新歌单 ID。`privacy=true` 时创建隐私歌单。
    pub async fn create_playlist(&mut self, name: &str, privacy: bool) -> Result<u64> {
        let privacy = if privacy { "10" } else { "0" };
        let q = self
            .base_query()
            .param("name", name)
            .param("privacy", privacy);
        let r = self.raw.playlist_create(&q).await?;
        self.capture_cookies(&r);
        r.body["playlist"]["id"]
            .as_u64()
            .ok_or_else(|| Error::Msg("创建歌单响应缺少 playlist.id".to_string()))
    }

    /// 向歌单批量添加歌曲（每批 300 首）。已在歌单中的会跳过并计数。
    pub async fn add_tracks(&mut self, playlist_id: u64, song_ids: &[u64]) -> Result<TrackAddReport> {
        let mut report = TrackAddReport::default();
        for chunk in song_ids.chunks(ADD_BATCH) {
            let ids = chunk
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let q = self
                .base_query()
                .param("pid", &playlist_id.to_string())
                .param("ids", &ids);
            let r = self.raw.playlist_track_add(&q).await?;
            self.capture_cookies(&r);
            match r.body["code"].as_i64().unwrap_or(0) {
                200 => report.added += chunk.len(),
                // 歌曲已在歌单中
                502 => report.skipped += chunk.len(),
                other => {
                    return Err(Error::Api {
                        code: other,
                        message: r.body["message"]
                            .as_str()
                            .unwrap_or("")
                            .to_string(),
                    })
                }
            }
        }
        Ok(report)
    }

    /// 从歌单批量删除歌曲。
    pub async fn delete_tracks(&mut self, playlist_id: u64, song_ids: &[u64]) -> Result<usize> {
        let mut removed = 0usize;
        for chunk in song_ids.chunks(ADD_BATCH) {
            let ids = chunk
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let q = self
                .base_query()
                .param("id", &playlist_id.to_string())
                .param("ids", &ids);
            let r = self.raw.playlist_track_delete(&q).await?;
            self.capture_cookies(&r);
            let code = r.body["code"].as_i64().unwrap_or(0);
            if code != 200 {
                return Err(Error::Api {
                    code,
                    message: r.body["message"].as_str().unwrap_or("").to_string(),
                });
            }
            removed += chunk.len();
        }
        Ok(removed)
    }
}

impl From<ncm_api_rs::NcmError> for Error {
    fn from(e: ncm_api_rs::NcmError) -> Self {
        match e {
            ncm_api_rs::NcmError::AuthRequired(_) => Error::NotLoggedIn,
            ncm_api_rs::NcmError::Api { code, msg } => Error::Api {
                code,
                message: msg,
            },
            ncm_api_rs::NcmError::Http(e) => Error::Http(e),
            other => Error::Msg(other.to_string()),
        }
    }
}

/// 歌单分享页地址。
pub fn playlist_url(id: u64) -> String {
    format!("https://music.163.com/#/playlist?id={id}")
}

/// 从歌单链接或纯 ID 字符串中解析出歌单 ID（同步版本）。
pub fn parse_playlist_id(input: &str) -> Result<u64> {
    let s = input.trim();
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }
    // 形如 ...?id=123&userid=456
    if let Some(q) = s.split_once('?').map(|(_, q)| q) {
        for pair in q.split('&') {
            if let Some(v) = pair.strip_prefix("id=")
                && let Ok(n) = v.trim().parse::<u64>()
            {
                return Ok(n);
            }
        }
    }
    // 形如 /playlist/123
    if let Some(idx) = s.find("/playlist/") {
        let rest = &s[idx + "/playlist/".len()..];
        let digits: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(n) = digits.parse::<u64>() {
            return Ok(n);
        }
    }
    Err(Error::BadInput {
        input: s.to_string(),
    })
}

/// 解析 `k=v; k2=v2` 形式的 cookie 字符串。
fn parse_cookie_str(s: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for part in s.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(idx) = part.find('=') {
            let k = part[..idx].trim().to_string();
            let v = part[idx + 1..].trim().to_string();
            if !k.is_empty() {
                map.insert(k, v);
            }
        }
    }
    map
}

/// 判断某个 cookie 名是否为 Set-Cookie 的属性（而非真正的 cookie）。
fn is_cookie_attribute(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "path" | "domain" | "expires" | "max-age" | "secure" | "httponly"
            | "samesite" | "version" | "comment" | "discard" | "port" | "priority"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_id() {
        assert_eq!(parse_playlist_id("3778678").unwrap(), 3778678);
    }

    #[test]
    fn parse_web_url() {
        assert_eq!(
            parse_playlist_id("https://music.163.com/#/playlist?id=3778678&userid=1").unwrap(),
            3778678
        );
        assert_eq!(
            parse_playlist_id("https://music.163.com/playlist?id=3778678").unwrap(),
            3778678
        );
        assert_eq!(
            parse_playlist_id("https://y.music.163.com/m/playlist?id=3778678").unwrap(),
            3778678
        );
    }

    #[test]
    fn parse_path_url() {
        assert_eq!(
            parse_playlist_id("https://music.163.com/playlist/3778678/").unwrap(),
            3778678
        );
    }

    #[test]
    fn reject_garbage() {
        assert!(parse_playlist_id("hello-world").is_err());
    }

    #[test]
    fn cookie_parsing() {
        let m = parse_cookie_str("MUSIC_U=abc; Path=/; __csrf=12345");
        assert_eq!(m.get("MUSIC_U").map(String::as_str), Some("abc"));
        assert_eq!(m.get("__csrf").map(String::as_str), Some("12345"));
        assert!(is_cookie_attribute("Path"));
        assert!(!is_cookie_attribute("MUSIC_U"));
    }
}
