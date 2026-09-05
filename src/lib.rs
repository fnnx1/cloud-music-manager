//! cloud-music-manager 库入口。
//!
//! 只包含产品自身的逻辑，**不包含任何网易云 API 对接代码**（网络请求与加密
//! 全部由 `ncm-api-rs` crate 承担，见各 bin/example）：
//!
//! - [`model`]：归一化领域模型（歌单/歌曲），可直接从 crate 返回的 JSON 构建
//! - [`filter`]：本地「播放器式」筛选规则

pub mod filter;
pub mod model;
