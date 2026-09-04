//! 收藏夹方法组（`impl BiliClient`）：用户创建/收藏的收藏夹列表与资源分页。

use super::client::BiliClient;
use super::error::{BiliError, BiliResult};
use super::models::{FolderListResp, FavFolder, FavItem, ResourceListResp};
use super::util::dedup_folders;

impl BiliClient {
    /// 列出当前登录用户的全部收藏夹（创建 + 收藏，创建的在前）。
    pub fn list_favorite_folders(&self) -> BiliResult<Vec<FavFolder>> {
        let mid = self.mid().ok_or_else(|| BiliError::Api {
            code: -101,
            message: "未登录（缺少 DedeUserID）".into(),
        })?;
        let mut folders = self.list_folder_pages("created", mid)?;
        folders.extend(self.list_folder_pages("collected", mid).unwrap_or_default());
        Ok(dedup_folders(folders))
    }

    /// 分页拉取一类收藏夹（`api` = created / collected），ps 上限 20，最多翻 50 页。
    fn list_folder_pages(&self, api: &str, mid: u64) -> BiliResult<Vec<FavFolder>> {
        let mut folders = Vec::new();
        let mut pn: u32 = 1;
        loop {
            let url = format!(
                "https://api.bilibili.com/x/v3/fav/folder/{api}/list?up_mid={mid}&pn={pn}&ps=20"
            );
            let (http, env) = self.get_json::<FolderListResp>(&url, &[])?;
            let page = if http >= 400 {
                return Err(BiliError::Api {
                    code: http as i64,
                    message: format!("fav/folder/{api}/list HTTP {http}"),
                });
            } else if env.code != 0 {
                return Err(BiliError::Api {
                    code: env.code,
                    message: env.message,
                });
            } else {
                // data 为 null（如无收藏的收藏夹）按空页处理，不当作错误。
                env.data.unwrap_or_default()
            };
            let has_more = page.has_more;
            folders.extend(page.list.into_iter().map(|f| FavFolder {
                id: f.id,
                title: f.title,
                media_count: f.media_count,
            }));
            if !has_more || pn >= 50 {
                break;
            }
            pn += 1;
        }
        Ok(folders)
    }

    /// 列出收藏夹资源（type=2 仅视频），返回 `(条目, 收藏夹总数)`。
    pub fn list_favorite_resources(&self, media_id: i64, pn: u32) -> BiliResult<(Vec<FavItem>, i64)> {
        // platform=web 为官方文档标注参数（影响内容列表类型），与 web 前端一致。
        let url = format!(
            "https://api.bilibili.com/x/v3/fav/resource/list?media_id={media_id}&pn={pn}&ps=20&order=mtime&type=2&platform=web"
        );
        let data: ResourceListResp = self.get_data(&url, "fav/resource/list")?;
        let items = data
            .medias
            .into_iter()
            .filter(|m| !m.bvid.is_empty())
            .map(|m| FavItem {
                bvid: m.bvid,
                title: m.title,
                owner: m.upper.name,
                duration_secs: m.duration.max(0) as f64,
                cover_url: Some(m.cover).filter(|c| !c.is_empty()),
            })
            .collect();
        Ok((items, data.info.media_count))
    }
}
