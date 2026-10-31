//! Layout and shapes of the mixer window, free of Windows calls so they can be tested anywhere:
//! `rustc --edition 2024 --test src/mixer_ui.rs`. `win::mixer` draws the text over them.
//!
//! Sizes are given at 96 DPI. Pixels are `0xRRGGBB`, as in a 32-bit Windows bitmap.

pub const ROWS: usize = 7;

pub const TEXT: u32 = 0xE8EAF0;
pub const SECONDARY_TEXT: u32 = 0x9BA1AE;
pub const MUTED_ICON: u32 = 0xFF7A33;
pub const HOVERED_ICON: u32 = 0xFFFFFF;

const BACKGROUND: u32 = 0x16181D;
const SELECTED_BACKGROUND: u32 = 0x262A33;
const LINE: u32 = 0x2A2D35;
const TRACK: u32 = 0x343843;
const FILL: u32 = 0xC4C8D2;
const SELECTED_FILL: u32 = 0xFF5A00;
const MUTED_FILL: u32 = 0x5C616E;
const THUMB: u32 = 0xFFFFFF;
const DISABLED_THUMB: u32 = 0x7D828E;

const WIDTH: f32 = 372.0;
const HEADER_HEIGHT: f32 = 48.0;
const ROW_HEIGHT: f32 = 44.0;
const SECTION_HEIGHT: f32 = 28.0;
const SECTION_GAP: f32 = 10.0;
const SIDE_PADDING: f32 = 16.0;
const LABEL_WIDTH: f32 = 56.0;
const COLUMN_GAP: f32 = 8.0;
const VALUE_WIDTH: f32 = 38.0;
const MUTE_WIDTH: f32 = 32.0;
const END_PADDING: f32 = 8.0;
const CHAT_PADDING: f32 = 12.0;
const CHAT_TITLE_HEIGHT: f32 = 20.0;
const CHAT_TITLE_GAP: f32 = 4.0;
const CHAT_SLIDER_HEIGHT: f32 = 28.0;
const CHAT_END_LABEL: f32 = 40.0;
const CHAT_NOTE_HEIGHT: f32 = 22.0;
const CHAT_BOTTOM_PADDING: f32 = 14.0;
const CHAT_STEP: i32 = 5;
const FOOTER_HEIGHT: f32 = 36.0;
const FOOTER_PADDING: f32 = 10.0;
const ICON_SIZE: f32 = 24.0;
const TRACK_RADIUS: f32 = 2.0;
const THUMB_RADIUS: f32 = 7.0;
const THUMB_HALO: f32 = 3.0;
const TICK_WIDTH: f32 = 2.0;
const TICK_HEIGHT: f32 = 12.0;
// Grabbing a slider doesn't need pixel precision.
const SLIDER_MARGIN: f32 = 10.0;

/// A slider of the window: one of the rows, or ChatMix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Spot {
    Row(usize),
    ChatMix,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Slider(Spot),
    Mute(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub struct Layout {
    scale: f32,
    has_chat_note: bool,
}

impl Layout {
    pub fn new(dpi: u32, has_chat_note: bool) -> Self {
        Self {
            scale: dpi as f32 / 96.0,
            has_chat_note,
        }
    }

    fn px(&self, value: f32) -> i32 {
        (value * self.scale).round() as i32
    }

    pub fn scaled(&self, value: f32) -> i32 {
        self.px(value)
    }

    fn frame(&self, x: f32, y: f32, width: f32, height: f32) -> Frame {
        Frame {
            x: self.px(x),
            y: self.px(y),
            width: self.px(width),
            height: self.px(height),
        }
    }

    fn row_top(&self, row: usize) -> f32 {
        if row == 0 {
            HEADER_HEIGHT
        } else {
            HEADER_HEIGHT + ROW_HEIGHT + SECTION_HEIGHT + ROW_HEIGHT * (row - 1) as f32
        }
    }

    fn chat_top(&self) -> f32 {
        self.row_top(ROWS) + SECTION_GAP
    }

    fn chat_slider_top(&self) -> f32 {
        self.chat_top() + CHAT_PADDING + CHAT_TITLE_HEIGHT + CHAT_TITLE_GAP
    }

    fn chat_bottom(&self) -> f32 {
        let note = if self.has_chat_note {
            CHAT_NOTE_HEIGHT
        } else {
            0.0
        };
        self.chat_slider_top() + CHAT_SLIDER_HEIGHT + note + CHAT_BOTTOM_PADDING
    }

    pub fn size(&self) -> (i32, i32) {
        (self.px(WIDTH), self.px(self.chat_bottom() + FOOTER_HEIGHT))
    }

    pub fn icon(&self) -> Frame {
        let top = (HEADER_HEIGHT - ICON_SIZE) / 2.0;
        self.frame(SIDE_PADDING, top, ICON_SIZE, ICON_SIZE)
    }

    pub fn title(&self) -> Frame {
        let x = SIDE_PADDING + ICON_SIZE + COLUMN_GAP;
        self.frame(x, 0.0, 130.0, HEADER_HEIGHT)
    }

    pub fn output_name(&self) -> Frame {
        let x = SIDE_PADDING + ICON_SIZE + COLUMN_GAP + 130.0;
        self.frame(x, 0.0, WIDTH - x - SIDE_PADDING, HEADER_HEIGHT)
    }

    pub fn section(&self) -> Frame {
        let top = HEADER_HEIGHT + ROW_HEIGHT + SECTION_GAP;
        self.frame(SIDE_PADDING, top, 100.0, SECTION_HEIGHT - SECTION_GAP)
    }

    pub fn row_label(&self, row: usize) -> Frame {
        self.frame(SIDE_PADDING, self.row_top(row), LABEL_WIDTH, ROW_HEIGHT)
    }

    pub fn row_value(&self, row: usize) -> Frame {
        let x = WIDTH - END_PADDING - MUTE_WIDTH - COLUMN_GAP - VALUE_WIDTH;
        self.frame(x, self.row_top(row), VALUE_WIDTH, ROW_HEIGHT)
    }

    pub fn row_mute(&self, row: usize) -> Frame {
        let x = WIDTH - END_PADDING - MUTE_WIDTH;
        self.frame(x, self.row_top(row), MUTE_WIDTH, ROW_HEIGHT)
    }

    pub fn chat_title(&self) -> Frame {
        let top = self.chat_top() + CHAT_PADDING;
        self.frame(SIDE_PADDING, top, 120.0, CHAT_TITLE_HEIGHT)
    }

    pub fn chat_value(&self) -> Frame {
        let top = self.chat_top() + CHAT_PADDING;
        let x = SIDE_PADDING + 120.0;
        self.frame(x, top, WIDTH - x - SIDE_PADDING, CHAT_TITLE_HEIGHT)
    }

    pub fn chat_game(&self) -> Frame {
        let top = self.chat_slider_top();
        self.frame(SIDE_PADDING, top, CHAT_END_LABEL, CHAT_SLIDER_HEIGHT)
    }

    pub fn chat_chat(&self) -> Frame {
        let x = WIDTH - SIDE_PADDING - CHAT_END_LABEL;
        let top = self.chat_slider_top();
        self.frame(x, top, CHAT_END_LABEL, CHAT_SLIDER_HEIGHT)
    }

    pub fn chat_note(&self) -> Frame {
        let top = self.chat_slider_top() + CHAT_SLIDER_HEIGHT;
        self.frame(SIDE_PADDING, top, WIDTH - 2.0 * SIDE_PADDING, CHAT_NOTE_HEIGHT)
    }

    pub fn footer(&self) -> Frame {
        let top = self.chat_bottom() + FOOTER_PADDING;
        let height = FOOTER_HEIGHT - 2.0 * FOOTER_PADDING;
        self.frame(SIDE_PADDING, top, WIDTH - 2.0 * SIDE_PADDING, height)
    }

    /// Left end, right end and vertical centre of a slider, in unscaled units.
    fn slider(&self, spot: Spot) -> (f32, f32, f32) {
        match spot {
            Spot::Row(row) => {
                let left = SIDE_PADDING + LABEL_WIDTH + COLUMN_GAP;
                let right = WIDTH - END_PADDING - MUTE_WIDTH - 2.0 * COLUMN_GAP - VALUE_WIDTH;
                (left, right, self.row_top(row) + ROW_HEIGHT / 2.0)
            }
            Spot::ChatMix => {
                let left = SIDE_PADDING + CHAT_END_LABEL + COLUMN_GAP;
                let right = WIDTH - SIDE_PADDING - CHAT_END_LABEL - COLUMN_GAP;
                (left, right, self.chat_slider_top() + CHAT_SLIDER_HEIGHT / 2.0)
            }
        }
    }

    fn range(spot: Spot) -> (i32, i32) {
        match spot {
            Spot::Row(_) => (0, 100),
            Spot::ChatMix => (-100, 100),
        }
    }

    /// The value of a slider for a mouse position, in window pixels.
    pub fn value_at(&self, spot: Spot, x: i32) -> i32 {
        let (left, right, _) = self.slider(spot);
        let (low, high) = Self::range(spot);
        let position = (x as f32 / self.scale - left) / (right - left);
        let value = low + (position.clamp(0.0, 1.0) * (high - low) as f32).round() as i32;
        match spot {
            Spot::Row(_) => value,
            Spot::ChatMix => (value as f32 / CHAT_STEP as f32).round() as i32 * CHAT_STEP,
        }
    }

    fn position(&self, spot: Spot, value: i32) -> f32 {
        let (left, right, _) = self.slider(spot);
        let (low, high) = Self::range(spot);
        left + (right - left) * (value - low) as f32 / (high - low) as f32
    }

    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        let (x, y) = (x as f32 / self.scale, y as f32 / self.scale);
        for row in 0..ROWS {
            let top = self.row_top(row);
            if !(top..top + ROW_HEIGHT).contains(&y) {
                continue;
            }
            let (left, right, _) = self.slider(Spot::Row(row));
            if x >= WIDTH - END_PADDING - MUTE_WIDTH {
                return Some(Hit::Mute(row));
            }
            let reach = left - SLIDER_MARGIN..right + SLIDER_MARGIN;
            return reach.contains(&x).then_some(Hit::Slider(Spot::Row(row)));
        }
        let (left, right, center) = self.slider(Spot::ChatMix);
        let on_chat = (left - SLIDER_MARGIN..right + SLIDER_MARGIN).contains(&x)
            && (y - center).abs() <= CHAT_SLIDER_HEIGHT / 2.0;
        on_chat.then_some(Hit::Slider(Spot::ChatMix))
    }
}

pub struct RowScene {
    pub percent: Option<i32>,
    pub muted: bool,
    pub selected: bool,
}

pub struct ChatScene {
    pub balance: Option<i32>,
    pub enabled: bool,
    pub selected: bool,
}

pub struct Canvas<'a> {
    pub pixels: &'a mut [u32],
    pub width: i32,
    pub height: i32,
}

fn blend(under: u32, over: u32, coverage: f32) -> u32 {
    let channel = |shift: u32| {
        let (below, above) = ((under >> shift) & 0xFF, (over >> shift) & 0xFF);
        let mixed = below as f32 + (above as f32 - below as f32) * coverage;
        (mixed.round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

impl Canvas<'_> {
    pub fn fill_rect(&mut self, frame: Frame, color: u32) {
        let right = (frame.x + frame.width).min(self.width);
        let bottom = (frame.y + frame.height).min(self.height);
        for y in frame.y.max(0)..bottom {
            for x in frame.x.max(0)..right {
                self.pixels[(y * self.width + x) as usize] = color;
            }
        }
    }

    /// An antialiased bar with round ends, or a disc if `left` equals `right`.
    fn fill_capsule(&mut self, left: f32, right: f32, center: f32, radius: f32, color: u32) {
        let first_x = ((left - radius - 1.0).floor() as i32).max(0);
        let last_x = ((right + radius + 1.0).ceil() as i32).min(self.width);
        let first_y = ((center - radius - 1.0).floor() as i32).max(0);
        let last_y = ((center + radius + 1.0).ceil() as i32).min(self.height);
        for y in first_y..last_y {
            for x in first_x..last_x {
                let (pixel_x, pixel_y) = (x as f32 + 0.5, y as f32 + 0.5);
                let across = pixel_x - pixel_x.clamp(left, right);
                let down = pixel_y - center;
                let distance = (across * across + down * down).sqrt();
                let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    let pixel = &mut self.pixels[(y * self.width + x) as usize];
                    *pixel = blend(*pixel, color, coverage);
                }
            }
        }
    }
}

struct SliderLook {
    from: i32,
    value: i32,
    fill: u32,
    thumb: u32,
    background: u32,
}

fn draw_slider(canvas: &mut Canvas, layout: &Layout, spot: Spot, look: &SliderLook) {
    let scale = layout.scale;
    let (left, right, center) = layout.slider(spot);
    let (left, right, center) = (left * scale, right * scale, center * scale);
    let radius = TRACK_RADIUS * scale;
    canvas.fill_capsule(left, right, center, radius, TRACK);
    if spot == Spot::ChatMix {
        let middle = layout.position(spot, 0) - TICK_WIDTH / 2.0;
        let top = center / scale - TICK_HEIGHT / 2.0;
        canvas.fill_rect(layout.frame(middle, top, TICK_WIDTH, TICK_HEIGHT), MUTED_FILL);
    }

    let start = layout.position(spot, look.from) * scale;
    let end = layout.position(spot, look.value) * scale;
    if start != end {
        canvas.fill_capsule(start.min(end), start.max(end), center, radius, look.fill);
    }
    let thumb = THUMB_RADIUS * scale;
    canvas.fill_capsule(end, end, center, thumb + THUMB_HALO * scale, look.background);
    canvas.fill_capsule(end, end, center, thumb, look.thumb);
}

pub fn draw(canvas: &mut Canvas, layout: &Layout, rows: &[RowScene; ROWS], chat: &ChatScene) {
    let (width, height) = layout.size();
    canvas.fill_rect(Frame { x: 0, y: 0, width, height }, BACKGROUND);

    for (index, row) in rows.iter().enumerate() {
        let background = if row.selected {
            let top = layout.row_top(index);
            canvas.fill_rect(layout.frame(0.0, top, WIDTH, ROW_HEIGHT), SELECTED_BACKGROUND);
            SELECTED_BACKGROUND
        } else {
            BACKGROUND
        };
        if let Some(value) = row.percent {
            let fill = match (row.muted, row.selected) {
                (true, _) => MUTED_FILL,
                (false, true) => SELECTED_FILL,
                (false, false) => FILL,
            };
            let look = SliderLook { from: 0, value, fill, thumb: THUMB, background };
            draw_slider(canvas, layout, Spot::Row(index), &look);
        }
    }

    let chat_top = layout.chat_top();
    let chat_bottom = layout.chat_bottom();
    let background = if chat.selected {
        let frame = layout.frame(0.0, chat_top, WIDTH, chat_bottom - chat_top);
        canvas.fill_rect(frame, SELECTED_BACKGROUND);
        SELECTED_BACKGROUND
    } else {
        BACKGROUND
    };
    canvas.fill_rect(layout.frame(0.0, chat_top, WIDTH, 1.0), LINE);
    canvas.fill_rect(layout.frame(0.0, chat_bottom, WIDTH, 1.0), LINE);

    let value = chat.balance.unwrap_or(0);
    let fill = if chat.selected { SELECTED_FILL } else { FILL };
    let thumb = if chat.enabled { THUMB } else { DISABLED_THUMB };
    let look = SliderLook { from: 0, value, fill, thumb, background };
    draw_slider(canvas, layout, Spot::ChatMix, &look);

    canvas.fill_rect(Frame { x: 0, y: 0, width, height: 1 }, LINE);
    canvas.fill_rect(Frame { x: 0, y: height - 1, width, height: 1 }, LINE);
    canvas.fill_rect(Frame { x: 0, y: 0, width: 1, height }, LINE);
    canvas.fill_rect(Frame { x: width - 1, y: 0, width: 1, height }, LINE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_with_the_screen() {
        assert_eq!(Layout::new(96, false).size(), (372, 508));
        assert_eq!(Layout::new(192, false).size(), (744, 1016));
    }

    #[test]
    fn a_chat_note_makes_room_for_itself() {
        let (_, without) = Layout::new(96, false).size();
        let (_, with) = Layout::new(96, true).size();
        assert_eq!(with - without, CHAT_NOTE_HEIGHT as i32);
    }

    #[test]
    fn finds_each_slider_and_mute_button() {
        let layout = Layout::new(96, false);
        assert_eq!(layout.hit(200, 70), Some(Hit::Slider(Spot::Row(0))));
        assert_eq!(layout.hit(200, 142), Some(Hit::Slider(Spot::Row(1))));
        assert_eq!(layout.hit(200, 362), Some(Hit::Slider(Spot::Row(6))));
        assert_eq!(layout.hit(350, 70), Some(Hit::Mute(0)));
        assert_eq!(layout.hit(350, 362), Some(Hit::Mute(6)));
        assert_eq!(layout.hit(200, 444), Some(Hit::Slider(Spot::ChatMix)));
    }

    #[test]
    fn empty_areas_do_nothing() {
        let layout = Layout::new(96, false);
        assert_eq!(layout.hit(30, 70), None);
        assert_eq!(layout.hit(200, 20), None);
        assert_eq!(layout.hit(200, 105), None);
        assert_eq!(layout.hit(200, 490), None);
    }

    #[test]
    fn hits_scale_with_the_screen() {
        let layout = Layout::new(144, false);
        assert_eq!(layout.hit(300, 105), Some(Hit::Slider(Spot::Row(0))));
    }

    #[test]
    fn maps_positions_to_percents_and_stops_at_the_ends() {
        let layout = Layout::new(96, false);
        assert_eq!(layout.value_at(Spot::Row(0), 80), 0);
        assert_eq!(layout.value_at(Spot::Row(0), 179), 50);
        assert_eq!(layout.value_at(Spot::Row(0), 278), 100);
        assert_eq!(layout.value_at(Spot::Row(0), 0), 0);
        assert_eq!(layout.value_at(Spot::Row(0), 372), 100);
    }

    #[test]
    fn chat_mix_snaps_to_its_step_around_the_centre() {
        let layout = Layout::new(96, false);
        assert_eq!(layout.value_at(Spot::ChatMix, 186), 0);
        assert_eq!(layout.value_at(Spot::ChatMix, 188), 0);
        assert_eq!(layout.value_at(Spot::ChatMix, 190), 5);
        assert_eq!(layout.value_at(Spot::ChatMix, 64), -100);
        assert_eq!(layout.value_at(Spot::ChatMix, 308), 100);
    }

    #[test]
    fn blends_towards_the_colour_on_top() {
        assert_eq!(blend(0x000000, 0xFFFFFF, 1.0), 0xFFFFFF);
        assert_eq!(blend(0x000000, 0xFFFFFF, 0.0), 0x000000);
        assert_eq!(blend(0x000000, 0xFF8000, 0.5), 0x804000);
    }

    fn drawn(selected: usize, percent: i32, muted: bool) -> (Vec<u32>, Layout) {
        let layout = Layout::new(96, false);
        let (width, height) = layout.size();
        let mut pixels = vec![0; (width * height) as usize];
        let rows = std::array::from_fn(|index| RowScene {
            percent: Some(percent),
            muted,
            selected: index == selected,
        });
        let chat = ChatScene { balance: Some(0), enabled: true, selected: false };
        let mut canvas = Canvas { pixels: &mut pixels, width, height };
        draw(&mut canvas, &layout, &rows, &chat);
        (pixels, layout)
    }

    #[test]
    fn highlights_the_wheel_target() {
        let (pixels, layout) = drawn(1, 50, false);
        let (width, _) = layout.size();
        let at = |x: i32, y: i32| pixels[(y * width + x) as usize];
        // Row 1 starts at y = 120 and its slider runs from x = 80.
        assert_eq!(at(20, 125), SELECTED_BACKGROUND);
        assert_eq!(at(100, 142), SELECTED_FILL);
        assert_eq!(at(20, 70), BACKGROUND);
        assert_eq!(at(100, 70), FILL);
    }

    #[test]
    fn a_muted_row_loses_its_colour() {
        let (pixels, layout) = drawn(1, 50, true);
        let (width, _) = layout.size();
        assert_eq!(pixels[(142 * width + 100) as usize], MUTED_FILL);
    }

    #[test]
    fn the_thumb_sits_on_the_value() {
        let (pixels, layout) = drawn(1, 50, false);
        let (width, _) = layout.size();
        assert_eq!(pixels[(142 * width + 179) as usize], THUMB);
    }
}
