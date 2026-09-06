//! Phone toolbar geometry and pointer gestures, independent of the Android runtime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rect {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Rect {
    pub(crate) fn from_bounds([left, top, right, bottom]: [u32; 4]) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }
    pub(crate) fn width(self) -> u32 {
        self.right.saturating_sub(self.left)
    }
    pub(crate) fn height(self) -> u32 {
        self.bottom.saturating_sub(self.top)
    }
    pub(crate) fn contains(self, x: f64, y: f64) -> bool {
        x >= f64::from(self.left)
            && x < f64::from(self.right)
            && y >= f64::from(self.top)
            && y < f64::from(self.bottom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub(crate) enum Control {
    Escape,
    Tab,
    Control,
    Left,
    Down,
    Up,
    Right,
    Keyboard,
    Select,
    Copy,
    Paste,
    PageUp,
    PageDown,
    Home,
    End,
    Insert,
    Delete,
    More,
}

impl Control {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Escape => "Esc",
            Self::Tab => "Tab",
            Self::Control => "Ctrl",
            Self::Left => "<",
            Self::Down => "v",
            Self::Up => "^",
            Self::Right => ">",
            Self::Keyboard => "Keys",
            Self::Select => "Sel",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            Self::PageUp => "PgUp",
            Self::PageDown => "PgDn",
            Self::Home => "Home",
            Self::End => "End",
            Self::Insert => "Ins",
            Self::Delete => "Del",
            Self::More => "More",
        }
    }

    pub(crate) fn accessibility_label(self) -> &'static str {
        match self {
            Self::Escape => "Escape",
            Self::Tab => "Tab",
            Self::Control => "Control modifier",
            Self::Left => "Left arrow",
            Self::Down => "Down arrow",
            Self::Up => "Up arrow",
            Self::Right => "Right arrow",
            Self::Keyboard => "Show or hide keyboard",
            Self::Select => "Select terminal text",
            Self::Copy => "Copy selection",
            Self::Paste => "Paste clipboard",
            Self::PageUp => "Page up",
            Self::PageDown => "Page down",
            Self::Home => "Home",
            Self::End => "End",
            Self::Insert => "Insert",
            Self::Delete => "Delete",
            Self::More => "More terminal controls",
        }
    }

    pub(crate) fn from_id(id: i32) -> Option<Self> {
        CONTROLS
            .iter()
            .copied()
            .chain([Self::More])
            .find(|control| *control as i32 == id)
    }
}

const CONTROLS: [Control; 17] = [
    Control::Escape,
    Control::Tab,
    Control::Control,
    Control::Left,
    Control::Down,
    Control::Up,
    Control::Right,
    Control::Keyboard,
    Control::Select,
    Control::Copy,
    Control::Paste,
    Control::PageUp,
    Control::PageDown,
    Control::Home,
    Control::End,
    Control::Insert,
    Control::Delete,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Button {
    pub control: Control,
    pub rect: Rect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolbarLayout {
    pub terminal: Rect,
    pub buttons: Vec<Button>,
    pub page_count: usize,
}

impl ToolbarLayout {
    pub(crate) fn new(bounds: Rect, scale: f64, page: usize) -> Self {
        let target = (48.0 * scale.clamp(0.5, 8.0)).ceil() as u32;
        let columns = (bounds.width() / target).clamp(1, 18) as usize;
        // Leave at least one target-height for the terminal. Short landscape windows
        // use a single paged row; normal phones fit all essential actions in two.
        let rows = if bounds.height() >= target * 3 && columns < CONTROLS.len() {
            2
        } else {
            1
        };
        let capacity = columns * rows;
        let paged = CONTROLS.len() > capacity;
        let per_page = if paged {
            capacity.saturating_sub(1).max(1)
        } else {
            CONTROLS.len()
        };
        let page_count = CONTROLS.len().div_ceil(per_page);
        let start = (page % page_count) * per_page;
        let mut controls = CONTROLS[start..(start + per_page).min(CONTROLS.len())].to_vec();
        if paged && capacity > 1 {
            controls.push(Control::More);
        }
        let height = (target * rows as u32).min(bounds.height());
        let top = bounds.bottom - height;
        let buttons = controls
            .into_iter()
            .enumerate()
            .map(|(index, control)| {
                let col = index % columns;
                let row = index / columns;
                Button {
                    control,
                    rect: Rect {
                        left: bounds.left + bounds.width() * col as u32 / columns as u32,
                        right: bounds.left + bounds.width() * (col + 1) as u32 / columns as u32,
                        top: top + height * row as u32 / rows as u32,
                        bottom: top + height * (row + 1) as u32 / rows as u32,
                    },
                }
            })
            .collect();
        Self {
            terminal: Rect {
                bottom: top,
                ..bounds
            },
            buttons,
            page_count,
        }
    }

    pub(crate) fn hit(&self, x: f64, y: f64) -> Option<Control> {
        self.buttons
            .iter()
            .find(|button| button.rect.contains(x, y))
            .map(|button| button.control)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum TouchRegion {
    Terminal,
    Toolbar(Control),
}

#[derive(Debug)]
pub(crate) struct TouchGesture {
    pub id: u64,
    pub region: TouchRegion,
    pub start: (f64, f64),
    pub position: (f64, f64),
    pub dragged: bool,
    remainder: f64,
}

impl TouchGesture {
    pub(crate) fn new(id: u64, region: TouchRegion, x: f64, y: f64) -> Self {
        Self {
            id,
            region,
            start: (x, y),
            position: (x, y),
            dragged: false,
            remainder: 0.0,
        }
    }

    pub(crate) fn move_to(&mut self, x: f64, y: f64, threshold: f64, line_height: f64) -> i32 {
        if !x.is_finite() || !y.is_finite() {
            self.dragged = true;
            return 0;
        }
        self.remainder += y - self.position.1;
        self.position = (x, y);
        self.dragged |= (x - self.start.0).hypot(y - self.start.1) >= threshold.max(1.0);
        if !self.dragged {
            return 0;
        }
        let lines = (self.remainder / line_height.max(1.0))
            .trunc()
            .clamp(-32.0, 32.0) as i32;
        self.remainder -= f64::from(lines) * line_height.max(1.0);
        lines
    }

    pub(crate) fn released_control(
        &self,
        layout: &ToolbarLayout,
        x: f64,
        y: f64,
    ) -> Option<Control> {
        match self.region {
            TouchRegion::Toolbar(control) if !self.dragged && layout.hit(x, y) == Some(control) => {
                Some(control)
            }
            _ => None,
        }
    }
}

/// Clear image/glyph overflow after native composition, without copying protocol pixels.
pub(crate) fn mask_outside(frame: &mut [u32], size: (u32, u32), clip: Rect, color: u32) {
    let (width, height) = size;
    if u64::from(width) * u64::from(height) > frame.len() as u64 {
        return;
    }
    for row in 0..height {
        let begin = row as usize * width as usize;
        let pixels = &mut frame[begin..begin + width as usize];
        if row < clip.top || row >= clip.bottom {
            pixels.fill(color);
        } else {
            pixels[..clip.left.min(width) as usize].fill(color);
            pixels[clip.right.min(width) as usize..].fill(color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_targets_stay_at_least_48dp_and_all_controls_are_reachable() {
        for width in [240, 320, 360, 393, 600, 1024] {
            for scale in [1.0, 2.75, 3.0] {
                let bounds = Rect {
                    left: 13,
                    top: 29,
                    right: 13 + (f64::from(width) * scale) as u32,
                    bottom: 29 + (640.0 * scale) as u32,
                };
                let first = ToolbarLayout::new(bounds, scale, 0);
                let mut seen = Vec::new();
                for page in 0..first.page_count {
                    let layout = ToolbarLayout::new(bounds, scale, page);
                    assert_eq!(layout.terminal, first.terminal);
                    for button in &layout.buttons {
                        assert!(f64::from(button.rect.width()) >= 48.0 * scale);
                        assert!(f64::from(button.rect.height()) >= 48.0 * scale);
                        assert_eq!(
                            layout.hit(f64::from(button.rect.left), f64::from(button.rect.top)),
                            Some(button.control)
                        );
                        assert!(
                            button.rect.bottom <= bounds.bottom
                                && button.rect.right <= bounds.right
                        );
                        seen.push(button.control);
                    }
                }
                for control in CONTROLS {
                    assert!(seen.contains(&control));
                }
                assert!(first
                    .hit(f64::from(bounds.right), f64::from(bounds.bottom))
                    .is_none());
            }
        }
    }

    #[test]
    fn short_landscape_keeps_a_terminal_and_uses_one_row() {
        let layout = ToolbarLayout::new(Rect::from_bounds([0, 24, 640, 144]), 1.0, 0);
        assert_eq!(layout.terminal.bottom, 96);
        assert!(layout
            .buttons
            .iter()
            .all(|button| button.rect.height() == 48));
    }

    #[test]
    fn horizontal_and_subcell_drags_never_become_button_taps() {
        let layout = ToolbarLayout::new(Rect::from_bounds([0, 0, 320, 640]), 1.0, 0);
        let button = &layout.buttons[0];
        let x = f64::from(button.rect.left + 10);
        let y = f64::from(button.rect.top + 10);
        let mut gesture = TouchGesture::new(4, TouchRegion::Toolbar(button.control), x, y);
        assert_eq!(gesture.move_to(x + 9.0, y, 8.0, 30.0), 0);
        assert!(gesture.released_control(&layout, x, y).is_none());
        let mut small = TouchGesture::new(5, TouchRegion::Terminal, 50.0, 50.0);
        assert_eq!(small.move_to(50.0, 59.0, 8.0, 30.0), 0);
        assert!(small.dragged);
        assert_eq!(small.move_to(50.0, 82.0, 8.0, 30.0), 1);
    }

    #[test]
    fn release_must_stay_in_original_control_and_scroll_keeps_fraction() {
        let layout = ToolbarLayout::new(Rect::from_bounds([0, 0, 360, 640]), 1.0, 0);
        let button = &layout.buttons[0];
        let gesture = TouchGesture::new(
            1,
            TouchRegion::Toolbar(button.control),
            2.0,
            f64::from(button.rect.top + 2),
        );
        assert_eq!(
            gesture.released_control(&layout, 2.0, f64::from(button.rect.top + 2)),
            Some(button.control)
        );
        assert!(gesture
            .released_control(&layout, 100.0, f64::from(button.rect.top + 2))
            .is_none());
        let mut scroll = TouchGesture::new(2, TouchRegion::Terminal, 0.0, 0.0);
        assert_eq!(scroll.move_to(0.0, 35.0, 8.0, 20.0), 1);
        assert_eq!(scroll.move_to(0.0, 41.0, 8.0, 20.0), 1);
    }

    #[test]
    fn composition_mask_preserves_only_content_rect_pixels() {
        let mut frame = vec![0xabcdef; 7 * 6];
        let clip = Rect::from_bounds([2, 1, 5, 4]);
        mask_outside(&mut frame, (7, 6), clip, 0x101010);
        for y in 0..6 {
            for x in 0..7 {
                assert_eq!(
                    frame[y * 7 + x],
                    if clip.contains(x as f64, y as f64) {
                        0xabcdef
                    } else {
                        0x101010
                    }
                );
            }
        }
    }
}
