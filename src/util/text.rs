//! 文本净化：过滤界面字体渲染不出来的「乱字符」。
//!
//! ## 为什么需要
//!
//! 本应用文字用内嵌 Noto Sans SC 渲染（可选系统字体），它不含 emoji/彩色修饰符
//! 字形；标题、歌词等来自网络的动态文本里这类字符很常见。缺字时 egui 会画出
//! **replacement 占位字形「?」**——歌词里「?」满天飞就是这个来源。字符串净化
//! 的通用做法正是把不可渲染/不可见码点剔除，这里分两层：
//!
//! 1. **与字体无关的「必删」类**（[`is_unsupported_char`]）：变体选择符、
//!    Regional Indicator（国旗基元）、肤色/发色修饰符、零宽/双向控制、tag 字符、
//!    私用区码点——这些即便某台机器的字体恰好覆盖，也只会渲染出错误观感
//!    （隐形字符、错误组合、随机图标）；
//! 2. **字体覆盖判定**（由调用方注入判定闭包）：内嵌字体不含的码点一律删除。
//!    判定闭包由 `fonts::sanitize_text` 提供（skrifa 查内嵌 Noto Sans SC 的 cmap），
//!    本模块保持无 egui / 无字体依赖，纯函数可离线单测。
//!
//! 删除字符后相邻空白折叠为一个空格、首尾空白剔除，避免 emoji 处留下「空洞」。

/// 过滤文本中「判定闭包认为不可渲染」的字符，并折叠残留空白。
///
/// - 输入为空 → 原样返回空串（调用方无需特判）；
/// - `renderable` 恒真时输出 = 输入去首尾空白 + 折叠连续空白。
pub fn sanitize_ui_text(text: &str, renderable: impl Fn(char) -> bool) -> String {
    if text.is_empty() {
        return String::new();
    }
    let kept: String = text.chars().filter(|&c| renderable(c)).collect();
    collapse_spaces(&kept)
}

/// 与字体无关的「必删」字符类。
///
/// 这些码点不依赖字体覆盖即可断定不该出现在显示文本里：要么不可见（零宽/控制），
/// 要么组合语义已被上游破坏（VS/修饰符/RI 单独出现），要么字形依赖特定图标字体
/// （PUA——本应用图标恒走自绘 Phosphor，不依赖文本里的私用区码点）。
pub fn is_unsupported_char(c: char) -> bool {
    matches!(c,
        // 变体选择符：emoji 的彩色呈现开关，egui 不级联彩色 emoji 字体，纯冗余。
        '\u{FE00}'..='\u{FE0F}'
        // Regional Indicator：国旗 emoji 的基元，单独/配对出现都是「?」。
        | '\u{1F1E6}'..='\u{1F1FF}'
        // 肤色与发色修饰符。
        | '\u{1F3FB}'..='\u{1F3FF}'
        | '\u{1F9B0}'..='\u{1F9B3}'
        // 零宽空格/连接符、方向标记。
        | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2066}'..='\u{2069}'
        | '\u{FEFF}'
        // 语言标注 tag 字符。
        | '\u{E0020}'..='\u{E007F}'
        // 私用区：字形只在装了对应图标字体的机器上有意义。
        | '\u{E000}'..='\u{F8FF}'
        | '\u{F0000}'..='\u{FFFFD}'
        | '\u{100000}'..='\u{10FFFD}'
    )
}

/// 无字体上下文时的宽松判定：只挡「必删」类，其余放行。
///
/// 供本模块单测与无字体环境使用；生产路径经 `fonts::sanitize_text` 注入真实覆盖判定。
pub fn permissive_renderable(c: char) -> bool {
    !is_unsupported_char(c)
}

/// 过滤收尾：折叠删除字符后残留的相邻空白（首尾不留空白）。
fn collapse_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            // 首部空白不落笔；尾部留待下一个非空白字符决定是否补一个空格。
            if !out.is_empty() {
                pending_space = true;
            }
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 模拟内嵌 Noto Sans SC 覆盖面的测试判定：必删类 + emoji 基元不可渲染，
    /// 其余（含 ❤♪★ 等符号）放行——与 `fonts` 生产判定的语义一致。
    fn noto_like_renderable(c: char) -> bool {
        permissive_renderable(c) && !matches!(c, '\u{1F000}'..='\u{1FAFF}')
    }

    /// 空 / 纯空白输入原样返回（调用方无需特判空串）。
    #[test]
    fn empty_and_whitespace_pass_through() {
        assert_eq!(sanitize_ui_text("", permissive_renderable), "");
        // 纯空白折叠后为空（不会留一串空白进布局）。
        assert_eq!(sanitize_ui_text("   ", permissive_renderable), "");
    }

    /// 宽松判定下可渲染字符（含汉字/假名/谚文/符号）一律保留。
    #[test]
    fn renderable_chars_are_preserved_permissively() {
        let s = "晴天 Hello 123 ·—！？。《》❤♪★";
        assert_eq!(sanitize_ui_text(s, permissive_renderable), s);
        let jp = "ハローワールド ひらがな カタカナ";
        assert_eq!(sanitize_ui_text(jp, permissive_renderable), jp);
        let kr = "한국어 가사";
        assert_eq!(sanitize_ui_text(kr, permissive_renderable), kr);
    }

    /// 注入自定义判定：只有 ASCII 可渲染时，非 ASCII 全部删除。
    #[test]
    fn custom_predicate_is_respected() {
        let ascii_only = |c: char| c.is_ascii_graphic() || c == ' ';
        assert_eq!(sanitize_ui_text("晴天 Hello 123", ascii_only), "Hello 123");
    }

    /// 覆盖判定拦截 emoji 基元：裸 emoji 删除，空白折叠不留洞。
    #[test]
    fn bare_emoji_removed_by_coverage() {
        let p = noto_like_renderable;
        assert_eq!(sanitize_ui_text("好听的\u{1F680}歌", p), "好听的歌");
        assert_eq!(sanitize_ui_text("\u{1F3B5}\u{266A}\u{1F31F}", p), "\u{266A}");
        assert_eq!(sanitize_ui_text("【A】\u{1F3B5}", p), "【A】");
        assert_eq!(sanitize_ui_text("\u{1F680}\u{1F680}", p), "");
    }

    /// 变体选择符被剔除后 emoji 降级为基础字符（❤ 基元 Noto 有覆盖 → 保留）。
    #[test]
    fn variation_selector_emoji_degrades() {
        assert_eq!(sanitize_ui_text("\u{2764}\u{FE0F}", noto_like_renderable), "\u{2764}");
        assert_eq!(sanitize_ui_text("赞\u{2764}\u{FE0F}", noto_like_renderable), "赞\u{2764}");
        // 宽松判定同样剔除 VS16。
        assert_eq!(sanitize_ui_text("\u{2764}\u{FE0F}", permissive_renderable), "\u{2764}");
    }

    /// 零宽字符 / 方向控制 / BOM 被剔除，相邻可渲染字符直接拼接。
    #[test]
    fn zero_width_chars_removed() {
        let p = permissive_renderable;
        assert_eq!(sanitize_ui_text("零\u{200B}宽", p), "零宽");
        assert_eq!(sanitize_ui_text("a\u{FEFF}b", p), "ab");
        assert_eq!(sanitize_ui_text("\u{202E}abc", p), "abc");
        assert_eq!(sanitize_ui_text("a\u{200D}b", p), "ab");
    }

    /// 国旗（RI 对）与肤色修饰符被剔除；中间可渲染字符保留。
    #[test]
    fn regional_indicator_and_modifier_removed() {
        let p = permissive_renderable;
        assert_eq!(sanitize_ui_text("\u{1F1E8}\u{1F1F3}", p), "");
        assert_eq!(sanitize_ui_text("歌\u{1F1E8}\u{1F1F3}单", p), "歌单");
        assert_eq!(sanitize_ui_text("手\u{1F3FB}势", p), "手势");
    }

    /// 私用区码点删除（本应用图标恒走自绘 Phosphor，不依赖文本 PUA）。
    #[test]
    fn private_use_area_removed() {
        let p = permissive_renderable;
        assert_eq!(sanitize_ui_text("前\u{E0B0}后", p), "前后");
        assert_eq!(sanitize_ui_text("\u{F8FF}x", p), "x");
    }

    /// 删除字符后的空白折叠：emoji 处消失的字符不会留下成排空格。
    #[test]
    fn whitespace_collapses_after_removal() {
        let p = noto_like_renderable;
        assert_eq!(sanitize_ui_text("A \u{1F680} B", p), "A B");
        assert_eq!(sanitize_ui_text("A \u{1F680}\u{1F680} B", p), "A B");
        assert_eq!(sanitize_ui_text("A\u{1F680}\u{1F680}B", p), "AB");
        assert_eq!(sanitize_ui_text("A \u{1F680}", p), "A");
        assert_eq!(sanitize_ui_text("\u{1F680} B", p), "B");
        // ZWJ 家庭组合：连接符 + 基元全删 → 只剩两端文本。
        assert_eq!(
            sanitize_ui_text("A \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} B", p),
            "A B"
        );
    }

    /// 歌词行典型样例：emoji + VS16 混排，可读部分完整保留。
    #[test]
    fn lyric_line_realistic_sample() {
        let line = "君の名は \u{1F338} 晴天\u{2764}\u{FE0F} (Live)";
        assert_eq!(
            sanitize_ui_text(line, noto_like_renderable),
            "君の名は 晴天\u{2764} (Live)"
        );
    }
}
