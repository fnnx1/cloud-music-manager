//! 统一错误类型。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("网络请求失败: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON 处理失败: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("无法识别的歌单链接或 ID: {input}")]
    BadInput { input: String },

    #[error("网易云接口返回错误 code={code}: {message}")]
    Api { code: i64, message: String },

    #[error("该操作需要先登录")]
    NotLoggedIn,

    #[error("{0}")]
    Msg(String),
}

impl Error {
    pub fn msg(message: impl Into<String>) -> Self {
        Error::Msg(message.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
