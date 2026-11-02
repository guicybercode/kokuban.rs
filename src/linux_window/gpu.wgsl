// One pipeline draws every layer of a terminal frame as instanced quads:
// solid rectangles (cell backgrounds, underlines, box drawing, cursor),
// glyphs from the RGBA atlas and kitty/sixel images. `fill` selects the
// source; blending is source-over with straight alpha, matching the CPU path
// and the Metal shader (`mix(bg, fg * tex, tex.a)`).

struct Uniforms {
    // Viewport size in pixels; the rest is padding for the 16-byte uniform.
    viewport: vec4<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var atlas_texture: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;
@group(1) @binding(0) var image_texture: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;

struct Instance {
    // x, y, width, height in pixels.
    @location(0) rect: vec4<f32>,
    // u0, v0, u1, v1 of the sampled texture.
    @location(1) uv: vec4<f32>,
    // Straight RGBA color (solid fills and glyph foreground).
    @location(2) color: vec4<f32>,
    // 0 solid, 1 atlas mask glyph, 2 atlas color glyph, 3 image.
    @location(3) fill: u32,
};

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) fill: u32,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex: u32, instance: Instance) -> Varyings {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let corner = corners[vertex];
    let pixel = instance.rect.xy + corner * instance.rect.zw;
    let ndc = vec2<f32>(
        pixel.x / uniforms.viewport.x * 2.0 - 1.0,
        1.0 - pixel.y / uniforms.viewport.y * 2.0,
    );
    var out: Varyings;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = instance.uv.xy + corner * (instance.uv.zw - instance.uv.xy);
    out.color = instance.color;
    out.fill = instance.fill;
    return out;
}

@fragment
fn fs_main(in: Varyings) -> @location(0) vec4<f32> {
    // Both samples stay in uniform control flow; `fill` picks one afterwards.
    let atlas = textureSample(atlas_texture, atlas_sampler, in.uv);
    let image = textureSample(image_texture, image_sampler, in.uv);
    var color = in.color;
    if (in.fill == 1u) {
        color = vec4<f32>(in.color.rgb * atlas.rgb, in.color.a * atlas.a);
    } else if (in.fill == 2u) {
        color = atlas;
    } else if (in.fill == 3u) {
        color = image;
    }
    return color;
}
