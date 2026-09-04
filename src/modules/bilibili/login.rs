//! 扫码登录方法组（`impl BiliClient`）：二维码生成/矩阵渲染/轮询，
//! 成功时捕获 cookies 并落盘会话。

use std::collections::BTreeMap;

use super::client::{parse_query_params, BiliClient};
use super::error::{BiliError, BiliResult};
use super::models::{ApiEnvelope, QrGenerateResp, QrLoginStart, QrPoll, QrPollResp};
use super::util::parse_set_cookie;

impl BiliClient {

    // ---- 扫码登录 ----

    /// 生成登录二维码。
    pub fn generate_qrcode(&self) -> BiliResult<QrLoginStart> {
        let data: QrGenerateResp = self.get_data(
            "https://passport.bilibili.com/x/passport-login/web/qrcode/generate",
            "qrcode/generate",
        )?;
        Ok(QrLoginStart {
            qrcode_key: data.qrcode_key,
            url: data.url,
        })
    }

    /// 把二维码内容编码成 bool 矩阵（true = 深色模块），行优先，边长 = 行数 = 列数。
    /// UI 渲染时请自行留出约 4 模块的静区（quiet zone）并反色（深色前景）。
    pub fn qrcode_matrix(content: &str) -> BiliResult<Vec<Vec<bool>>> {
        let code = qrcode::QrCode::with_error_correction_level(
            content.as_bytes(),
            qrcode::EcLevel::M,
        )?;
        let width = code.width();
        let colors = code.to_colors();
        Ok(colors
            .chunks(width)
            .map(|row| row.iter().map(|c| *c == qrcode::Color::Dark).collect())
            .collect())
    }

    /// 轮询扫码登录结果。成功时自动捕获 cookies 并落盘（含 buvid）。
    pub fn poll_login(&mut self, qrcode_key: &str) -> BiliResult<QrPoll> {
        let url = "https://passport.bilibili.com/x/passport-login/web/qrcode/poll";
        let resp = self
            .http
            .get(url)
            .header(reqwest::header::COOKIE, self.cookie_header())
            .query(&[("qrcode_key", qrcode_key)])
            .send()?;

        // 先捕获 Set-Cookie（成功时才有 SESSDATA/bili_jct/DedeUserID）。
        let mut cookies = BTreeMap::new();
        for value in resp.headers().get_all(reqwest::header::SET_COOKIE) {
            if let Ok(sc) = value.to_str() {
                cookies.extend(parse_set_cookie(sc));
            }
        }

        let status = resp.status().as_u16();
        let text = resp.text()?;
        let env: ApiEnvelope<QrPollResp> = serde_json::from_str(&text).map_err(|e| {
            BiliError::Local(format!("poll 响应解析失败: {e}; body[:200]={}", &text[..text.len().min(200)]))
        })?;
        let data = env
            .data
            .ok_or_else(|| BiliError::Local("poll 响应缺少 data".into()))?;

        if env.code != 0 {
            return Err(BiliError::Api {
                code: env.code,
                message: env.message,
            });
        }
        if status >= 400 {
            return Err(BiliError::Api {
                code: status as i64,
                message: format!("poll HTTP {status}"),
            });
        }

        match data.code {
            0 => {
                // Set-Cookie 失败时兜底：成功响应的 data.url 里也带了全部登录参数。
                for (k, v) in parse_query_params(&data.url) {
                    cookies.entry(k).or_insert(v);
                }
                let mid: u64 = cookies
                    .get("DedeUserID")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                // 合并进会话并落盘（落盘尽力而为，不因 IO 失败判定登录失败）。
                for (k, v) in &cookies {
                    self.session.set(k.clone(), v.clone());
                }
                let _ = self.persist_session();
                Ok(QrPoll::Success { cookies, mid })
            }
            86038 => Ok(QrPoll::Expired),
            86090 => Ok(QrPoll::WaitingConfirm),
            86101 => Ok(QrPoll::WaitingScan),
            other => Err(BiliError::Api {
                code: other,
                message: data.message,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qrcode_matrix_shape() {
        let m = BiliClient::qrcode_matrix("https://passport.bilibili.com/x?qrcode_key=test123").unwrap();
        assert!(!m.is_empty());
        assert_eq!(m.len(), m[0].len(), "矩阵应为正方形");
        let dark = m.iter().flatten().filter(|b| **b).count();
        assert!(dark > 0, "二维码必须有深色模块");
        // 找回三个定位角的实心块（左上角 7x7 内应有大量深色）。
        let corner_dark = m[0][0] && m[0][6] && m[6][0] && m[3][3];
        assert!(corner_dark, "定位角应存在");
    }
}
