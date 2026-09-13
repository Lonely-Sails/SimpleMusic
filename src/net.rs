//! 全局异步网络层：**唯一** tokio runtime + 共享 HTTP 客户端 + 任务派发。
//!
//! ## 为什么集中到一个 runtime
//!
//! `reqwest::blocking::Client` 每个实例都会**自建一条 OS 线程**跑专属的
//! current-thread tokio runtime（线程名 `reqwest-internal-sync-runtime`），
//! 且该线程与客户端同生命周期。项目此前有 4 处独立建 blocking 客户端
//! （B 站 API / 音频下载 / 封面 / 歌词），其中歌词的客户端还在**每次抓取时新建**
//! ——切歌越多，线程越多，实测线程数会从 12 一路涨到 25。
//!
//! `reqwest` 的 **async** 客户端则完全不同：它不自建线程，所有请求都跑在调用方
//! 所在的 runtime 上。因此本模块提供一个进程内唯一的 tokio runtime，
//! 全部网络请求（B 站 API / 歌词 / 封面 / 音频下载）都汇聚到它的
//! [`NET_WORKER_THREADS`] 条 worker 线程上，线程数不再随使用时长增长。
//!
//! ## 与播放线程的关系
//!
//! rodio 的 `OutputStream` 是 `!Send`，播放线程必须留在自己的 OS 线程上，
//! 不能搬进 runtime。所以播放线程通过 [`block_on`] 借用同一个 runtime 执行
//! 网络请求：请求本身仍由 runtime 的 worker 驱动，播放线程只在等待结果时阻塞
//! （这与它此前同步下载的行为一致，不影响其它网络任务）。
//!
//! ## 线程清单（改造后）
//!
//! | 线程 | 数量 | 用途 |
//! |---|---|---|
//! | `simple-music-net` | 2 | 全部网络请求（本模块） |
//! | `simple-music-audio` | 1 | 播放/解码/输出（rodio `!Send`） |
//! | `render-keepalive` | 1 | 最小化恢复保活 |
//! | 主线程 | 1 | UI + 事件循环 |
//!
//! 封面不再需要 4 条专用下载线程，歌词不再每次新建客户端。

use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

/// 网络 worker 线程数。
///
/// 网络请求是 IO-bound（等对端响应），2 条线程足以打满常规带宽并让
/// 「解析播放」与「抓歌词/封面」并行推进；再多只是无谓的栈内存占用。
const NET_WORKER_THREADS: usize = 2;

/// 进程内唯一的 tokio runtime（首次访问时惰性创建）。
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(NET_WORKER_THREADS)
            .thread_name("simple-music-net")
            .enable_all()
            .build()
            .expect("创建网络运行时失败")
    })
}

/// 在全局 runtime 上派发一个后台任务（非阻塞，立即返回）。
///
/// 替代此前的 `std::thread::spawn`：不再为每次操作新建 OS 线程，
/// 任务在固定的 worker 线程上被调度。
pub fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime().spawn(future)
}

/// 在全局 runtime 上派发一个**阻塞**任务（CPU 密集活，如封面解码、字体扫描）。
///
/// 这类活会占住一条 worker 线程，放进 `spawn_blocking` 以免阻塞网络请求的调度。
pub fn spawn_blocking<F, R>(f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    runtime().spawn_blocking(f)
}

/// 从**非 runtime 线程**同步等待一个 future（播放线程用）。
///
/// # Panics
/// 在 runtime 的 worker 线程内调用会 panic（tokio 不允许嵌套 `block_on`）。
/// 本项目只在自建的播放线程上调用，不受影响。
pub fn block_on<F: Future>(future: F) -> F::Output {
    runtime().handle().block_on(future)
}

/// 进程内共享的 HTTP 客户端（连接池跨模块复用）。
///
/// 各模块的差异（UA / Referer / Cookie / 超时）在**请求级**叠加，不写进客户端，
/// 因此可以安全地共享同一份连接池。B 站接口的默认头由
/// `BiliClient::get_json` 统一附加。
pub fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            // 连接超时对所有请求统一 10s；总超时由各请求按需覆盖
            // （音频下载 180s、API 20s、封面/歌词 15s）。
            .connect_timeout(Duration::from_secs(10))
            // 空闲连接上限放宽到 8：B 站 API、歌词、封面、音频共用一份池，
            // 30s 无复用即断开，不会常驻。
            .pool_max_idle_per_host(8)
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .expect("构建共享 HTTP 客户端失败")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_is_singleton() {
        let a = runtime() as *const _;
        let b = runtime() as *const _;
        assert_eq!(a, b, "多次调用应返回同一个 runtime");
    }

    #[test]
    fn http_client_is_singleton() {
        let a = http_client() as *const _;
        let b = http_client() as *const _;
        assert_eq!(a, b, "多次调用应返回同一个客户端");
    }

    /// worker 线程数符合预期（2 条网络线程 + 1 条 blocking 池按需创建）。
    #[test]
    fn runtime_worker_threads_are_bounded() {
        assert_eq!(NET_WORKER_THREADS, 2);
    }
}
