//! 网易云音乐 API 对接层。
//!
//! - [`api`]：业务封装（读取 / 登录 / 写操作），底层请求与加密由
//!   [`ncm_api_rs`](https://docs.rs/ncm-api-rs)（SPlayer-Dev）提供
//! - [`types`]：领域模型与原始 DTO
//! - [`filter`]：本地「播放器式」筛选规则（后续可扩展为完整筛选引擎）
//! - [`error`]：错误类型

pub mod api;
pub mod error;
pub mod filter;
pub mod types;

pub use api::{
    parse_playlist_id, playlist_url, NcmClient, QrLogin, QrPollStatus, TrackAddReport,
};
pub use error::{Error, Result};
pub use types::{AccountInfo, PlaylistDump, PlaylistInfo, Song};
