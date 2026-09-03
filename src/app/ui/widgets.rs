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

/// 用 painter 直接画圆角封面图（纯绘制，不创建 widget）。
///
/// 若走 `ui.put(... egui::Image ...)` 会创建一个子 `Ui`，从而改变列表行与行之间的
/// 间距（实测行顶从 59px 变为 53px，整列内容依次上移 6px），导致封面下载完成后
/// 布局「跳位」抖动。这里改用形状绘制，与 [`paint_placeholder_cover`] 一致不参与
/// 布局，行间距在占位符↔图片之间保持不变。
pub fn paint_cover_image(painter: &Painter, rect: Rect, texture_id: egui::TextureId) {
    let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    painter.add(
        egui::epaint::RectShape::filled(
            rect,
            egui::CornerRadius::same(theme::CORNER),
            Color32::WHITE,
        )
        .with_texture(texture_id, uv),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 封面从占位符切换到图片时，歌曲行的垂直布局必须保持不变。
    ///
    /// 回归：之前用 `ui.put(cover_rect, egui::Image)` 绘制封面，`put` 会创建子 Ui
    /// 并改变行间距（行顶从 59px 变 53px），封面加载完成后整列内容依次上移产生抖动。
    #[test]
    fn cover_image_paint_does_not_shift_rows() {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let screen = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 700.0));
        let img = egui::ColorImage::filled([96, 96], Color32::from_rgb(200, 60, 60));
        let tex = ctx
            .load_texture("simple-music-cover:test", img, egui::TextureOptions::LINEAR)
            .id();

        let mut placeholder_tops = Vec::new();
        let mut image_tops = Vec::new();
        for use_image in [false, true] {
            let mut input = egui::RawInput::default();
            input.screen_rect = Some(screen);
            let mut full = ctx.run_ui(input, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for _ in 0..4 {
                            let (rect, _) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 56.0),
                                Sense::click(),
                            );
                            let cover = Rect::from_min_size(
                                Pos2::new(rect.left() + 10.0, rect.center().y - 22.0),
                                Vec2::splat(44.0),
                            );
                            if use_image {
                                paint_cover_image(ui.painter(), cover, tex);
                            } else {
                                paint_placeholder_cover(ui.painter(), cover);
                            }
                            let tops = if use_image {
                                &mut image_tops
                            } else {
                                &mut placeholder_tops
                            };
                            tops.push(rect.min.y);
                        }
                    });
            });
            full.textures_delta.clear();
        }

        assert_eq!(
            placeholder_tops,
            image_tops,
            "封面加载后不应改变歌曲行布局（防止整列内容跳位抖动）"
        );
    }
}
