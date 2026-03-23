use super::tree::{PaneNode, SplitDirection};
use super::PaneId;

pub const DIVIDER_THICKNESS: f32 = 6.0;

#[derive(Debug, Clone, Copy)]
pub struct PixelRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PixelRect {
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    };

    pub fn split(self, direction: SplitDirection, ratio: f32) -> (PixelRect, PixelRect) {
        let half_div = DIVIDER_THICKNESS / 2.0;
        match direction {
            SplitDirection::Vertical => {
                let split_x = self.x + self.width * ratio;
                let left = PixelRect {
                    x: self.x,
                    y: self.y,
                    width: (split_x - half_div - self.x).max(0.0),
                    height: self.height,
                };
                let right = PixelRect {
                    x: split_x + half_div,
                    y: self.y,
                    width: (self.x + self.width - split_x - half_div).max(0.0),
                    height: self.height,
                };
                (left, right)
            }
            SplitDirection::Horizontal => {
                let split_y = self.y + self.height * ratio;
                let top = PixelRect {
                    x: self.x,
                    y: self.y,
                    width: self.width,
                    height: (split_y - half_div - self.y).max(0.0),
                };
                let bottom = PixelRect {
                    x: self.x,
                    y: split_y + half_div,
                    width: self.width,
                    height: (self.y + self.height - split_y - half_div).max(0.0),
                };
                (top, bottom)
            }
        }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DividerInfo {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub direction: SplitDirection,
    pub touches_focused: bool,
}

pub fn compute_layout(
    node: &PaneNode,
    rect: PixelRect,
    results: &mut Vec<(PaneId, PixelRect)>,
    dividers: &mut Vec<DividerInfo>,
    focused: PaneId,
) {
    match node {
        PaneNode::Leaf(id) => {
            results.push((*id, rect));
        }
        PaneNode::Split {
            direction,
            left,
            right,
            ratio,
        } => {
            let (left_rect, right_rect) = rect.split(*direction, *ratio);

            // Record divider
            let (x0, y0, x1, y1) = match direction {
                SplitDirection::Vertical => {
                    let mid_x = rect.x + rect.width * ratio;
                    (mid_x, rect.y, mid_x, rect.y + rect.height)
                }
                SplitDirection::Horizontal => {
                    let mid_y = rect.y + rect.height * ratio;
                    (rect.x, mid_y, rect.x + rect.width, mid_y)
                }
            };
            let left_has_focused = node_contains(left, focused);
            let right_has_focused = node_contains(right, focused);
            dividers.push(DividerInfo {
                x0,
                y0,
                x1,
                y1,
                direction: *direction,
                touches_focused: left_has_focused || right_has_focused,
            });

            compute_layout(left, left_rect, results, dividers, focused);
            compute_layout(right, right_rect, results, dividers, focused);
        }
    }
}

fn node_contains(node: &PaneNode, id: PaneId) -> bool {
    match node {
        PaneNode::Leaf(leaf_id) => *leaf_id == id,
        PaneNode::Split { left, right, .. } => {
            node_contains(left, id) || node_contains(right, id)
        }
    }
}
