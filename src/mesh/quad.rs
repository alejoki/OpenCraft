use bytemuck::{Pod, Zeroable};

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FaceDir {
    PosX = 0,
    NegX = 1,
    PosY = 2,
    NegY = 3,
    PosZ = 4,
    NegZ = 5,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct PackedQuad(pub [u32; 2]);

impl PackedQuad {
    #[inline]
    pub const fn new(
        x: u32,
        y: u32,
        z: u32,
        w: u32,
        h: u32,
        face_dir: u32,
        tex_layer: u8,
        ao_packed: u8,
    ) -> Self {
        debug_assert!(x < 32 && y < 32 && z < 32);
        debug_assert!(w >= 1 && w <= 32 && h >= 1 && h <= 32);
        debug_assert!(face_dir < 6);
        let word0 = (x & 31)
            | ((y & 31) << 5)
            | ((z & 31) << 10)
            | (((w - 1) & 31) << 15)
            | (((h - 1) & 31) << 20)
            | ((face_dir & 7) << 25);
        let word1 = (tex_layer as u32) | ((ao_packed as u32) << 8);
        Self([word0, word1])
    }

    #[inline]
    pub const fn unpack(self) -> (u32, u32, u32, u32, u32, u32, u8, u8) {
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
        (x, y, z, w, h, face_dir, tex_layer, ao_packed)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ChunkMesh {
    pub buckets: [Vec<PackedQuad>; 6],
}

impl ChunkMesh {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.iter().all(|b| b.is_empty())
    }

    pub fn total_quads(&self) -> usize {
        self.buckets.iter().map(|b| b.len()).sum()
    }
}
