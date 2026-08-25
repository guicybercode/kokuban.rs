pub type ImageId = u32;

#[derive(Debug, Clone)]
pub struct ImagePlacement {
    pub image_id: ImageId,
    /// Non-zero renderer-local identity. Sixel reserves zero as its source marker.
    pub placement_id: u32,
    /// Protocol `p=` supplied by the Kitty client, distinct from local identity.
    pub client_placement_id: Option<u32>,
    pub mode: PlacementMode,
    pub z_index: i32,
}

#[derive(Debug, Clone)]
pub enum PlacementMode {
    Inline {
        row: usize,
        col: usize,
        cols: u32,
        rows: u32,
    },
    Overlay {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
}
