//! cloud-music-manager 库入口。
//!
//! 目前只暴露网易云 API 对接层 [`ncm`]，未来可在此增加歌单整理、筛选引擎
//! 等模块，供 CLI / GUI 共同复用。

pub mod ncm;
