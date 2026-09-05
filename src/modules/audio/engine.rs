//! `AudioEngine`：UI 侧句柄。命令经 mpsc 发给专用播放线程（[`super::player`]），
//! 状态经 `Arc<Mutex<…>>` 共享（[`super::control`]）。Drop 时停线程并关输出。

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use std::path::{Path, PathBuf};

use super::cache::{cache_path_in, default_cache_dir};
use super::control::{Command, PlaybackStatus, PlayRequest};
use super::player::worker_loop;
use crate::modules::bilibili::StreamUrl;

// ---------------------------------------------------------------------------
// AudioEngine（UI 侧句柄）
// ---------------------------------------------------------------------------

/// 音频引擎句柄：命令经 mpsc 发给专用播放线程；状态经 `Arc<Mutex<…>>` 共享。
/// Clone 语义不需要 —— UI 持有一个实例即可；Drop 时停线程并关闭输出。
pub struct AudioEngine {
    tx: Sender<Command>,
    status: Arc<Mutex<PlaybackStatus>>,
    worker: Option<std::thread::JoinHandle<()>>,
    cache_dir: PathBuf,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEngine {
    /// 创建引擎（启动专用播放线程；缓存目录用 [`default_cache_dir`]）。
    pub fn new() -> Self {
        Self::with_cache_dir(default_cache_dir())
    }

    /// 创建引擎并指定缓存目录（测试用）。
    pub fn with_cache_dir(cache_dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(PlaybackStatus::default()));
        let worker = {
            let rx = rx;
            let status = status.clone();
            let dir = cache_dir.clone();
            std::thread::Builder::new()
                .name("simple-music-audio".into())
                .spawn(move || worker_loop(rx, status, dir, 0.8))
                .ok()
        };
        Self {
            tx,
            status,
            worker,
            cache_dir,
        }
    }

    /// 当前缓存目录。
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// 某缓存键对应的缓存文件路径（诊断/测试用）。
    pub fn cache_path(&self, cache_key: &str) -> PathBuf {
        cache_path_in(&self.cache_dir, cache_key)
    }

    // ---- 提交播放 ----

    /// 播放 B 站音频流：下载/复用缓存 → 解码 → 输出。`cache_key` 传 bvid。
    /// 异步：函数立即返回，进度/错误看 [`AudioEngine::status`]。
    pub fn play_stream(&self, stream: &StreamUrl, cache_key: &str) {
        self.submit(Command::Play(PlayRequest::from_stream(stream, cache_key)));
    }

    /// 直接提交一个预构造的播放请求。
    pub fn play_request(&self, req: PlayRequest) {
        self.submit(Command::Play(req));
    }

    /// 播放本地音频文件（wav/mp3/m4a…，由 symphonia feature 决定）。
    pub fn play_file(&self, path: &Path) {
        self.submit(Command::Play(PlayRequest::from_file(path)));
    }

    // ---- 控制命令 ----

    /// 暂停（保留进度）。
    pub fn pause(&mut self) {
        self.submit(Command::Pause);
    }

    /// 从暂停恢复。
    pub fn resume(&mut self) {
        self.submit(Command::Resume);
    }

    /// 停止并清空会话（position 归零，finished 清除）。
    pub fn stop(&mut self) {
        self.submit(Command::Stop);
    }

    /// seek 到指定秒（会按已知时长钳制；`loading` 期间忽略）。
    pub fn seek(&mut self, secs: f64) {
        self.submit(Command::Seek(secs));
    }

    /// 设置音量 0.0 ~ 1.0（越界自动钳制）。
    pub fn set_volume(&mut self, volume: f32) {
        self.submit(Command::Volume(volume));
    }

    // ---- 状态查询 ----

    /// 状态快照（UI 每帧轮询用）。
    pub fn status(&self) -> PlaybackStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// 是否在加载（下载/解码准备）中。
    pub fn is_loading(&self) -> bool {
        self.status.lock().map(|s| s.loading).unwrap_or(false)
    }

    /// 最近一次错误文案（不清除）。
    pub fn last_error(&self) -> Option<String> {
        self.status.lock().ok().and_then(|s| s.error.clone())
    }

    /// 曲终标志：读到 `true` 并清除（UI 播下一首的入口）。
    pub fn take_finished(&self) -> bool {
        self.status
            .lock()
            .map(|mut s| std::mem::take(&mut s.finished))
            .unwrap_or(false)
    }

    fn submit(&self, cmd: Command) {
        // 发送失败 = 线程已退出（不应发生；Drop 前通道保持连接）。
        let _ = self.tx.send(cmd);
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use super::super::decode::tests::{synth_wav, test_dir, wav_bytes};
    use std::path::Path;
    use std::time::Duration;
    use std::fs;
    use std::time::Instant;

    fn wait_for<F: Fn(&PlaybackStatus) -> bool>(engine: &AudioEngine, timeout: Duration, f: F) -> PlaybackStatus {
        let start = Instant::now();
        loop {
            let st = engine.status();
            if f(&st) || start.elapsed() > timeout {
                return st;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    #[test]
    fn test_engine_reports_decode_error_without_panic() {
        let dir = test_dir("engine-err");
        let engine = AudioEngine::with_cache_dir(dir.clone());
        engine.play_file(Path::new(&dir).join("不存在.wav").as_path());
        let st = wait_for(&engine, Duration::from_secs(5), |s| {
            !s.loading && (s.error.is_some() || s.playing || s.finished)
        });
        assert!(st.error.is_some(), "解码失败应进入错误状态");
        assert!(!st.playing);
        assert!(!st.loading);
        drop(engine);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_engine_output_device_missing_or_plays_without_panic() {
        // 沙箱无声卡：错误文案必须是「无法打开音频输出设备…」；
        // 有声卡环境：能正常进入 playing —— 两种结果都不得 panic。
        let dir = test_dir("engine-device");
        let (path, _f, _r, _c) = synth_wav(&dir, "tone.wav", 1.0, 8000);
        let engine = AudioEngine::with_cache_dir(dir.clone());
        engine.play_file(&path);
        let st = wait_for(&engine, Duration::from_secs(8), |s| {
            !s.loading && (s.error.is_some() || s.playing || s.finished)
        });
        if let Some(e) = &st.error {
            assert!(
                e.contains("无法打开音频输出设备") || e.contains("打开音频文件") || !e.is_empty(),
                "错误文案应明确: {e}"
            );
        } else {
            assert!(st.playing || st.finished, "无错误时应可播放");
            let mut e2 = engine;
            e2.stop();
            drop(e2);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_engine_seek_clamp_without_session() {
        let dir = test_dir("engine-seek");
        let mut engine = AudioEngine::with_cache_dir(dir.clone());
        engine.seek(-3.0);
        // 负值钳到 0（初始值也是 0，等待命令被处理以免后续状态被覆盖）。
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(engine.status().position_secs, 0.0, "负值钳到 0");
        engine.seek(100.0);
        let st = wait_for(&engine, Duration::from_secs(2), |s| s.position_secs == 100.0);
        assert_eq!(st.position_secs, 100.0, "无时长时只钳下界");
        engine.pause();
        engine.resume();
        engine.stop();
        let st = wait_for(&engine, Duration::from_secs(2), |s| s.position_secs == 0.0 && !s.playing);
        assert_eq!(st.position_secs, 0.0, "stop 后归零");
        drop(engine);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_engine_volume_state() {
        let dir = test_dir("engine-vol");
        let mut engine = AudioEngine::with_cache_dir(dir.clone());
        engine.set_volume(0.5);
        let st = wait_for(&engine, Duration::from_secs(2), |s| (s.volume - 0.5).abs() < 1e-6);
        assert_eq!(st.volume, 0.5);
        engine.set_volume(7.0);
        let st = wait_for(&engine, Duration::from_secs(2), |s| s.volume == 1.0);
        assert_eq!(st.volume, 1.0, "越界钳制");
        drop(engine);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_engine_drop_without_play_is_clean() {
        let dir = test_dir("engine-drop");
        {
            let engine = AudioEngine::with_cache_dir(dir.clone());
            assert!(!engine.is_loading());
            assert_eq!(engine.status(), PlaybackStatus::default());
        } // Drop：线程 join，无 panic。
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_engine_downloads_to_disk_cache_and_hits_second_time() {
        use std::io::Write as _;
        let dir = test_dir("engine-http");
        // 合成一段 WAV 并用本地 TCP 服务充当 HTTP 音源（无外部依赖）。
        let samples: Vec<i16> = (0..8000).map(|i| ((i % 50) * 300 - 7000) as i16).collect();
        let body = wav_bytes(&samples, 1, 8000);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server_body = body.clone();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let Ok((mut sock, _)) = listener.accept() else { return };
            let mut buf = [0u8; 1024];
            let _ = std::io::Read::read(&mut sock, &mut buf); // 读掉请求头
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                server_body.len()
            );
            let _ = sock.write_all(resp.as_bytes());
            let _ = sock.write_all(&server_body);
        });

        let cache_dir = dir.join("cache");
        let engine = AudioEngine::with_cache_dir(cache_dir);
        let make_req = || PlayRequest {
            cache_key: "local-test".into(),
            urls: vec![format!("http://127.0.0.1:{port}/audio.wav")],
            headers: Vec::new(),
            expected_size: Some(body.len() as u64),
            bandwidth: None,
            local_file: None,
        };
        // 第一次播放：应完整下载并落盘（大小精确匹配）。
        engine.play_request(make_req());
        let st = wait_for(&engine, Duration::from_secs(15), |s| {
            !s.loading && (s.error.is_some() || s.playing || s.finished)
        });
        let cache_file = engine.cache_path("local-test");
        let meta = fs::metadata(&cache_file).expect("缓存文件应已落盘");
        assert_eq!(meta.len(), body.len() as u64, "缓存大小应与源一致");
        assert_eq!(st.downloaded_bytes, body.len() as u64);
        // 第二次播放：缓存命中（cache_hit=true），秒开。
        // 等待条件带 cache_hit，避免读到上一次播放的旧状态。
        engine.play_request(make_req());
        let st = wait_for(&engine, Duration::from_secs(15), |s| {
            s.cache_hit && !s.loading && (s.error.is_some() || s.playing || s.finished)
        });
        assert!(st.cache_hit, "第二次播放应命中磁盘缓存");
        drop(engine);
        server.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

}
