//! 集中式主题样式：深色淡雅、简洁大气、小圆角。
//!
//! 暴露 `pub fn apply(ctx)` 应用全局主题，以及语义色板常量供 UI 代码使用，
//! 避免魔法颜色散落各处。

use eframe::egui::{
    self,
    style::{HandleShape, ScrollStyle, Selection, TextCursorStyle},
    Color32, CornerRadius, FontFamily, FontId, Margin, RichText, Stroke, Style, TextStyle, Vec2,
};

// ===========================================================================
// 语义色板（低饱和、淡雅）
// ===========================================================================

/// 窗口底（中央内容区）
pub const BG_WINDOW: Color32 = Color32::from_rgb(0x15, 0x1A, 0x21);
/// 面板/列表底（顶部栏、左侧队列、底部播放条）
pub const BG_PANEL: Color32 = Color32::from_rgb(0x20, 0x26, 0x2E);
/// 卡片/条目底（按钮、输入框、队列条目）
pub const BG_CARD: Color32 = Color32::from_rgb(0x26, 0x2D, 0x37);
/// 悬停底（hover 亮一档）
pub const BG_HOVER: Color32 = Color32::from_rgb(0x2C, 0x35, 0x40);
/// 激活/按下底
pub const BG_ACTIVE: Color32 = Color32::from_rgb(0x32, 0x3C, 0x49);
/// 滑块轨道底（未播段低对比灰蓝）
pub const BG_TRACK: Color32 = Color32::from_rgb(0x2A, 0x33, 0x3E);

/// 主点缀色（雾青蓝，低饱和）
pub const ACCENT: Color32 = Color32::from_rgb(0x7F, 0xA8, 0xC9);
/// 点缀色 hover 亮一档
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x8F, 0xB8, 0xD0);
/// 点缀色按下/深色
pub const ACCENT_DEEP: Color32 = Color32::from_rgb(0x6E, 0x9B, 0xB8);
/// 辅助强调色（淡金，极少使用）
pub const GOLD: Color32 = Color32::from_rgb(0xC9, 0xA8, 0x7C);

/// 主文本色
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xE6, 0xEA, 0xF0);
/// 次级文本（UP主、时长）
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0x98, 0xA2, 0xAE);
/// 弱文本（提示、占位符）
pub const TEXT_WEAK: Color32 = Color32::from_rgb(0x5F, 0x6B, 0x78);
/// 错误文本（柔和淡红）
pub const TEXT_ERROR: Color32 = Color32::from_rgb(0xD0, 0x8C, 0x8C);
/// 点缀色按钮上的深色文字
pub const TEXT_ON_ACCENT: Color32 = Color32::from_rgb(0x0E, 0x14, 0x1B);
/// 桌面歌词当前句（瓷白）
pub const LYRIC_CURRENT: Color32 = Color32::from_rgb(0xF2, 0xF5, 0xF8);
/// 桌面歌词下一句
pub const LYRIC_NEXT: Color32 = Color32::from_rgb(0x8F, 0xB8, 0xD0);
/// 桌面歌词半透明底
pub const LYRIC_BG: Color32 = Color32::from_rgba_premultiplied(0x1A, 0x20, 0x28, 230);

/// 弱分隔线/描边
pub const BORDER_SOFT: Color32 = Color32::from_rgb(0x2E, 0x36, 0x40);
/// 自定义标题栏底色（比窗口底略亮一档，像悬浮卡片的“檐”）
pub const TITLEBAR_BG: Color32 = Color32::from_rgb(0x1C, 0x22, 0x2A);

// ===========================================================================
// 圆角常量
// ===========================================================================

/// 统一小圆角（按钮、输入框、卡片、弹窗、桌面歌词条）
pub const CORNER: u8 = 6;
/// 悬浮主窗口卡片的大圆角（浮窗感）
pub const CORNER_XL: u8 = 14;

// ===========================================================================
// public helpers
// ===========================================================================

/// 创建一个主操作按钮（点缀色填充、深色文字、小圆角）。
pub fn primary_button(text: impl Into<RichText>) -> egui::Button<'static> {
    // 用 RichText 设置文字颜色，fill 设置按钮背景，stroke 去掉边框。
    let rich = text.into().color(TEXT_ON_ACCENT);
    egui::Button::new(rich)
        .fill(ACCENT)
        .stroke(Stroke::NONE)
        .corner_radius(CORNER)
}

/// 创建一个次级小按钮（卡片底、次级文字、小圆角），用于顶栏/内嵌操作。
pub fn small_button(text: impl Into<RichText>) -> egui::Button<'static> {
    let rich = text.into().color(TEXT_SECONDARY);
    egui::Button::new(rich)
        .fill(BG_CARD)
        .stroke(Stroke::new(1.0, BORDER_SOFT))
        .corner_radius(CORNER)
}

// ===========================================================================
// 主题构建
// ===========================================================================

/// 应用全局主题（深色淡雅、简洁大气、小圆角）。
pub fn apply(ctx: &egui::Context) {
    ctx.set_visuals(visuals());
    ctx.all_styles_mut(|style| {
        *style = style_build();
    });
}

/// 构建 Visuals：以 `Visuals::dark()` 为基底，修改关键字段。
fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.override_text_color = None;
    v.weak_text_alpha = 0.6;
    v.weak_text_color = Some(TEXT_WEAK);

    // Widgets
    v.widgets.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.noninteractive.bg_fill = BG_PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_SOFT);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    v.widgets.noninteractive.corner_radius = CornerRadius::same(CORNER);

    v.widgets.inactive.weak_bg_fill = BG_CARD;
    v.widgets.inactive.bg_fill = BG_TRACK;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER_SOFT);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    v.widgets.inactive.corner_radius = CornerRadius::same(CORNER);

    v.widgets.hovered.weak_bg_fill = BG_HOVER;
    v.widgets.hovered.bg_fill = BG_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_HOVER);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    v.widgets.hovered.corner_radius = CornerRadius::same(CORNER);

    v.widgets.active.weak_bg_fill = BG_ACTIVE;
    v.widgets.active.bg_fill = BG_ACTIVE;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT_DEEP);
    v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    v.widgets.active.corner_radius = CornerRadius::same(CORNER);

    v.widgets.open.weak_bg_fill = BG_HOVER;
    v.widgets.open.bg_fill = BG_HOVER;
    v.widgets.open.bg_stroke = Stroke::new(1.0, ACCENT_HOVER);
    v.widgets.open.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    v.widgets.open.corner_radius = CornerRadius::same(CORNER);

    // Selection
    v.selection = Selection {
        bg_fill: ACCENT,
        stroke: Stroke::new(1.0, TEXT_PRIMARY),
    };

    v.hyperlink_color = ACCENT_HOVER;
    v.faint_bg_color = BG_HOVER;
    v.extreme_bg_color = BG_CARD;
    v.text_edit_bg_color = Some(BG_CARD);
    v.code_bg_color = BG_CARD;
    v.warn_fg_color = GOLD;
    v.error_fg_color = TEXT_ERROR;

    // Window
    v.window_fill = BG_WINDOW;
    v.window_corner_radius = CornerRadius::same(CORNER);
    v.window_shadow = egui::epaint::Shadow {
        offset: [8, 16],
        blur: 20,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.window_stroke = Stroke::new(1.0, BORDER_SOFT);

    // Panel
    v.panel_fill = BG_PANEL;

    // Popup shadow
    v.popup_shadow = egui::epaint::Shadow {
        offset: [4, 8],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };

    // Text cursor
    v.text_cursor = TextCursorStyle {
        stroke: Stroke::new(2.0, ACCENT_HOVER),
        preview: false,
        blink: true,
        on_duration: 0.5,
        off_duration: 0.35,
    };

    // Slider
    v.slider_trailing_fill = true;
    v.handle_shape = HandleShape::Circle;

    v.button_frame = true;
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = false;
    v.striped = false;
    v.interact_cursor = None;
    v.image_loading_spinners = true;
    v.disabled_alpha = 0.45;

    v
}

/// 构建 Spacing。
fn spacing() -> egui::style::Spacing {
    let mut s = egui::style::Spacing::default();
    // 宽松布局：元素间、控件内留白都比默认更大，视觉更透气。
    s.item_spacing = Vec2::new(12.0, 10.0);
    s.window_margin = Margin::same(16);
    s.button_padding = Vec2::new(14.0, 6.0);
    s.menu_margin = Margin::same(10);
    s.indent = 20.0;
    s.interact_size = Vec2::new(30.0, 26.0);
    s.slider_width = 120.0;
    s.slider_rail_height = 4.0;
    s.text_edit_width = 220.0;
    s.combo_width = 140.0;
    s.combo_height = 220.0;
    s.icon_width = 18.0;
    s.icon_width_inner = 10.0;
    s.icon_spacing = 6.0;
    s.default_area_size = Vec2::splat(220.0);
    s.tooltip_width = 400.0;
    s.menu_width = 200.0;
    s.menu_spacing = 6.0;
    s.extra_text_line_spacing = 1.0;
    s.indent_ends_with_horizontal_line = false;

    s.scroll = ScrollStyle {
        floating: false,
        content_margin: Margin::ZERO,
        bar_width: 6.0,
        handle_min_length: 20.0,
        bar_inner_margin: 2.0,
        bar_outer_margin: 0.0,
        floating_width: 3.0,
        floating_allocated_width: 0.0,
        foreground_color: false,
        dormant_background_opacity: 0.0,
        active_background_opacity: 0.2,
        interact_background_opacity: 0.5,
        dormant_handle_opacity: 0.0,
        active_handle_opacity: 0.75,
        interact_handle_opacity: 1.0,
        fade: Default::default(),
    };

    s
}

/// 构建 Style（以 egui 默认 Style 为基底，覆盖字段）。
fn style_build() -> Style {
    let mut style = Style::default();
    style.visuals = visuals();

    let mut text_styles = std::collections::BTreeMap::new();
    let body = FontId::new(14.0, FontFamily::Proportional);
    text_styles.insert(TextStyle::Body, body.clone());
    text_styles.insert(TextStyle::Button, FontId::new(14.0, FontFamily::Proportional));
    text_styles.insert(TextStyle::Heading, FontId::new(17.0, FontFamily::Proportional));
    text_styles.insert(TextStyle::Small, FontId::new(11.0, FontFamily::Proportional));
    text_styles.insert(TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace));
    style.text_styles = text_styles;

    style.spacing = spacing();
    style.interaction.resize_grab_radius_side = 4.0;
    style.interaction.resize_grab_radius_corner = 8.0;
    style.interaction.interact_radius = 3.0;
    style.interaction.show_tooltips_only_when_still = true;
    style.interaction.tooltip_delay = 0.4;
    style.interaction.tooltip_grace_time = 0.1;
    style.interaction.selectable_labels = true;
    style.interaction.multi_widget_text_select = true;

    style.animation_time = 0.1;
    style.compact_menu_style = false;
    style.always_scroll_the_only_direction = true;
    style
}