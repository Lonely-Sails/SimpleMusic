//! 纯 painter 矢量图标：播放 / 暂停 / 上一首 / 下一首 / 音量 / 关闭。
//!
//! 背景：egui 默认字体对 ⏮(U+23EE) ⏭(U+23ED) ⏸(U+23F8) ▶(U+25B6) 等媒体控制码点
//! 缺字形（NotoEmoji 未收录），在 macOS 上渲染成 "?"。本模块用 painter 绘制
//! 线条/填充图形，不依赖任何字体字形，跨平台稳定。
//!
//! 约定：所有函数签名统一为
//! `fn xxx(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32)`，
//! 图形居中于 `rect`，可用 `rect.shrink(…)` 控制图标留白。

use eframe::egui::{
    epaint::PathStroke, pos2, Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, Vec2,
};

/// 播放：实心右三角。
pub fn play(painter: &Painter, rect: Rect, color: Color32) {
    let w = rect.width();
    let h = rect.height();
    let points = vec![
        pos2(rect.left() + w * 0.30, rect.top() + h * 0.18),
        pos2(rect.left() + w * 0.30, rect.bottom() - h * 0.18),
        pos2(rect.right() - w * 0.18, rect.center().y),
    ];
    painter.add(Shape::convex_polygon(points, color, PathStroke::NONE));
}

/// 暂停：两条圆头竖杠。
pub fn pause(painter: &Painter, rect: Rect, color: Color32) {
    let w = rect.width();
    let gap = w * 0.18;
    let bar_w = (w - gap * 2.0) * 0.5;
    let corner = CornerRadius::same((bar_w * 0.5).clamp(1.0, 8.0) as u8);
    for x in [rect.left() + gap, rect.right() - gap - bar_w] {
        painter.rect_filled(
            Rect::from_min_max(pos2(x, rect.top()), pos2(x + bar_w, rect.bottom())),
            corner,
            color,
        );
    }
}

/// 上一首：竖条 + 左三角。
pub fn prev(painter: &Painter, rect: Rect, color: Color32) {
    let w = rect.width();
    let h = rect.height();
    let bar_w = w * 0.14;
    // 右侧竖条。
    let bar = Rect::from_min_max(
        pos2(rect.right() - bar_w, rect.top() + h * 0.16),
        pos2(rect.right(), rect.bottom() - h * 0.16),
    );
    painter.rect_filled(bar, CornerRadius::same(2), color);
    // 左三角（尖端朝左）。
    let points = vec![
        pos2(rect.left() + w * 0.02, rect.center().y),
        pos2(bar.left() - w * 0.06, rect.top() + h * 0.18),
        pos2(bar.left() - w * 0.06, rect.bottom() - h * 0.18),
    ];
    painter.add(Shape::convex_polygon(points, color, PathStroke::NONE));
}

/// 下一首：右三角 + 竖条。
pub fn next(painter: &Painter, rect: Rect, color: Color32) {
    let w = rect.width();
    let h = rect.height();
    let bar_w = w * 0.14;
    // 左侧竖条。
    let bar = Rect::from_min_max(
        pos2(rect.left(), rect.top() + h * 0.16),
        pos2(rect.left() + bar_w, rect.bottom() - h * 0.16),
    );
    painter.rect_filled(bar, CornerRadius::same(2), color);
    // 右三角（尖端朝右）。
    let points = vec![
        pos2(rect.right() - w * 0.02, rect.center().y),
        pos2(bar.right() + w * 0.06, rect.top() + h * 0.18),
        pos2(bar.right() + w * 0.06, rect.bottom() - h * 0.18),
    ];
    painter.add(Shape::convex_polygon(points, color, PathStroke::NONE));
}

/// 音量：喇叭体 + 两段声波弧线。
pub fn volume(painter: &Painter, rect: Rect, color: Color32) {
    let w = rect.width();
    let h = rect.height();
    let cy = rect.center().y;
    // 喇叭体（小圆角方块 + 锥形）。
    let box_w = w * 0.34;
    let box_h = h * 0.42;
    let box_rect = Rect::from_min_size(
        pos2(rect.left() + w * 0.02, cy - box_h * 0.5),
        Vec2::new(box_w, box_h),
    );
    painter.rect_filled(box_rect, CornerRadius::same(2), color);
    let cone = vec![
        pos2(box_rect.right(), cy - h * 0.30),
        pos2(box_rect.right(), cy + h * 0.30),
        pos2(box_rect.right() + w * 0.20, cy),
    ];
    painter.add(Shape::convex_polygon(cone, color, PathStroke::NONE));
    // 两段声波弧线（朝右开口）。
    let arc_cx = box_rect.right() + w * 0.26;
    for r in [h * 0.30, h * 0.46] {
        let points = arc_points(pos2(arc_cx, cy), r, -0.85, 0.85);
        painter.line(points, PathStroke::new(2.0, color));
    }
}

/// 关闭 ✕：两条交叉线段。
pub fn cross(painter: &Painter, rect: Rect, color: Color32) {
    let inset = rect.width() * 0.24;
    let stroke = Stroke::new(2.0, color);
    painter.line_segment(
        [
            pos2(rect.left() + inset, rect.top() + inset),
            pos2(rect.right() - inset, rect.bottom() - inset),
        ],
        stroke,
    );
    painter.line_segment(
        [
            pos2(rect.right() - inset, rect.top() + inset),
            pos2(rect.left() + inset, rect.bottom() - inset),
        ],
        stroke,
    );
}

/// 自定义标题栏：最小化 —— 一条居中短横线。
pub fn window_minimize(painter: &Painter, rect: Rect, color: Color32) {
    let y = rect.center().y;
    let w = rect.width() * 0.52;
    let stroke = Stroke::new(2.0, color);
    painter.line_segment(
        [
            pos2(rect.center().x - w * 0.5, y),
            pos2(rect.center().x + w * 0.5, y),
        ],
        stroke,
    );
}

/// 自定义标题栏：右下角缩放把手 —— 三条递增斜线。
pub fn window_resize(painter: &Painter, rect: Rect, color: Color32) {
    let stroke = Stroke::new(1.6, color);
    let right = rect.right() - rect.width() * 0.06;
    let bottom = rect.bottom() - rect.height() * 0.06;
    let step = rect.width() * 0.16;
    for i in 0..3 {
        let len = rect.width() * (0.12 + i as f32 * 0.09);
        let x0 = right - step * (2 - i) as f32 - len;
        let y0 = bottom - step * i as f32;
        painter.line_segment(
            [pos2(x0, y0), pos2(x0 + len, y0 - len)],
            stroke,
        );
    }
}

/// 生成一段圆弧上的点（用于音量弧线等）。
fn arc_points(center: Pos2, radius: f32, start: f32, end: f32) -> Vec<Pos2> {
    const N: usize = 10;
    (0..=N)
        .map(|i| {
            let t = start + (end - start) * (i as f32 / N as f32);
            center + Vec2::angled(t) * radius
        })
        .collect()
}
