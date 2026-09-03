//! 跨区域复用的纯 egui 小组件与文本工具。
//!
//! 所有函数均为无 `self` 的纯 egui 组件，供其他 `ui/*.rs` 文件调用。

use crate::theme;
use eframe::egui::{
    self, Color32, FontId, Painter, Pos2, Rect, Sense, Vec2,
};

// ---------------------------------------------------------------------------
// 播放条圆形按钮
// ---------------------------------------------------------------------------

pub fn transport_button(
    ui: &mut egui::Ui,
    size: f32,
    icon: fn(&Painter, Rect, Color32),
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let bg = if resp.is_pointer_button_down_on() {
        theme::BG_ACTIVE
    } else if resp.hovered() {
        theme::BG_HOVER
    } else {
        Color32::TRANSPARENT
    };
    let painter = ui.painter();
    if bg != Color32::TRANSPARENT {
        painter.circle_filled(rect.center(), size * 0.5, bg);
    }
    let icon_rect = rect.shrink(size * 0.30);
    icon(&painter, icon_rect, theme::TEXT_PRIMARY);
    resp.clicked()
}

// ---------------------------------------------------------------------------
// 通用图标小按钮
// ---------------------------------------------------------------------------

pub fn icon_button(
    ui: &mut egui::Ui,
    size: f32,
    icon: fn(&Painter, Rect, Color32),
    tooltip: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let bg = if resp.is_pointer_button_down_on() {
        theme::BG_ACTIVE
    } else if resp.hovered() {
        theme::BG_HOVER
    } else {
        theme::BG_CARD
    };
    let painter = ui.painter();
    painter.rect_filled(rect, theme::CORNER, bg);
    icon(&painter, rect.shrink(size * 0.24), theme::TEXT_SECONDARY);
    resp.on_hover_text(tooltip)
}

// ---------------------------------------------------------------------------
// 加载转圈
// ---------------------------------------------------------------------------

pub fn spinner_arc(painter: &Painter, center: Pos2, radius: f32, color: Color32) {
    use std::f32::consts::TAU;
    let points: Vec<Pos2> = (0..=10)
        .map(|i| {
            let t = (i as f32 / 10.0) * TAU * 0.75;
            center + Vec2::angled(t) * radius
        })
        .collect();
    painter.line(points, eframe::egui::epaint::PathStroke::new(2.0, color));
}

// ---------------------------------------------------------------------------
// 封面占位符（音符图标 Fallback）
// ---------------------------------------------------------------------------

pub fn paint_placeholder_cover(painter: &Painter, rect: Rect) {
    painter.rect_filled(rect, theme::CORNER, theme::BG_TRACK);
    let c = rect.center();
    let r = (rect.width() * 0.14).max(2.0);
    let dot = Pos2::new(c.x - r * 0.6, c.y + r * 0.9);
    painter.circle_filled(dot, r, theme::TEXT_WEAK);
    let stroke = eframe::egui::Stroke::new((r * 0.30).max(1.5), theme::TEXT_WEAK);
    let stem_x = dot.x + r;
    let stem_top = Pos2::new(stem_x, c.y - r * 1.2);
    painter.line_segment(
        [Pos2::new(stem_x, dot.y), stem_top],
        stroke,
    );
    painter.line_segment(
        [stem_top, Pos2::new(stem_top.x + r * 1.5, stem_top.y + r * 0.8)],
        stroke,
    );
}

// ---------------------------------------------------------------------------
// 二维码绘制
// ---------------------------------------------------------------------------

pub fn draw_qr(ui: &mut egui::Ui, matrix: &[Vec<bool>], size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().rect_filled(rect, 4.0, Color32::WHITE);
    let rows = matrix.len();
    if rows == 0 {
        return;
    }
    let cols = matrix[0].len();
    if cols == 0 {
        return;
    }
    let quiet = 4.0;
    let inner = rect.shrink(quiet);
    let cell = inner.width().min(inner.height()) / cols.max(rows) as f32;
    let qr_w = cell * cols as f32;
    let qr_h = cell * rows as f32;
    let org = inner.center() - Vec2::new(qr_w / 2.0, qr_h / 2.0);
    let dark = Color32::from_rgb(25, 25, 25);
    for (r, row) in matrix.iter().enumerate() {
        for (c, &is_dark) in row.iter().enumerate() {
            if is_dark {
                let min = org + Vec2::new(c as f32 * cell, r as f32 * cell);
                let cell_rect = Rect::from_min_size(min, Vec2::new(cell + 0.4, cell + 0.4));
                ui.painter().rect_filled(cell_rect, 0.0, dark);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 文本截断
// ---------------------------------------------------------------------------

/// 将文本缩小到不超过 `max_width` 的宽度（末尾加「…」）。
pub fn fit_text(ctx: &egui::Context, text: &str, font: &FontId, max_width: f32) -> String {
    if text.is_empty() {
        return String::new();
    }
    let width_of = |s: &str| {
        ctx.fonts_mut(|f| f.layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE))
            .size()
            .x
    };
    if width_of(text) <= max_width {
        return text.to_owned();
    }
    const ELLIPSIS: &str = "…";
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let cand: String = chars.iter().collect::<String>() + ELLIPSIS;
        if width_of(&cand) <= max_width {
            return cand;
        }
    }
    ELLIPSIS.to_string()
}

/// 用 `FontId::proportional(13.0)` 截断文本（列表行标签常用）。
pub fn truncate_label(ui: &egui::Ui, text: &str, max_width: f32) -> String {
    if max_width <= 0.0 {
        return text.to_owned();
    }
    if ui
        .ctx()
        .fonts_mut(|f| f.layout_no_wrap(text.to_owned(), FontId::proportional(13.0), Color32::WHITE))
        .size()
        .x
        <= max_width
    {
        return text.to_owned();
    }
    fit_text(ui.ctx(), text, &FontId::proportional(13.0), max_width)
}