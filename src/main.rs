//! cloud-music-manager —— 网易云音乐歌单管理（API 对接演示 CLI）
//!
//! 目前提供四个子命令，用于验证 API 对接层的完整闭环：
//!
//! ```text
//!   fetch  <歌单链接|ID> [筛选选项]   抓取歌单 -> 本地筛选 -> 存 JSON 缓存
//!   login                           二维码登录并持久化 Cookie
//!   status                          查看当前登录状态
//!   push   <歌单|缓存文件> [选项]     登录 -> 创建新歌单 -> 批量加入歌曲
//! ```
//!
//! 筛选选项（可叠加，未来 GUI 会复用同一套模型）：
//!   --dedupe            按（歌手,歌名）去重
//!   --drop-vip          丢弃仅限 VIP 的歌曲
//!   --drop-unavailable  丢弃已下架的歌曲
//!   --min-seconds <N>   只保留时长 >= N 秒的歌曲
//!   --keyword <词>      按歌名/歌手/专辑关键词过滤
//!
//! 数据目录：`data/`（Cookie 与歌单缓存）。

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use cloud_music_manager::ncm::filter::{dedupe_by_id, SongFilter};
use cloud_music_manager::ncm::types::{PlaylistDump, PlaylistInfo, Song};
use cloud_music_manager::ncm::{
    playlist_url, AccountInfo, Error, NcmClient, QrPollStatus, Result,
};

const DATA_DIR: &str = "data";
const COOKIE_FILE: &str = "data/cookies.txt";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        return;
    }
    let cmd = args[0].as_str();
    let rest = &args[1..];
    let result = match cmd {
        "fetch" => cmd_fetch(rest).await,
        "login" => cmd_login(rest).await,
        "status" => cmd_status(rest).await,
        "push" => cmd_push(rest).await,
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => {
            eprintln!("未知命令: {other}\n");
            print_usage();
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("\n[错误] {e}");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// 子命令实现
// ---------------------------------------------------------------------------

async fn cmd_fetch(args: &[String]) -> Result<()> {
    let input = take_input(args)?;
    let filter = parse_filter(args);
    let _ = std::fs::create_dir_all(DATA_DIR);

    let client = build_client()?;
    println!("正在解析歌单: {input}");
    let (meta, songs) = client.fetch_playlist(&input).await?;

    print_playlist_meta(&meta);
    println!(
        "抓取到 {} 首（其中已下架 {} 首）",
        songs.len(),
        songs.iter().filter(|s| !s.available).count()
    );

    let kept = filter.apply(&songs);
    println!("筛选后保留 {} 首", kept.len());

    // 保存为本地 JSON 缓存（后续 push 可直接使用）
    let dump = PlaylistDump {
        meta: meta.clone(),
        songs: kept.clone(),
        fetched_at_ms: now_ms(),
    };
    let path = PathBuf::from(DATA_DIR).join(format!("playlist-{}.json", meta.id));
    let json = serde_json::to_string_pretty(&dump)?;
    std::fs::write(&path, json)?;
    println!("已保存到 {}", path.display());

    print_song_summary(&kept);
    Ok(())
}

async fn cmd_login(_args: &[String]) -> Result<()> {
    let mut client = build_client()?;
    let qr = client.qr_login_begin().await?;
    println!("请使用网易云音乐 App 扫码登录（90 秒内有效）：");
    println!("  {}\n", qr.url);
    match client
        .qr_login_poll(&qr.unikey, std::time::Duration::from_secs(90))
        .await?
    {
        QrPollStatus::Success(info) => {
            println!("登录成功：{} (uid={})", info.nickname, info.user_id);
            println!("Cookie 已保存到 {COOKIE_FILE}");
            Ok(())
        }
        QrPollStatus::Expired => Err(Error::msg("二维码已过期，请重试")),
        QrPollStatus::Timeout => Err(Error::msg("等待扫码超时")),
        QrPollStatus::Error { code, message } => Err(Error::Api { code, message }),
        _ => Err(Error::msg("扫码流程未完成")),
    }
}

async fn cmd_status(_args: &[String]) -> Result<()> {
    let client = build_client()?;
    if !client.has_login_cookie() {
        println!("未登录（{COOKIE_FILE} 不存在或缺少 MUSIC_U）");
        return Ok(());
    }
    match client.account_info().await {
        Ok(info) => {
            println!("已登录：{} (uid={})", info.nickname, info.user_id);
            Ok(())
        }
        Err(e) => {
            println!("Cookie 已失效：{e}");
            Ok(())
        }
    }
}

async fn cmd_push(args: &[String]) -> Result<()> {
    let input = take_input(args)?;
    let filter = parse_filter(args);
    let name_flag = arg_value(args, "--name");
    let privacy = has_flag(args, "--privacy");
    let _ = std::fs::create_dir_all(DATA_DIR);

    // 1) 登录（必要时引导扫码）
    let mut client = build_client()?;
    let me: AccountInfo = client.ensure_logged_in().await?;
    println!("已登录：{} (uid={})\n", me.nickname, me.user_id);

    // 2) 取得歌曲：本地缓存文件 或 实时抓取+筛选
    let (meta, mut songs, from_cache) = load_or_fetch_songs(&client, &input, &filter).await?;
    print_playlist_meta(&meta);

    // 推送前按 id 去重（同首歌只加一次），并剔除已下架歌曲
    let before = songs.len();
    songs.retain(|s| s.available);
    songs = dedupe_by_id(&songs);
    if songs.len() != before {
        println!("剔除已下架/重复后剩余 {} 首", songs.len());
    }
    if songs.is_empty() {
        return Err(Error::msg("没有可推送的歌曲"));
    }

    // 3) 创建新歌单
    let default_name = format!("{}-整理", meta.name);
    let new_name = name_flag.unwrap_or(default_name);
    println!("\n正在创建歌单「{new_name}」...");
    let new_id = client.create_playlist(&new_name, privacy).await?;
    println!("创建成功: {}", playlist_url(new_id));

    // 4) 批量添加
    let ids: Vec<u64> = songs.iter().map(|s| s.id).collect();
    let report = client.add_tracks(new_id, &ids).await?;
    println!(
        "添加完成：成功 {} 首，已存在跳过 {} 首",
        report.added, report.skipped
    );

    if !from_cache {
        // 把最终整理结果也保存一份缓存
        let dump = PlaylistDump {
            meta: meta.clone(),
            songs: songs.clone(),
            fetched_at_ms: now_ms(),
        };
        let path = PathBuf::from(DATA_DIR).join(format!("playlist-{}.json", meta.id));
        std::fs::write(&path, serde_json::to_string_pretty(&dump)?)?;
        println!("整理结果已保存到 {}", path.display());
    }
    println!("\n新歌单: {}", playlist_url(new_id));
    Ok(())
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

fn build_client() -> Result<NcmClient> {
    let mut client = NcmClient::new()?;
    if let Ok(ip) = std::env::var("NCM_REAL_IP")
        && !ip.trim().is_empty()
    {
        client.set_real_ip(ip.trim());
    }
    if let Err(e) = client.load_cookies(COOKIE_FILE) {
        eprintln!("[提示] 加载 Cookie 失败（可忽略）: {e}");
    }
    Ok(client)
}

/// 从本地缓存文件或实时抓取获得 (meta, songs, 是否来自缓存)。
async fn load_or_fetch_songs(
    client: &NcmClient,
    input: &str,
    filter: &SongFilter,
) -> Result<(PlaylistInfo, Vec<Song>, bool)> {
    // 若是已存在的 JSON 缓存文件，直接读取
    let path = PathBuf::from(input);
    if path.is_file() {
        let raw = std::fs::read_to_string(&path)?;
        let dump: PlaylistDump = serde_json::from_str(&raw)?;
        println!("使用本地缓存 {}（{} 首）", path.display(), dump.songs.len());
        return Ok((dump.meta, dump.songs, true));
    }
    println!("正在抓取歌单: {input}");
    let (meta, songs) = client.fetch_playlist(input).await?;
    let kept = filter.apply(&songs);
    Ok((meta, kept, false))
}

fn take_input(args: &[String]) -> Result<String> {
    args.iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .ok_or_else(|| Error::msg("缺少歌单链接/ID 参数"))
}

fn parse_filter(args: &[String]) -> SongFilter {
    SongFilter {
        drop_unavailable: has_flag(args, "--drop-unavailable"),
        drop_vip_only: has_flag(args, "--drop-vip"),
        min_seconds: arg_value(args, "--min-seconds").and_then(|v| v.parse::<u64>().ok()),
        keyword: arg_value(args, "--keyword"),
        dedupe: has_flag(args, "--dedupe"),
    }
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn print_playlist_meta(meta: &PlaylistInfo) {
    println!("\n────────────────────────────────────────────");
    println!("歌单：{}（ID {}）", meta.name, meta.id);
    if let Some(desc) = &meta.description {
        let desc = if desc.chars().count() > 80 {
            format!("{}…", desc.chars().take(80).collect::<String>())
        } else {
            desc.clone()
        };
        println!("简介：{desc}");
    }
    if let Some(creator) = &meta.creator_name {
        println!("创建者：{creator}");
    }
    let plays = meta
        .play_count
        .map(|p| format!("{:.1} 万", p as f64 / 10_000.0))
        .unwrap_or_else(|| "未知".to_string());
    println!("曲目数：{} | 播放量：{plays}", meta.track_count);
    if !meta.tags.is_empty() {
        println!("标签：{}", meta.tags.join(" / "));
    }
    println!("────────────────────────────────────────────");
}

/// 打印一份便于核对抓取结果的统计。
fn print_song_summary(songs: &[Song]) {
    if songs.is_empty() {
        println!("（无歌曲可统计）");
        return;
    }
    let total_ms: u64 = songs.iter().map(|s| s.duration_ms).sum();
    let vip = songs.iter().filter(|s| s.is_vip_only()).count();
    let with_year = songs.iter().filter(|s| s.publish_year().is_some()).count();
    println!(
        "总时长约 {:.1} 小时 | 其中 VIP 歌曲 {} 首 | {} 首已知发行年份",
        total_ms as f64 / 3_600_000.0,
        vip,
        with_year
    );

    // 年代分布
    let mut decades: std::collections::BTreeMap<i32, usize> = Default::default();
    for s in songs {
        if let Some(y) = s.publish_year() {
            *decades.entry(y / 10 * 10).or_insert(0) += 1;
        }
    }
    if !decades.is_empty() {
        let line = decades
            .iter()
            .map(|(d, n)| format!("{d}s: {n}"))
            .collect::<Vec<_>>()
            .join("  ");
        println!("发行年代分布: {line}");
    }
}

fn print_usage() {
    println!(
        r#"cloud-music-manager —— 网易云音乐歌单管理（API 演示）

用法: cloud-music-manager <命令> [参数]

命令:
  fetch <歌单链接|ID> [筛选选项]   抓取歌单 -> 本地筛选 -> 存 JSON 缓存
  login                           二维码登录并持久化 Cookie
  status                          查看当前登录状态
  push  <歌单|缓存文件> [选项]     登录 -> 创建新歌单 -> 批量加入歌曲
  help                            显示本帮助

筛选选项（fetch / push 可用）:
  --dedupe            按（歌手,歌名）去重，保留首现
  --drop-vip          丢弃仅限 VIP 的歌曲
  --drop-unavailable  丢弃已下架的歌曲
  --min-seconds <N>   只保留时长 >= N 秒的歌曲
  --keyword <词>       按歌名/歌手/专辑关键词过滤

push 额外选项:
  --name <新歌单名>    新歌单名称（默认「原歌单名-整理」）
  --privacy           创建为隐私歌单

示例:
  cloud-music-manager fetch "https://music.163.com/#/playlist?id=3778678" --dedupe --drop-vip
  cloud-music-manager push 3778678 --name "我的精选" --privacy
  cloud-music-manager push data/playlist-3778678.json --name "我的精选"
"#
    );
}
