//! 跨区域复用的纯 egui 小组件与文本工具。
//!
//! 所有函数均为无 `self` 的纯 egui 组件，供其他 `ui/*.rs` 文件调用。

use crate::theme;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Sense, Stroke, Vec2,
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
// 音量悬浮弹层
// ---------------------------------------------------------------------------

/// 弹层与按钮之间的间距（px）。
const VOL_POPUP_GAP: f32 = 4.0;
/// 「指针在弹层上」判定的外扩余量（px），覆盖按钮与弹层之间的间距缝隙。
const VOL_POPUP_PAD: f32 = 6.0;

/// 音量弹层的跨帧状态。
#[derive(Clone, Copy)]
struct VolPopupState {
    /// 上一帧弹层的实际矩形（用于「指针还在弹层上则保持打开」）。
    rect: Rect,
    /// 本次按住是否发生在弹层内（锁存，松开鼠标复位）。
    ///
    /// 按住拖动期间必须保持弹层打开：滑块只有 ~18px 高，横向拖动时指针难免有
    /// 垂直漂移，一旦超出弹层矩形，弹层会连滑块一起消失、拖拽被中断——体感就是
    /// 「音量滑块拖不动」。
    ///
    /// 这里不能用 `Response::dragged()`：egui 的拖拽捕获要到按下后的**下一帧**才
    /// 生效（按下帧 `dragged()` 仍为 false），据此保持打开会慢一帧、弹层先关后拖。
    press_in_popup: bool,
}

/// 音量悬浮弹层：hover 音量按钮即在按钮正上方弹出可拖动滑块。
///
/// 不能用 egui 的 tooltip（`on_hover_ui`）承载滑块：tooltip 要求指针静止满
/// `tooltip_delay` 才出现，且首次出现的那一帧弹层内还没有交互控件，会被标记为
/// 不可交互，鼠标一移向滑块就关闭——体感就是「悬浮没反应 / 滑块拖不动」。
///
/// 这里自绘 [`egui::Area`]，打开条件为：按钮被悬浮、指针落在上一帧弹层矩形内，
/// 或本次按住起于弹层内（拖动期间保持打开）。返回本帧拖动后的新音量（`None` 未改变）。
pub fn volume_hover_popup(
    ui: &mut egui::Ui,
    btn_rect: Rect,
    on_button: bool,
    volume: f32,
) -> Option<f32> {
    let ctx = ui.ctx().clone();
    // 固定 id：整个应用只有一个音量弹层，避免依赖父 Ui 的自增 id 稳定性。
    let popup_id = egui::Id::new("sm_vol_hover_popup");

    let last = ctx.data(|d| d.get_temp::<VolPopupState>(popup_id));
    // 用 interact_pos 兜底：按住拖动时 hover_pos 可能为 None。
    let pointer = ctx.input(|i| i.pointer.interact_pos().or_else(|| i.pointer.hover_pos()));
    let held = ctx.input(|i| i.pointer.any_down());
    let in_rect =
        pointer.is_some_and(|p| last.is_some_and(|s| s.rect.expand(VOL_POPUP_PAD).contains(p)));
    // 按住且（本次按住起于弹层内 或 指针当前就在弹层内）→ 锁存保持打开。
    let latched = held && last.is_some_and(|s| s.press_in_popup || in_rect);

    if !(on_button || in_rect || latched) {
        if last.is_some() {
            ctx.data_mut(|d| d.remove::<VolPopupState>(popup_id));
        }
        return None;
    }

    // Area 首次显示会走 egui 的 sizing pass：该帧内容不可见，且矩形按估算尺寸计算，
    // 与下一帧的真实位置差很多（实测错位 100+px）。这一帧不能缓存矩形，否则会在播放条
    // 上留下一个错误的「保持打开」区域。
    let first_show = egui::AreaState::load(&ctx, popup_id).is_none();

    let mut changed: Option<f32> = None;
    let area = egui::Area::new(popup_id)
        .order(egui::Order::Tooltip)
        .pivot(Align2::CENTER_BOTTOM)
        // fixed_pos 每帧强制重定位（并隐含 movable=false），按钮位置变化时弹层跟随。
        .fixed_pos(Pos2::new(
            btn_rect.center().x,
            btn_rect.top() - VOL_POPUP_GAP,
        ))
        .constrain(true)
        .show(&ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let mut v = volume;
                    let slider = ui.add(
                        egui::Slider::new(&mut v, 0.0..=1.0)
                            .show_value(false)
                            .trailing_fill(true),
                    );
                    if slider.changed() {
                        changed = Some(v);
                    }
                    ui.label(
                        egui::RichText::new(format!("{:>3.0}%", volume * 100.0))
                            .color(theme::TEXT_SECONDARY)
                            .monospace()
                            .size(11.0),
                    );
                });
            });
        });
    if !first_show {
        ctx.data_mut(|d| {
            d.insert_temp(
                popup_id,
                VolPopupState {
                    rect: area.response.rect,
                    press_in_popup: latched,
                },
            )
        });
    }
    changed
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
// 圆形头像（状态栏左上）
// ---------------------------------------------------------------------------

/// 绘制圆形头像：有纹理画圆角图（圆角=半径即圆），无纹理画占位圆 + 昵称首字/音符。
pub fn paint_avatar(
    painter: &Painter,
    rect: Rect,
    texture_id: Option<egui::TextureId>,
    initial: Option<&str>,
) {
    let radius = (rect.width() * 0.5).round();
    let corner = CornerRadius::same(radius as u8);
    if let Some(tex) = texture_id {
        let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.add(
            egui::epaint::RectShape::filled(rect, corner, Color32::WHITE).with_texture(tex, uv),
        );
    } else {
        painter.circle_filled(rect.center(), radius, theme::BG_CARD);
        match initial {
            Some(ch) if !ch.is_empty() => {
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    ch,
                    FontId::proportional(rect.width() * 0.42),
                    theme::TEXT_SECONDARY,
                );
            }
            _ => {
                crate::icons::note(painter, rect.shrink(rect.width() * 0.24), theme::TEXT_WEAK);
            }
        }
    }
    painter.circle_stroke(rect.center(), radius, Stroke::new(1.0, theme::BORDER_SOFT));
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
mod vol_popup_tests {
    use super::*;
    use eframe::egui::{Event, Modifiers, PointerButton};

    /// 多帧 egui 输入模拟器：驱动 `volume_hover_popup` 并跟踪指针状态。
    struct Sim {
        ctx: egui::Context,
        btn: Rect,
        vol: f32,
        t: f64,
        pointer: Option<Pos2>,
        popup_id: egui::Id,
    }

    impl Sim {
        fn new() -> Self {
            let ctx = egui::Context::default();
            Self {
                ctx,
                btn: Rect::from_min_size(Pos2::new(480.0, 300.0), Vec2::splat(28.0)),
                vol: 0.5,
                t: 0.0,
                pointer: None,
                popup_id: egui::Id::new("sm_vol_hover_popup"),
            }
        }

        fn frame(&mut self, events: Vec<Event>) -> Option<Rect> {
            let mut input = egui::RawInput::default();
            input.screen_rect = Some(Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(1000.0, 400.0),
            ));
            self.t += 0.016;
            input.time = Some(self.t);
            input.events = events;
            let btn = self.btn;
            let on_button = self.pointer.is_some_and(|p| btn.contains(p));
            let vol = &mut self.vol;
            let mut full = self.ctx.run_ui(input, move |ui| {
                if let Some(v) = volume_hover_popup(ui, btn, on_button, *vol) {
                    *vol = v;
                }
            });
            // 无头测试不处理纹理，显式丢弃以免 epaint 断言未应用的 delta。
            full.textures_delta.clear();
            let id = self.popup_id;
            self.ctx
                .data(|d| d.get_temp::<VolPopupState>(id))
                .map(|s| s.rect)
        }

        fn hover(&mut self, p: Pos2) -> Option<Rect> {
            self.pointer = Some(p);
            self.frame(vec![Event::PointerMoved(p)])
        }

        fn press(&mut self, p: Pos2) -> Option<Rect> {
            self.pointer = Some(p);
            self.frame(vec![
                Event::PointerMoved(p),
                Event::PointerButton {
                    pos: p,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::default(),
                },
            ])
        }

        fn release(&mut self, p: Pos2) -> Option<Rect> {
            self.pointer = Some(p);
            self.frame(vec![Event::PointerButton {
                pos: p,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::default(),
            }])
        }
    }

    /// 让弹层稳定：Area 首帧走 sizing pass（内容不可见、矩形按估算尺寸算），
    /// 需在按钮上多停一帧，之后矩形才是真实位置。
    fn settle(sim: &mut Sim) -> Rect {
        sim.hover(sim.btn.center());
        sim.hover(sim.btn.center());
        sim.hover(sim.btn.center())
            .expect("悬浮音量按钮应立即弹出滑块弹层")
    }

    /// 回归：音量弹层 hover 立即出现、鼠标移到弹层上保持打开、滑块可按住拖动。
    #[test]
    fn volume_popup_hovers_open_and_slider_is_draggable() {
        let mut sim = Sim::new();

        // 1) 悬浮音量按钮 → 弹层立即出现（无静止延迟），并稳定下来。
        let rect = settle(&mut sim);
        assert!(rect.width() > 40.0, "弹层应包含完整滑块，实际 {rect:?}");

        // 2) 鼠标从按钮移到弹层上 → 弹层保持打开（不能中途关闭）。
        let center = rect.center();
        let rect2 = sim
            .hover(center)
            .expect("鼠标移到弹层上时弹层应保持打开");
        assert!(
            rect2.contains(center),
            "稳定后的弹层矩形应覆盖其自身中心，实际 {rect2:?} / {center:?}"
        );

        // 3) 在滑块上按下并横向拖动 → 音量必须随之改变。
        let before = sim.vol;
        sim.press(Pos2::new(rect2.min.x + rect2.width() * 0.2, center.y));
        sim.hover(Pos2::new(rect2.min.x + rect2.width() * 0.45, center.y));
        sim.hover(Pos2::new(rect2.min.x + rect2.width() * 0.6, center.y));
        sim.release(Pos2::new(rect2.min.x + rect2.width() * 0.6, center.y));
        assert!(
            sim.vol > before,
            "拖动滑块应改变音量（{before} -> {}），拖不动即为回归",
            sim.vol
        );

        // 4) 鼠标离开按钮与弹层 → 弹层关闭。
        assert!(
            sim.hover(Pos2::new(20.0, 20.0)).is_none(),
            "鼠标离开后弹层应关闭"
        );
    }

    /// 回归：拖动滑块时指针带垂直漂移（真实用户必然如此，滑块仅 ~18px 高）。
    /// 修复前弹层会在指针漂出弹层矩形的那一帧连滑块一起消失，拖拽被中断——
    /// 用户体感就是「音量滑块拖不动」。
    ///
    /// 断言必须落在「拖动是否跟随到最终位置」上：egui 滑块按下即定位，
    /// 只断言音量变过是无效测试（按下那一帧就已满足）。
    #[test]
    fn volume_slider_drag_survives_vertical_pointer_drift() {
        let mut sim = Sim::new();
        let rect = settle(&mut sim);
        let y = rect.center().y;

        // 在滑块左侧按下（按下即把音量设到该处，作为拖动起点）。
        let x0 = rect.min.x + rect.width() * 0.2;
        sim.press(Pos2::new(x0, y));
        let after_press = sim.vol;

        // 向右拖动，同时 y 持续漂到弹层矩形上方之外。
        let mut drift_y = y;
        for i in 1..=8 {
            let dx = rect.width() * 0.06 * i as f32;
            drift_y = y - 30.0 - i as f32;
            assert!(
                sim.hover(Pos2::new(x0 + dx, drift_y)).is_some(),
                "第 {i} 帧指针已漂到 y={drift_y}（弹层 {rect:?}）之外，弹层仍须保持打开"
            );
        }
        sim.release(Pos2::new(x0 + rect.width() * 0.48, drift_y));

        assert!(
            sim.vol > after_press + 0.15,
            "拖动必须跟随指针到最终位置（按下 {after_press} -> 最终 {}），\
             否则说明垂直漂移中断了拖拽 = 音量滑块拖不动",
            sim.vol
        );
    }
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
