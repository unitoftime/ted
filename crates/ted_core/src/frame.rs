//! What the core hands a frontend to draw: a `Frame` of rectangles and styled text runs in
//! pixel coordinates, plus the cursor. Frontends only rasterize it.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);

    /// Parses `#rrggbb` or `#rrggbbaa`.
    pub fn parse_hex(s: &str) -> Option<Color> {
        let hex = s.strip_prefix('#')?;
        let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
        match hex.len() {
            6 => Some(Color::rgb(byte(0)?, byte(2)?, byte(4)?)),
            8 => Some(Color::rgba(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
            _ => None,
        }
    }
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    /// Underline color, if underlined.
    pub underline: Option<Color>,
}

impl Default for Style {
    fn default() -> Self {
        Self { fg: Color::rgb(220, 225, 230), bg: Color::rgb(20, 22, 26), bold: false, italic: false, underline: None }
    }
}

/// Font cell metrics supplied by the frontend. Everything in the core is laid out on a
/// monospace grid of `char_w` x `line_h` cells: text, cursors and highlights of a row all
/// start at the row's top, and the frontend places glyphs vertically within the cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub char_w: f32,
    pub line_h: f32,
}

impl Metrics {
    pub const fn new(char_w: f32, line_h: f32) -> Self {
        Self { char_w, line_h }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new(9.0, 22.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

#[derive(Debug, Clone)]
pub struct TextSpan {
    pub text: String,
    pub style: Style,
    pub x: f32,
    pub y: f32,
    pub clip: Option<Rect>,
}

#[derive(Debug, Clone)]
pub struct CursorVisual {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: Color,
}

#[derive(Debug, Clone)]
pub enum DrawCmd {
    Rect { rect: Rect, color: Color },
    Text(TextSpan),
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub width: f32,
    pub height: f32,
    pub bg_color: Color,
    pub commands: Vec<DrawCmd>,
    pub cursor: Option<CursorVisual>,
}

impl Frame {
    pub fn new(width: f32, height: f32, bg_color: Color) -> Self {
        Self { width, height, bg_color, commands: Vec::new(), cursor: None }
    }

    pub fn clear(&mut self, width: f32, height: f32, bg_color: Color) {
        self.width = width;
        self.height = height;
        self.bg_color = bg_color;
        self.commands.clear();
        self.cursor = None;
    }

    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        self.commands.push(DrawCmd::Rect { rect, color });
    }

    pub fn draw_rect_outline(&mut self, rect: Rect, stroke: f32, color: Color) {
        let stroke = stroke.max(1.0);
        self.fill_rect(Rect::new(rect.x, rect.y, rect.w, stroke), color);
        self.fill_rect(Rect::new(rect.x, rect.y + rect.h - stroke, rect.w, stroke), color);
        self.fill_rect(Rect::new(rect.x, rect.y, stroke, rect.h), color);
        self.fill_rect(Rect::new(rect.x + rect.w - stroke, rect.y, stroke, rect.h), color);
    }

    pub fn draw_text(&mut self, x: f32, y: f32, text: impl Into<String>, style: Style) {
        self.commands.push(DrawCmd::Text(TextSpan { text: text.into(), style, x, y, clip: None }));
    }

    pub fn draw_text_clipped(&mut self, x: f32, y: f32, text: impl Into<String>, style: Style, clip: Rect) {
        self.commands.push(DrawCmd::Text(TextSpan { text: text.into(), style, x, y, clip: Some(clip) }));
    }

    pub fn set_cursor(&mut self, cursor: CursorVisual) {
        self.cursor = Some(cursor);
    }

    /// Draws the primary cursor as a hollow box instead, as unfocused windows show theirs.
    pub fn hollow_cursor(&mut self) {
        if let Some(c) = self.cursor.take() {
            self.draw_rect_outline(Rect::new(c.x, c.y, c.w, c.h), 1.0, c.color);
        }
    }
}
