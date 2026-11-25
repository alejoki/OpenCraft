use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct PackedQuad(pub [u32; 2]);

impl PackedQuad {
    #[inline]
    pub const fn new(
        pos: [u32; 3],
        extent: [u32; 2],
        face_dir: u32,
        tex_layer: u8,
        ao_packed: u8,
        light_packed: u16,
    ) -> Self {
        let [x, y, z] = pos;
        let [w, h] = extent;
        debug_assert!(x < 32 && y < 32 && z < 32);
        debug_assert!(w >= 1 && w <= 32 && h >= 1 && h <= 32);
        // Face directions 6 and 7 are the two crossed planes of a plant
        // (model = cross|crop). They pack into the same 3-bit field as the
        // six cube directions; only the renderer needs to interpret them.
        debug_assert!(face_dir < 8);
        let word0 = (x & 31)
            | ((y & 31) << 5)
            | ((z & 31) << 10)
            | (((w - 1) & 31) << 15)
            | (((h - 1) & 31) << 20)
            | ((face_dir & 7) << 25);
        let word1 = (tex_layer as u32) | ((ao_packed as u32) << 8) | ((light_packed as u32) << 16);
        Self([word0, word1])
    }

    #[inline]
    pub const fn unpack(self) -> (u32, u32, u32, u32, u32, u32, u8, u8, u16) {
        let w0 = self.0[0];
        let w1 = self.0[1];
        let x = w0 & 31;
        let y = (w0 >> 5) & 31;
        let z = (w0 >> 10) & 31;
        let w = ((w0 >> 15) & 31) + 1;
        let h = ((w0 >> 20) & 31) + 1;
        let face_dir = (w0 >> 25) & 7;
        let tex_layer = (w1 & 0xFF) as u8;
        let ao_packed = ((w1 >> 8) & 0xFF) as u8;
        let light_packed = ((w1 >> 16) & 0xFFFF) as u16;
        (x, y, z, w, h, face_dir, tex_layer, ao_packed, light_packed)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ChunkMesh {
    /// The six cube face buckets holding opaque **and** cutout quads. These are
    /// the only quads the opaque GPU upload (`ChunkRenderStore::upload_mesh`)
    /// ever sees; cutout is an alpha-test state of the same draw, not a second
    /// buffer.
    pub buckets: [Vec<PackedQuad>; 6],
    /// Water surface quads, deliberately outside `buckets` (D070). Until S04
    /// wires the translucent pass nothing uploads this field, so a water quad
    /// can never enter the opaque vertex buffer or sample the opaque
    /// texture-array layers. Fail-soft by construction: a missing water pass
    /// leaves the opaque world rendering exactly as before.
    pub water_quads: Vec<PackedQuad>,
    /// Alpha-blended solid-cube faces (currently glass). Kept outside
    /// `buckets` so the sorted translucent pass of S03-T05 can own them; until
    /// then nothing uploads this field and the opaque world is unchanged.
    pub blend_quads: Vec<PackedQuad>,
    /// Crossed-plant quads (`face_dir` 6 and 7), two per cross/crop block.
    /// Plants never emit cube faces, so this list is their only geometry.
    pub cross_quads: Vec<PackedQuad>,
}

impl ChunkMesh {
    pub fn new() -> Self {
        Self::default()
    }

    /// A mesh is empty only when it has no geometry of any class. Streaming
    /// bookkeeping uses this so a water-only (open ocean surface) or
    /// glass/plant-only chunk is never treated as "nothing to draw".
    pub fn is_empty(&self) -> bool {
        self.buckets.iter().all(|b| b.is_empty())
            && self.water_quads.is_empty()
            && self.blend_quads.is_empty()
            && self.cross_quads.is_empty()
    }

    /// Opaque + cutout quad count. `ChunkRenderStore::upload_mesh` sizes its
    /// vertex buffer and VRAM accounting from this, so water, blend and cross
    /// must stay out of it until their own passes exist (otherwise the opaque
    /// upload would reserve space for quads it never writes).
    pub fn total_quads(&self) -> usize {
        self.buckets.iter().map(|b| b.len()).sum()
    }

    /// Water surface quad count (never uploaded by the opaque pass).
    pub fn water_quad_count(&self) -> usize {
        self.water_quads.len()
    }

    /// Alpha-blended cube face count (never uploaded by the opaque pass).
    pub fn blend_quad_count(&self) -> usize {
        self.blend_quads.len()
    }

    /// Crossed-plant quad count, two per cross/crop block.
    pub fn cross_quad_count(&self) -> usize {
        self.cross_quads.len()
    }

    /// Every quad the mesher produced: opaque, cutout, water, blend and cross.
    /// Diagnostic only; never feed this to GPU upload accounting.
    pub fn total_quads_including_water(&self) -> usize {
        self.total_quads()
            + self.water_quad_count()
            + self.blend_quad_count()
            + self.cross_quad_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_quad_layout_and_round_trip_unchanged() {
        let ao = 0b1010_0101u8;
        let light = 0xA5C3u16;
        let quad = PackedQuad::new([5, 6, 7], [3, 4], 5, 9, ao, light);

        assert_eq!(
            quad.0[0],
            5 | (6 << 5) | (7 << 10) | (2 << 15) | (3 << 20) | (5 << 25)
        );
        assert_eq!(quad.0[1], 9 | ((ao as u32) << 8) | ((light as u32) << 16));
        // Light occupies word1 bits 16-31 and does not bleed into tex_layer/AO.
        assert_eq!(quad.0[1] >> 16, light as u32);
        assert_eq!(quad.unpack(), (5, 6, 7, 3, 4, 5, 9, ao, light));
    }

    #[test]
    fn water_quads_are_outside_the_opaque_buckets_and_count_toward_emptiness() {
        let mut mesh = ChunkMesh::new();
        assert!(mesh.is_empty());
        assert_eq!(mesh.total_quads(), 0);

        mesh.water_quads
            .push(PackedQuad::new([0, 0, 0], [1, 1], 2, 0, 0xFF, 0xFFFF));

        assert!(
            !mesh.is_empty(),
            "a water-only mesh is not empty for streaming"
        );
        assert_eq!(mesh.total_quads(), 0, "opaque upload count excludes water");
        assert_eq!(mesh.water_quad_count(), 1);
        assert_eq!(mesh.total_quads_including_water(), 1);
        for (dir, bucket) in mesh.buckets.iter().enumerate() {
            assert!(
                bucket.is_empty(),
                "water must never enter opaque bucket {dir}"
            );
        }
    }

    #[test]
    fn blend_and_cross_quads_are_counted_and_keep_the_mesh_non_empty() {
        let mut mesh = ChunkMesh::new();
        assert!(mesh.is_empty());

        // A glass-only chunk puts every quad in the blend list and nothing in
        // the opaque upload.
        mesh.blend_quads
            .push(PackedQuad::new([1, 2, 3], [1, 1], 0, 25, 0xFF, 0xFFFF));
        assert!(!mesh.is_empty(), "a blend-only mesh is not empty");
        assert_eq!(
            mesh.total_quads(),
            0,
            "blend stays out of the opaque upload"
        );
        assert_eq!(mesh.blend_quad_count(), 1);
        assert_eq!(mesh.cross_quad_count(), 0);
        assert_eq!(mesh.water_quad_count(), 0);
        assert_eq!(mesh.total_quads_including_water(), 1);

        // A wheat chunk puts two crossed planes in the cross list.
        let mut wheat = ChunkMesh::new();
        for face_dir in 6..8 {
            wheat.cross_quads.push(PackedQuad::new(
                [4, 5, 6],
                [1, 1],
                face_dir,
                19,
                0xFF,
                0xFFFF,
            ));
        }
        assert!(!wheat.is_empty(), "a cross-only mesh is not empty");
        assert_eq!(wheat.total_quads(), 0, "plants never emit cube faces");
        assert_eq!(wheat.cross_quad_count(), 2);
        assert_eq!(wheat.blend_quad_count(), 0);
        assert_eq!(wheat.total_quads_including_water(), 2);

        // The diagnostic total spans every class while `total_quads` does not.
        mesh.cross_quads.extend(wheat.cross_quads);
        mesh.water_quads
            .push(PackedQuad::new([0, 0, 0], [1, 1], 2, 0, 0xFF, 0xFFFF));
        assert_eq!(mesh.total_quads(), 0);
        assert_eq!(mesh.total_quads_including_water(), 4);
    }

    #[test]
    fn cross_face_directions_6_and_7_round_trip() {
        for face_dir in 6..8 {
            let quad = PackedQuad::new([1, 2, 3], [1, 1], face_dir, 7, 0xFF, 0x1234);
            let (x, y, z, w, h, unpacked_dir, layer, ao, light) = quad.unpack();
            assert_eq!((x, y, z, w, h), (1, 2, 3, 1, 1));
            assert_eq!(unpacked_dir, face_dir);
            assert_eq!((layer, ao, light), (7, 0xFF, 0x1234));
        }
    }

    #[test]
    fn packed_quad_light_bits_round_trip_at_boundaries() {
        for light in [0u16, 1, 15, 0x00FF, 0xFF00, 0xFFFF] {
            let quad = PackedQuad::new([0, 0, 0], [1, 1], 0, 0, 0, light);
            assert_eq!(quad.unpack().8, light);
            assert_eq!(quad.unpack().6, 0);
            assert_eq!(quad.unpack().7, 0);
        }
    }
}
