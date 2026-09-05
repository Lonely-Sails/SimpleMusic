//! 引擎与 UI 之间的共享状态与命令协议：`PlaybackStatus`（UI 只读轮询）、
//! `Command`（mpsc 命令）、`PlayRequest`（一次播放任务的完整描述）。

use std::path::PathBuf;

use crate::modules::bilibili::StreamUrl;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlaybackStatus {
    /// 下载/解码准备中（play 已接受，尚未开始出声）。
    pub loading: bool,
    /// 正在出声（未暂停、未结束、无错误）。
    pub playing: bool,
    /// 自然播放到结尾（与 stop 区分；UI 读后可用 take_finished 清除）。
    pub finished: bool,
    /// 当前播放位置（秒，按已输出帧数累计）。
    pub position_secs: f64,
    /// 曲目时长估计（秒）；完全未知时为 0。
    pub duration_secs: f64,
    /// 音量 0.0 ~ 1.0。
    pub volume: f32,
    /// 下载进度：已下载字节。
    pub downloaded_bytes: u64,
    /// 下载进度：总字节（Content-Length 或 StreamUrl.size_bytes；未知为 None）。
    pub total_bytes: Option<u64>,
    /// 错误状态（下载/解码/输出设备）；非 None 时 UI 可直接展示。
    pub error: Option<String>,
    /// 本次播放是否直接命中磁盘缓存（诊断用）。
    pub cache_hit: bool,
}

// ---------------------------------------------------------------------------
// 播放请求
// ---------------------------------------------------------------------------

/// 一次播放任务描述。
#[derive(Debug, Clone)]
pub struct PlayRequest {
    /// 缓存键。建议传 bvid —— 直链带签名参数每次解析都会变，用 bvid 才能命中缓存秒开。
    pub cache_key: String,
    /// 主音频直链 + 备用 CDN 地址（403/410/5xx 时按序尝试）。
    pub urls: Vec<String>,
    /// 下载必须携带的 HTTP 头（StreamUrl.required_headers：UA/Referer/Cookie）。
    pub headers: Vec<(String, String)>,
    /// 音频文件期望大小（字节），用于缓存校验与 total_bytes 展示。
    pub expected_size: Option<u64>,
    /// 音频码率（bps），用于在容器读不出时长时按 size/bandwidth 估算时长。
    pub bandwidth: Option<i64>,
    /// 直接播放本地文件（测试/本地音乐）；设置后跳过网络下载。
    pub local_file: Option<PathBuf>,
}

impl PlayRequest {
    /// 从 B 站解析结果构造播放请求。`cache_key` 传 bvid（不要传直链）。
    pub fn from_stream(stream: &StreamUrl, cache_key: &str) -> Self {
        let mut urls = vec![stream.audio_url.clone()];
        urls.extend(stream.audio_backup_urls.iter().cloned());
        Self {
            cache_key: cache_key.to_string(),
            urls,
            headers: stream.required_headers.clone(),
            expected_size: stream.size_bytes.map(|s| s.max(0) as u64),
            bandwidth: stream.bandwidth,
            local_file: None,
        }
    }

    /// 播放本地文件（无需网络）。
    pub fn from_file(path: impl Into<PathBuf>) -> Self {
        let p = path.into();
        Self {
            cache_key: p.display().to_string(),
            urls: Vec::new(),
            headers: Vec::new(),
            expected_size: None,
            bandwidth: None,
            local_file: Some(p),
        }
    }
}

// ---------------------------------------------------------------------------
// 命令通道
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(super) enum Command {
    Play(PlayRequest),
    Pause,
    Resume,
    Stop,
    Seek(f64),
    Volume(f32),
    Shutdown,
}

// ---------------------------------------------------------------------------
// 媒体数据来源（磁盘文件或内存缓冲）
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::cache::cache_path_in;
    use crate::modules::bilibili::md5_hex;
    use std::path::Path;

    #[test]
    fn test_play_request_from_stream() {
        let stream = StreamUrl {
            audio_url: "https://upos.example.com/a.m4s?e=1".into(),
            video_url: None,
            ttl_secs: 300,
            audio_id: Some(30232),
            audio_codec: Some("mp4a.40.2".into()),
            bandwidth: Some(155622),
            size_bytes: Some(40000000),
            audio_backup_urls: vec!["https://backup.example.com/a.m4s".into()],
            required_headers: vec![
                ("User-Agent".into(), "UA".into()),
                ("Referer".into(), "https://www.bilibili.com/".into()),
            ],
            signed_with_wbi: false,
        };
        let req = PlayRequest::from_stream(&stream, "BV1xx411c7mD");
        assert_eq!(req.cache_key, "BV1xx411c7mD");
        assert_eq!(req.urls.len(), 2, "主地址 + 备用地址");
        assert_eq!(req.urls[0], stream.audio_url);
        assert_eq!(req.expected_size, Some(40000000));
        assert_eq!(req.bandwidth, Some(155622));
        assert_eq!(req.headers.len(), 2);
        assert!(req.local_file.is_none());
        // 缓存键 → md5 路径。
        let p = cache_path_in(Path::new("/tmp/c"), &req.cache_key);
        assert!(p.to_string_lossy().contains(&md5_hex("BV1xx411c7mD")));
    }
}
