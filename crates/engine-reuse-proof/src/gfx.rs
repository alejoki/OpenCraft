//! Device-free gfx proof over the neutral mesh, texture-array and model APIs.
//!
//! Every call here is CPU-side data, byte math or ABI inspection. The proof
//! builds a [`ChunkMesh`] from packed quads, validates a [`TextureArraySpec`]
//! and its mip-chain byte math, and reads the pinned [`ModelVertex`] layout.
//! Nothing here creates an adapter, device, surface or pipeline, so the proof
//! runs headless on a CI runner with no GPU driver.

use std::mem::size_of;

use opencraft_core::rgba::downsample_rgba_2x;
use opencraft_gfx::buffer::{
    chunk_vram_bytes, opaque_buffer_bytes, water_buffer_bytes, water_vertex_range,
};
use opencraft_gfx::mesh::quad::{ChunkMesh, PackedQuad};
use opencraft_gfx::model::{
    model_vertex_layout, ModelVertex, MODEL_CAMERA_UNIFORM_BYTES, MODEL_VERTEX_ATTRIBUTES,
    MODEL_VERTEX_BYTES,
};
use opencraft_gfx::texture::{TextureArraySpec, TextureArraySpecError};

/// The observable result of one device-free gfx proof run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GfxProof {
    /// Opaque vertex-buffer bytes for the one-opaque-quad mesh.
    pub opaque_bytes: usize,
    /// Water vertex-buffer bytes for the one-water-quad mesh.
    pub water_bytes: usize,
    /// RGBA bytes in one 16x16 texture-array layer.
    pub texture_layer_bytes: usize,
    /// Byte lengths of the 16x16 RGBA mip chain, base mip first.
    pub mip_bytes: [usize; 5],
    /// Bytes in one model vertex.
    pub model_vertex_bytes: usize,
}

/// Halve `base` `size` times and report the byte length of every mip.
fn mip_chain_bytes(base: &[u8], size: u32) -> [usize; 5] {
    const MIP_LEVELS: usize = 5;
    let mut chain = [0usize; MIP_LEVELS];
    let mut current = base.to_vec();
    let mut dim = size;
    for (mip, slot) in chain.iter_mut().enumerate() {
        *slot = current.len();
        if mip + 1 < MIP_LEVELS {
            current = downsample_rgba_2x(&current, dim);
            dim /= 2;
        }
    }
    chain
}

/// Run the mesh, texture-array and model ABI proofs deterministically.
pub fn run() -> GfxProof {
    // Mesh wire format: one opaque quad in bucket 0, one water quad outside
    // the buckets, and the CPU-side byte/vertex math over both.
    let quad = PackedQuad::new([1, 2, 3], [3, 4], 5, 9, 0b1010_0101, 0xA5C3);
    assert_eq!(size_of::<PackedQuad>(), 8, "PackedQuad wire size");
    assert_eq!(
        quad.unpack(),
        (1, 2, 3, 3, 4, 5, 9, 0b1010_0101, 0xA5C3),
        "PackedQuad round trip"
    );

    let mut mesh = ChunkMesh::new();
    mesh.buckets[0].push(quad);
    mesh.water_quads.push(quad);
    assert_eq!(mesh.total_quads(), 1, "opaque quads exclude water");
    assert_eq!(mesh.water_quad_count(), 1);
    assert_eq!(mesh.total_quads_including_water(), 2);
    let opaque_bytes = opaque_buffer_bytes(&mesh);
    let water_bytes = water_buffer_bytes(&mesh);
    assert_eq!(opaque_bytes, 8, "one 8-byte packed quad");
    assert_eq!(water_bytes, 8, "one 8-byte packed water quad");
    assert_eq!(chunk_vram_bytes(&mesh), 16);
    assert_eq!(water_vertex_range(&mesh), 0..6);

    // Texture-array shape and mip math, all before any upload call.
    let spec = TextureArraySpec {
        label: "engine-reuse-proof",
        size: 16,
        layers: 2,
        mip_levels: 5,
    };
    assert_eq!(spec.validate(), Ok(()), "the proof spec must validate");
    let texture_layer_bytes = spec.layer_bytes();
    assert_eq!(texture_layer_bytes, 1024);

    let exact_layers = vec![vec![0u8; texture_layer_bytes]; spec.layers as usize];
    assert!(spec.matches_shape(&exact_layers));
    assert!(!spec.matches_shape(&exact_layers[..1]), "one layer short");
    let mut short_layer = vec![vec![0u8; texture_layer_bytes]; spec.layers as usize];
    short_layer[1].pop();
    assert!(!spec.matches_shape(&short_layer), "short layer bytes");

    let mip_dims: [u32; 5] = std::array::from_fn(|mip| spec.mip_dim(mip as u32));
    assert_eq!(mip_dims, [16, 8, 4, 2, 1]);

    // Deterministic RGBA base -> box-filtered chain. Only lengths are
    // asserted; byte values are the shared `downsample_rgba_2x` contract's job.
    let base_rgba: Vec<u8> = (0..texture_layer_bytes).map(|i| (i % 251) as u8).collect();
    assert_eq!(base_rgba.len(), 1024);
    let mip_bytes = mip_chain_bytes(&base_rgba, spec.size);
    assert_eq!(mip_bytes, [1024, 256, 64, 16, 4]);

    let invalid = |size: u32, layers: u32, mip_levels: u32| TextureArraySpec {
        label: "engine-reuse-proof-invalid",
        size,
        layers,
        mip_levels,
    };
    assert_eq!(
        invalid(0, 1, 1).validate(),
        Err(TextureArraySpecError::ZeroSize)
    );
    assert_eq!(
        invalid(16, 0, 1).validate(),
        Err(TextureArraySpecError::ZeroLayers)
    );
    assert_eq!(
        invalid(16, 1, 0).validate(),
        Err(TextureArraySpecError::ZeroMipLevels)
    );
    assert_eq!(
        invalid(16, 1, 6).validate(),
        Err(TextureArraySpecError::TooManyMipLevels)
    );

    // Model vertex ABI: pinned size, public fields, layout stride/attributes.
    let model_vertex_bytes = size_of::<ModelVertex>();
    assert_eq!(model_vertex_bytes, MODEL_VERTEX_BYTES);
    assert_eq!(MODEL_VERTEX_BYTES, 32);
    let vertex = ModelVertex {
        position: [0.0, 1.0, 2.0],
        uv: [0.25, 0.75],
        tex_layer: 3,
        shade: 0.5,
        flash: 0.0,
    };
    let expected_position = [0.0f32, 1.0, 2.0];
    for (actual, expected) in vertex.position.iter().zip(expected_position) {
        assert!(
            (*actual - expected).abs() <= f32::EPSILON,
            "position {actual} != {expected}"
        );
    }
    let expected_uv = [0.25f32, 0.75];
    for (actual, expected) in vertex.uv.iter().zip(expected_uv) {
        assert!(
            (*actual - expected).abs() <= f32::EPSILON,
            "uv {actual} != {expected}"
        );
    }
    assert_eq!(vertex.tex_layer, 3);
    assert!(
        (vertex.shade - 0.5).abs() <= f32::EPSILON,
        "shade {}",
        vertex.shade
    );
    assert!(vertex.flash.abs() <= f32::EPSILON, "flash {}", vertex.flash);

    let layout = model_vertex_layout();
    assert_eq!(layout.array_stride, MODEL_VERTEX_BYTES as u64);
    assert_eq!(layout.array_stride, 32);
    assert_eq!(layout.attributes.len(), MODEL_VERTEX_ATTRIBUTES.len());
    assert_eq!(layout.attributes.len(), 5);
    assert_eq!(MODEL_CAMERA_UNIFORM_BYTES, 112);

    GfxProof {
        opaque_bytes,
        water_bytes,
        texture_layer_bytes,
        mip_bytes,
        model_vertex_bytes,
    }
}
