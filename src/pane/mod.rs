pub mod pane;

use crate::graphics::{retain_unreferenced_image_ids, ImageId};
use crate::layout::{
    compute_layout, DividerInfo, PaneId, PaneNode, PixelRect, SplitDirection,
};
use crate::parser::ansi::GraphicsSupport;
use crate::renderer::kitty_handler::KittyHandlerOptions;
use pane::Pane;

use std::collections::{HashMap, HashSet};

const MIN_PANE_COLS: usize = 10;
const MIN_PANE_ROWS: usize = 3;

pub struct PaneTree {
    root: PaneNode,
    panes: HashMap<PaneId, Pane>,
    pub focused: PaneId,
    next_id: PaneId,
    scrollback_max: usize,
    default_cols: u16,
    default_rows: u16,
    kitty_options: KittyHandlerOptions,
    graphics_support: GraphicsSupport,
}

#[must_use]
pub struct PaneCloseOutcome {
    pub should_terminate: bool,
    pub closed_pane: Option<Pane>,
}

impl PaneTree {
    pub fn new(
        cols: u16,
        rows: u16,
        scrollback_max: usize,
        kitty_options: KittyHandlerOptions,
        graphics_support: GraphicsSupport,
    ) -> Result<Self, crate::pty::PtyError> {
        let id: PaneId = 1;
        let pane = Pane::new(
            id,
            cols,
            rows,
            scrollback_max,
            kitty_options,
            graphics_support,
        )?;
        let mut panes = HashMap::new();
        panes.insert(id, pane);

        Ok(Self {
            root: PaneNode::Leaf(id),
            panes,
            focused: id,
            next_id: 2,
            scrollback_max,
            default_cols: cols,
            default_rows: rows,
            kitty_options,
            graphics_support,
        })
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.get(&id)
    }

    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.get_mut(&id)
    }

    pub fn focused_pane(&self) -> Option<&Pane> {
        self.panes.get(&self.focused)
    }

    pub fn focused_pane_mut(&mut self) -> Option<&mut Pane> {
        self.panes.get_mut(&self.focused)
    }

    pub fn pane_ids(&self) -> Vec<PaneId> {
        let mut ids = Vec::new();
        self.root.collect_leaf_ids(&mut ids);
        ids
    }

    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// Keep only cache-deletion candidates unused by every pane and protocol.
    pub(crate) fn retain_unreferenced_image_ids(
        &self,
        candidates: &mut HashSet<ImageId>,
    ) {
        retain_unreferenced_image_ids(
            candidates,
            self.panes
                .values()
                .flat_map(|pane| pane.grid.all_image_placements()),
        );
    }

    /// Split the focused pane in the given direction. Returns the new pane's ID.
    pub fn split(
        &mut self,
        direction: SplitDirection,
        cell_w: f32,
        cell_h: f32,
    ) -> Result<PaneId, crate::pty::PtyError> {
        let target = self.focused;
        let target_pane = self.panes.get(&target).unwrap();
        let rect = target_pane.rect;

        // Check if resulting panes would be too small
        let (left_rect, right_rect) = rect.split(direction, 0.5);
        let min_w = MIN_PANE_COLS as f32 * cell_w;
        let min_h = MIN_PANE_ROWS as f32 * cell_h;
        if left_rect.width < min_w || left_rect.height < min_h
            || right_rect.width < min_w || right_rect.height < min_h
        {
            log::warn!("Cannot split: resulting pane too small");
            return Ok(target); // Return existing pane ID
        }

        let new_id = self.next_id;
        self.next_id += 1;

        // Calculate cols/rows for new pane from its rect
        let new_cols = (right_rect.width / cell_w).floor() as u16;
        let new_rows = (right_rect.height / cell_h).floor() as u16;
        let mut new_pane = Pane::new(
            new_id,
            new_cols.max(MIN_PANE_COLS as u16),
            new_rows.max(MIN_PANE_ROWS as u16),
            self.scrollback_max,
            self.kitty_options,
            self.graphics_support,
        )?;

        // Copy color config from existing pane for OSC 10/11 queries
        if let Some(existing) = self.panes.get(&target) {
            new_pane.grid.default_fg_hex = existing.grid.default_fg_hex.clone();
            new_pane.grid.default_bg_hex = existing.grid.default_bg_hex.clone();
        }

        self.panes.insert(new_id, new_pane);
        self.root.split_leaf(target, new_id, direction);
        self.focused = new_id;

        Ok(new_id)
    }

    /// Detach a pane so its PTY workers can be retired outside the tree lock.
    pub fn close(&mut self, id: PaneId) -> PaneCloseOutcome {
        if !self.panes.contains_key(&id) || !PaneNode::contains(&self.root, id) {
            return PaneCloseOutcome {
                should_terminate: false,
                closed_pane: None,
            };
        }

        if self.root.is_single_leaf() {
            // Last pane
            let closed_pane = self.panes.remove(&id);
            return PaneCloseOutcome {
                should_terminate: closed_pane.is_some(),
                closed_pane,
            };
        }

        if !self.root.remove_leaf(id) {
            return PaneCloseOutcome {
                should_terminate: false,
                closed_pane: None,
            };
        }
        let closed_pane = self.panes.remove(&id);

        // If focused pane was closed, focus the first remaining pane
        if self.focused == id {
            let ids = self.pane_ids();
            self.focused = ids.first().copied().unwrap_or(0);
        }
        PaneCloseOutcome {
            should_terminate: false,
            closed_pane,
        }
    }

    /// Drain panes only after the application has stopped using the layout tree.
    pub fn take_all_panes(&mut self) -> Vec<Pane> {
        self.panes.drain().map(|(_, pane)| pane).collect()
    }

    /// Adjust the split ratio near the focused pane.
    pub fn resize_focused(&mut self, delta: f32) {
        self.root.adjust_ratio(self.focused, delta);
    }

    /// Navigate focus to a neighbor in the given direction using spatial lookup.
    pub fn focus_neighbor(&mut self, direction: SplitDirection, positive: bool) {
        let current = match self.panes.get(&self.focused) {
            Some(p) => p.rect,
            None => return,
        };

        let cx = current.x + current.width / 2.0;
        let cy = current.y + current.height / 2.0;

        let mut best_id = None;
        let mut best_dist = f32::MAX;

        for (&id, pane) in &self.panes {
            if id == self.focused {
                continue;
            }
            let r = pane.rect;
            let px = r.x + r.width / 2.0;
            let py = r.y + r.height / 2.0;

            let is_valid = match (direction, positive) {
                (SplitDirection::Vertical, true) => px > cx + 1.0,   // right
                (SplitDirection::Vertical, false) => px < cx - 1.0,  // left
                (SplitDirection::Horizontal, true) => py > cy + 1.0, // down
                (SplitDirection::Horizontal, false) => py < cy - 1.0, // up
            };

            if is_valid {
                let dist = (px - cx).abs() + (py - cy).abs();
                if dist < best_dist {
                    best_dist = dist;
                    best_id = Some(id);
                }
            }
        }

        if let Some(id) = best_id {
            self.focused = id;
        }
    }

    /// Recompute layout and update all pane rects. Also resize grids if needed.
    pub fn relayout(&mut self, viewport: PixelRect, cell_w: f32, cell_h: f32, status_bar_height: f32) {
        let mut layout_results = Vec::new();
        let mut dividers = Vec::new();
        compute_layout(&self.root, viewport, &mut layout_results, &mut dividers, self.focused);

        for (id, rect) in layout_results {
            if let Some(pane) = self.panes.get_mut(&id) {
                pane.rect = rect;
                // Grid area excludes status bar
                let grid_height = (rect.height - status_bar_height).max(cell_h);
                let cols = (rect.width / cell_w).floor() as usize;
                let rows = (grid_height / cell_h).floor() as usize;
                pane.resize_grid(
                    cols.max(MIN_PANE_COLS),
                    rows.max(MIN_PANE_ROWS),
                );
            }
        }
    }

    /// Get layout info for rendering.
    pub fn layout_info(&self, viewport: PixelRect) -> (Vec<(PaneId, PixelRect)>, Vec<DividerInfo>) {
        let mut layouts = Vec::new();
        let mut dividers = Vec::new();
        compute_layout(&self.root, viewport, &mut layouts, &mut dividers, self.focused);
        (layouts, dividers)
    }

    /// Find which pane contains the given pixel coordinate.
    pub fn pane_at(&self, x: f32, y: f32) -> Option<PaneId> {
        for (&id, pane) in &self.panes {
            if pane.rect.contains(x, y) {
                return Some(id);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{GraphicsSupport, KittyHandlerOptions, PaneTree, PixelRect, SplitDirection};
    use crate::graphics::{ImagePlacement, InlineRenderSize, PlacementMode};
    use std::collections::HashSet;

    fn image_placement(image_id: u64) -> ImagePlacement {
        ImagePlacement {
            image_id,
            placement_id: image_id as u32,
            client_placement_id: Some(image_id as u32),
            mode: PlacementMode::Inline {
                row: 0,
                col: 0,
                cols: 1,
                rows: 1,
                x_offset: 0,
                y_offset: 0,
                render_size: InlineRenderSize::CellAnchored,
            },
            z_index: 0,
        }
    }

    #[test]
    fn image_cache_retention_includes_hidden_primary_screen() {
        let mut tree = PaneTree::new(
            80,
            24,
            1_000,
            KittyHandlerOptions::from_megabytes(1, false),
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
        )
        .expect("pane tree should spawn its initial shell");
        let focused = tree.focused;
        let grid = &mut tree
            .focused_pane_mut()
            .expect("focused pane should exist")
            .grid;
        grid.image_placements.push(image_placement(11));
        grid.enter_alt_screen();
        grid.image_placements.push(image_placement(21));

        let mut candidates = HashSet::from([11, 21, 99]);
        tree.retain_unreferenced_image_ids(&mut candidates);
        assert_eq!(candidates, HashSet::from([99]));

        tree.focused_pane_mut()
            .expect("focused pane should exist")
            .grid
            .leave_alt_screen();
        let mut candidates = HashSet::from([11, 21, 99]);
        tree.retain_unreferenced_image_ids(&mut candidates);
        assert_eq!(candidates, HashSet::from([21, 99]));

        tree.close(focused)
            .closed_pane
            .expect("focused pane should detach for cleanup")
            .retire();
    }

    #[test]
    fn close_detaches_valid_panes_and_preserves_stale_ids() {
        let mut tree = PaneTree::new(
            80,
            24,
            1_000,
            KittyHandlerOptions::from_megabytes(1, false),
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
        )
        .expect("pane tree should spawn its initial shell");
        tree.relayout(
            PixelRect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 480.0,
            },
            10.0,
            20.0,
            0.0,
        );
        let second = tree
            .split(SplitDirection::Vertical, 10.0, 20.0)
            .expect("split pane should spawn its shell");

        let stale = tree.close(999);
        assert!(!stale.should_terminate);
        assert!(stale.closed_pane.is_none());
        assert_eq!(tree.pane_count(), 2);

        let detached = tree.close(second);
        assert!(!detached.should_terminate);
        assert!(tree.pane(second).is_none());
        assert_eq!(tree.pane_count(), 1);
        detached
            .closed_pane
            .expect("closed split should be returned for cleanup")
            .retire();

        let last = tree.close(1);
        assert!(last.should_terminate);
        assert_eq!(tree.pane_count(), 0);
        last.closed_pane
            .expect("last pane should be returned for cleanup")
            .retire();
    }
}
