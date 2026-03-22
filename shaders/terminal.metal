#include <metal_stdlib>
using namespace metal;

struct VertexIn {
    float2 position [[attribute(0)]];
    float2 uv       [[attribute(1)]];
    uint   fg_color [[attribute(2)]];
    uint   bg_color [[attribute(3)]];
};

struct VertexOut {
    float4 position [[position]];
    float2 uv;
    float4 fg_color;
    float4 bg_color;
};

struct Uniforms {
    float2 viewport_size;
};

float4 unpack_color(uint packed) {
    float r = float((packed >> 24) & 0xFF) / 255.0;
    float g = float((packed >> 16) & 0xFF) / 255.0;
    float b = float((packed >> 8)  & 0xFF) / 255.0;
    float a = float( packed        & 0xFF) / 255.0;
    return float4(r, g, b, a);
}

vertex VertexOut vertex_main(
    VertexIn in [[stage_in]],
    constant Uniforms &uniforms [[buffer(1)]]
) {
    VertexOut out;
    float2 ndc;
    ndc.x = (in.position.x / uniforms.viewport_size.x) * 2.0 - 1.0;
    ndc.y = 1.0 - (in.position.y / uniforms.viewport_size.y) * 2.0;
    out.position = float4(ndc, 0.0, 1.0);
    out.uv = in.uv;
    out.fg_color = unpack_color(in.fg_color);
    out.bg_color = unpack_color(in.bg_color);
    return out;
}

fragment float4 fragment_main(
    VertexOut in [[stage_in]],
    texture2d<float> atlas [[texture(0)]],
    sampler samp [[sampler(0)]]
) {
    float coverage = atlas.sample(samp, in.uv).r;
    return mix(in.bg_color, in.fg_color, coverage);
}
