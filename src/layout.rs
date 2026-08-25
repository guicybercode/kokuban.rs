mod tree;

pub use tree::PaneNode;

pub type PaneId = u64;

pub const DIVIDER_THICKNESS: f32 = 6.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq)]
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

#[derive(Debug, Clone, Copy, PartialEq)]
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
        PaneNode::Split { left, right, .. } => node_contains(left, id) || node_contains(right, id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_rects_around_a_fixed_width_divider() {
        let rect = PixelRect {
            x: 10.0,
            y: 20.0,
            width: 200.0,
            height: 100.0,
        };

        let (left, right) = rect.split(SplitDirection::Vertical, 0.25);
        assert_eq!(
            left,
            PixelRect {
                x: 10.0,
                y: 20.0,
                width: 47.0,
                height: 100.0,
            }
        );
        assert_eq!(
            right,
            PixelRect {
                x: 63.0,
                y: 20.0,
                width: 147.0,
                height: 100.0,
            }
        );
        assert_eq!(left.width + DIVIDER_THICKNESS + right.width, rect.width);

        let (top, bottom) = rect.split(SplitDirection::Horizontal, 0.5);
        assert_eq!(
            top,
            PixelRect {
                x: 10.0,
                y: 20.0,
                width: 200.0,
                height: 47.0,
            }
        );
        assert_eq!(
            bottom,
            PixelRect {
                x: 10.0,
                y: 73.0,
                width: 200.0,
                height: 47.0,
            }
        );
        assert_eq!(top.height + DIVIDER_THICKNESS + bottom.height, rect.height);

        assert!(rect.contains(10.0, 20.0));
        assert!(rect.contains(209.0, 119.0));
        assert!(!rect.contains(210.0, 119.0));
        assert!(!rect.contains(209.0, 120.0));

        let tiny = PixelRect {
            x: 0.0,
            y: 0.0,
            width: 4.0,
            height: 4.0,
        };
        let (left, right) = tiny.split(SplitDirection::Vertical, 0.5);
        let (top, bottom) = tiny.split(SplitDirection::Horizontal, 0.5);
        assert_eq!((left.width, right.width), (0.0, 0.0));
        assert_eq!((top.height, bottom.height), (0.0, 0.0));
    }

    #[test]
    fn computes_nested_layout_in_depth_first_order() {
        let root = PaneNode::Split {
            direction: SplitDirection::Vertical,
            left: Box::new(PaneNode::Split {
                direction: SplitDirection::Horizontal,
                left: Box::new(PaneNode::Leaf(1)),
                right: Box::new(PaneNode::Leaf(2)),
                ratio: 0.5,
            }),
            right: Box::new(PaneNode::Split {
                direction: SplitDirection::Horizontal,
                left: Box::new(PaneNode::Leaf(3)),
                right: Box::new(PaneNode::Leaf(4)),
                ratio: 0.5,
            }),
            ratio: 0.5,
        };
        let mut panes = Vec::new();
        let mut dividers = Vec::new();

        compute_layout(
            &root,
            PixelRect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 100.0,
            },
            &mut panes,
            &mut dividers,
            2,
        );

        assert_eq!(
            panes,
            vec![
                (
                    1,
                    PixelRect {
                        x: 0.0,
                        y: 0.0,
                        width: 97.0,
                        height: 47.0,
                    },
                ),
                (
                    2,
                    PixelRect {
                        x: 0.0,
                        y: 53.0,
                        width: 97.0,
                        height: 47.0,
                    },
                ),
                (
                    3,
                    PixelRect {
                        x: 103.0,
                        y: 0.0,
                        width: 97.0,
                        height: 47.0,
                    },
                ),
                (
                    4,
                    PixelRect {
                        x: 103.0,
                        y: 53.0,
                        width: 97.0,
                        height: 47.0,
                    },
                ),
            ]
        );
        assert_eq!(
            dividers,
            vec![
                DividerInfo {
                    x0: 100.0,
                    y0: 0.0,
                    x1: 100.0,
                    y1: 100.0,
                    direction: SplitDirection::Vertical,
                    touches_focused: true,
                },
                DividerInfo {
                    x0: 0.0,
                    y0: 50.0,
                    x1: 97.0,
                    y1: 50.0,
                    direction: SplitDirection::Horizontal,
                    touches_focused: true,
                },
                DividerInfo {
                    x0: 103.0,
                    y0: 50.0,
                    x1: 200.0,
                    y1: 50.0,
                    direction: SplitDirection::Horizontal,
                    touches_focused: false,
                },
            ]
        );

        panes.clear();
        dividers.clear();
        compute_layout(
            &root,
            PixelRect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 100.0,
            },
            &mut panes,
            &mut dividers,
            4,
        );
        assert_eq!(
            dividers
                .iter()
                .map(|divider| divider.touches_focused)
                .collect::<Vec<_>>(),
            vec![true, false, true]
        );
    }

    #[test]
    fn pane_tree_split_resize_and_remove_cycle_preserves_ids() {
        let mut root = PaneNode::Leaf(1);

        assert!(root.split_leaf(1, 2, SplitDirection::Vertical));
        let mut ids = Vec::new();
        root.collect_leaf_ids(&mut ids);
        assert_eq!(ids, vec![1, 2]);

        assert!(root.adjust_ratio(1, 1.0));
        assert_eq!(split_ratio(&root), 0.9);
        assert!(root.adjust_ratio(1, -1.0));
        assert_eq!(split_ratio(&root), 0.1);

        assert!(root.split_leaf(2, 3, SplitDirection::Horizontal));
        ids.clear();
        root.collect_leaf_ids(&mut ids);
        assert_eq!(ids, vec![1, 2, 3]);

        assert!(root.adjust_ratio(3, 1.0));
        assert_eq!(nested_right_split_ratio(&root), 0.9);
        assert!(root.adjust_ratio(3, -1.0));
        assert_eq!(nested_right_split_ratio(&root), 0.1);

        assert!(!root.split_leaf(99, 4, SplitDirection::Vertical));
        assert!(!root.adjust_ratio(99, 0.1));
        assert!(!root.remove_leaf(99));

        assert!(root.remove_leaf(2));
        ids.clear();
        root.collect_leaf_ids(&mut ids);
        assert_eq!(ids, vec![1, 3]);

        assert!(root.remove_leaf(1));
        assert!(root.is_single_leaf());
        ids.clear();
        root.collect_leaf_ids(&mut ids);
        assert_eq!(ids, vec![3]);
    }

    fn split_ratio(node: &PaneNode) -> f32 {
        match node {
            PaneNode::Split { ratio, .. } => *ratio,
            PaneNode::Leaf(_) => panic!("expected a split node"),
        }
    }

    fn nested_right_split_ratio(node: &PaneNode) -> f32 {
        match node {
            PaneNode::Split { right, .. } => split_ratio(right),
            PaneNode::Leaf(_) => panic!("expected a split node"),
        }
    }
}
