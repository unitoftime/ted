//! Software renderer: draws a `Frame` into a softbuffer surface.
//!
//! Text is drawn on the monospace cell grid from a glyph cache: each (char, bold, italic)
//! is shaped and rasterized once, then blitted at `x + column * char_w`.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use cosmic_text::fontdb::Source;
use cosmic_text::{
    Attrs, Buffer as CosmicBuffer, Family, FontSystem, Metrics, Shaping, Style as CosmicStyle, SwashCache,
    SwashContent, Weight,
};
use softbuffer::{Context, Surface};
use ted_core::frame::{Color, DrawCmd, Frame, Rect, TextSpan};
use ted_core::view::WIDE_CONTINUATION;
use winit::window::Window;

/// A rasterized glyph, positioned relative to the top-left of its cell.
struct Glyph {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    /// Alpha mask, or RGBA for color glyphs (emoji).
    data: Vec<u8>,
    color: bool,
}

type GlyphKey = (char, bool, bool);

/// The monospace family all text is drawn in. It is compiled into the binary so text
/// looks the same on every machine; system fonts stay loaded as the fallback for what it
/// doesn't cover (CJK, emoji, symbols).
const FONT_FAMILY: &str = "Source Code Pro";

const FONT_FACES: [&[u8]; 4] = [
    include_bytes!("../fonts/SourceCodePro-Regular.otf"),
    include_bytes!("../fonts/SourceCodePro-Bold.otf"),
    include_bytes!("../fonts/SourceCodePro-It.otf"),
    include_bytes!("../fonts/SourceCodePro-BoldIt.otf"),
];

/// System fonts plus the bundled family, which replaces any installed copy of it: an
/// installed copy would win ties with the bundled one and could be a different version.
fn font_system() -> FontSystem {
    let mut font_system = FontSystem::new_with_fonts(FONT_FACES.map(|face| Source::Binary(Arc::new(face))));
    let db = font_system.db_mut();
    let installed: Vec<_> = db
        .faces()
        .filter(|face| !matches!(face.source, Source::Binary(_)))
        .filter(|face| face.families.iter().any(|(name, _)| name.eq_ignore_ascii_case(FONT_FAMILY)))
        .map(|face| face.id)
        .collect();
    for id in installed {
        db.remove_face(id);
    }
    font_system
}

fn attrs(bold: bool, italic: bool) -> Attrs<'static> {
    let attrs = Attrs::new().family(Family::Name(FONT_FAMILY));
    let attrs = if bold { attrs.weight(Weight::BOLD) } else { attrs };
    if italic {
        attrs.style(CosmicStyle::Italic)
    } else {
        attrs
    }
}

pub struct GuiRenderer {
    _context: Context<Arc<Window>>,
    surface: Surface<Arc<Window>, Arc<Window>>,
    font_system: FontSystem,
    swash_cache: SwashCache,
    shaper: CosmicBuffer,
    glyphs: HashMap<GlyphKey, Option<Glyph>>,
    /// (size, line height) in pixels the cell metrics and glyph cache are built for.
    font: (f32, f32),
    /// Baseline offset from the top of a cell, shared by every glyph (fallback fonts
    /// included) so text sits on one line: the primary font's ascent + descent is
    /// centered in the cell.
    baseline: f32,
    pub char_w: f32,
    pub line_h: f32,
}

impl GuiRenderer {
    /// A renderer drawing text at `font` (size, line height) in pixels.
    pub fn new(window: Arc<Window>, font: (f32, f32)) -> Self {
        let context = Context::new(window.clone()).expect("Failed to create softbuffer context");
        let surface = Surface::new(&context, window.clone()).expect("Failed to create softbuffer surface");
        let mut font_system = font_system();
        let shaper = CosmicBuffer::new(&mut font_system, Metrics::new(font.0, font.1));
        let mut renderer = Self {
            _context: context,
            surface,
            font_system,
            swash_cache: SwashCache::new(),
            shaper,
            glyphs: HashMap::new(),
            font: (0.0, 0.0),
            baseline: 0.0,
            char_w: 0.0,
            line_h: 0.0,
        };
        renderer.set_font(font);
        renderer
    }

    /// Switches to `font` (size, line height) in pixels: remeasures the cell and drops
    /// glyphs rasterized at the old size. Cheap when the font is unchanged.
    pub fn set_font(&mut self, font: (f32, f32)) {
        if font == self.font {
            return;
        }
        let (font_size, line_height) = font;
        self.font = font;
        self.shaper.set_metrics(&mut self.font_system, Metrics::new(font_size, line_height));
        self.shaper.set_text(&mut self.font_system, "M", attrs(false, false), Shaping::Advanced);
        self.shaper.shape_until_scroll(&mut self.font_system, false);
        let first_run = self.shaper.layout_runs().next();
        let char_w = first_run.as_ref().and_then(|run| run.glyphs.first()).map(|g| g.w).unwrap_or(9.0);
        self.baseline = first_run.map(|run| run.line_y).unwrap_or(font_size);
        self.char_w = char_w.max(font_size * 0.5);
        self.line_h = line_height;
        self.glyphs.clear();
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
            let _ = self.surface.resize(w, h);
        }
    }

    pub fn render(&mut self, frame: &Frame, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let Ok(mut buffer) = self.surface.buffer_mut() else {
            return;
        };
        let mut canvas = Canvas { px: &mut buffer, width, height };
        canvas.px.fill(pack(frame.bg_color));

        for cmd in &frame.commands {
            match cmd {
                DrawCmd::Rect { rect, color } => canvas.fill(*rect, *color),
                DrawCmd::Text(span) => {
                    if let Some(color) = span.style.underline {
                        let cells = span.text.chars().count() as f32;
                        let line = Rect::new(span.x, span.y + self.baseline + 2.0, cells * self.char_w, 1.0);
                        canvas.fill(clip_rect(line, span.clip), color);
                    }
                    for (col, ch) in span.text.chars().enumerate() {
                        if ch == ' ' || ch == WIDE_CONTINUATION {
                            continue;
                        }
                        let key = (ch, span.style.bold, span.style.italic);
                        if !self.glyphs.contains_key(&key) {
                            let glyph = rasterize(
                                &mut self.font_system,
                                &mut self.swash_cache,
                                &mut self.shaper,
                                self.baseline,
                                key,
                            );
                            self.glyphs.insert(key, glyph);
                        }
                        if let Some(glyph) = &self.glyphs[&key] {
                            canvas.blit(glyph, span, span.x + col as f32 * self.char_w);
                        }
                    }
                }
            }
        }

        if let Some(cur) = &frame.cursor {
            let rect = Rect::new(cur.x, cur.y, cur.w.max(1.0), cur.h);
            if cur.w <= 3.0 {
                canvas.fill(rect, cur.color);
            } else {
                canvas.invert(rect, cur.color);
            }
        }

        let _ = buffer.present();
    }
}

fn rasterize(
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    shaper: &mut CosmicBuffer,
    baseline: f32,
    (ch, bold, italic): GlyphKey,
) -> Option<Glyph> {
    let mut utf8 = [0u8; 4];
    shaper.set_text(font_system, ch.encode_utf8(&mut utf8), attrs(bold, italic), Shaping::Advanced);
    shaper.shape_until_scroll(font_system, false);
    let run = shaper.layout_runs().next()?;
    let physical = run.glyphs.first()?.physical((0.0, 0.0), 1.0);
    let image = swash_cache.get_image_uncached(font_system, physical.cache_key)?;
    Some(Glyph {
        left: physical.x + image.placement.left,
        top: baseline.round() as i32 + physical.y - image.placement.top,
        width: image.placement.width,
        height: image.placement.height,
        color: !matches!(image.content, SwashContent::Mask),
        data: image.data,
    })
}

fn clip_rect(rect: Rect, clip: Option<Rect>) -> Rect {
    let Some(c) = clip else { return rect };
    let (x0, y0) = (rect.x.max(c.x), rect.y.max(c.y));
    let (x1, y1) = ((rect.x + rect.w).min(c.x + c.w), (rect.y + rect.h).min(c.y + c.h));
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

#[inline(always)]
fn pack(c: Color) -> u32 {
    ((c.r as u32) << 16) | ((c.g as u32) << 8) | (c.b as u32)
}

#[inline(always)]
fn blend(dst: u32, r: u32, g: u32, b: u32, a: u32) -> u32 {
    let inv = 255 - a;
    let mix = |src: u32, shift: u32| (src * a + ((dst >> shift) & 0xFF) * inv) / 255;
    (mix(r, 16) << 16) | (mix(g, 8) << 8) | mix(b, 0)
}

struct Canvas<'a> {
    px: &'a mut [u32],
    width: u32,
    height: u32,
}

impl Canvas<'_> {
    /// Pixel bounds of `rect` clamped to the canvas. Edges round like glyph origins do,
    /// so cells, cursors and the glyphs in them line up to the pixel.
    fn bounds(&self, rect: Rect) -> (u32, u32, u32, u32) {
        let edge = |v: f32, max: u32| (v.max(0.0).round() as u32).min(max);
        (
            edge(rect.x, self.width),
            edge(rect.y, self.height),
            edge(rect.x + rect.w, self.width),
            edge(rect.y + rect.h, self.height),
        )
    }

    fn fill(&mut self, rect: Rect, color: Color) {
        let (x0, y0, x1, y1) = self.bounds(rect);
        let packed = pack(color);
        for y in y0..y1 {
            let row = &mut self.px[(y * self.width + x0) as usize..(y * self.width + x1) as usize];
            if color.a == 255 {
                row.fill(packed);
            } else if color.a > 0 {
                for p in row {
                    *p = blend(*p, color.r as u32, color.g as u32, color.b as u32, color.a as u32);
                }
            }
        }
    }

    /// XORs `rect` with `color` so text under a block cursor stays readable.
    fn invert(&mut self, rect: Rect, color: Color) {
        let (x0, y0, x1, y1) = self.bounds(rect);
        let packed = pack(color);
        for y in y0..y1 {
            for p in &mut self.px[(y * self.width + x0) as usize..(y * self.width + x1) as usize] {
                *p ^= packed;
            }
        }
    }

    fn blit(&mut self, glyph: &Glyph, span: &TextSpan, x: f32) {
        let clip = span.clip.unwrap_or(Rect::new(0.0, 0.0, self.width as f32, self.height as f32));
        let (cx0, cy0, cx1, cy1) = self.bounds(clip);
        let origin_x = x.round() as i32 + glyph.left;
        let origin_y = span.y.round() as i32 + glyph.top;
        let fg = span.style.fg;
        let stride = if glyph.color { 4 } else { 1 };
        for gy in 0..glyph.height as i32 {
            let py = origin_y + gy;
            if py < cy0 as i32 || py >= cy1 as i32 {
                continue;
            }
            for gx in 0..glyph.width as i32 {
                let px = origin_x + gx;
                if px < cx0 as i32 || px >= cx1 as i32 {
                    continue;
                }
                let i = (gy * glyph.width as i32 + gx) as usize * stride;
                let (r, g, b, a) = if glyph.color {
                    let d = &glyph.data[i..i + 4];
                    (d[0] as u32, d[1] as u32, d[2] as u32, d[3] as u32)
                } else {
                    (fg.r as u32, fg.g as u32, fg.b as u32, glyph.data[i] as u32 * fg.a as u32 / 255)
                };
                if a > 0 {
                    let idx = (py as u32 * self.width + px as u32) as usize;
                    self.px[idx] = blend(self.px[idx], r, g, b, a);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every style draws from its own bundled face: a fallback family's glyphs don't fit
    /// the cell grid, and an installed copy of the family could be another version.
    #[test]
    fn test_styles_use_bundled_faces() {
        let mut font_system = font_system();
        let mut buf = CosmicBuffer::new(&mut font_system, Metrics::new(14.0, 20.0));
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            buf.set_text(&mut font_system, "a", attrs(bold, italic), Shaping::Advanced);
            buf.shape_until_scroll(&mut font_system, false);
            let font_id = buf.layout_runs().next().and_then(|run| run.glyphs.first().map(|g| g.font_id)).unwrap();
            let face = font_system.db().face(font_id).unwrap();
            assert!(matches!(face.source, Source::Binary(_)), "{:?} is not bundled", face.post_script_name);
            assert_eq!(face.weight >= Weight::BOLD, bold, "{:?}", face.post_script_name);
            assert_eq!(face.style == CosmicStyle::Italic, italic, "{:?}", face.post_script_name);
        }
    }

    #[test]
    fn test_shaping_advanced_no_ligature_collapse() {
        let mut font_system = font_system();

        let mut buf = CosmicBuffer::new(&mut font_system, Metrics::new(14.0, 20.0));
        buf.set_text(&mut font_system, "Find file: ", attrs(false, false), Shaping::Advanced);
        buf.shape_until_scroll(&mut font_system, false);

        let glyphs: Vec<_> = buf.layout_runs().flat_map(|r| r.glyphs.iter().cloned()).collect();
        // "Find file: " has 11 chars. "fi" must not collapse into a single glyph.
        assert_eq!(glyphs.len(), 11);
    }

    #[test]
    fn test_shaping_advanced_makefile_lines() {
        let mut font_system = font_system();

        let mut buf = CosmicBuffer::new(&mut font_system, Metrics::new(14.0, 20.0));
        buf.set_text(&mut font_system, "all: build", attrs(false, false), Shaping::Advanced);
        buf.shape_until_scroll(&mut font_system, false);

        let glyphs: Vec<_> = buf.layout_runs().flat_map(|r| r.glyphs.iter().cloned()).collect();
        assert_eq!(glyphs.len(), 10);
        let char_w = glyphs[0].w;
        for (i, g) in glyphs.iter().enumerate() {
            assert!(
                (g.x - i as f32 * char_w).abs() < 0.1,
                "Glyph {} x mismatch: expected {}, got {}",
                i,
                i as f32 * char_w,
                g.x
            );
        }

        let makefile_line = "\tcargo build --release";
        buf.set_text(&mut font_system, makefile_line, attrs(false, false), Shaping::Advanced);
        buf.shape_until_scroll(&mut font_system, false);

        let adv_glyphs: Vec<_> = buf.layout_runs().flat_map(|r| r.glyphs.iter().cloned()).collect();
        assert_eq!(adv_glyphs.len(), makefile_line.len());
        // All non-tab characters must have identical width char_w
        for g in &adv_glyphs[1..] {
            assert!((g.w - char_w).abs() < 0.1, "Glyph width mismatch: expected {}, got {}", char_w, g.w);
        }
    }
}
