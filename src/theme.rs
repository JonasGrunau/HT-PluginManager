//! House style from the audio-plugin SDK (PhaseFucker / BuildApp core):
//! IBM Plex Sans Condensed + Plex Mono, flat drawing, one 2 px radius, crop
//! marks, lettered sections, the LEFT/RIGHT wordmark, a footer help line and
//! one 880-wide sheet scaled as a whole.
//!
//! Palette: "graphite", a monochrome night sheet. Every plug-in owns a hue
//! (vermilion, acid, ultraviolet, magenta, PsyBox's rainbow); the manager owns
//! none, so it never reads as one of them. The neutrals are true greys
//! (chroma 0), unlike the warm house night sheet, and sit on the house
//! contrast ladder: ink 15 : 1, inkSoft 6.4 : 1, inkFaint 2.6 : 1,
//! hairline 1.35 : 1, grid 1.1 : 1.
//!
//! ACCENT MEANS: what needs you or is happening now, i.e. an update waiting
//! or a job running. Without a hue the accent is a solid ink fill with
//! sheet-coloured text; everything at rest is an outline or steps back to inkFaint.

use egui::epaint::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Galley, Painter, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2,
    vec2,
};
use std::sync::Arc;

pub mod col {
    use egui::Color32;
    pub const SHEET: Color32 = Color32::from_rgb(0x11, 0x11, 0x11);
    pub const WELL: Color32 = Color32::from_rgb(0x0A, 0x0A, 0x0A);
    pub const RAISED: Color32 = Color32::from_rgb(0x19, 0x19, 0x19);
    pub const INK: Color32 = Color32::from_rgb(0xE6, 0xE6, 0xE6);
    pub const INK_SOFT: Color32 = Color32::from_rgb(0x96, 0x96, 0x96);
    pub const INK_FAINT: Color32 = Color32::from_rgb(0x57, 0x57, 0x57);
    pub const HAIRLINE: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x2C);
    pub const GRID: Color32 = Color32::from_rgb(0x1C, 0x1C, 0x1C);
    /// The accent: lit keys and running jobs. Pure white so a lit key under
    /// the pointer still brightens.
    pub const LIT: Color32 = INK;
    pub const LIT_HOVER: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
}

/// The type scale (`theme::size` in the plug-in template). No other sizes.
pub mod size {
    pub const WORDMARK: f32 = 27.0;
    pub const SUBLINE: f32 = 9.5;
    pub const SECTION: f32 = 10.0;
    pub const TAG_LETTER: f32 = 9.0;
    pub const CONTROL: f32 = 9.5;
    pub const VALUE: f32 = 11.0;
    pub const HELP: f32 = 10.5;
    pub const FOOT_RIGHT: f32 = 9.5;
    pub const INFO: f32 = 10.0;
    pub const EMPTY: f32 = 13.0;
    pub const READOUT: f32 = 30.0;
    /// Product names in the list: a wordmark one step under the masthead's.
    pub const PRODUCT: f32 = 20.0;
    pub const BODY: f32 = 12.5;
}

/// Sheet geometry, in logical pixels of the 880-wide sheet.
pub mod sheet {
    pub const W: f32 = 880.0;
    pub const LEFT: f32 = 24.0;
    pub const RIGHT: f32 = 856.0;
    pub const CONTENT_W: f32 = RIGHT - LEFT;
    /// The app's own title bar above the sheet.
    /// The app's own title bar. On macOS the traffic lights sit centred on it
    /// with 10.5 pt above and below, a little more than their 9 pt left inset.
    pub const TITLE_H: f32 = 36.0;
    pub const MAST_H: f32 = 76.0;
    pub const MAST_RULE_Y: f32 = 74.0;
    /// Section tag 12 below the masthead rule, content 20 below the tag.
    pub const TAG_Y: f32 = 86.0;
    pub const TAG_TO_CONTENT: f32 = 20.0;
    /// Footer rule at H − 32, help line in the 22 px band under it.
    pub const FOOTER_H: f32 = 32.0;
    pub const RADIUS: u8 = 2;
    /// PB's 4 px spacing scale, used inside sections.
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 24.0;
    pub const GROUP_GAP: f32 = 20.0;
}

pub fn radius() -> CornerRadius {
    CornerRadius::same(sheet::RADIUS)
}

#[derive(Clone, Copy)]
pub enum Face {
    /// Plex Sans Condensed Medium: running text.
    Text,
    /// Plex Sans Condensed SemiBold: captions, keys, titles.
    Label,
    /// Plex Mono Regular: every number.
    Mono,
    /// Plex Mono Medium: readouts.
    MonoStrong,
}

impl Face {
    fn family(self) -> FontFamily {
        FontFamily::Name(
            match self {
                Face::Text => "cond",
                Face::Label => "cond-semi",
                Face::Mono => "mono",
                Face::MonoStrong => "mono-medium",
            }
            .into(),
        )
    }
}

pub fn font(face: Face, size: f32) -> FontId {
    FontId::new(size, face.family())
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let faces: [(&str, &'static [u8]); 4] = [
        ("cond", include_bytes!("../assets/fonts/IBMPlexSansCondensed-Medium.ttf")),
        ("cond-semi", include_bytes!("../assets/fonts/IBMPlexSansCondensed-SemiBold.ttf")),
        ("mono", include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf")),
        ("mono-medium", include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf")),
    ];
    for (name, bytes) in faces {
        fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_static(bytes)));
        // Plex has no arrows or ellipsis in every cut; egui's defaults fill gaps.
        let mut chain = vec![name.to_string()];
        chain.extend(fonts.families[&FontFamily::Proportional].iter().cloned());
        fonts.families.insert(FontFamily::Name(name.into()), chain);
    }
    fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "cond".into());
    fonts.families.get_mut(&FontFamily::Monospace).unwrap().insert(0, "mono".into());
    ctx.set_fonts(fonts);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = col::SHEET;
    visuals.window_fill = col::SHEET;
    visuals.extreme_bg_color = col::WELL;
    visuals.override_text_color = Some(col::INK);
    visuals.selection.bg_fill = col::INK_FAINT;
    visuals.hyperlink_color = col::INK;
    for w in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        w.corner_radius = radius();
        w.bg_fill = col::HAIRLINE;
        w.weak_bg_fill = col::HAIRLINE;
        w.bg_stroke = Stroke::NONE;
        w.fg_stroke = Stroke::new(1.0, col::INK_SOFT);
        w.expansion = 0.0;
    }
    visuals.widgets.hovered.bg_fill = col::INK_FAINT;
    visuals.widgets.active.bg_fill = col::INK_SOFT;
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|s| {
        s.spacing.scroll = egui::style::ScrollStyle::thin();
        s.spacing.scroll.bar_width = 4.0;
        s.spacing.scroll.floating_allocated_width = 0.0;
        s.spacing.item_spacing = vec2(0.0, 0.0);
    });
}

/// Lays out text with tracking (`extra_letter_spacing`, in px). `wrap` is
/// `(width, rows)`; overflow ends in an ellipsis.
pub fn layout(painter: &Painter, text: &str, font: FontId, color: Color32, tracking: f32, wrap: Option<(f32, usize)>) -> Arc<Galley> {
    let mut job = LayoutJob::default();
    job.append(text, 0.0, TextFormat { font_id: font, color, extra_letter_spacing: tracking, ..Default::default() });
    if let Some((width, rows)) = wrap {
        job.wrap = TextWrapping { max_width: width, max_rows: rows, break_anywhere: false, overflow_character: Some('…') };
    }
    painter.layout_job(job)
}

/// Draws text anchored like `Painter::text`, with tracking. Returns its rect.
pub fn text(painter: &Painter, pos: Pos2, anchor: Align2, s: &str, font: FontId, color: Color32, tracking: f32) -> Rect {
    let g = layout(painter, s, font, color, tracking, None);
    let rect = anchor.anchor_size(pos, g.size());
    painter.galley(rect.min, g, color);
    rect
}

/// Uppercase caption tracked +0.12 em, the house label.
pub fn caption(painter: &Painter, pos: Pos2, anchor: Align2, s: &str, size: f32, color: Color32) -> Rect {
    text(painter, pos, anchor, &s.to_uppercase(), font(Face::Label, size), color, size * 0.12)
}

/// Every number in mono.
pub fn mono(painter: &Painter, pos: Pos2, anchor: Align2, s: &str, size: f32, color: Color32) -> Rect {
    text(painter, pos, anchor, s, font(Face::Mono, size), color, 0.0)
}

pub fn text_width(painter: &Painter, s: &str, font: FontId, tracking: f32) -> f32 {
    layout(painter, s, font, Color32::WHITE, tracking, None).size().x
}

/// `LEFT/RIGHT`: one word split by a slash, set tight. The slash takes the
/// accent role; in graphite that is inkFaint, so the name carries the weight.
pub fn wordmark(painter: &Painter, left_top: Pos2, left: &str, right: &str, size: f32) -> Rect {
    let f = font(Face::Label, size);
    let tracking = -0.01 * size;
    let a = text(painter, left_top, Align2::LEFT_TOP, left, f.clone(), col::INK, tracking);
    if right.is_empty() {
        return a;
    }
    let slash = text(painter, pos2(a.right() + 1.0, left_top.y), Align2::LEFT_TOP, "/", f.clone(), col::INK_FAINT, tracking);
    let b = text(painter, pos2(slash.right() + 1.0, left_top.y), Align2::LEFT_TOP, right, f, col::INK, tracking);
    a.union(b)
}

/// The "printed sheet" cue: inset 8, length 7, inkFaint, all four corners.
pub fn crop_marks(painter: &Painter, area: Rect) {
    let r = area.shrink(8.0);
    let len = 7.0;
    let s = Stroke::new(1.0, col::INK_FAINT);
    for (corner, dx, dy) in
        [(r.left_top(), 1.0, 1.0), (r.right_top(), -1.0, 1.0), (r.left_bottom(), 1.0, -1.0), (r.right_bottom(), -1.0, -1.0)]
    {
        painter.line_segment([corner, corner + vec2(len * dx, 0.0)], s);
        painter.line_segment([corner, corner + vec2(0.0, len * dy)], s);
    }
}

/// `[A] NAME`: 13 px box with the letter, the name 6 px after it.
pub fn section_tag(painter: &Painter, left_top: Pos2, letter: &str, name: &str) -> Rect {
    let boxr = Rect::from_min_size(left_top, Vec2::splat(13.0));
    painter.rect_stroke(boxr, radius(), Stroke::new(1.0, col::INK_SOFT), StrokeKind::Inside);
    caption(painter, boxr.center() + vec2(0.5, 0.0), Align2::CENTER_CENTER, letter, size::TAG_LETTER, col::INK);
    let name_r = caption(painter, pos2(boxr.right() + 6.0, boxr.center().y), Align2::LEFT_CENTER, name, size::SECTION, col::INK);
    boxr.union(name_r)
}

/// Diagonal hatching inside `rect`: what is missing or blocked.
pub fn hatch(painter: &Painter, rect: Rect, color: Color32) {
    let p = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let step = 6.0;
    let mut x = rect.left() - rect.height();
    while x < rect.right() {
        p.line_segment([pos2(x, rect.bottom()), pos2(x + rect.height(), rect.top())], Stroke::new(1.0, color));
        x += step;
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum KeyKind {
    /// The accent: solid ink, sheet text. For what needs you.
    Lit,
    /// Outline in inkSoft: an action at rest.
    Outline,
    /// Hairline outline, faint text: a secondary action that stays out of the
    /// way of the primary one (BUILD FROM MAIN under INSTALL). It comes up to an
    /// outline key only under the pointer.
    Secondary,
    /// Hairline outline, soft text: a minor action.
    Quiet,
}

/// A house key: radius 2, ≥ 8 px text padding, uppercase label.
pub fn key(ui: &mut Ui, rect: Rect, label: &str, kind: KeyKind, enabled: bool) -> Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let resp = ui.allocate_rect(rect, sense);
    let hot = enabled && resp.hovered();
    let pressed = enabled && resp.is_pointer_button_down_on();
    let painter = ui.painter();
    let (fill, stroke, text_col) = match (kind, enabled) {
        (_, false) => (None, col::HAIRLINE, col::INK_FAINT),
        (KeyKind::Lit, _) => (Some(if hot { col::LIT_HOVER } else { col::LIT }), Color32::TRANSPARENT, col::SHEET),
        (KeyKind::Outline, _) => (pressed.then_some(col::RAISED), if hot { col::INK } else { col::INK_SOFT }, col::INK),
        (KeyKind::Secondary, _) => {
            (pressed.then_some(col::RAISED), if hot { col::INK_SOFT } else { col::HAIRLINE }, if hot { col::INK } else { col::INK_FAINT })
        }
        (KeyKind::Quiet, _) => {
            (pressed.then_some(col::RAISED), if hot { col::INK_SOFT } else { col::HAIRLINE }, if hot { col::INK } else { col::INK_SOFT })
        }
    };
    if let Some(fill) = fill {
        painter.rect_filled(rect, radius(), fill);
    }
    if stroke != Color32::TRANSPARENT {
        painter.rect_stroke(rect, radius(), Stroke::new(1.0, stroke), StrokeKind::Inside);
    }
    let s = if rect.height() >= 30.0 { size::CONTROL + 0.5 } else { size::CONTROL };
    caption(painter, rect.center() + vec2(0.5, 0.5), Align2::CENTER_CENTER, label, s, text_col);
    if hot {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Width a key needs for `label`: text plus 12 px a side.
pub fn key_width(painter: &Painter, label: &str) -> f32 {
    text_width(painter, &label.to_uppercase(), font(Face::Label, size::CONTROL), size::CONTROL * 0.12) + 24.0
}

/// A thin progress track: hairline well, lit fill. `frac` None = indeterminate.
pub fn progress(painter: &Painter, rect: Rect, frac: Option<f32>, time: f64) {
    painter.rect_filled(rect, radius(), col::HAIRLINE);
    match frac {
        Some(f) => {
            let w = rect.width() * f.clamp(0.0, 1.0);
            painter.rect_filled(Rect::from_min_size(rect.min, vec2(w, rect.height())), radius(), col::LIT);
        }
        None => {
            // A segment a quarter wide sweeping across: work without a count.
            let seg = rect.width() * 0.25;
            let t = (time * 0.6).fract() as f32;
            let x = rect.left() - seg + (rect.width() + seg) * t;
            let r = Rect::from_min_max(pos2(x.max(rect.left()), rect.top()), pos2((x + seg).min(rect.right()), rect.bottom()));
            if r.width() > 0.0 {
                painter.rect_filled(r, radius(), col::LIT);
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum WindowGlyph {
    Minimize,
    Zoom,
    Close,
}

/// A title-bar key: a hairline glyph at rest, a raised well under the
/// pointer; close fills with ink, like every key that acts at once.
pub fn window_key(ui: &mut Ui, rect: Rect, glyph: WindowGlyph) -> Response {
    let resp = ui.allocate_rect(rect, Sense::click());
    let hot = resp.hovered();
    let painter = ui.painter();
    let (fill, ink) = match (glyph, hot) {
        (WindowGlyph::Close, true) => (Some(col::LIT), col::SHEET),
        (_, true) => (Some(col::RAISED), col::INK),
        (_, false) => (None, col::INK_SOFT),
    };
    if let Some(fill) = fill {
        painter.rect_filled(rect, radius(), fill);
    }
    let c = rect.center();
    let s = Stroke::new(1.2, ink);
    let h = 4.0;
    match glyph {
        WindowGlyph::Minimize => {
            painter.line_segment([c + vec2(-h, 0.0), c + vec2(h, 0.0)], s);
        }
        WindowGlyph::Zoom => {
            painter.rect_stroke(Rect::from_center_size(c, Vec2::splat(2.0 * h)), CornerRadius::ZERO, s, StrokeKind::Inside);
        }
        WindowGlyph::Close => {
            painter.line_segment([c + vec2(-h, -h), c + vec2(h, h)], s);
            painter.line_segment([c + vec2(-h, h), c + vec2(h, -h)], s);
        }
    }
    resp
}
