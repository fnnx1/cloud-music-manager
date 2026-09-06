//! egui 图形界面：拉取歌单 → 本地副本 → 组合筛选 → 当前列表 → 登录 → 创建歌单。
//!
//! 网络操作在独立后台线程的 tokio runtime 里串行执行，通过通道与 UI 通信，
//! 保证界面不卡顿。登录态 Cookie 由本程序独立存储于 `data/cookies.txt`。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use cloud_music_manager::model::{PlaylistDump, PlaylistInfo, Song};
use eframe::egui;
use egui_extras::{Column, TableBuilder};
use ncm_api_rs::{create_client, ApiClient, ApiResponse, Query};

const COOKIE_FILE: &str = "data/cookies.txt";
const DATA_DIR: &str = "data";
const SONG_BATCH: usize = 500;
const ADD_BATCH: usize = 300;

// ---- 视觉调色板（深色 · 网易红点缀）----
const ACCENT: egui::Color32 = egui::Color32::from_rgb(236, 65, 65);
const SIDEBAR_BG: egui::Color32 = egui::Color32::from_rgb(24, 25, 28);
const CONTENT_BG: egui::Color32 = egui::Color32::from_rgb(12, 13, 15);
const CARD_BG: egui::Color32 = egui::Color32::from_rgb(29, 31, 35);
const INPUT_BG: egui::Color32 = egui::Color32::from_rgb(19, 20, 23);
const BORDER: egui::Color32 = egui::Color32::from_rgb(52, 55, 61);
const TEXT_MAIN: egui::Color32 = egui::Color32::from_rgb(226, 228, 232);
const TEXT_WEAK: egui::Color32 = egui::Color32::from_rgb(140, 145, 153);

// ---------------------------------------------------------------------------
// 后台消息
// ---------------------------------------------------------------------------

enum Cmd {
    Fetch { input: String },
    CheckLogin,
    SendCode { phone: String },
    LoginPhone { phone: String, code: String },
    Create { name: String, ids: Vec<u64> },
    Cover { url: String },
}

#[derive(Clone, Debug)]
struct UiAccount {
    uid: u64,
    nickname: String,
}

#[allow(clippy::large_enum_variant)]
enum Ev {
    Fetched {
        meta: PlaylistInfo,
        songs: Vec<Song>,
        cover_url: Option<String>,
    },
    Login(UiAccount),
    NotLogged,
    CodeSent { phone: String },
    Created { id: u64, name: String },
    Cover(Vec<u8>),
    Info(String),
    Error(String),
}

// ---------------------------------------------------------------------------
// 领域层：Cookie 工具（与 CLI 一致的小工具，独立维护登录态）
// ---------------------------------------------------------------------------

fn parse_cookie_str(s: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for part in s.split(';') {
        let part = part.trim();
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

fn is_cookie_attribute(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "path" | "domain" | "expires" | "max-age" | "secure" | "httponly"
            | "samesite" | "version" | "comment" | "discard" | "port" | "priority"
    )
}

fn cookie_str(map: &HashMap<String, String>) -> String {
    map.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn load_cookie_map() -> HashMap<String, String> {
    let path = PathBuf::from(COOKIE_FILE);
    if path.exists()
        && let Ok(content) = std::fs::read_to_string(&path)
    {
        return parse_cookie_str(&content);
    }
    HashMap::new()
}

fn save_cookie_map(map: &HashMap<String, String>) -> Result<()> {
    let path = PathBuf::from(COOKIE_FILE);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, cookie_str(map))?;
    Ok(())
}

fn sync_cookie(client: &mut ApiClient, cookies: &HashMap<String, String>) {
    let s = cookie_str(cookies);
    if !s.is_empty() {
        client.set_cookie(s);
    }
}

fn capture_cookies(map: &mut HashMap<String, String>, r: &ApiResponse) {
    for header in &r.cookie {
        for (k, v) in parse_cookie_str(header) {
            if !is_cookie_attribute(&k) {
                map.insert(k, v);
            }
        }
    }
}

fn ensure_music_u_from_body(cookies: &mut HashMap<String, String>, r: &ApiResponse) {
    if cookies.contains_key("MUSIC_U") {
        return;
    }
    if let Some(tok) = r.body.get("token").and_then(|t| t.as_str())
        && !tok.is_empty()
    {
        cookies.insert("MUSIC_U".to_string(), tok.to_string());
    }
}

fn client_with_cookie(map: &HashMap<String, String>) -> ApiClient {
    let mut client = create_client(None);
    sync_cookie(&mut client, map);
    client
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 从「链接 / 纯 ID」解析歌单 ID。
fn parse_playlist_id(input: &str) -> Result<u64> {
    let s = input.trim();
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }
    if let Some(q) = s.split_once('?').map(|(_, q)| q) {
        for pair in q.split('&') {
            if let Some(v) = pair.strip_prefix("id=")
                && let Ok(n) = v.trim().parse::<u64>()
            {
                return Ok(n);
            }
        }
    }
    if let Some(idx) = s.find("/playlist/") {
        let digits: String = s[idx + "/playlist/".len()..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(n) = digits.parse::<u64>() {
            return Ok(n);
        }
    }
    Err(anyhow!("无法从输入解析出歌单 ID：{s}"))
}

fn format_duration(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

fn parse_year(ms: Option<i64>) -> String {
    match ms {
        Some(t) if t > 0 => {
            let days = (t / 1000).div_euclid(86_400);
            let y = days_to_year(days);
            y.to_string()
        }
        _ => "-".to_string(),
    }
}

fn days_to_year(days: i64) -> i32 {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }) as i32
}

// ---------------------------------------------------------------------------
// 后台操作（非交互，直接调用 crate）
// ---------------------------------------------------------------------------

async fn op_fetch(input: &str) -> Result<(PlaylistInfo, Vec<Song>)> {
    let id = parse_playlist_id(input)?;
    let client = create_client(None);
    let r = client
        .playlist_detail(&Query::new().param("id", &id.to_string()))
        .await?;
    let body = r.body;
    if body["code"].as_i64().unwrap_or(-1) != 200 || body["playlist"].is_null() {
        bail!("歌单不存在、已被删除或为私密不可见");
    }
    let meta = PlaylistInfo::from_value(&body["playlist"]);
    let songs = op_song_details(&client, &meta.track_ids).await?;
    Ok((meta, songs))
}

async fn op_song_details(client: &ApiClient, ids: &[u64]) -> Result<Vec<Song>> {
    let mut out = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(SONG_BATCH) {
        let joined = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        let r = client
            .song_detail(&Query::new().param("ids", &joined))
            .await?;
        let songs = r.body["songs"].as_array().cloned().unwrap_or_default();
        let mut by_id: HashMap<u64, Song> = HashMap::new();
        for v in &songs {
            let s = Song::from_value(v);
            by_id.insert(s.id, s);
        }
        for id in chunk {
            if let Some(song) = by_id.remove(id) {
                out.push(song);
            } else {
                out.push(Song::unavailable(*id));
            }
        }
    }
    Ok(out)
}

async fn op_check_login() -> Result<Option<UiAccount>> {
    let map = load_cookie_map();
    if !map.contains_key("MUSIC_U") {
        return Ok(None);
    }
    let client = client_with_cookie(&map);
    let r = client.user_account(&Query::new()).await?;
    let uid = r.body["profile"]["userId"].as_u64().unwrap_or(0);
    if uid == 0 {
        return Ok(None);
    }
    Ok(Some(UiAccount {
        uid,
        nickname: r.body["profile"]["nickname"]
            .as_str()
            .unwrap_or("未知用户")
            .to_string(),
    }))
}

async fn op_send_code(phone: &str) -> Result<()> {
    let client = create_client(None);
    let r = client
        .captcha_sent(&Query::new().param("phone", phone))
        .await?;
    let code = r.body["code"].as_i64().unwrap_or(-1);
    if code != 200 {
        bail!(
            "发送验证码失败 code={code}: {}",
            r.body["message"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

/// 验证码登录（按 Node 原版只发 captcha、不发 password）。
async fn op_login_phone(phone: &str, code: &str) -> Result<UiAccount> {
    let client = create_client(None);
    let data = serde_json::json!({
        "type": "1",
        "https": "true",
        "phone": phone,
        "countrycode": "86",
        "captcha": code,
        "remember": "true",
        "secureCaptcha": "",
    });
    let option = ncm_api_rs::RequestOption {
        crypto: ncm_api_rs::CryptoType::Weapi,
        ..Default::default()
    };
    let r = client
        .request("/api/w/login/cellphone", data, option)
        .await?;
    let biz = r.body["code"].as_i64().unwrap_or(-1);
    if biz != 200 {
        bail!("登录失败 code={biz}: {}", r.body["message"].as_str().unwrap_or(""));
    }

    let mut map = HashMap::new();
    capture_cookies(&mut map, &r);
    ensure_music_u_from_body(&mut map, &r);
    if !map.contains_key("MUSIC_U") {
        bail!("登录成功但未获得会话 Cookie，请重试");
    }
    let client = client_with_cookie(&map);
    let acc = account_from_client(&client)?;
    save_cookie_map(&map)?;
    Ok(acc)
}

fn account_from_client(client: &ApiClient) -> Result<UiAccount> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let r = rt.block_on(client.user_account(&Query::new()))?;
    let uid = r.body["profile"]["userId"].as_u64().unwrap_or(0);
    if uid == 0 {
        bail!("未登录（Cookie 无效或缺失）");
    }
    Ok(UiAccount {
        uid,
        nickname: r.body["profile"]["nickname"]
            .as_str()
            .unwrap_or("未知用户")
            .to_string(),
    })
}

async fn op_create(name: &str, ids: &[u64]) -> Result<Option<u64>> {
    let map = load_cookie_map();
    if !map.contains_key("MUSIC_U") {
        return Ok(None); // 需要登录
    }
    let client = client_with_cookie(&map);
    let r = client.user_account(&Query::new()).await;
    if r.is_err() {
        return Ok(None);
    }
    let p = "0";
    let r = client
        .playlist_create(&Query::new().param("name", name).param("privacy", p))
        .await?;
    let new_id = r.body["playlist"]["id"]
        .as_u64()
        .ok_or_else(|| anyhow!("创建歌单响应缺少 playlist.id"))?;

    // 批量加入（manipulate/tracks）
    for chunk in ids.chunks(ADD_BATCH) {
        let joined = chunk
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let q = Query::new()
            .param("op", "add")
            .param("pid", &new_id.to_string())
            .param("tracks", &joined);
        let r = client.playlist_tracks(&q).await?;
        let code = r.body["code"].as_i64().unwrap_or(0);
        if code != 200 {
            bail!("添加歌曲失败 code={code}: {}", r.body["message"]);
        }
    }
    Ok(Some(new_id))
}

async fn op_cover(url: &str) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    Ok(client.get(url).send().await?.bytes().await?.to_vec())
}

// ---------------------------------------------------------------------------
// Worker：串行执行后台操作
// ---------------------------------------------------------------------------

fn spawn_worker(ev_tx: Sender<Ev>) -> Sender<Cmd> {
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("创建后台 runtime 失败");
        while let Ok(cmd) = cmd_rx.recv() {
            let out = rt.block_on(async {
                match cmd {
                    Cmd::Fetch { input } => match op_fetch(&input).await {
                        Ok((meta, songs)) => {
                            let cover_url = meta.cover_url.clone();
                            Ev::Fetched {
                                meta,
                                songs,
                                cover_url,
                            }
                        }
                        Err(e) => Ev::Error(format!("拉取失败: {e}")),
                    },
                    Cmd::CheckLogin => match op_check_login().await {
                        Ok(Some(acc)) => Ev::Login(acc),
                        Ok(None) => Ev::NotLogged,
                        Err(e) => Ev::Error(format!("登录状态检查失败: {e}")),
                    },
                    Cmd::SendCode { phone } => match op_send_code(&phone).await {
                        Ok(()) => Ev::CodeSent { phone },
                        Err(e) => Ev::Error(format!("发送验证码失败: {e}")),
                    },
                    Cmd::LoginPhone { phone, code } => match op_login_phone(&phone, &code).await {
                        Ok(acc) => Ev::Login(acc),
                        Err(e) => Ev::Error(format!("登录失败: {e}")),
                    },
                    Cmd::Create { name, ids } => match op_create(&name, &ids).await {
                        Ok(Some(id)) => Ev::Created { id, name },
                        Ok(None) => Ev::NotLogged,
                        Err(e) => Ev::Error(format!("创建歌单失败: {e}")),
                    },
                    Cmd::Cover { url } => match op_cover(&url).await {
                        Ok(bytes) => Ev::Cover(bytes),
                        Err(_) => Ev::Info("封面加载失败（可忽略）".to_string()),
                    },
                }
            });
            let _ = ev_tx.send(out);
        }
    });
    cmd_tx
}

// ---------------------------------------------------------------------------
// 筛选面板状态
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Filters {
    keyword: String,
    artist: String,
    album: String,
    min_secs: u64,
    max_secs: u64,
    min_year: i32,
    max_year: i32,
    drop_unavailable: bool,
    drop_vip: bool,
    only_vip: bool,
    dedupe: bool,
}

impl Filters {
    /// 生成筛选结果的“拷贝”（不修改原歌单表）。
    fn apply(&self, originals: &[Song]) -> Vec<Song> {
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<Song> = Vec::new();
        for s in originals {
            if self.drop_unavailable && !s.available {
                continue;
            }
            if self.drop_vip && s.is_vip_only() {
                continue;
            }
            if self.only_vip && !s.is_vip_only() {
                continue;
            }
            let artist_text = s.artists.join(" / ");
            if !self.keyword.is_empty()
                && !format!("{} {} {}", s.name, artist_text, s.album.clone().unwrap_or_default())
                    .to_lowercase()
                    .contains(&self.keyword.to_lowercase())
            {
                continue;
            }
            if !self.artist.is_empty()
                && !artist_text
                    .to_lowercase()
                    .contains(&self.artist.to_lowercase())
            {
                continue;
            }
            if !self.album.is_empty() {
                let album = s.album.clone().unwrap_or_default().to_lowercase();
                let kw = self.album.to_lowercase();
                if !album.contains(&kw) {
                    continue;
                }
            }
            let secs = s.duration_ms / 1000;
            if self.min_secs > 0 && secs < self.min_secs {
                continue;
            }
            if self.max_secs > 0 && secs > self.max_secs {
                continue;
            }
            if let Some(y) = s.publish_year() {
                if self.min_year > 0 && y < self.min_year {
                    continue;
                }
                if self.max_year > 0 && y > self.max_year {
                    continue;
                }
            }
            if self.dedupe && !seen.insert(s.id) {
                continue;
            }
            out.push(s.clone());
        }
        out
    }

    /// 用于默认歌单名的条件描述，如「歌手:洛天依」「VIP 除外」。
    fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.keyword.is_empty() {
            parts.push(format!("关键词:{}", self.keyword));
        }
        if !self.artist.is_empty() {
            parts.push(format!("歌手:{}", self.artist));
        }
        if !self.album.is_empty() {
            parts.push(format!("专辑:{}", self.album));
        }
        if self.min_secs > 0 && self.max_secs > 0 {
            parts.push(format!("时长 {}-{} 秒", self.min_secs, self.max_secs));
        } else if self.min_secs > 0 {
            parts.push(format!("时长 ≥{} 秒", self.min_secs));
        } else if self.max_secs > 0 {
            parts.push(format!("时长 ≤{} 秒", self.max_secs));
        }
        if self.min_year > 0 && self.max_year > 0 {
            parts.push(format!("{}-{} 年", self.min_year, self.max_year));
        } else if self.min_year > 0 {
            parts.push(format!("{} 年后", self.min_year));
        } else if self.max_year > 0 {
            parts.push(format!("{} 年前", self.max_year));
        }
        if self.drop_unavailable {
            parts.push("剔除下架".to_string());
        }
        if self.drop_vip {
            parts.push("VIP 除外".to_string());
        }
        if self.only_vip {
            parts.push("仅 VIP".to_string());
        }
        if self.dedupe {
            parts.push("去重".to_string());
        }
        if parts.is_empty() {
            "全部".to_string()
        } else {
            parts.join(" & ")
        }
    }
}

// ---------------------------------------------------------------------------
// 应用主体
// ---------------------------------------------------------------------------

pub(crate) struct App {
    cmd_tx: Sender<Cmd>,
    ev_rx: Receiver<Ev>,
    busy: Option<String>,

    input: String,
    meta: Option<PlaylistInfo>,
    originals: Vec<Song>,
    work: Vec<Song>,
    applied_desc: String,
    filters: Filters,
    name: String,

    account: Option<UiAccount>,
    login_open: bool,
    phone: String,
    code: String,
    pending_create: Option<(String, Vec<u64>)>,
    created_msg: Option<String>,

    cover_tex: Option<egui::TextureHandle>,
    pending_cover: Option<Vec<u8>>,
    status: String,
}

impl App {
    pub(crate) fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
        style_theme(&cc.egui_ctx);
        let (ev_tx, ev_rx) = channel();
        let cmd_tx = spawn_worker(ev_tx);
        let mut app = Self {
            cmd_tx,
            ev_rx,
            busy: None,
            input: "https://music.163.com/#/playlist?id=3778678".to_string(),
            meta: None,
            originals: Vec::new(),
            work: Vec::new(),
            applied_desc: String::new(),
            filters: Filters::default(),
            name: String::new(),
            account: None,
            login_open: false,
            phone: String::new(),
            code: String::new(),
            pending_create: None,
            created_msg: None,
            cover_tex: None,
            pending_cover: None,
            status: "就绪".to_string(),
        };
        app.send(Cmd::CheckLogin);
        app
    }

    fn send(&mut self, cmd: Cmd) {
        let _ = self.cmd_tx.send(cmd);
    }

    fn set_busy(&mut self, label: impl Into<String>) {
        self.busy = Some(label.into());
        self.status = self.busy.clone().unwrap_or_default();
    }

    fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.ev_rx.try_recv() {
            self.busy = None;
            match ev {
                Ev::Fetched { meta, songs, cover_url } => {
                    self.meta = Some(meta.clone());
                    self.originals = songs.clone();
                    self.work = songs.clone();
                    self.filters = Filters::default();
                    self.applied_desc = "全部".to_string();
                    self.name = format!("{}-[全部]", meta.name);
                    self.created_msg = None;
                    self.status = format!("已拉取 {} 首，本地副本已保存", songs.len());
                    // 保存本地副本（原始快照）
                    let _ = std::fs::create_dir_all(DATA_DIR);
                    let dump = PlaylistDump {
                        meta: meta.clone(),
                        songs: songs.clone(),
                        fetched_at_ms: now_ms(),
                    };
                    if let Ok(json) = serde_json::to_string_pretty(&dump) {
                        let path = PathBuf::from(DATA_DIR).join(format!("playlist-{}.json", meta.id));
                        let _ = std::fs::write(&path, json);
                    }
                    if let Some(url) = cover_url {
                        self.send(Cmd::Cover { url });
                    }
                }
                Ev::Login(acc) => {
                    self.account = Some(acc.clone());
                    self.login_open = false;
                    self.status = format!("已登录：{} (uid={})", acc.nickname, acc.uid);
                    if let Some((name, ids)) = self.pending_create.take() {
                        self.send(Cmd::Create { name, ids });
                    }
                }
                Ev::NotLogged => {
                    if self.account.take().is_some() {
                        self.status = "登录状态已失效".to_string();
                    }
                    if self.pending_create.is_some() {
                        self.login_open = true;
                        self.status = "请登录后再创建歌单".to_string();
                    }
                }
                Ev::CodeSent { phone } => {
                    self.status = format!("验证码已发送到 {phone}（App/短信查收）");
                }
                Ev::Created { id, name } => {
                    self.created_msg = Some(format!(
                        "创建成功：{name}\nhttps://music.163.com/#/playlist?id={id}"
                    ));
                    self.status = "歌单创建成功".to_string();
                }
                Ev::Cover(bytes) => {
                    self.pending_cover = Some(bytes);
                }
                Ev::Info(s) => self.status = s,
                Ev::Error(s) => {
                    self.status = s.clone();
                    if self.pending_create.is_some() && s.contains("登录") {
                        self.login_open = true;
                    }
                    self.created_msg = Some(s);
                }
            }
        }
        // 在 UI 线程加载封面纹理
        if let Some(bytes) = self.pending_cover.take() {
            self.cover_tex = load_cover_texture(ctx, &bytes);
        }
    }

    fn fetch(&mut self) {
        let input = self.input.trim().to_string();
        if input.is_empty() || self.is_busy() {
            return;
        }
        self.set_busy("正在拉取歌单…");
        self.send(Cmd::Fetch { input });
    }

    fn apply_filter(&mut self) {
        if self.meta.is_none() {
            return;
        }
        self.work = self.filters.apply(&self.originals);
        self.applied_desc = self.filters.describe();
        self.name = format!("{}-[{}]", self.meta.as_ref().unwrap().name, self.applied_desc);
        self.status = format!(
            "已应用筛选：{} → {} / {} 首",
            self.applied_desc,
            self.work.len(),
            self.originals.len()
        );
    }

    fn reset_filter(&mut self) {
        self.filters = Filters::default();
        self.apply_filter();
    }

    fn create_clicked(&mut self) {
        if self.meta.is_none() || self.is_busy() {
            return;
        }
        let name = if self.name.trim().is_empty() {
            format!(
                "{}-[{}]",
                self.meta.as_ref().unwrap().name,
                self.applied_desc
            )
        } else {
            self.name.trim().to_string()
        };
        let ids: Vec<u64> = self.work.iter().map(|s| s.id).collect();
        if ids.is_empty() {
            self.status = "当前列表为空，无法创建".to_string();
            return;
        }
        if self.account.is_some() {
            self.set_busy("正在创建歌单并添加歌曲…");
            self.send(Cmd::Create { name, ids });
        } else {
            self.pending_create = Some((name, ids));
            self.login_open = true;
            self.status = "请先登录".to_string();
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain(ctx);

        // ---- 底部状态栏（先占位，横贯整行）----
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(19, 20, 23))
                    .inner_margin(egui::Margin::symmetric(12.0, 5.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(b) = &self.busy {
                        ui.spinner();
                        ui.label(b);
                    } else {
                        ui.label(egui::RichText::new(&self.status).weak());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(acc) = &self.account {
                            ui.label(
                                egui::RichText::new(format!(
                                    "已登录：{} · uid={}",
                                    acc.nickname, acc.uid
                                ))
                                .color(ACCENT),
                            );
                        } else {
                            ui.label(egui::RichText::new("未登录").weak());
                        }
                    });
                });
            });

        // ---- 左侧：歌单 + 筛选 + 创建（固定宽度、柔和分区、不画硬分隔线） ----
        egui::SidePanel::left("left")
            .resizable(false)
            .exact_width(340.0)
            .frame(
                egui::Frame::none()
                    .fill(SIDEBAR_BG)
                    .inner_margin(egui::Margin::symmetric(14.0, 12.0)),
            )
            .show_separator_line(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.left_panel(ui);
                    });
            });

        // ---- 中央：当前列表 ----
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(CONTENT_BG)
                    .inner_margin(egui::Margin::symmetric(14.0, 10.0)),
            )
            .show(ctx, |ui| {
                self.central_panel(ui);
            });

        // ---- 登录窗口 ----
        if self.login_open {
            self.login_window(ctx);
        }
    }
}

impl App {
    fn left_panel(&mut self, ui: &mut egui::Ui) {
        // ---- 顶部品牌区 ----
        ui.horizontal(|ui| {
            let (dot, _) =
                ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            ui.painter().circle_filled(dot.center(), 7.0, ACCENT);
            ui.add_space(2.0);
            ui.label(egui::RichText::new("网易云 · 歌单管理器").size(17.0).strong());
        });
        ui.label(
            egui::RichText::new("拉取 → 组合筛选 → 登录创建")
                .weak()
                .size(11.5),
        );
        ui.add_space(12.0);

        // ---- 歌单链接：输入框独占一行（圆角「封口」完整可见），下方显式按钮，回车同样触发 ----
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            section_title(ui, "歌单链接 / ID");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .hint_text("粘贴链接或输入歌单 ID，回车也可拉取")
                    .desired_width(f32::INFINITY),
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter {
                self.fetch();
            }
            ui.add_space(6.0);
            if full_button(ui, "拉取", !self.is_busy()).clicked() {
                self.fetch();
            }
        });

        // ---- 封面 + 元信息 ----
        if let Some(meta) = &self.meta {
            ui.add_space(10.0);
            card(ui, |ui| {
                ui.horizontal_top(|ui| {
                    cover_or_placeholder(ui, self.cover_tex.as_ref(), meta);
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(&meta.name)
                                .strong()
                                .size(15.0)
                                .color(TEXT_MAIN),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "创建者：{}",
                                meta.creator_name.clone().unwrap_or_else(|| "未知".into())
                            ))
                            .weak()
                            .size(12.0),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "{} 首 · {} 次播放",
                                meta.track_count,
                                meta.play_count.unwrap_or(0)
                            ))
                            .weak()
                            .size(12.0),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            stat_chip(ui, &format!("原歌单 {}", self.originals.len()), false);
                            stat_chip(ui, &format!("当前 {}", self.work.len()), true);
                        });
                    });
                });
            });
            if !self.applied_desc.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!("已应用条件：[{}]", self.applied_desc))
                        .weak()
                        .size(12.0),
                );
            }
        }

        ui.add_space(10.0);
        egui::CollapsingHeader::new(egui::RichText::new("组合筛选").strong().size(14.0))
            .default_open(true)
            .show(ui, |ui| {
                card(ui, |ui| {
                    self.filter_ui(ui);
                });
            });

        ui.add_space(10.0);
        egui::CollapsingHeader::new(egui::RichText::new("创建歌单").strong().size(14.0))
            .default_open(true)
            .show(ui, |ui| {
                card(ui, |ui| {
                    section_title(ui, "新歌单名称");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.name)
                            .hint_text("默认：原歌单名-[筛选条件]")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(6.0);
                    let enabled = !self.is_busy() && self.meta.is_some() && !self.work.is_empty();
                    let btn = full_button(ui, "创建歌单", enabled);
                    if btn
                        .on_hover_text("登录后，以你的账号为所有者创建新歌单并加入当前列表歌曲")
                        .clicked()
                    {
                        self.create_clicked();
                    }
                });
            });
        if let Some(msg) = &self.created_msg {
            ui.add_space(8.0);
            card(ui, |ui| {
                ui.label(egui::RichText::new(msg).size(12.0));
            });
        }
        ui.add_space(8.0);
    }

    fn filter_ui(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("filter_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("关键词");
                ui.add(
                    egui::TextEdit::singleline(&mut self.filters.keyword)
                        .hint_text("歌名 / 歌手 / 专辑")
                        .desired_width(150.0),
                );
                ui.end_row();

                ui.label("歌手包含");
                ui.add(
                    egui::TextEdit::singleline(&mut self.filters.artist).desired_width(150.0),
                );
                ui.end_row();

                ui.label("专辑包含");
                ui.add(
                    egui::TextEdit::singleline(&mut self.filters.album).desired_width(150.0),
                );
                ui.end_row();

                ui.label("最短时长(秒)");
                ui.add(egui::DragValue::new(&mut self.filters.min_secs).range(0..=7200).speed(5));
                ui.end_row();

                ui.label("最长时长(秒)");
                ui.add(egui::DragValue::new(&mut self.filters.max_secs).range(0..=7200).speed(5));
                ui.end_row();

                ui.label("发行年份起");
                ui.add(egui::DragValue::new(&mut self.filters.min_year).range(0..=2026).speed(1));
                ui.end_row();

                ui.label("发行年份止");
                ui.add(egui::DragValue::new(&mut self.filters.max_year).range(0..=2026).speed(1));
                ui.end_row();

                ui.label("VIP");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.filters.only_vip, "仅 VIP");
                    ui.checkbox(&mut self.filters.drop_vip, "排除 VIP");
                });
                ui.end_row();

                ui.label("其他");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.filters.drop_unavailable, "剔除已下架");
                    ui.checkbox(&mut self.filters.dedupe, "按 ID 去重");
                });
                ui.end_row();
            });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let done = small_button(ui, "完成", !self.is_busy() && self.meta.is_some());
            if done
                .on_hover_text("应用到一份副本，不修改原歌单")
                .clicked()
            {
                self.apply_filter();
            }
            if ui.button("重置").clicked() {
                self.reset_filter();
            }
        });
    }

    fn central_panel(&mut self, ui: &mut egui::Ui) {
        // ---- 顶部工具栏 ----
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("歌曲列表").size(16.0).strong());
                ui.add_space(6.0);
                if self.meta.is_some() {
                    stat_chip(ui, &format!("原歌单 {} 首", self.originals.len()), false);
                    stat_chip(ui, &format!("当前 {} 首", self.work.len()), true);
                } else {
                    ui.label(egui::RichText::new("尚未拉取歌单").weak());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !self.applied_desc.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("条件 [{}]", self.applied_desc))
                                .weak()
                                .size(12.0),
                        );
                    }
                });
            });
        });
        ui.add_space(10.0);

        if self.work.is_empty() {
            // 空态：占满剩余高度并居中引导，避免“看起来收起来”的错觉
            let hint = if self.meta.is_none() {
                "在左侧输入歌单链接，点击「拉取」\n拉取完成后，歌曲会显示在这里"
            } else {
                "当前没有符合条件的歌曲\n请调整左侧筛选条件后点击「完成」"
            };
            let avail = ui.available_height();
            card(ui, |ui| {
                ui.set_min_height((avail - 4.0).max(220.0));
                ui.add_space(((ui.available_height()) * 0.35).max(24.0));
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("♪ ♪ ♪")
                            .size(40.0)
                            .color(egui::Color32::from_rgb(90, 96, 108)),
                    );
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new(hint).color(TEXT_WEAK).size(13.0));
                    if self.meta.is_some() {
                        ui.add_space(10.0);
                        if small_button(ui, "应用当前筛选（完成）", !self.is_busy()).clicked() {
                            self.apply_filter();
                        }
                    }
                });
            });
            return;
        }

        // ---- 歌曲表格 ----
        card(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let text_height = 18.0;
                    TableBuilder::new(ui)
                        .striped(true)
                        .resizable(true)
                        .column(Column::auto().at_least(40.0))
                        .column(Column::initial(240.0).at_least(140.0).clip(true))
                        .column(Column::initial(190.0).at_least(110.0).clip(true))
                        .column(Column::initial(190.0).at_least(90.0).clip(true))
                        .column(Column::auto().at_least(60.0))
                        .column(Column::auto().at_least(50.0))
                        .column(Column::auto().at_least(60.0))
                        .header(text_height + 6.0, |mut header| {
                            for title in ["#", "歌名", "歌手", "专辑", "时长", "年份", "状态"] {
                                header.col(|ui| {
                                    ui.strong(title);
                                });
                            }
                        })
                        .body(|body| {
                            body.rows(text_height, self.work.len(), |mut row| {
                                let i = row.index();
                                let s = &self.work[i];
                                let year = parse_year(s.publish_time_ms);
                                row.col(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("{}", i + 1)).weak(),
                                    );
                                });
                                row.col(|ui| {
                                    ui.label(&s.name).on_hover_text(&s.name);
                                });
                                row.col(|ui| {
                                    ui.label(s.artists.join(" / "));
                                });
                                row.col(|ui| {
                                    ui.label(s.album.clone().unwrap_or_default());
                                });
                                row.col(|ui| {
                                    ui.label(format_duration(s.duration_ms));
                                });
                                row.col(|ui| {
                                    ui.label(year);
                                });
                                row.col(|ui| {
                                    let mut tags: Vec<String> = Vec::new();
                                    if !s.available {
                                        tags.push("已下架".into());
                                    }
                                    if s.is_vip_only() {
                                        tags.push("VIP".into());
                                    }
                                    ui.label(
                                        egui::RichText::new(tags.join(" / "))
                                            .color(if tags.is_empty() {
                                                TEXT_WEAK
                                            } else {
                                                egui::Color32::from_rgb(236, 152, 60)
                                            }),
                                    );
                                });
                            });
                        });
                });
        });
    }

    fn login_window(&mut self, ctx: &egui::Context) {
        let mut open = self.login_open;
        egui::Window::new("登录网易云")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_width(320.0);
                if ui
                    .add_enabled(!self.is_busy(), egui::Button::new("使用本地已保存的 Cookie 登录"))
                    .clicked()
                {
                    self.set_busy("正在校验本地 Cookie…");
                    self.send(Cmd::CheckLogin);
                }
                ui.separator();
                ui.strong("或 手机号 + 验证码");
                ui.add(
                    egui::TextEdit::singleline(&mut self.phone)
                        .hint_text("手机号")
                        .desired_width(260.0),
                );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.code)
                            .hint_text("短信验证码")
                            .desired_width(160.0),
                    );
                    if ui
                        .add_enabled(
                            !self.is_busy() && self.phone.trim().len() >= 6,
                            egui::Button::new("发送验证码"),
                        )
                        .clicked()
                    {
                        self.set_busy("正在发送验证码…");
                        let phone = self.phone.trim().to_string();
                        self.send(Cmd::SendCode { phone });
                    }
                });
                ui.add_space(4.0);
                let login_btn = ui.add_enabled(
                    !self.is_busy() && self.phone.trim().len() >= 6 && self.code.trim().len() >= 4,
                    egui::Button::new("登录"),
                );
                if login_btn.clicked() {
                    self.set_busy("正在登录…");
                    let phone = self.phone.trim().to_string();
                    let code = self.code.trim().to_string();
                    self.send(Cmd::LoginPhone { phone, code });
                }
                if let Some(acc) = &self.account {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("已登录：{}", acc.nickname)).color(
                            egui::Color32::from_rgb(45, 156, 219),
                        ),
                    );
                }
                ui.add_space(4.0);
                ui.label(egui::RichText::new("未收到验证码？先确认手机号已绑定网易云").weak());
            });
        if !open {
            self.login_open = false;
        }
    }
}

// ---------------------------------------------------------------------------
// 视觉：主题、卡片与按钮
// ---------------------------------------------------------------------------

/// 统一深色主题（卡片化 · 网易红点缀 · 无生硬黑线）。
fn style_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let mut v = egui::Visuals::dark();
    v.panel_fill = CONTENT_BG;
    v.window_fill = CONTENT_BG;
    v.extreme_bg_color = INPUT_BG;
    v.faint_bg_color = egui::Color32::from_rgb(20, 21, 24);
    v.code_bg_color = INPUT_BG;
    v.window_rounding = egui::Rounding::same(12.0);
    v.window_stroke = egui::Stroke::new(1.0_f32, BORDER);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.selection.stroke = egui::Stroke::new(1.0_f32, ACCENT);
    v.hyperlink_color = egui::Color32::from_rgb(86, 156, 255);

    let idle = egui::style::WidgetVisuals {
        bg_fill: egui::Color32::from_rgb(52, 55, 61),
        weak_bg_fill: egui::Color32::from_rgb(52, 55, 61),
        bg_stroke: egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(72, 76, 84)),
        rounding: egui::Rounding::same(6.0),
        fg_stroke: egui::Stroke::new(1.0_f32, TEXT_MAIN),
        expansion: 0.0,
    };
    let hover = egui::style::WidgetVisuals {
        bg_fill: egui::Color32::from_rgb(70, 74, 82),
        weak_bg_fill: egui::Color32::from_rgb(70, 74, 82),
        bg_stroke: egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(120, 124, 132)),
        rounding: egui::Rounding::same(6.0),
        fg_stroke: egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
        expansion: 0.0,
    };
    let active = egui::style::WidgetVisuals {
        bg_fill: egui::Color32::from_rgb(92, 96, 104),
        weak_bg_fill: egui::Color32::from_rgb(92, 96, 104),
        bg_stroke: egui::Stroke::new(1.0_f32, ACCENT),
        rounding: egui::Rounding::same(6.0),
        fg_stroke: egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
        expansion: 0.0,
    };
    let plain = egui::style::WidgetVisuals {
        bg_fill: CARD_BG,
        weak_bg_fill: egui::Color32::TRANSPARENT,
        bg_stroke: egui::Stroke::new(1.0_f32, BORDER),
        rounding: egui::Rounding::same(6.0),
        fg_stroke: egui::Stroke::new(1.0_f32, TEXT_MAIN),
        expansion: 0.0,
    };
    v.widgets.inactive = idle;
    v.widgets.hovered = hover;
    v.widgets.active = active;
    v.widgets.open = active;
    v.widgets.noninteractive = plain;

    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 4.0);
    style.spacing.interact_size = egui::vec2(64.0, 24.0);
    style.spacing.slider_width = 120.0;
    style.spacing.indent = 0.0;
    style.visuals = v;
    ctx.set_style(style);
}

/// 圆角卡片容器。
fn card<R>(
    ui: &mut egui::Ui,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    egui::Frame::none()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .rounding(egui::Rounding::same(10.0))
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, add)
}

/// 主操作按钮：启用时为红色（网易云主色），禁用时为弱化灰。
fn make_accent<'a>(text: &'a str, enabled: bool) -> egui::Button<'a> {
    if enabled {
        egui::Button::new(egui::RichText::new(text).strong().color(egui::Color32::WHITE))
            .fill(ACCENT)
            .stroke(egui::Stroke::new(1.0_f32, ACCENT))
            .rounding(egui::Rounding::same(6.0))
    } else {
        egui::Button::new(egui::RichText::new(text).strong().weak())
    }
}

/// 整行宽度的主按钮。
fn full_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    let size = egui::vec2(ui.available_width(), 30.0);
    let btn = make_accent(text, enabled);
    if enabled {
        ui.add_sized(size, btn)
    } else {
        ui.add_enabled_ui(false, |ui| ui.add_sized(size, btn)).inner
    }
}

/// 随内容宽度的主按钮（用于按钮组）。
fn small_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    let btn = make_accent(text, enabled);
    if enabled {
        ui.add(btn)
    } else {
        ui.add_enabled(false, btn)
    }
}

/// 小节标题（弱化的小字）。
fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(egui::RichText::new(title).weak().size(12.0));
    ui.add_space(2.0);
}

/// 圆角小徽章（统计计数）。
fn stat_chip(ui: &mut egui::Ui, text: &str, accent: bool) {
    let font = egui::FontId::proportional(12.0);
    let color = if accent {
        egui::Color32::WHITE
    } else {
        TEXT_WEAK
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font, color);
    let pad = egui::vec2(9.0, 3.0);
    let size = galley.size() + pad * 2.0;
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let bg = if accent {
        ACCENT
    } else {
        egui::Color32::from_rgb(43, 46, 52)
    };
    ui.painter()
        .rect(rect, egui::Rounding::same(11.0), bg, egui::Stroke::NONE);
    ui.painter().galley(rect.min + pad, galley, color);
    ui.add_space(6.0);
}

// ---------------------------------------------------------------------------
// 封面与字体
// ---------------------------------------------------------------------------

fn cover_or_placeholder(
    ui: &mut egui::Ui,
    tex: Option<&egui::TextureHandle>,
    meta: &PlaylistInfo,
) {
    let size = egui::vec2(118.0, 118.0);
    let rounding = egui::Rounding::same(8.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    if let Some(tex) = tex {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter().image(tex.id(), rect, uv, egui::Color32::WHITE);
        // 圆角描边，让方形封面与圆角卡片衔接更自然
        ui.painter()
            .rect_stroke(rect, rounding, egui::Stroke::new(1.0_f32, BORDER));
    } else {
        ui.painter()
            .rect(rect, rounding, INPUT_BG, egui::Stroke::new(1.0_f32, BORDER));
        let ch = meta.name.chars().next().unwrap_or('♪').to_string();
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            ch,
            egui::FontId::proportional(46.0),
            egui::Color32::from_rgb(150, 155, 165),
        );
    }
    resp.on_hover_text("歌单封面");
}

fn load_cover_texture(ctx: &egui::Context, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
    Some(ctx.load_texture("cover", color, egui::TextureOptions::LINEAR))
}

/// 尝试加载中文字体（找不到则中文会显示为方框，会打印提示）。
fn setup_fonts(ctx: &egui::Context) {
    match find_cjk_font_bytes() {
        Some(bytes) => {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("cjk".to_owned(), egui::FontData::from_owned(bytes));
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push("cjk".to_owned());
            }
            ctx.set_fonts(fonts);
        }
        None => {
            eprintln!("[提示] 未找到中文字体，界面中文可能显示为方框。");
            eprintln!("       可用环境变量指定字体，例如：CM_FONT=/usr/share/fonts/.../xxx.ttf cargo run");
        }
    }
}

/// 给 CJK 相关字体文件名打分（越高越优先，排除纯拉丁字体）。
fn cjk_score(stem: &str) -> i32 {
    let mut score = 0;
    for kw in ["regular", "medium", "normal"] {
        if stem.contains(kw) {
            score += 8;
        }
    }
    for kw in [
        "noto", "source", "han", "wqy", "zenhei", "microhei", "droid", "lxgw", "harmony",
        "cjk", "yahei", "simhei", "msyh", "hei", "kai", "song", "ming", "gothic", "sc", "cn",
    ] {
        if stem.contains(kw) {
            score += 4;
        }
    }
    if stem.contains("serif") {
        score -= 2;
    }
    score
}

fn scan_cjk_fonts(dir: &Path, out: &mut Vec<(i32, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            scan_cjk_fonts(&p, out);
        } else if let Some(ext) = p.extension().and_then(|e| e.to_str())
            && matches!(ext.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc")
        {
            let stem = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            let score = cjk_score(&stem);
            if score > 0 {
                out.push((score, p));
            }
        }
    }
}

fn find_cjk_font_bytes() -> Option<Vec<u8>> {
    // 1) 环境变量显式指定
    if let Ok(p) = std::env::var("CM_FONT")
        && let Ok(bytes) = std::fs::read(&p)
        && !bytes.is_empty()
    {
        return Some(bytes);
    }

    // 2) 递归扫描常见字体目录，收集 CJK 字体并按得分排序
    let mut roots = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ];
    if let Ok(home) = std::env::var("HOME") {
        roots.push(PathBuf::from(&home).join(".fonts"));
        roots.push(PathBuf::from(&home).join(".local/share/fonts"));
    }
    let mut found: Vec<(i32, PathBuf)> = Vec::new();
    for root in &roots {
        scan_cjk_fonts(root, &mut found);
    }
    found.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    // 优先 ttf/otf（单字体文件，epaint 解析可靠；.ttc 集合可能解析失败）
    for (_, p) in &found {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if (ext == "ttf" || ext == "otf")
            && let Ok(bytes) = std::fs::read(p)
            && !bytes.is_empty()
        {
            return Some(bytes);
        }
    }
    // 3) 已知路径兜底（含 mac/windows 的 .ttc）
    const KNOWN: &[&str] = &[
        "/usr/share/fonts/harmonyos-sans/HarmonyOS_Sans_SC.ttf",
        "/usr/share/fonts/adobe-source-han-sans/SourceHanSansCN-Regular.otf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "C:/Windows/Fonts/msyh.ttc",
    ];
    for p in KNOWN {
        if let Ok(bytes) = std::fs::read(p)
            && !bytes.is_empty()
        {
            return Some(bytes);
        }
    }
    None
}

pub(crate) fn run() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1140.0, 760.0])
            .with_min_inner_size([900.0, 620.0])
            .with_title("网易云歌单管理器"),
        ..Default::default()
    };
    eframe::run_native(
        "cloud-music-manager",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
    .map_err(|e| anyhow!(e.to_string()))
}
