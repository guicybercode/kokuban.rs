use super::Vertex;
use crate::layout::{PaneId, PixelRect};
use std::collections::HashMap;

pub(crate) struct SceneImage<Texture> {
    pub vertices: [Vertex; 6],
    pub texture: Texture,
}

/// Only the visible scene is retained, never the terminal's scrollback or parser.
pub(crate) struct PaneScene<Texture> {
    pub origin: [f32; 2],
    pub content: Vec<Vertex>,
    pub atlas_texture: Texture,
    pub images: Vec<SceneImage<Texture>>,
}

pub(crate) struct PaneSceneCache<Texture> {
    scenes: HashMap<PaneId, PaneScene<Texture>>,
}

impl<Texture> Default for PaneSceneCache<Texture> {
    fn default() -> Self {
        Self { scenes: HashMap::new() }
    }
}

impl<Texture> PaneSceneCache<Texture> {
    /// The builder reads the live grid, so it must not run during a synchronized
    /// update. A pane that has never presented anything remains empty until ESU.
    pub fn update_with(
        &mut self,
        id: PaneId,
        synchronized: bool,
        build: impl FnOnce() -> PaneScene<Texture>,
    ) {
        if !synchronized {
            self.scenes.insert(id, build());
        }
    }

    pub fn get(&self, id: PaneId) -> Option<&PaneScene<Texture>> {
        self.scenes.get(&id)
    }

    pub fn get_mut(&mut self, id: PaneId) -> Option<&mut PaneScene<Texture>> {
        self.scenes.get_mut(&id)
    }

    pub fn retain(&mut self, mut is_live: impl FnMut(PaneId) -> bool) {
        self.scenes.retain(|id, _| is_live(*id));
    }
}

/// Move the previous scene with its pane after a split or resize. Its pixels
/// keep their original size until the next complete frame is available.
pub(crate) fn append_translated(
    output: &mut Vec<Vertex>,
    vertices: &[Vertex],
    old_origin: [f32; 2],
    rect: PixelRect,
) {
    let delta = [rect.x - old_origin[0], rect.y - old_origin[1]];
    output.extend(vertices.iter().map(|vertex| {
        let mut vertex = *vertex;
        vertex.position[0] += delta[0];
        vertex.position[1] += delta[1];
        vertex
    }));
}

pub(crate) fn content_clip(
    rect: PixelRect,
    status_bar_height: f32,
    viewport: [f32; 2],
) -> Option<[usize; 4]> {
    let x = rect.x.max(0.0).ceil();
    let y = rect.y.max(0.0).ceil();
    let right = (rect.x + rect.width).min(viewport[0]).floor();
    let bottom = (rect.y + (rect.height - status_bar_height).max(0.0))
        .min(viewport[1])
        .floor();
    if right <= x || bottom <= y {
        return None;
    }
    Some([x as usize, y as usize, (right - x) as usize, (bottom - y) as usize])
}

#[cfg(test)]
mod tests {
    use super::{append_translated, content_clip, PaneScene, PaneSceneCache, SceneImage};
    use crate::grid::Grid;
    use crate::layout::PixelRect;
    use crate::renderer::Vertex;
    use std::sync::Arc;

    fn scene(value: u8) -> PaneScene<Arc<u8>> {
        let vertex = Vertex::new(10.0, 20.0, 0.25, 0.5, value as u32, 0);
        PaneScene {
            origin: [10.0, 20.0],
            content: vec![vertex],
            atlas_texture: Arc::new(value),
            images: vec![SceneImage { vertices: [vertex; 6], texture: Arc::new(value) }],
        }
    }

    #[test]
    fn partial_output_and_image_replacement_stay_hidden_until_release() {
        let mut cache = PaneSceneCache::default();
        cache.update_with(1, false, || scene(1));
        let old_atlas = Arc::downgrade(&cache.get(1).unwrap().atlas_texture);
        let old_image = Arc::downgrade(&cache.get(1).unwrap().images[0].texture);

        for _ in 0..3 {
            cache.update_with(1, true, || panic!("must not read the partial grid"));
            let visible = cache.get(1).unwrap();
            assert_eq!(visible.content[0].fg_color, 1);
            assert_eq!(*visible.atlas_texture, 1);
            assert_eq!(*visible.images[0].texture, 1);
        }

        cache.update_with(1, false, || scene(2));
        assert_eq!(cache.get(1).unwrap().content[0].fg_color, 2);
        assert!(old_atlas.upgrade().is_none());
        assert!(old_image.upgrade().is_none());
    }

    #[test]
    fn new_frozen_pane_is_empty_and_other_panes_continue_to_present() {
        let mut cache = PaneSceneCache::default();
        cache.update_with(1, true, || panic!("first partial frame must stay hidden"));
        assert!(cache.get(1).is_none());
        cache.update_with(2, false, || scene(2));
        cache.update_with(1, true, || panic!("still frozen"));
        cache.update_with(2, false, || scene(3));
        assert_eq!(cache.get(2).unwrap().content[0].fg_color, 3);

        cache.update_with(1, false, || scene(4));
        assert_eq!(cache.get(1).unwrap().content[0].fg_color, 4);
        let closed_image = Arc::downgrade(&cache.get(2).unwrap().images[0].texture);
        cache.retain(|id| id == 1);
        assert!(cache.get(2).is_none());
        assert!(closed_image.upgrade().is_none());
    }

    #[test]
    fn resized_frozen_scene_moves_and_clips_without_rescaling_glyphs_or_uvs() {
        let scene = scene(1);
        let rect = PixelRect { x: 30.0, y: 40.0, width: 50.0, height: 60.0 };
        let mut vertices = Vec::new();
        append_translated(&mut vertices, &scene.content, scene.origin, rect);
        assert_eq!(vertices[0].position, [30.0, 40.0]);
        assert_eq!(vertices[0].uv, [0.25, 0.5]);
        assert_eq!(content_clip(rect, 10.0, [70.0, 100.0]), Some([30, 40, 40, 50]));
        assert_eq!(content_clip(rect, 60.0, [70.0, 100.0]), None);
        assert_eq!(content_clip(rect, 10.0, [20.0, 100.0]), None);
    }

    #[test]
    fn missing_end_marker_releases_the_scene_when_the_idle_timer_expires() {
        let mut grid = Grid::new(4, 2, 0);
        grid.put_char('A');
        let mut cache = PaneSceneCache::default();
        cache.update_with(1, false, || scene(grid.visible_cell(0, 0).c as u8));
        let future = std::time::Instant::now() + std::time::Duration::from_secs(60);
        grid.set_synchronized_output_at(true, future);
        grid.set_cursor_pos(0, 0);
        grid.put_char('B');
        cache.update_with(1, grid.synchronized_output_active(), || {
            panic!("incomplete output must remain hidden")
        });
        assert_eq!(cache.get(1).unwrap().content[0].fg_color, u32::from(b'A'));

        // No further terminal bytes arrive; only the timer checks the deadline.
        assert!(grid.expire_synchronized_output(grid.synchronized_output_deadline().unwrap()));
        cache.update_with(1, grid.synchronized_output_active(), || {
            scene(grid.visible_cell(0, 0).c as u8)
        });
        assert_eq!(cache.get(1).unwrap().content[0].fg_color, u32::from(b'B'));
    }
}
