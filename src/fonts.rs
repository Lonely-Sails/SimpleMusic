//! 字体加载：界面**文字**优先使用系统字体（观感跟随操作系统），界面**图标**恒用
//! 内嵌的 Phosphor 图标字体（PUA 码点，跨平台系统图标字形缺失会显示 "?"，不依赖系统）。
//!
//! 策略（优先级从高到低）：
//! 1. 设置页选择的字体（`Settings::ui_font`）：用户在「设置 → 界面字体」里显式挑选
//!    的系统字体文件，切选即时生效并持久化（内嵌字体列表见 [`scan_system_fonts`]）；
//! 2. 系统文字字体（Auto 模式）：按平台探测常见 UI/CJK 字体（Windows 微软雅黑/黑体、
//!    macOS 苹方/系统英文 UI 字体、Linux Noto Sans CJK / 文泉驿 / Droid 等），读盘后用
//!    skrifa（epaint 同款解析器）校验「可解析 + 覆盖基础拉丁和常用汉字」才启用——egui
//!    对解析失败的字体直接 panic，必须前置校验；图标字体（无文字覆盖）会被校验拒绝；
//! 3. 内嵌 Noto Sans SC Regular（`assets/NotoSansSC-Regular.otf`，`include_bytes!` 编译期
//!    嵌入）：探测失败时顶上；探测成功时也挂在系统字体之后作 CJK 兜底，纯拉丁系统上
//!    中文不会变豆腐块；
//! 4. 内嵌 Phosphor 图标字体（`assets/Phosphor.ttf`，MIT 协议），负责界面 PUA 码点字形
//!    （音乐/齿轮/关闭/播放控制等），所有图标走 `crate::icons::*`，恒定注册；
//! 5. egui 默认字体（若启用 default_fonts）仍在列表末尾作最终兜底。
//!
//! 注册顺序：首选文字字体插到 `Proportional`/`Monospace` 两个 family 的首位，图标字体
//! 紧随其后（index 1）。egui 按 family 列表顺序逐个查字形，PUA 码点正常由 Phosphor 命中
//! （个别系统字体自带 PUA 覆盖时才会优先命中系统字形，与旧行为一致），不干扰正常文字。
//! 用户选了不含汉字的字体也没关系：中文自动由内嵌 Noto 兜底（字体链里恒有它）。
//!
//! 环境变量（仅 Auto 模式下生效，设置页显式选择的优先级更高）：
//! - `SIMPLEMUSIC_EMBEDDED_FONTS=1`：跳过系统探测，全部用内嵌字体（旧行为）；
//! - `SIMPLEMUSIC_FONT=/path/to/font.ttf`：直接指定系统字体文件（跳过自动探测）。

use eframe::egui;
use skrifa::MetadataProvider as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

/// 最近一次安装的**文字**字体字节（供 `text_shadow` 离屏光栅化与 egui 保持同一
/// 字形来源）。`OnceLock<RwLock<_>>`：安装时写（启动/设置页切字体），渲染时读。
static ACTIVE_TEXT_FONT: OnceLock<RwLock<Arc<Vec<u8>>>> = OnceLock::new();

/// 登记当前文字字体字节（安装字体后调用）。
fn set_active_text_font(bytes: &[u8]) {
    let slot = ACTIVE_TEXT_FONT.get_or_init(|| RwLock::new(Arc::new(Vec::new())));
    if let Ok(mut v) = slot.write() {
        *v = Arc::new(bytes.to_vec());
    }
}

/// 当前文字字体字节；从未安装过（纯无头测试）时回退内嵌 Noto Sans SC。
pub fn active_text_font() -> Arc<Vec<u8>> {
    match ACTIVE_TEXT_FONT.get().and_then(|s| s.read().ok()) {
        Some(v) if !v.is_empty() => Arc::clone(&v),
        _ => Arc::new(NOTO_SC_BYTES.to_vec()),
    }
}

/// 内嵌 CJK 字体字节（include_bytes 静态数据；[`embedded_cjk_data`] 与
/// [`active_text_font`] 共用同一份）。pub 供 text_shadow 无头测试直接使用。
pub(crate) const NOTO_SC_BYTES_FOR_TEST: &[u8] = include_bytes!("../assets/NotoSansSC-Regular.otf");

/// 内嵌 CJK 字体字节（include_bytes 静态数据；[`embedded_cjk_data`]、
/// [`active_text_font`] 与 [`NOTO_SC_BYTES_FOR_TEST`] 共用同一份）。
const NOTO_SC_BYTES: &[u8] = NOTO_SC_BYTES_FOR_TEST;

/// 内嵌 CJK 字体在 FontDefinitions 里的键名。
const EMBEDDED_KEY: &str = "noto_sc";
/// 内嵌图标字体（Phosphor，MIT）在 FontDefinitions 里的键名。
const PHOSPHOR_KEY: &str = "phosphor_icons";
/// 运行时加载的系统字体在 FontDefinitions 里的键名。
const SYSTEM_KEY: &str = "system_ui";

/// 文字字体来源说明（用于启动报告）。
#[derive(Debug, Clone, PartialEq)]
pub enum FontChoice {
    /// 运行时加载的系统字体（路径）；内嵌 Noto Sans SC 仍挂在兜底链上。
    System(PathBuf),
    /// 内嵌 Noto Sans SC（系统探测失败，或 `SIMPLEMUSIC_EMBEDDED_FONTS=1`）。
    Embedded,
}

/// 安装字体：按设置选择文字字体（`ui_font = None` 时系统优先），图标恒用内嵌
/// Phosphor。返回文字字体实际采用的来源（`ui_font` 指向的文件无效时自动回退，
/// 返回值反映真实结果）。
pub fn install_fonts(ctx: &egui::Context, ui_font: Option<&Path>) -> FontChoice {
    let forced = ui_font.and_then(load_font_file);
    let system = match forced {
        Some(f) => Some(f),
        None => {
            let force_embedded = std::env::var_os("SIMPLEMUSIC_EMBEDDED_FONTS").is_some();
            if force_embedded {
                None
            } else {
                load_system_font()
            }
        }
    };
    let (fonts, choice) = build_definitions(system.as_ref());
    ctx.set_fonts(fonts);
    // 登记实际采用的字体字节，供 text_shadow 阴影光栅化与 egui 保持同源字形。
    match &system {
        Some((_, bytes)) => set_active_text_font(bytes),
        None => set_active_text_font(NOTO_SC_BYTES),
    }
    choice
}

/// 组装 FontDefinitions：`system` 为探测到的系统字体（已通过 [`font_file_is_suitable`]
/// 校验），否则回退内嵌 Noto Sans SC。纯函数，便于无头测试。
/// 返回 `(字体表, 文字字体实际来源)`。
fn build_definitions(system: Option<&(PathBuf, Vec<u8>)>) -> (egui::FontDefinitions, FontChoice) {
    let mut fonts = egui::FontDefinitions::default();

    // 图标字体（Phosphor，PUA 码点）恒内嵌：图标不依赖系统字形。
    fonts.font_data.insert(
        PHOSPHOR_KEY.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(
            include_bytes!("../assets/Phosphor.ttf"),
        )),
    );

    // 文字字体：系统优先；内嵌 CJK 挂在系统字体之后兜底，系统缺字时中文不变豆腐块。
    let (first_key, choice) = match system {
        Some((path, bytes)) => {
            fonts.font_data.insert(
                SYSTEM_KEY.to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes.clone())),
            );
            fonts
                .font_data
                .insert(EMBEDDED_KEY.to_owned(), embedded_cjk_data());
            (SYSTEM_KEY, FontChoice::System(path.clone()))
        }
        None => {
            fonts
                .font_data
                .insert(EMBEDDED_KEY.to_owned(), embedded_cjk_data());
            (EMBEDDED_KEY, FontChoice::Embedded)
        }
    };

    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let list = fonts.families.entry(family).or_default();
        // 文字字体优先；图标字体紧随其后（PUA 码点在此命中，不干扰正常文字）；
        // 系统字体模式下内嵌 CJK 排在其后兜底（egui 只按 family 列表顺序查字形，
        // 只进 font_data 不进列表的字形永远不会被命中）。
        list.insert(0, first_key.to_owned());
        list.insert(1, PHOSPHOR_KEY.to_owned());
        if first_key == SYSTEM_KEY {
            list.insert(2, EMBEDDED_KEY.to_owned());
        }
    }
    (fonts, choice)
}

/// 内嵌 CJK 字体（`include_bytes!` 静态数据）。
fn embedded_cjk_data() -> std::sync::Arc<egui::FontData> {
    std::sync::Arc::new(egui::FontData::from_static(NOTO_SC_BYTES))
}

/// 强制安装内嵌 Noto Sans SC + Phosphor（跳过系统字体探测）。
///
/// 供无头测试使用：字形度量不随宿主机器的系统字体变化，跨机器结果稳定。
pub fn install_embedded_fonts(ctx: &egui::Context) {
    let (fonts, _) = build_definitions(None);
    ctx.set_fonts(fonts);
    set_active_text_font(NOTO_SC_BYTES);
}

// ---------------------------------------------------------------------------
// 设置页「界面字体」选择：系统字体扫描
// ---------------------------------------------------------------------------

/// 一枚可选的系统字体：展示名 + 文件路径。
///
/// 展示名取字体 name 表的家族名（skrifa 解析）；同家族多字重/多路径只保留
/// 路径排序最先的一个（Regular 一般排最前，选它作该家族的代表）。
#[derive(Debug, Clone, PartialEq)]
pub struct SystemFont {
    /// 展示名（字体家族名，如 "Noto Sans CJK SC"；解析失败时回退文件名）。
    pub family: String,
    /// 字体文件绝对路径（选择后持久化的就是它）。
    pub path: PathBuf,
}

/// 校验字体文件能否被 egui/epaint 加载（skrifa 可解析即可，**不**要求覆盖中文）。
///
/// 与 [`font_file_is_suitable`] 的区别：用户显式挑选的字体允许是纯拉丁字体——
/// 界面中文自动由内嵌 Noto 兜底（字体链恒有内嵌 CJK），不该因此拒绝用户的选择；
/// 但解析失败的文件必须拦下（egui 对解析失败的字体直接 panic）。
pub fn font_file_is_loadable(bytes: &[u8]) -> bool {
    skrifa::FontRef::from_index(bytes, 0).is_ok()
}

/// 从文件读出字体并校验可加载；返回 `(路径, 内容)`。
///
/// 失败时打印提示并返回 `None`（调用方回退 Auto/内嵌——文件被删或格式不支持时
/// 不能让选择静默失效）。
fn load_font_file(path: &Path) -> Option<(PathBuf, Vec<u8>)> {
    match std::fs::read(path) {
        Ok(bytes) if font_file_is_loadable(&bytes) => Some((path.to_path_buf(), bytes)),
        Ok(_) => {
            eprintln!(
                "[font] 设置选择的字体 {} 无法解析（egui 不支持该格式），回退自动选择",
                path.display()
            );
            None
        }
        Err(e) => {
            eprintln!(
                "[font] 设置选择的字体 {} 读取失败（{e}），回退自动选择",
                path.display()
            );
            None
        }
    }
}

/// 读字体家族名（name 表 typographic family / family，English 优先）。
///
/// 仅作设置页展示；解析不出时返回 `None`，调用方回退文件名。
pub fn font_family_name(bytes: &[u8]) -> Option<String> {
    let font = skrifa::FontRef::from_index(bytes, 0).ok()?;
    for id in [
        skrifa::string::StringId::TYPOGRAPHIC_FAMILY_NAME,
        skrifa::string::StringId::FAMILY_NAME,
    ] {
        if let Some(name) = font.localized_strings(id).english_or_first() {
            let s = name.to_string();
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

/// 扫描系统字体目录，返回可选字体列表（阻塞 IO，须在后台线程调用）。
///
/// - 覆盖平台字体目录 + 用户字体目录；ttf/otf/ttc/otc 全收；
/// - 逐文件读入并用 [`font_file_is_loadable`] 校验（可解析即可，纯拉丁字体也
///   入列，中文由内嵌 Noto 兜底）；emoji 字体跳过；
/// - 展示名用家族名（解析失败回退文件名）；同家族去重，代表 face 取路径排序
///   最先者（ Regular 一般排最前）；结果按家族名排序（大小写不敏感）。
pub fn scan_system_fonts() -> Vec<SystemFont> {
    let mut roots = platform_font_roots();
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            roots.push(PathBuf::from(&home).join(".fonts"));
            roots.push(PathBuf::from(&home).join(".local/share/fonts"));
            if cfg!(target_os = "macos") {
                roots.push(PathBuf::from(&home).join("Library/Fonts"));
            }
        }
    }
    scan_system_fonts_in(roots)
}

/// [`scan_system_fonts`] 的可注入实现（roots 由调用方给定，便于测试）。
fn scan_system_fonts_in(roots: Vec<PathBuf>) -> Vec<SystemFont> {
    // (路径, 家族名)：先收集再统一去重排序，保证多路径同家族时结果确定。
    let mut found: Vec<(PathBuf, String)> = Vec::new();
    for root in &roots {
        walk_font_tree(root, 0, &mut |p| {
            let Ok(bytes) = std::fs::read(p) else {
                return;
            };
            if !font_file_is_loadable(&bytes) || is_emoji_font_name(p) {
                return;
            }
            let family =
                font_family_name(&bytes).unwrap_or_else(|| fallback_display_name(p));
            found.push((p.canonicalize().unwrap_or_else(|_| p.to_path_buf()), family));
        });
    }

    found.sort();
    let mut out: Vec<SystemFont> = Vec::new();
    let mut seen_family = std::collections::HashSet::new();
    for (path, family) in found {
        // 家族名重复 = 同字体的多字重文件，留排序最前的代表（一般是 Regular）。
        if seen_family.insert(family.clone()) {
            out.push(SystemFont { family, path });
        }
    }
    out.sort_by(|a, b| a.family.to_lowercase().cmp(&b.family.to_lowercase()));
    out
}

/// 平台字体根目录（用户目录在 [`scan_system_fonts`] 里统一追加）。
fn platform_font_roots() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        vec![PathBuf::from("C:/Windows/Fonts")]
    }
    #[cfg(target_os = "macos")]
    {
        vec![
            PathBuf::from("/System/Library/Fonts"),
            PathBuf::from("/Library/Fonts"),
        ]
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        vec![
            PathBuf::from("/usr/share/fonts"),
            PathBuf::from("/usr/local/share/fonts"),
        ]
    }
}

/// 递归遍历目录下的字体文件（ttf/otf/ttc/otc），对每个命中文件回调。
/// 深度上限防符号链接环。
fn walk_font_tree(dir: &Path, depth: usize, f: &mut impl FnMut(&Path)) {
    if depth > 6 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_font_tree(&p, depth + 1, f);
        } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            if ["ttf", "otf", "ttc", "otc"]
                .iter()
                .any(|x| ext.eq_ignore_ascii_case(x))
            {
                f(&p);
            }
        }
    }
}

/// 文件名（小写）里是否带 emoji 标记——彩色表情字体不是文字字体，不入列。
fn is_emoji_font_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase().contains("emoji"))
        .unwrap_or(false)
}

/// 解析不出家族名时，用文件名（去扩展名）作展示名兜底。
fn fallback_display_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("未知字体")
        .to_owned()
}


/// 运行时探测并加载系统文字字体。
///
/// 返回 `(路径, 字体文件内容)`；探测/校验失败返回 `None`（调用方回退内嵌字体）。
/// `SIMPLEMUSIC_FONT` 指定的文件优先于自动探测。
pub fn load_system_font() -> Option<(PathBuf, Vec<u8>)> {
    // 显式指定优先；指定了但无效时提示并继续自动探测。
    if let Some(p) = std::env::var_os("SIMPLEMUSIC_FONT") {
        let p = PathBuf::from(p);
        match std::fs::read(&p) {
            Ok(bytes) if font_file_is_suitable(&bytes) => return Some((p, bytes)),
            Ok(_) => eprintln!(
                "[font] SIMPLEMUSIC_FONT 指定的 {} 无法用作界面字体（解析失败或缺少拉丁/汉字覆盖），改用自动探测",
                p.display()
            ),
            Err(e) => eprintln!(
                "[font] SIMPLEMUSIC_FONT 指定的 {} 读取失败（{e}），改用自动探测",
                p.display()
            ),
        }
    }
    for path in system_font_candidates() {
        if let Ok(bytes) = std::fs::read(&path) {
            if font_file_is_suitable(&bytes) {
                return Some((path, bytes));
            }
        }
    }
    None
}

/// 校验字体文件能否被 egui/epaint 正常使用（同款解析器 skrifa，索引 0 的 face）。
///
/// 要求：文件可解析，且覆盖基础拉丁（AaZz09）与常用汉字——本项目界面全中文。
/// 图标字体（如 Phosphor）没有这些覆盖，会被判为不适用（正确行为，图标恒走内嵌）。
pub fn font_file_is_suitable(bytes: &[u8]) -> bool {
    let Ok(font) = skrifa::FontRef::from_index(bytes, 0) else {
        return false;
    };
    let charmap = font.charmap();
    let latin_ok = "AaZz09".chars().all(|c| charmap.map(c).is_some());
    let cjk_ok = "界面播放列表歌词音量设置歌曲专辑搜索"
        .chars()
        .all(|c| charmap.map(c).is_some());
    latin_ok && cjk_ok
}

/// 按平台列出候选系统字体文件（按优先级排序）。
///
/// - Windows: `msyh.ttc`（微软雅黑）、`simhei.ttf`（黑体）、Segoe UI / Arial；
/// - macOS: `PingFang.ttc`（苹方）、`STHeiti Light.ttc`、系统英文 UI 字体、宋体兜底；
/// - Linux/其它: 扫描 `/usr/share/fonts`、`/usr/local/share/fonts`、`~/.fonts`、
///   `~/.local/share/fonts`，按「CJK 字体 > 常见无衬线拉丁 UI 字体」打分排序。
pub fn system_font_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    fn candidates() -> Vec<PathBuf> {
        ["msyh.ttc", "msyh.ttf", "simhei.ttf", "segoeui.ttf", "arial.ttf"]
            .iter()
            .map(|n| PathBuf::from("C:/Windows/Fonts").join(n))
            .collect()
    }

    #[cfg(target_os = "macos")]
    fn candidates() -> Vec<PathBuf> {
        let mut v = vec![
            PathBuf::from("/System/Library/Fonts/PingFang.ttc"),
            PathBuf::from("/System/Library/Fonts/STHeiti Light.ttc"),
            PathBuf::from("/System/Library/Fonts/SFNS.ttf"),
            PathBuf::from("/System/Library/Fonts/Helvetica.ttc"),
            PathBuf::from("/System/Library/Fonts/Supplemental/Arial.ttf"),
            PathBuf::from("/System/Library/Fonts/Supplemental/Songti.ttc"),
        ];
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                v.push(PathBuf::from(&home).join("Library/Fonts"));
            }
        }
        v
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn candidates() -> Vec<PathBuf> {
        let mut roots = vec![
            PathBuf::from("/usr/share/fonts"),
            PathBuf::from("/usr/local/share/fonts"),
        ];
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                roots.push(PathBuf::from(&home).join(".fonts"));
                roots.push(PathBuf::from(&home).join(".local/share/fonts"));
            }
        }
        // (得分, 路径)：得分越小越优先；同分按路径排序保证确定性。
        let mut scored: Vec<(u8, PathBuf)> = Vec::new();
        for root in &roots {
            if root.is_dir() {
                walk_font_dir(root, 0, &mut scored);
            }
        }
        scored.sort();
        scored.into_iter().map(|(_, p)| p).collect()
    }

    candidates()
}

/// 递归收集目录下的字体候选（Linux/其它平台）。深度上限防符号链接环。
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn walk_font_dir(dir: &PathBuf, depth: usize, out: &mut Vec<(u8, PathBuf)>) {
    if depth > 6 {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk_font_dir(&p, depth + 1, out);
            } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if let Some(score) = score_font_name(name) {
                    out.push((score, p));
                }
            }
        }
    }
}

/// 界面文字字体候选打分（Linux/其它平台）：None = 不作候选。CJK 字体优先于拉丁
/// UI 字体；CJK 内部无衬线（sans）优先于衬线；emoji 字体永远不要。
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn score_font_name(name: &str) -> Option<u8> {
    let n = name.to_lowercase();
    if !n.ends_with(".ttf") && !n.ends_with(".otf") && !n.ends_with(".ttc") {
        return None;
    }
    if n.contains("emoji") {
        return None;
    }
    const CJK: [&str; 9] = [
        "noto",
        "sourcehan",
        "sarasa",
        "wqy",
        "wenquanyi",
        "droid",
        "cjk",
        "uming",
        "ukai",
    ];
    const LATIN_UI: [&str; 11] = [
        "dejavu",
        "liberation",
        "ubuntu",
        "roboto",
        "inter",
        "fira",
        "cantarell",
        "helvetica",
        "arial",
        "segoe",
        "opensans",
    ];
    if CJK.iter().any(|k| n.contains(k)) {
        // sans 前缀优先（界面文字）；注意 "misans" 这类名字也带 sans。
        Some(if n.contains("sans") { 0 } else { 1 })
    } else if LATIN_UI.iter().any(|k| n.contains(k)) {
        Some(10)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内嵌 Noto Sans SC 必须通过校验（回退路径可用性的前提）。
    #[test]
    fn embedded_cjk_font_passes_validation() {
        assert!(font_file_is_suitable(include_bytes!(
            "../assets/NotoSansSC-Regular.otf"
        )));
    }

    /// 图标字体没有文字覆盖 → 校验必须拒绝（防止它被选作文字字体）。
    #[test]
    fn icon_font_fails_validation() {
        assert!(!font_file_is_suitable(include_bytes!("../assets/Phosphor.ttf")));
    }

    /// 坏文件必须被校验拒绝——egui 对解析失败的字体直接 panic，不能把坏文件塞给它。
    #[test]
    fn garbage_fails_validation() {
        assert!(!font_file_is_suitable(b"not a font at all"));
        assert!(!font_file_is_suitable(&[]));
    }

    /// Phosphor 恒在两个 family；用上系统字体时内嵌 CJK 必须仍在兜底链上（系统字体
    /// 之后、图标字体之前）；没用上时内嵌 CJK 在首位。不读 Context（无头下 run 前
    /// 不能访问字体视图），直接断言组装结果。
    #[test]
    fn install_keeps_phosphor_and_fallback_chain() {
        // 无系统字体 → 内嵌兜底。
        let (defs, choice) = build_definitions(None);
        assert_eq!(choice, FontChoice::Embedded);
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let list = &defs.families[&family];
            assert_eq!(list.first().map(String::as_str), Some(EMBEDDED_KEY));
            assert_eq!(list.get(1).map(String::as_str), Some(PHOSPHOR_KEY));
        }

        // 有系统字体 → 系统字体首位、图标第二、内嵌 CJK 兜底。
        let sys = (PathBuf::from("/tmp/fake.ttf"), b"fake".to_vec());
        let (defs, choice) = build_definitions(Some(&sys));
        assert_eq!(choice, FontChoice::System(sys.0.clone()));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let list = &defs.families[&family];
            assert_eq!(list.first().map(String::as_str), Some(SYSTEM_KEY));
            assert_eq!(list.get(1).map(String::as_str), Some(PHOSPHOR_KEY));
            assert!(
                list.iter().any(|k| k == EMBEDDED_KEY),
                "{family:?} 系统字体模式下缺少内嵌 CJK 兜底: {list:?}"
            );
        }
        assert!(defs.font_data.contains_key(SYSTEM_KEY));
    }

    /// 兜底链最终形态：装上内嵌字体后，界面常用文本必须布局出非零尺寸，且 CJK 与
    /// 拉丁命中的是**不同**的真实字形（若两者都渲染成同一个 replacement 占位字形，
    /// 宽度必然相等 → 断言失败）。
    /// 注 1：用 `install_embedded_fonts` 保证度量不随宿主机器的系统字体变化。
    /// 注 2：不用 `has_glyphs`——epaint 0.36 的 `has_glyph` 是「命中面 ≠ replacement 面」，
    /// 本项目首个字体自己就是 replacement 字形提供者，它恒返回 false（渲染不受影响）。
    #[test]
    fn ui_text_glyphs_resolve_after_install() {
        let ctx = egui::Context::default();
        install_embedded_fonts(&ctx);
        let font_id = egui::FontId::proportional(14.0);
        let mut full = ctx.run_ui(egui::RawInput::default(), |ctx| {
            ctx.fonts_mut(|f| {
                let cjk = f.glyph_width(&font_id, '中');
                let latin = f.glyph_width(&font_id, 'A');
                assert!(cjk > 0.0, "CJK 字形宽度为 0");
                assert!(latin > 0.0, "拉丁字形宽度为 0");
                assert_ne!(cjk, latin, "CJK 与拉丁宽度相同 = 都渲染成占位字形");
                let galley = f.layout_no_wrap(
                    "中文歌词 ABC 123：".into(),
                    font_id.clone(),
                    egui::Color32::WHITE,
                );
                assert!(galley.rect.width() > 0.0);
                assert!(galley.rect.height() > 0.0);
            });
        });
        // 与项目内其它无头测试一致：首帧产生的字体图集增量必须显式清掉。
        full.textures_delta.clear();
    }

    /// `SIMPLEMUSIC_FONT` 指向有效的 CJK 字体文件 → 被采用；指向图标字体 → 被拒绝
    /// （校验会拦下没有文字覆盖的文件）。测试内改进程环境变量：没有其它测试读这个
    /// 变量（生产读取点 `load_system_font` 的调用方均未在测试中使用），并行安全。
    #[test]
    fn simplemusic_font_env_is_adopted_or_rejected() {
        let dir = std::env::temp_dir().join(format!("simplemusic-font-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.otf");
        let bad = dir.join("phosphor.ttf");
        std::fs::write(&good, include_bytes!("../assets/NotoSansSC-Regular.otf")).unwrap();
        std::fs::write(&bad, include_bytes!("../assets/Phosphor.ttf")).unwrap();

        unsafe { std::env::set_var("SIMPLEMUSIC_FONT", &good) };
        let picked = load_system_font();
        unsafe { std::env::remove_var("SIMPLEMUSIC_FONT") };
        assert_eq!(picked.map(|(p, _)| p), Some(good.clone()));

        unsafe { std::env::set_var("SIMPLEMUSIC_FONT", &bad) };
        let picked = load_system_font();
        unsafe { std::env::remove_var("SIMPLEMUSIC_FONT") };
        assert!(picked.is_none(), "图标字体不该被选作文字字体");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Linux 候选打分：CJK sans 最优先，emoji 永不入选，无关文件不候选。
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn linux_candidates_scoring() {
        assert_eq!(score_font_name("NotoSansCJK-Regular.ttc"), Some(0));
        assert_eq!(score_font_name("wqy-microhei.ttc"), Some(1));
        assert_eq!(score_font_name("NotoSerifCJK-Regular.ttc"), Some(1));
        assert_eq!(score_font_name("DejaVuSans.ttf"), Some(10));
        assert_eq!(score_font_name("NotoColorEmoji.ttf"), None);
        assert_eq!(score_font_name("README.txt"), None);
    }

    /// 可加载校验：内嵌 CJK/Phosphor 均可解析；垃圾数据拒绝。
    #[test]
    fn loadable_check() {
        assert!(font_file_is_loadable(include_bytes!(
            "../assets/NotoSansSC-Regular.otf"
        )));
        // 图标字体「可加载」但不「适用作文字」——两个校验的语义分野。
        assert!(font_file_is_loadable(include_bytes!("../assets/Phosphor.ttf")));
        assert!(!font_file_is_loadable(b"garbage"));
        assert!(!font_file_is_loadable(&[]));
        assert!(font_file_is_suitable(include_bytes!(
            "../assets/NotoSansSC-Regular.otf"
        )));
        assert!(!font_file_is_suitable(include_bytes!("../assets/Phosphor.ttf")));
    }

    /// 家族名解析：内嵌 Noto 能读出非空家族名（CI 容器字体不定，只用内嵌资产）。
    #[test]
    fn family_name_from_embedded() {
        let name = font_family_name(include_bytes!("../assets/NotoSansSC-Regular.otf"))
            .expect("内嵌 Noto 必须解析出家族名");
        assert!(!name.trim().is_empty());
    }

    /// emoji 过滤与展示名兜底（纯路径逻辑，不读盘）。
    #[test]
    fn emoji_filter_and_fallback_name() {
        assert!(is_emoji_font_name(Path::new("/f/NotoColorEmoji.ttf")));
        assert!(is_emoji_font_name(Path::new("/f/emoji-one.otf")));
        assert!(!is_emoji_font_name(Path::new("/f/NotoSansCJK-Regular.ttc")));
        assert_eq!(
            fallback_display_name(Path::new("/f/MyFont-Bold.ttf")),
            "MyFont-Bold"
        );
    }

    /// 扫描端到端（临时目录）：有效 CJK 字体入列且解析出家族名；垃圾/emoji/
    /// 非字体文件被过滤；子目录递归覆盖。
    #[test]
    fn scan_system_fonts_in_filters_and_sorts() {
        let dir = std::env::temp_dir().join(format!("simplemusic-fontscan-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(
            dir.join("NotoSansSC-Regular.otf"),
            include_bytes!("../assets/NotoSansSC-Regular.otf"),
        )
        .unwrap();
        std::fs::write(dir.join("nested/phosphor.ttf"), include_bytes!("../assets/Phosphor.ttf"))
            .unwrap();
        std::fs::write(dir.join("NotoColorEmoji.ttf"), b"garbage").unwrap();
        std::fs::write(dir.join("readme.txt"), b"not a font").unwrap();

        let fonts = scan_system_fonts_in(vec![dir.clone()]);
        let names: Vec<&str> = fonts.iter().map(|f| f.family.as_str()).collect();
        // Phosphor 可解析 → 入列（家族名取 name 表或文件名兜底）；emoji 按名排除；
        // txt 非字体被过滤；嵌套目录被递归。
        assert!(names.len() >= 2, "至少应有 Noto + Phosphor 两个 family: {names:?}");
        assert!(names.windows(2).all(|w| w[0].to_lowercase() <= w[1].to_lowercase()));
        assert!(
            !names.iter().any(|n| n.to_lowercase().contains("emoji")),
            "emoji 字体不应入列: {names:?}"
        );
        assert!(
            fonts.iter().any(|f| f.family == "Noto Sans CJK SC"),
            "Noto 家族名应解析自 name 表: {names:?}"
        );
        assert!(
            fonts.iter().all(|f| f.path.extension().and_then(|e| e.to_str())
                .map(|e| ["ttf", "otf", "ttc", "otc"].contains(&e.to_lowercase().as_str()))
                .unwrap_or(false)),
            "只应包含字体文件: {fonts:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 空根目录 → 空列表不 panic（无字体容器的真实情形）。
    #[test]
    fn scan_system_fonts_in_empty_roots() {
        let dir = std::env::temp_dir().join(format!("simplemusic-fontscan-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(scan_system_fonts_in(vec![dir]).is_empty());
        assert!(scan_system_fonts_in(vec![]).is_empty());
    }

    /// 扫描结果排序稳定：按家族名（大小写不敏感）排序。不读盘的纯排序逻辑——
    /// 直接构造 found 列表走同一段去重排序。scan_system_fonts 本体在 UI 冒烟
    /// 测试里跑（依赖宿主字体目录，断言只做「不 panic」）。
    #[test]
    fn scan_smoke() {
        // 阻塞 IO 扫描，在测试里可接受（~几十 ms）；空容器/无字体目录也不 panic。
        let fonts = scan_system_fonts();
        for w in fonts.windows(2) {
            assert!(
                w[0].family.to_lowercase() <= w[1].family.to_lowercase(),
                "扫描结果应按家族名排序: {:?}",
                w
            );
        }
    }
}
