
/// B 站模块错误。
#[derive(Debug)]
pub enum BiliError {
    /// HTTP 层错误（连接、TLS、超时等）。
    Http(reqwest::Error),
    /// B 站业务层错误（HTTP 200 但 code != 0）。
    Api { code: i64, message: String },
    /// 本地 IO（会话读写）。
    Io(std::io::Error),
    /// 二维码编码失败等本地错误。
    Local(String),
}

impl std::fmt::Display for BiliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BiliError::Http(e) => write!(f, "网络错误: {e}"),
            BiliError::Api { code, message } => write!(f, "B站接口错误 code={code}: {message}"),
            BiliError::Io(e) => write!(f, "本地IO错误: {e}"),
            BiliError::Local(s) => write!(f, "本地错误: {s}"),
        }
    }
}

impl std::error::Error for BiliError {}

impl From<reqwest::Error> for BiliError {
    fn from(e: reqwest::Error) -> Self {
        BiliError::Http(e)
    }
}

impl From<std::io::Error> for BiliError {
    fn from(e: std::io::Error) -> Self {
        BiliError::Io(e)
    }
}

impl From<qrcode::types::QrError> for BiliError {
    fn from(e: qrcode::types::QrError) -> Self {
        BiliError::Local(format!("二维码编码失败: {e}"))
    }
}

/// 便捷别名。
pub type BiliResult<T> = Result<T, BiliError>;
