//! [`mixin_key`] 置换出 32 位混合 key → query 参数按字典序拼接后与 `wts`
//! （当前秒级时间戳）一起 md5，得到 `w_rid`。

use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// WBI 签名（官方 web 前端用的 query 签名；部分接口/风控下必需）
// ---------------------------------------------------------------------------

/// WBI 使用的 64 位置换表（来源：bilibili-API-collect，web 端 wbi 签名）。
const MIXIN_KEY_ENC_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// 由 nav 接口拿到的 img/sub key（各 32 位 hex）派生 32 位 mixin key。
/// 纯函数：`mixin_key[i] = (img + sub)[TAB[i]]`。
pub fn mixin_key(img_key: &str, sub_key: &str) -> String {
    let concat: Vec<char> = format!("{img_key}{sub_key}").chars().collect();
    MIXIN_KEY_ENC_TAB
        .iter()
        .filter_map(|&i| concat.get(i))
        .take(32)
        .collect()
}

/// 从 `wbi_img.img_url / sub_url`（如 `https://i0.hdslb.com/bfs/wbi/xxxx.png`）
/// 提取文件名（去扩展名）得到 img_key / sub_key。
pub fn wbi_key_from_url(url: &str) -> String {
    let file = url.rsplit('/').next().unwrap_or("");
    let stem = file.split('.').next().unwrap_or("");
    stem.to_string()
}

/// 等价于 JS `encodeURIComponent`（WBI 签名要求该编码方式，与表单编码不同：
/// 空格 -> %20；`!'()*-._~` 等不转义）。
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'!' | b'*'
            | b'\'' | b'(' | b')' => out.push(b as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// 计算 WBI 签名：向 `params` 追加 `wts` 并返回 `w_rid`。
/// 约定：`params` 是 query 的键值对（顺序任意，函数内部会排序）。
/// 返回 `(wts, w_rid)`，调用方把它们追加进真正的请求 query。
pub fn wbi_sign_params(params: &mut Vec<(String, String)>, mixin_key: &str) -> (u64, String) {
    let wts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let w_rid = wbi_sign_params_with_wts(params, mixin_key, wts);
    (wts, w_rid)
}

/// 给定固定 `wts` 的签名（测试用，生产走 [`wbi_sign_params`]）。
pub fn wbi_sign_params_with_wts(
    params: &mut Vec<(String, String)>,
    mixin_key: &str,
    wts: u64,
) -> String {
    params.push(("wts".to_string(), wts.to_string()));
    // 按 key 的 ASCII 升序排序；value 过滤 WBI 特殊字符（!'()*）。
    let mut pairs: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (k.clone(), v.chars().filter(|c| !"!'()*".contains(*c)).collect()))
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let query = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode_uri_component(k), encode_uri_component(v)))
        .collect::<Vec<_>>()
        .join("&");
    md5_hex(format!("{query}{mixin_key}"))
}

/// 小写十六进制 MD5。
pub fn md5_hex(s: impl AsRef<[u8]>) -> String {
    let digest = md5::compute(s.as_ref());
    let mut out = String::with_capacity(32);
    for b in digest.0 {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// WBI key 集合（nav 接口获取）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WbiKeys {
    pub img_key: String,
    pub sub_key: String,
}

impl WbiKeys {
    /// 从 nav 响应构造。
    pub fn from_urls(img_url: &str, sub_url: &str) -> Self {
        Self {
            img_key: wbi_key_from_url(img_url),
            sub_key: wbi_key_from_url(sub_url),
        }
    }

    /// 派生当前 mixin key（随 B 站更新轮换，理论上最多 24h 缓存）。
    pub fn mixin_key(&self) -> String {
        mixin_key(&self.img_key, &self.sub_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WBI 文档通用示例：img/sub 各 32 位 hex。
    const WBI_TEST_IMG: &str = "7cd084941338484aae1ad9425b84077c";
    const WBI_TEST_SUB: &str = "4932caff0ff746eab6f01bf08b70ac45";

    #[test]
    fn test_mixin_key_properties() {
        let key = mixin_key(WBI_TEST_IMG, WBI_TEST_SUB);
        assert_eq!(key.chars().count(), 32, "mixin key 固定 32 位");
        // 确定性：重复计算一致。
        assert_eq!(key, mixin_key(WBI_TEST_IMG, WBI_TEST_SUB));
        // 只会取自 img/sub 的字符集合。
        let alphabet: String = format!("{WBI_TEST_IMG}{WBI_TEST_SUB}").chars().collect();
        assert!(key.chars().all(|c| alphabet.contains(c)));
        // 快照（独立脚本按同一置换表计算并核对；正确性另由真实 playurl 签名
        // 请求终验，见 examples/bili_probe.rs —— B 站校验失败会直接返回 -403）。
        assert_eq!(key, "ea1db124af3c7062474693fa704f4ff8");
    }

    #[test]
    fn test_wbi_key_from_url() {
        assert_eq!(
            wbi_key_from_url("https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png"),
            WBI_TEST_IMG
        );
        assert_eq!(wbi_key_from_url("4932caff0ff746eab6f01bf08b70ac45.webp"), WBI_TEST_SUB);
    }

    #[test]
    fn test_encode_uri_component() {
        assert_eq!(encode_uri_component("a b"), "a%20b");
        assert_eq!(encode_uri_component("a+b"), "a%2Bb");
        assert_eq!(encode_uri_component("!'()*-._~"), "!'()*-._~");
        assert_eq!(encode_uri_component("中"), "%E4%B8%AD");
        assert_eq!(encode_uri_component("a=b&c"), "a%3Db%26c");
    }

    #[test]
    fn test_md5_hex_known_vectors() {
        assert_eq!(md5_hex(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex("hello"), "5d41402abc4b2a76b9719d911017c592");
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn test_wbi_sign_params_with_fixed_wts() {
        let key = mixin_key(WBI_TEST_IMG, WBI_TEST_SUB);
        let mut params = vec![
            ("foo".to_string(), "one two".to_string()),
            ("zoo".to_string(), "12".to_string()),
            ("bar".to_string(), "!'()*".to_string()),
        ];
        let w_rid = wbi_sign_params_with_wts(&mut params, &key, 1_700_000_000);
        assert_eq!(w_rid.len(), 32);
        assert!(w_rid.chars().all(|c| c.is_ascii_hexdigit()));
        // wts 已被追加进 params（供调用方拼 query）。
        assert!(params.iter().any(|(k, v)| k == "wts" && v == "1700000000"));
        // 同输入同输出（不含 wts 时值里的特殊字符被过滤）。
        let mut params2 = params.clone();
        params2.pop(); // 去掉 wts
        assert_eq!(wbi_sign_params_with_wts(&mut params2, &key, 1_700_000_000), w_rid);
    }
}
