//! External contract for the device-free gfx half of the reuse proof.
//!
//! This target is an outside consumer: it imports the proof crate plus the
//! engine gfx types the proof exercises, and never names the `opencraft` game
//! package or a GPU API. Every assertion below is CPU-side data, byte math or
//! ABI inspection, so the target runs headless without an adapter.

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

use engine_reuse_proof::gfx;

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= f32::EPSILON,
        "expected {expected}, got {actual}"
    );
}

fn proof_spec() -> TextureArraySpec {
    TextureArraySpec {
        label: "engine-reuse-proof",
        size: 16,
        layers: 2,
        mip_levels: 5,
    }
}

#[test]
fn proof_reports_the_mesh_bytes_and_water_vertex_range() {
    let proof = gfx::run();

    assert_eq!(proof.opaque_bytes, 8, "one packed quad");
    assert_eq!(proof.water_bytes, 8, "one packed water quad");

    let quad = PackedQuad::new([1, 2, 3], [3, 4], 5, 9, 0b1010_0101, 0xA5C3);
    assert_eq!(size_of::<PackedQuad>(), 8);
    assert_eq!(quad.unpack(), (1, 2, 3, 3, 4, 5, 9, 0b1010_0101, 0xA5C3));

    let mut mesh = ChunkMesh::new();
    mesh.buckets[0].push(quad);
    mesh.water_quads.push(quad);

    assert_eq!(mesh.total_quads(), 1, "opaque count excludes water");
    assert_eq!(mesh.water_quad_count(), 1);
    assert_eq!(mesh.total_quads_including_water(), 2);
    assert_eq!(opaque_buffer_bytes(&mesh), 8);
    assert_eq!(water_buffer_bytes(&mesh), 8);
    assert_eq!(chunk_vram_bytes(&mesh), 16);
    assert_eq!(water_vertex_range(&mesh), 0..6);
}

#[test]
fn proof_reports_the_texture_layer_and_mip_chain_bytes() {
    let proof = gfx::run();

    assert_eq!(proof.texture_layer_bytes, 1024);
    assert_eq!(proof.mip_bytes, [1024, 256, 64, 16, 4]);

    let spec = proof_spec();
    assert_eq!(spec.layer_bytes(), proof.texture_layer_bytes);

    let base: Vec<u8> = (0..proof.texture_layer_bytes)
        .map(|i| (i % 251) as u8)
        .collect();
    let mut chain = vec![base];
    let mut dim = spec.size;
    for _ in 0..spec.mip_levels - 1 {
        let next = downsample_rgba_2x(chain.last().expect("base mip"), dim);
        chain.push(next);
        dim /= 2;
    }
    let lengths: Vec<usize> = chain.iter().map(Vec::len).collect();
    assert_eq!(lengths, proof.mip_bytes.to_vec());
}

#[test]
fn texture_spec_shape_and_mip_math_are_data_only() {
    let spec = proof_spec();

    assert_eq!(spec.validate(), Ok(()));
    assert_eq!(spec.layer_bytes(), 1024);
    assert!(spec.matches_shape(&[vec![0u8; 1024], vec![0u8; 1024]]));
    assert!(!spec.matches_shape(&[vec![0u8; 1024]]), "one layer short");
    assert!(
        !spec.matches_shape(&[vec![0u8; 1024], vec![0u8; 1023]]),
        "short layer bytes"
    );

    let mip_dims: Vec<u32> = (0..spec.mip_levels).map(|mip| spec.mip_dim(mip)).collect();
    assert_eq!(mip_dims, vec![16, 8, 4, 2, 1]);
}

#[test]
fn invalid_texture_specs_return_the_exact_error_variants() {
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
}

#[test]
fn proof_reports_the_pinned_model_vertex_abi() {
    let proof = gfx::run();
    assert_eq!(proof.model_vertex_bytes, 32);

    assert_eq!(size_of::<ModelVertex>(), MODEL_VERTEX_BYTES);
    assert_eq!(MODEL_VERTEX_BYTES, 32);

    // Field-by-field construction from outside proves every field is public
    // and typed as the shader ABI expects.
    let vertex = ModelVertex {
        position: [0.0, 1.0, 2.0],
        uv: [0.25, 0.75],
        tex_layer: 3,
        shade: 0.5,
        flash: 0.0,
    };
    for (actual, expected) in vertex.position.iter().zip([0.0f32, 1.0, 2.0]) {
        assert_close(*actual, expected);
    }
    for (actual, expected) in vertex.uv.iter().zip([0.25f32, 0.75]) {
        assert_close(*actual, expected);
    }
    assert_eq!(vertex.tex_layer, 3);
    assert_close(vertex.shade, 0.5);
    assert_close(vertex.flash, 0.0);

    // Reading the returned layout needs no direct GPU API import: the stride
    // is the usize constant converted to u64, and the attribute count must
    // match the pinned five.
    let layout = model_vertex_layout();
    assert_eq!(layout.array_stride, MODEL_VERTEX_BYTES as u64);
    assert_eq!(layout.array_stride, 32);
    assert_eq!(layout.attributes.len(), MODEL_VERTEX_ATTRIBUTES.len());
    assert_eq!(layout.attributes.len(), 5);
    assert_eq!(MODEL_CAMERA_UNIFORM_BYTES, 112);
}

#[test]
fn consumer_gfx_source_calls_no_device_api() {
    let source = include_str!("../src/gfx.rs");
    for forbidden in [
        "wgpu::",
        "create_texture(",
        "create_view(",
        "upload_layer(",
        "upload_layers(",
        "create_model_pipeline(",
    ] {
        assert!(
            !source.contains(forbidden),
            "the gfx proof must stay device-free; found {forbidden}"
        );
    }
}
