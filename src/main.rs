//! cloud-music-manager —— 网易云音乐歌单管理
//!
//! 默认启动图形界面（egui）；加参数 `--cli` 使用命令行流程：
//! 拉取歌单 → 手动输入关键词筛选（歌名/歌手/专辑）→ 登录 → 创建新歌单并加入。
//!
//! 网络请求 / 加密 / 登录全部直接使用 `ncm-api-rs` crate。

mod gui;

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use cloud_music_manager::filter::{dedupe_by_id, SongFilter};
use cloud_music_manager::model::{PlaylistInfo, Song};
use ncm_api_rs::{create_client, ApiClient, ApiResponse, Query};

const COOKIE_FILE: &str = "data/cookies.txt";
/// 批量取歌曲详情时每个请求的曲目数
const SONG_BATCH: usize = 500;
/// 添加歌曲时每个请求的曲目数
const ADD_BATCH: usize = 300;
/// 预览打印的最多歌曲数
const PREVIEW_MAX: usize = 10;

#[derive(Debug)]
struct AccountInfo {
    user_id: u64,
    nickname: String,
}

/// 入口：`--cli` 走命令行主流程，否则启动图形界面。
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--cli" || a == "-c") {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("创建 tokio runtime 失败");
        if let Err(e) = rt.block_on(cli_main()) {
            eprintln!("\n[错误] {e}");
            std::process::exit(1);
        }
    } else if let Err(e) = gui::run() {
        eprintln!("GUI 启动失败: {e}");
        std::process::exit(1);
    }
}

/// 原命令行主流程（保留入口：`cloud-music-manager --cli [歌单链接]`）。
async fn cli_main() -> Result<()> {
    // ---------- 1. 拉取歌单 ----------
    let input = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .unwrap_or_else(|| "https://music.163.com/#/playlist?id=3778678".to_string());
    let (mut client, mut cookies) = new_session()?;

    println!("[1/5] 正在拉取歌单: {input}");
    let id = resolve_playlist_input(&input)?;
    let meta = playlist_meta(&client, &mut cookies, id).await?;
    let all = song_details(&client, &meta.track_ids).await?;
    println!("歌单「{}」共 {} 首（其中已下架 {} 首）",
        meta.name, all.len(), all.iter().filter(|s| !s.available).count());

    // ---------- 2. 手动输入筛选条件（只保留关键词过滤） ----------
    println!("\n[2/5] 请输入筛选关键词（匹配 歌名/歌手/专辑；直接回车 = 全部保留）");
    let kw = read_input("关键词: ")?;
    let filter = SongFilter {
        // 与 GUI 一致：先剔除已下架，再按关键词匹配
        drop_unavailable: true,
        keyword: if kw.is_empty() { None } else { Some(kw.clone()) },
        ..Default::default()
    };
    let mut matched: Vec<Song> = filter.apply(&all);
    matched = dedupe_by_id(&matched); // 同一首歌只保留一次
    let desc = if kw.is_empty() {
        "全部".to_string()
    } else {
        format!("含「{kw}」")
    };
    println!("筛选结果（{desc}，已剔除已下架）：{} 首", matched.len());
    if matched.is_empty() {
        bail!("没有找到符合条件的歌曲，流程结束");
    }
    for (i, s) in matched.iter().take(PREVIEW_MAX).enumerate() {
        println!("    {}. {}", i + 1, s.display());
    }
    if matched.len() > PREVIEW_MAX {
        println!("    … 其余 {} 首略", matched.len() - PREVIEW_MAX);
    }

    // ---------- 3. 请求用户登录 ----------
    println!("\n[3/5] 需要登录后才能创建新歌单");
    let me = ensure_logged_in(&mut client, &mut cookies).await?;
    println!("已登录：{} (uid={})", me.nickname, me.user_id);

    // ---------- 4. 以该用户为所有者创建新歌单 ----------
    let name = if kw.is_empty() {
        format!("{}-全部", meta.name)
    } else {
        format!("{}-{}精选", meta.name, kw)
    };
    println!("\n[4/5] 正在为你创建歌单「{name}」...");
    let new_id = create_playlist(&mut client, &name, false).await?;
    println!("创建成功（所有者：{}）", me.nickname);

    // ---------- 5. 加入筛选出的歌曲 ----------
    println!("[5/5] 正在加入 {} 首歌曲（每批 {ADD_BATCH} 首）...", matched.len());
    let ids: Vec<u64> = matched.iter().map(|s| s.id).collect();
    let (added, skipped) = add_tracks(&mut client, &mut cookies, new_id, &ids).await?;
    println!("加入完成：成功 {added} 首，已存在跳过 {skipped} 首");

    println!("\n全部完成！新歌单地址：{}", playlist_url(new_id));
    Ok(())
}

// ---------------------------------------------------------------------------
// 抓取（公开读取，无需登录）
// ---------------------------------------------------------------------------

async fn playlist_meta(
    client: &ApiClient,
    cookies: &mut HashMap<String, String>,
    id: u64,
) -> Result<PlaylistInfo> {
    let r = client
        .playlist_detail(&Query::new().param("id", &id.to_string()))
        .await?;
    capture_cookies(cookies, &r);
    let body = r.body;
    if body["code"].as_i64().unwrap_or(-1) != 200 || body["playlist"].is_null() {
        bail!("歌单不存在、已被删除或为私密不可见");
    }
    Ok(PlaylistInfo::from_value(&body["playlist"]))
}

async fn song_details(client: &ApiClient, ids: &[u64]) -> Result<Vec<Song>> {
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
                out.push(Song::unavailable(*id)); // 已下架/查不到详情
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 登录 / 写操作
// ---------------------------------------------------------------------------

fn new_session() -> Result<(ApiClient, HashMap<String, String>)> {
    let mut client = create_client(None);
    let cookies = load_cookie_map()?;
    sync_cookie(&mut client, &cookies);
    Ok((client, cookies))
}

/// 登录：① 复用本地 Cookie → ② 环境变量 MUSIC_U → ③ 选择登录方式
/// （手机号验证码 / 粘贴网页 Cookie / App 扫码）。
async fn ensure_logged_in(
    client: &mut ApiClient,
    cookies: &mut HashMap<String, String>,
) -> Result<AccountInfo> {
    // 1) 已有本地 Cookie 且有效，直接复用（登录一次后即免登录）
    if cookies.contains_key("MUSIC_U") {
        if let Ok(me) = account_info(client).await {
            sync_cookie(client, cookies);
            println!("（复用本地登录 Cookie）");
            return Ok(me);
        }
        println!("本地 Cookie 已失效，请重新登录。\n");
        reset_session(client, cookies); // 清掉失效 Cookie，避免干扰后续登录
    }

    // 2) 支持环境变量 MUSIC_U（脚本/非交互场景）
    if let Ok(cookie) = std::env::var("MUSIC_U")
        && !cookie.trim().is_empty()
        && let Some(me) = try_web_login(client, cookies, &cookie).await?
    {
        println!("（已通过环境变量 MUSIC_U 登录网页版会话）");
        return Ok(me);
    }

    // 3) 选择登录方式
    println!("请选择登录方式：");
    println!("  1) 手机号 + 短信验证码（推荐：自动登录并保存 Cookie）");
    println!("  2) 网页版 Cookie（已在浏览器登录网页版时粘贴）");
    println!("  3) 手机 App 扫码");
    let choice = read_input("> ")?;
    match choice.as_str() {
        // 粘贴网页版 Cookie
        "2" => {
            println!("请打开 https://music.163.com → F12 → Application/存储 → Cookies，");
            println!("复制 MUSIC_U 的值粘贴（直接回车则改用 App 扫码）：");
            let line = read_input("> ")?;
            if !line.is_empty() {
                if let Some(me) = try_web_login(client, cookies, &line).await? {
                    println!("（已通过网页版登录 Cookie 登录）");
                    return Ok(me);
                }
                println!("Cookie 无效，改用手机 App 扫码登录。\n");
            }
            qr_login(client, cookies).await
        }
        // App 扫码
        "3" => qr_login(client, cookies).await,
        // 默认：手机号 + 验证码
        _ => phone_login(client, cookies).await,
    }
}

/// 手机号 + 短信验证码登录（登录成功后自动把 Cookie 存入本地，无需手动复制）。
async fn phone_login(
    client: &mut ApiClient,
    cookies: &mut HashMap<String, String>,
) -> Result<AccountInfo> {
    // 登录请求必须从干净会话开始，否则旧 Cookie 会干扰服务端签发新登录态
    reset_session(client, cookies);

    // 1) 手机号
    let phone = read_input("请输入手机号: ")?;
    if phone.is_empty() || !phone.chars().all(|c| c.is_ascii_digit()) {
        bail!("手机号格式不正确");
    }

    // 2) 发送验证码
    let q = Query::new().param("phone", &phone);
    match client.captcha_sent(&q).await {
        Ok(r) => {
            let code = r.body["code"].as_i64().unwrap_or(-1);
            if code != 200 {
                bail!(
                    "发送验证码失败 code={code}: {}",
                    r.body["message"].as_str().unwrap_or("")
                );
            }
            capture_cookies(cookies, &r);
            sync_cookie(client, cookies);
            println!("验证码已发送到 {phone}，请注意查收（网易云 App / 短信）。");
        }
        Err(e) => {
            bail!("发送验证码失败：{e}（若提示风控 -462，请改用方式 2/3 登录）");
        }
    }

    // 3) 输入验证码登录（最多 3 次重试）
    for attempt in 1..=3 {
        let captcha = read_input("请输入短信验证码: ")?;
        if captcha.is_empty() {
            continue;
        }
        match login_cellphone_captcha(client, &phone, &captcha).await {
            Ok(r) => {
                // crate 会把 400/502/201 等业务码映射成 Ok，这里必须校验业务 code
                let biz = r.body["code"].as_i64().unwrap_or(-1);
                if biz != 200 {
                    let msg = r.body["message"].as_str().unwrap_or("").to_string();
                    if attempt == 3 {
                        bail!("登录失败 code={biz}: {msg}");
                    }
                    eprintln!("登录失败（第 {attempt} 次）code={biz}: {msg}，请重新输入验证码。");
                    continue;
                }
                capture_cookies(cookies, &r); // MUSIC_U 等登录 Cookie
                ensure_music_u_from_body(cookies, &r); // 兜底：从 body.token 合成
                sync_cookie(client, cookies);
                match account_info(client).await {
                    Ok(me) => {
                        save_cookie_map(cookies)?;
                        println!("登录成功：{} (uid={})", me.nickname, me.user_id);
                        return Ok(me);
                    }
                    Err(e) => {
                        // 诊断信息：帮助确认登录响应里到底有没有拿到 MUSIC_U
                        let keys: Vec<&String> = cookies.keys().collect();
                        eprintln!("[诊断] 登录后持有 Cookie: {keys:?}");
                        bail!("登录后校验账号失败：{e}");
                    }
                }
            }
            Err(e) => {
                if attempt == 3 {
                    bail!("验证码错误次数过多，请稍后重试：{e}");
                }
                eprintln!("登录失败（第 {attempt} 次）：{e}，请重新输入验证码。");
            }
        }
    }
    bail!("未能完成登录")
}

/// 用验证码登录手机号。
///
/// ⚠️ 不能使用 crate 的 `login_cellphone`：它会在带 `captcha` 时**同时发送
/// `password=captcha`**，与上游 Node 原版不符，导致服务器按密码登录校验并返回
/// `502 账号或密码错误`。这里按 Node 原版只发 `captcha`、不发 `password`。
async fn login_cellphone_captcha(
    client: &ApiClient,
    phone: &str,
    captcha: &str,
) -> Result<ncm_api_rs::ApiResponse> {
    let data = serde_json::json!({
        "type": "1",
        "https": "true",
        "phone": phone,
        "countrycode": "86",
        "captcha": captcha,
        "remember": "true",
        "secureCaptcha": "",
    });
    let option = ncm_api_rs::RequestOption {
        crypto: ncm_api_rs::CryptoType::Weapi,
        ..Default::default()
    };
    Ok(client.request("/api/w/login/cellphone", data, option).await?)
}

/// 尝试用「网页版登录 Cookie」登录。
///
/// `raw` 可以是：整段 Cookie 字符串（`MUSIC_U=xxx; __csrf=yyy`），
/// 或仅 MUSIC_U 的值。校验成功返回账号信息并落盘；失败返回 None。
async fn try_web_login(
    client: &mut ApiClient,
    cookies: &mut HashMap<String, String>,
    raw: &str,
) -> Result<Option<AccountInfo>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.contains('=') {
        for (k, v) in parse_cookie_str(raw) {
            if !is_cookie_attribute(&k) {
                cookies.insert(k, v);
            }
        }
    } else {
        cookies.insert("MUSIC_U".to_string(), raw.trim_matches('"').to_string());
    }
    sync_cookie(client, cookies);

    match account_info(client).await {
        Ok(me) => {
            save_cookie_map(cookies)?;
            Ok(Some(me))
        }
        Err(_) => {
            // 清掉无效凭据，避免影响后续扫码登录
            cookies.remove("MUSIC_U");
            sync_cookie(client, cookies);
            Ok(None)
        }
    }
}

async fn qr_login(
    client: &mut ApiClient,
    cookies: &mut HashMap<String, String>,
) -> Result<AccountInfo> {
    // 二维码登录也从干净会话开始
    reset_session(client, cookies);
    let r = client.login_qr_key(&Query::new()).await.context("获取二维码 key 失败")?;
    capture_cookies(cookies, &r);
    sync_cookie(client, cookies);
    let unikey = r.body["data"]["unikey"]
        .as_str()
        .or_else(|| r.body["unikey"].as_str())
        .ok_or_else(|| anyhow!("登录接口未返回 unikey"))?
        .to_string();

    let r2 = client
        .login_qr_create(&Query::new().param("key", &unikey))
        .await?;
    let url = r2.body["data"]["qrurl"]
        .as_str()
        .unwrap_or(&format!("https://music.163.com/login?codekey={unikey}"))
        .to_string();

    println!("请用网易云音乐 App 扫码登录（90 秒内有效）：");
    println!("  {url}\n");

    // 轮询：800 过期 / 801 等扫码 / 802 已扫待确认 / 803 成功
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let r = client
            .login_qr_check(&Query::new().param("key", &unikey))
            .await?;
        capture_cookies(cookies, &r);
        let qr_code = r.body["code"].as_i64().unwrap_or(0);
        if qr_code == 803 {
            ensure_music_u_from_body(cookies, &r); // 兜底：从 body.token 合成
            sync_cookie(client, cookies);
            break; // 成功（MUSIC_U 等登录 Cookie 已捕获）
        }
        sync_cookie(client, cookies);
        match qr_code {
            801 | 802 => {}
            800 => bail!("二维码已过期，请重试"),
            other => bail!("扫码出错 code={other}: {}", r.body["message"]),
        }
        if Instant::now() >= deadline {
            bail!("等待扫码超时");
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }

    save_cookie_map(cookies)?;
    account_info(client).await
}

async fn account_info(client: &ApiClient) -> Result<AccountInfo> {
    let r = client.user_account(&Query::new()).await?;
    let uid = r.body["profile"]["userId"].as_u64().unwrap_or(0);
    if uid == 0 {
        bail!("未登录（Cookie 无效或缺失）");
    }
    Ok(AccountInfo {
        user_id: uid,
        nickname: r.body["profile"]["nickname"]
            .as_str()
            .unwrap_or("未知用户")
            .to_string(),
    })
}

async fn create_playlist(client: &mut ApiClient, name: &str, privacy: bool) -> Result<u64> {
    let p = if privacy { "10" } else { "0" };
    let r = client
        .playlist_create(&Query::new().param("name", name).param("privacy", p))
        .await?;
    r.body["playlist"]["id"]
        .as_u64()
        .ok_or_else(|| anyhow!("创建歌单响应缺少 playlist.id"))
}

async fn add_tracks(
    client: &mut ApiClient,
    cookies: &mut HashMap<String, String>,
    playlist_id: u64,
    song_ids: &[u64],
) -> Result<(usize, usize)> {
    let mut added = 0usize;
    let mut skipped = 0usize;
    // 网易云 add 逐首插到顶部（单次请求内也整体反转），先把 id 列表整体反转再分批。
    let ordered: Vec<u64> = song_ids.iter().rev().copied().collect();
    for chunk in ordered.chunks(ADD_BATCH) {
        let ids = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        // 注意：用 manipulate/tracks（op=add）。track/add 接口对网页 Cookie
        // 会话会返回 401「无权限操作歌单」，而 manipulate/tracks 正常。
        let q = Query::new()
            .param("op", "add")
            .param("pid", &playlist_id.to_string())
            .param("tracks", &ids);
        let r = client.playlist_tracks(&q).await?;
        capture_cookies(cookies, &r);
        match r.body["code"].as_i64().unwrap_or(0) {
            200 => added += chunk.len(),
            502 => skipped += chunk.len(), // 已在歌单中
            other => bail!("添加歌曲失败 code={other}: {}", r.body["message"]),
        }
    }
    Ok((added, skipped))
}

// ---------------------------------------------------------------------------
// Cookie 持久化（应用层）
// ---------------------------------------------------------------------------

fn load_cookie_map() -> Result<HashMap<String, String>> {
    let path = std::path::PathBuf::from(COOKIE_FILE);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content = std::fs::read_to_string(&path)?;
    Ok(parse_cookie_str(&content))
}

fn save_cookie_map(map: &HashMap<String, String>) -> Result<()> {
    let path = std::path::PathBuf::from(COOKIE_FILE);
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

/// 清空会话：登录前调用，避免旧（失效）Cookie 干扰服务端签发新登录态。
fn reset_session(client: &mut ApiClient, cookies: &mut HashMap<String, String>) {
    cookies.clear();
    client.set_cookie(String::new());
}

/// 兜底：某些登录响应不通过 Set-Cookie 而是把 token 放在 body，
/// 此时用 `MUSIC_U=<token>` 补上登录凭据。
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

fn capture_cookies(map: &mut HashMap<String, String>, r: &ApiResponse) {
    for header in &r.cookie {
        for (k, v) in parse_cookie_str(header) {
            if !is_cookie_attribute(&k) {
                map.insert(k, v);
            }
        }
    }
}

fn cookie_str(map: &HashMap<String, String>) -> String {
    map.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

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

fn is_cookie_attribute(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "path" | "domain" | "expires" | "max-age" | "secure" | "httponly"
            | "samesite" | "version" | "comment" | "discard" | "port" | "priority"
    )
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

/// 打印提示并读取标准输入一行（去除首尾空白）。EOF 时返回空串。
fn read_input(prompt: &str) -> Result<String> {
    use std::io::{BufRead, Write};
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// 从「链接 / 纯 ID」解析歌单 ID（短链请先手动打开取最终链接）。
fn resolve_playlist_input(input: &str) -> Result<u64> {
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

fn playlist_url(id: u64) -> String {
    format!("https://music.163.com/#/playlist?id={id}")
}

#[allow(dead_code)]
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
