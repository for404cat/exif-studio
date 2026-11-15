use std::collections::BTreeMap;

use eframe::egui::{
    self, Align2, Color32, FontId, Frame, Margin, Pos2, Rect, Response, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};

pub const BG: Color32 = Color32::from_rgb(15, 17, 21);
pub const PANEL: Color32 = Color32::from_rgb(23, 26, 32);
pub const PANEL_SOFT: Color32 = Color32::from_rgb(29, 33, 40);
pub const BORDER: Color32 = Color32::from_rgb(43, 48, 57);
pub const TEXT: Color32 = Color32::from_rgb(224, 228, 235);
pub const MUTED: Color32 = Color32::from_rgb(139, 147, 160);
pub const ACCENT: Color32 = Color32::from_rgb(232, 182, 102);
pub const TEAL: Color32 = Color32::from_rgb(94, 199, 187);
pub const RED: Color32 = Color32::from_rgb(235, 120, 120);
pub const GREEN: Color32 = Color32::from_rgb(122, 200, 140);

pub fn accent_soft(alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(ACCENT.r(), ACCENT.g(), ACCENT.b(), alpha)
}

pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(14)
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

pub fn collapsible_header(ui: &mut Ui, title: &str, hint: &str, collapsed: bool) -> bool {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click());
    if ui.is_rect_visible(rect) {
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, 8, Color32::from_rgba_unmultiplied(255, 255, 255, 7));
        }
        let expand =
            ui.ctx()
                .animate_bool_with_time(ui.make_persistent_id(title), !collapsed, 0.18);
        let angle = expand * std::f32::consts::FRAC_PI_2;
        let (sin, cos) = angle.sin_cos();
        let rotate = |x: f32, y: f32| Vec2::new(x * cos - y * sin, x * sin + y * cos);
        let center = rect.left_center() + Vec2::new(8.0, 0.0);
        let tip = center + rotate(2.0, 0.0);
        let upper = center + rotate(-3.0, -4.5);
        let lower = center + rotate(-3.0, 4.5);
        let stroke = Stroke::new(1.6, ACCENT);
        ui.painter().line_segment([upper, tip], stroke);
        ui.painter().line_segment([tip, lower], stroke);
        let title_font = FontId::proportional(15.5);
        ui.painter().text(
            rect.left_center() + Vec2::new(20.0, 0.0),
            Align2::LEFT_CENTER,
            title,
            title_font.clone(),
            TEXT,
        );
        if !hint.is_empty() {
            let title_width = ui
                .painter()
                .layout_no_wrap(title.to_string(), title_font, TEXT)
                .size()
                .x;
            ui.painter().text(
                rect.left_center() + Vec2::new(28.0 + title_width, 1.0),
                Align2::LEFT_CENTER,
                hint,
                FontId::proportional(11.5),
                MUTED,
            );
        }
        ui.painter().text(
            rect.right_center() - Vec2::new(2.0, 0.0),
            Align2::RIGHT_CENTER,
            if collapsed { "展开" } else { "折叠" },
            FontId::proportional(11.0),
            if response.hovered() { ACCENT } else { MUTED },
        );
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("点击展开或折叠")
        .clicked()
}

pub fn section_header(ui: &mut Ui, title: &str, hint: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(15.5).strong().color(TEXT));
        if !hint.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new(hint).size(11.5).color(MUTED));
        }
    });
    ui.add_space(10.0);
}

pub fn empty_hint(ui: &mut Ui, text: &str) {
    ui.add_space(14.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(text).size(12.5).color(MUTED));
    });
    ui.add_space(14.0);
}

pub fn pill(ui: &mut Ui, text: &str, color: Color32) -> Response {
    Frame::new()
        .fill(Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            28,
        ))
        .corner_radius(20)
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.5).color(color));
        })
        .response
}

pub fn bar_rows(ui: &mut Ui, data: &[(String, usize)], color: Color32) {
    if data.is_empty() {
        empty_hint(ui, "暂无可统计数据");
        return;
    }
    let max = data.iter().map(|(_, v)| *v).max().unwrap_or(1).max(1);
    for (label, value) in data {
        bar_row(ui, label, *value, max, color);
    }
}

fn bar_row(ui: &mut Ui, label: &str, value: usize, max: usize, color: Color32) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 23.0), Sense::hover());
    let label_width = (width * 0.34).clamp(90.0, 150.0);
    let count_text = value.to_string();
    let font = FontId::proportional(12.0);
    let count_width = ui
        .painter()
        .layout_no_wrap(count_text.clone(), font.clone(), TEXT)
        .size()
        .x;
    let bar_left = rect.left() + label_width;
    let bar_right = (rect.right() - count_width - 10.0).max(bar_left + 4.0);
    let bar_rect = Rect::from_min_max(
        Pos2::new(bar_left, rect.center().y - 3.5),
        Pos2::new(bar_right, rect.center().y + 3.5),
    );
    ui.painter().rect_filled(
        bar_rect,
        4,
        Color32::from_rgba_unmultiplied(255, 255, 255, 10),
    );
    let fraction = value as f32 / max as f32;
    if fraction > 0.0 {
        let filled = Rect::from_min_size(
            bar_rect.min,
            Vec2::new((bar_rect.width() * fraction).max(2.0), bar_rect.height()),
        );
        ui.painter().rect_filled(filled, 4, color);
    }
    ui.painter().text(
        Pos2::new(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        truncate(label, 20),
        font.clone(),
        MUTED,
    );
    ui.painter().text(
        Pos2::new(rect.right(), rect.center().y),
        Align2::RIGHT_CENTER,
        count_text,
        font,
        TEXT,
    );
}

pub fn vertical_bars(
    ui: &mut Ui,
    labels: &[String],
    values: &[usize],
    color: Color32,
    height: f32,
) {
    if labels.is_empty() || values.is_empty() {
        empty_hint(ui, "暂无时间数据");
        return;
    }
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height + 22.0), Sense::hover());
    let chart = Rect::from_min_max(
        Pos2::new(rect.left(), rect.top() + 14.0),
        Pos2::new(rect.right(), rect.bottom() - 22.0),
    );
    let max = values.iter().copied().max().unwrap_or(1).max(1);
    let count = labels.len().max(1);
    let gap = (chart.width() / count as f32 * 0.26).clamp(2.0, 10.0);
    let bar_width =
        ((chart.width() - gap * (count.saturating_sub(1)) as f32) / count as f32).max(2.0);

    for (index, (label, value)) in labels.iter().zip(values.iter()).enumerate() {
        let x = chart.left() + index as f32 * (bar_width + gap);
        let bar_height =
            (chart.height() * (*value as f32 / max as f32)).max(if *value > 0 { 3.0 } else { 0.0 });
        let bar_rect = Rect::from_min_max(
            Pos2::new(x, chart.bottom() - bar_height),
            Pos2::new(x + bar_width, chart.bottom()),
        );
        ui.painter().rect_filled(
            Rect::from_min_max(
                Pos2::new(x, chart.top()),
                Pos2::new(x + bar_width, chart.bottom()),
            ),
            4,
            Color32::from_rgba_unmultiplied(255, 255, 255, 8),
        );
        if *value > 0 {
            ui.painter().rect_filled(bar_rect, 4, color);
            if bar_width > 16.0 {
                ui.painter().text(
                    Pos2::new(x + bar_width / 2.0, bar_rect.top() - 2.0),
                    Align2::CENTER_BOTTOM,
                    value.to_string(),
                    FontId::proportional(10.0),
                    MUTED,
                );
            }
        }
        let show_label = count <= 13 || index % ((count as f32 / 10.0).ceil() as usize) == 0;
        if show_label && bar_width >= 10.0 {
            ui.painter().text(
                Pos2::new(x + bar_width / 2.0, rect.bottom() - 4.0),
                Align2::CENTER_BOTTOM,
                label,
                FontId::proportional(10.0),
                MUTED,
            );
        }
    }
}

pub fn years_chart(ui: &mut Ui, years: &BTreeMap<i32, usize>, color: Color32) {
    let labels: Vec<String> = years.keys().map(|y| y.to_string()).collect();
    let values: Vec<usize> = years.values().copied().collect();
    vertical_bars(ui, &labels, &values, color, 128.0);
}

pub fn months_chart(ui: &mut Ui, months: &[usize; 12], color: Color32) {
    let labels: Vec<String> = (1..=12).map(|m| format!("{m}月")).collect();
    vertical_bars(ui, &labels, months, color, 118.0);
}

pub fn gps_scatter(ui: &mut Ui, points: &[(f64, f64)]) {
    if points.is_empty() {
        empty_hint(ui, "这些文件中没有 GPS 位置信息");
        return;
    }
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 180.0), Sense::hover());
    let chart = rect.shrink2(Vec2::new(6.0, 8.0));
    ui.painter()
        .rect_filled(chart, 8, Color32::from_rgba_unmultiplied(255, 255, 255, 5));
    for i in 1..4 {
        let t = i as f32 / 4.0;
        let x = chart.left() + chart.width() * t;
        let y = chart.top() + chart.height() * t;
        ui.painter().line_segment(
            [Pos2::new(x, chart.top()), Pos2::new(x, chart.bottom())],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 8)),
        );
        ui.painter().line_segment(
            [Pos2::new(chart.left(), y), Pos2::new(chart.right(), y)],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 8)),
        );
    }
    ui.painter().line_segment(
        [
            Pos2::new(chart.left(), chart.center().y),
            Pos2::new(chart.right(), chart.center().y),
        ],
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 12)),
    );
    ui.painter().text(
        chart.left_top() + Vec2::new(6.0, 4.0),
        Align2::LEFT_TOP,
        "纬度",
        FontId::proportional(10.0),
        MUTED,
    );
    ui.painter().text(
        chart.right_bottom() - Vec2::new(6.0, 2.0),
        Align2::RIGHT_BOTTOM,
        "经度",
        FontId::proportional(10.0),
        MUTED,
    );

    let (mut min_lat, mut max_lat) = (f64::MAX, f64::MIN);
    let (mut min_lon, mut max_lon) = (f64::MAX, f64::MIN);
    for (lat, lon) in points {
        min_lat = min_lat.min(*lat);
        max_lat = max_lat.max(*lat);
        min_lon = min_lon.min(*lon);
        max_lon = max_lon.max(*lon);
    }
    let lat_span = (max_lat - min_lat).max(0.5);
    let lon_span = (max_lon - min_lon).max(0.5);
    for (lat, lon) in points {
        let x = chart.left() + ((lon - min_lon) / lon_span) as f32 * chart.width();
        let y = chart.bottom() - ((lat - min_lat) / lat_span) as f32 * chart.height();
        ui.painter().circle_filled(
            Pos2::new(x, y),
            2.6,
            Color32::from_rgba_unmultiplied(TEAL.r(), TEAL.g(), TEAL.b(), 190),
        );
    }
    ui.painter().text(
        chart.left_bottom() + Vec2::new(0.0, 2.0),
        Align2::LEFT_TOP,
        format!("{min_lat:.1}° – {max_lat:.1}°"),
        FontId::proportional(9.5),
        MUTED,
    );
    ui.painter().text(
        chart.right_bottom() + Vec2::new(0.0, 2.0),
        Align2::RIGHT_TOP,
        format!("{min_lon:.1}° – {max_lon:.1}°"),
        FontId::proportional(9.5),
        MUTED,
    );
}

pub fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, BORDER);
}

pub fn selected_fill(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode {
        accent_soft(26)
    } else {
        accent_soft(36)
    }
}

pub fn stroke_rect(ui: &Ui, rect: Rect, stroke: Stroke, radius: u8) {
    ui.painter()
        .rect_stroke(rect, radius, stroke, StrokeKind::Inside);
}

pub fn truncate(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        text.to_string()
    } else {
        let mut result: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        result.push('…');
        result
    }
}

pub fn app_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.visuals.dark_mode = true;
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = PANEL;
    style.visuals.extreme_bg_color = Color32::from_rgb(11, 13, 16);
    style.visuals.faint_bg_color = Color32::from_rgb(26, 29, 35);
    style.visuals.override_text_color = None;
    style.visuals.hyperlink_color = ACCENT;
    style.visuals.selection.bg_fill = accent_soft(60);
    style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.noninteractive.bg_fill = PANEL;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.noninteractive.corner_radius = 10.into();
    style.visuals.widgets.inactive.bg_fill = PANEL_SOFT;
    style.visuals.widgets.inactive.weak_bg_fill = PANEL_SOFT;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.inactive.corner_radius = 8.into();
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(38, 43, 52);
    style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(38, 43, 52);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(70, 78, 92));
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.hovered.corner_radius = 8.into();
    style.visuals.widgets.active.bg_fill = accent_soft(45);
    style.visuals.widgets.active.weak_bg_fill = accent_soft(45);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.active.corner_radius = 8.into();
    style.visuals.window_stroke = Stroke::new(1.0, BORDER);
    style.visuals.window_corner_radius = 14.into();
    style.visuals.menu_corner_radius = 10.into();
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(11.0, 6.0);
    style.spacing.interact_size.y = 28.0;
    style.spacing.scroll.bar_width = 9.0;
    // Keep the scroll handle large enough to grab even in 39k-item folders;
    // streaming lists shrink to this size instead of becoming a sliver.
    style.spacing.scroll.handle_min_length = 30.0;
    style.animation_time = 0.16;
    style.text_styles.insert(
        egui::TextStyle::Body,
        FontId::new(14.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        FontId::new(14.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        FontId::new(11.5, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Heading,
        FontId::new(22.0, egui::FontFamily::Proportional),
    );
    ctx.set_style_of(egui::Theme::Dark, style);
}
