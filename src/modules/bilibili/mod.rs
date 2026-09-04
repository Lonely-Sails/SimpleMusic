//! B 站数据源模块：HTTP 基座 / 扫码登录 / 收藏夹 / BV 解析 / playurl 音频流提取。
//!
//! 网络层：`reqwest` blocking + rustls（无 openssl 依赖）。cookies 手动管理
//! （`Cookie` 头逐请求拼接），持久化交给 [`crate::modules::storage::BiliSession`]。
//!
//! 安全约定：任何日志/Debug 输出不得包含 SESSDATA/bili_jct（storage 层 Debug 已脱敏）。
//!
//! 子模块分工（`BiliClient` 的方法按内聚职责拆成多个 `impl` 块）：
//! - [`error`]：[`BiliError`] / [`BiliResult`]；
//! - [`models`]：对外数据模型 + API 响应结构体；
//! - [`wbi`]：WBI 签名（mixin key / md5 / `w_rid`）；
//! - [`client`]：`BiliClient` 基座（HTTP 构建、会话、信封解包、WBI key 缓存）；
//! - [`login`]：扫码登录（generate / matrix / poll）；
//! - [`fav`]：收藏夹列表与资源分页；
//! - [`resolve`]：BV 解析、video_info、playurl 音流提取、「识别音乐」；
//! - [`util`]：纯函数工具（音质选择 / token 扫描 / Set-Cookie 解析…）。
//!
//! 公共 API 速览：
//! - 登录：[`BiliClient::generate_qrcode`] / [`BiliClient::qrcode_matrix`]
//!   / [`BiliClient::poll_login`] / [`BiliClient::logged_in`] / [`BiliClient::logout`]
//! - 收藏夹：[`BiliClient::list_favorite_folders`] / [`BiliClient::list_favorite_resources`]
//! - BV：[`BiliClient::parse_bvid`] / [`BiliClient::video_info`] / [`BiliClient::resolve_stream`]
//! - WBI：[`WbiKeys`] / [`wbi_sign_params`] / [`mixin_key`]
//!
//! 注意：`qrcode` crate 目前最新稳定版是 0.14.1（不存在 1.x），本模块使用 0.14.1
//! `default-features = false`（不需要 image/svg 渲染，只要 bool 矩阵）。

mod client;
mod error;
mod fav;
mod login;
mod models;
mod resolve;
mod util;
mod wbi;

pub use client::BiliClient;
pub use error::{BiliError, BiliResult};
pub use models::{
    Dash, DashStream, DurlEntry, FavFolder, FavItem, MusicHint, NavUser, PlayUrlData, PlayUrlResp,
    QrLoginStart, QrPoll, StreamUrl, VideoDetail, VideoInfo,
};
pub use util::pick_dash_audio;
pub use wbi::{
    encode_uri_component, md5_hex, mixin_key, wbi_key_from_url, wbi_sign_params,
    wbi_sign_params_with_wts, WbiKeys,
};

/// B 站接口普遍校验的桌面 Chrome UA。
pub const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
/// 音频流/接口请求的 Referer（缺失会被 CDN 拒绝 403）。
pub const REFERER: &str = "https://www.bilibili.com/";
/// Origin 头（部分接口校验）。
pub const ORIGIN: &str = "https://www.bilibili.com";
