//! 最小示例：使用库 API 抓取歌单并打印统计。
//!
//! 用法: cargo run --example quick_fetch -- [歌单链接或ID]
//!
//! 说明：底层 HTTP/加密由 `ncm-api-rs` 提供，本库负责强类型封装、
//! 歌单解析、Cookie 持久化与领域模型，GUI / 工具可像本示例一样直接调用。

use cloud_music_manager::ncm::{NcmClient, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let input = std::env::args().nth(1).unwrap_or_else(|| "3778678".to_string());
    let client = NcmClient::new()?;
    let (meta, songs) = client.fetch_playlist(&input).await?;
    println!("歌单: {}（{} 首）", meta.name, meta.track_count);
    let available = songs.iter().filter(|s| s.available).count();
    println!("实际抓取可用 {} 首，示例前 3 首：", available);
    for s in songs.iter().filter(|s| s.available).take(3) {
        println!("  {}", s.display());
    }
    Ok(())
}
