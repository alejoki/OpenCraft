//! Neutral texture-array shape and upload machinery.
//!
//! The host owns every content-identity constant (layer count, authored layer
//! bases, procedural art); this module owns only the device-facing part: it
//! validates a `TextureArraySpec`, creates the matching RGBA8 sRGB array and
//! its D2-array view, and uploads each layer's base mip plus a box-filtered
//! mip chain. The spec carries the label the host wants on the texture so the
//! crate stays free of host content names.

use opencraft_core::rgba::downsample_rgba_2x;

/// Why a [`TextureArraySpec`] cannot describe a valid texture array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureArraySpecError {
    /// `size` is zero.
    ZeroSize,
    /// `layers` is zero.
    ZeroLayers,
    /// `mip_levels` is zero.
    ZeroMipLevels,
    /// `mip_levels` exceeds the full chain a square `size` can hold.
    TooManyMipLevels,
}

impl std::fmt::Display for TextureArraySpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::ZeroSize => "texture array size must be non-zero",
            Self::ZeroLayers => "texture array layer count must be non-zero",
            Self::ZeroMipLevels => "texture array mip level count must be non-zero",
            Self::TooManyMipLevels => {
                "texture array mip level count exceeds the size's full mip chain"
            }
        };
        f.write_str(message)
    }
}

impl std::error::Error for TextureArraySpecError {}

/// Shape of an RGBA8 sRGB texture array plus the label the host wants on it.
///
/// The host builds this from its own content constants and keeps ownership of
/// them; the spec is the neutral boundary the upload machinery consumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureArraySpec {
    /// Label applied to the texture, its view, and derived debug names.
    pub label: &'static str,
    /// Side length in pixels of every (square) layer.
    pub size: u32,
    /// Number of array layers.
    pub layers: u32,
    /// Full mip chain length, from the base mip to the 1x1 mip.
    pub mip_levels: u32,
}

impl TextureArraySpec {
    /// Bytes in one RGBA layer: `size * size * 4`.
    pub const fn layer_bytes(&self) -> usize {
        self.size as usize * self.size as usize * 4
    }

    /// Reject shapes that cannot describe an uploadable texture array.
    ///
    /// Zero dimensions and mip chains longer than `size` can hold are the
    /// typed boundary for external callers; the upload methods themselves
    /// keep debug assertions so release builds never panic on bad input.
    pub fn validate(&self) -> Result<(), TextureArraySpecError> {
        if self.size == 0 {
            return Err(TextureArraySpecError::ZeroSize);
        }
        if self.layers == 0 {
            return Err(TextureArraySpecError::ZeroLayers);
        }
        if self.mip_levels == 0 {
            return Err(TextureArraySpecError::ZeroMipLevels);
        }
        if self.mip_levels > self.size.ilog2() + 1 {
            return Err(TextureArraySpecError::TooManyMipLevels);
        }
        Ok(())
    }

    /// True when `layers` has exactly this spec's layer count and every layer
    /// holds exactly [`Self::layer_bytes`] RGBA bytes.
    pub fn matches_shape(&self, layers: &[Vec<u8>]) -> bool {
        layers.len() == self.layers as usize
            && layers.iter().all(|layer| layer.len() == self.layer_bytes())
    }

    /// Side length of `mip`, halving toward a one-pixel mip.
    pub fn mip_dim(&self, mip: u32) -> u32 {
        (self.size >> mip).max(1)
    }

    /// Create the RGBA8 sRGB texture array this spec describes.
    pub fn create_texture(&self, device: &wgpu::Device) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(self.label),
            size: wgpu::Extent3d {
                width: self.size,
                height: self.size,
                depth_or_array_layers: self.layers,
            },
            mip_level_count: self.mip_levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// Create the D2-array view over every layer of `texture`.
    pub fn create_view(&self, texture: &wgpu::Texture) -> wgpu::TextureView {
        let label = format!("{}_view", self.label);
        texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some(&label),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            array_layer_count: Some(self.layers),
            ..Default::default()
        })
    }

    /// Upload one layer's base mip and its box-filtered mip chain into
    /// `texture` at `layer`.
    ///
    /// `rgba` must be exactly [`Self::layer_bytes`] bytes and `layer` must be
    /// below [`Self::layers`]; both are debug assertions so release builds
    /// never panic here. The texture, view, and bind group are never
    /// recreated (only contents change).
    pub fn upload_layer(
        &self,
        texture: &wgpu::Texture,
        queue: &wgpu::Queue,
        layer: u32,
        rgba: &[u8],
    ) {
        debug_assert!(
            layer < self.layers,
            "layer {layer} is outside the {}-layer texture array",
            self.layers
        );
        debug_assert_eq!(
            rgba.len(),
            self.layer_bytes(),
            "layer bytes must match {}x{} RGBA",
            self.size,
            self.size
        );

        let mut current = rgba.to_vec();
        let mut dim = self.size;
        for mip in 0..self.mip_levels {
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture,
                    mip_level: mip,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &current,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * dim),
                    rows_per_image: Some(dim),
                },
                wgpu::Extent3d {
                    width: dim,
                    height: dim,
                    depth_or_array_layers: 1,
                },
            );
            if mip + 1 < self.mip_levels {
                current = downsample_rgba_2x(&current, dim);
                dim = (dim / 2).max(1);
            }
        }
    }

    /// Upload every layer's base mip and its box-filtered mip chain.
    ///
    /// Delegates to [`Self::upload_layer`] so the bulk and single-layer paths
    /// cannot drift.
    pub fn upload_layers(&self, texture: &wgpu::Texture, queue: &wgpu::Queue, layers: &[Vec<u8>]) {
        for (layer_idx, base_rgba) in layers.iter().enumerate() {
            self.upload_layer(texture, queue, layer_idx as u32, base_rgba);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(size: u32, layers: u32, mip_levels: u32) -> TextureArraySpec {
        TextureArraySpec {
            label: "test_array",
            size,
            layers,
            mip_levels,
        }
    }

    #[test]
    fn rgba_layer_bytes_follow_size_squared_times_four() {
        assert_eq!(spec(16, 256, 5).layer_bytes(), 1024);
        assert_eq!(spec(1, 1, 1).layer_bytes(), 4);
        assert_eq!(spec(32, 3, 6).layer_bytes(), 4096);
    }

    #[test]
    fn mip_dimensions_halve_to_one_and_stay_at_least_one() {
        let spec = spec(16, 256, 5);
        let dims: Vec<u32> = (0..spec.mip_levels).map(|mip| spec.mip_dim(mip)).collect();
        assert_eq!(dims, vec![16, 8, 4, 2, 1]);
        assert_eq!(spec.mip_dim(4), 1);
    }

    #[test]
    fn matches_shape_accepts_exact_layers_and_rejects_count_or_size_drift() {
        let spec = spec(2, 3, 2);
        let exact = vec![vec![0u8; 16]; 3];
        assert!(spec.matches_shape(&exact));

        assert!(!spec.matches_shape(&exact[..2]), "one layer short");
        let mut one_extra = exact.clone();
        one_extra.push(vec![0u8; 16]);
        assert!(!spec.matches_shape(&one_extra), "one layer long");

        let mut short_layer = exact.clone();
        short_layer[1].pop();
        assert!(!spec.matches_shape(&short_layer), "short layer bytes");

        let mut long_layer = exact;
        long_layer[0].push(0);
        assert!(!spec.matches_shape(&long_layer), "long layer bytes");
    }

    #[test]
    fn validate_rejects_zero_size_zero_layers_and_too_many_mips() {
        assert_eq!(spec(16, 256, 5).validate(), Ok(()));
        assert_eq!(
            spec(16, 256, 6).validate(),
            Err(TextureArraySpecError::TooManyMipLevels)
        );
        assert_eq!(
            spec(0, 1, 1).validate(),
            Err(TextureArraySpecError::ZeroSize)
        );
        assert_eq!(
            spec(16, 0, 1).validate(),
            Err(TextureArraySpecError::ZeroLayers)
        );
        assert_eq!(
            spec(16, 1, 0).validate(),
            Err(TextureArraySpecError::ZeroMipLevels)
        );
    }

    #[test]
    fn create_uses_the_spec_shape_mip_count_and_rgba8_srgb_format() {
        let src = include_str!("texture.rs");
        let tests_at = src.find("#[cfg(test)]").expect("tests module exists");
        let production = &src[..tests_at];

        let at = production
            .find("pub fn create_texture(")
            .expect("create_texture exists");
        let body = &production[at..];
        let body = &body[..body.find("\n    }\n").expect("method body ends") + 6];

        assert!(
            body.contains("self.size") && body.contains("self.layers"),
            "the descriptor shape must come from the spec"
        );
        assert!(
            body.contains("self.mip_levels"),
            "the descriptor mip count must come from the spec"
        );
        assert!(
            body.contains("Rgba8UnormSrgb"),
            "the array format must stay RGBA8 sRGB"
        );
        assert!(
            body.contains("TEXTURE_BINDING") && body.contains("COPY_DST"),
            "the array must be bindable and uploadable"
        );

        let at = production
            .find("pub fn create_view(")
            .expect("create_view exists");
        let view = &production[at..];
        let view = &view[..view.find("\n    }\n").expect("method body ends") + 6];
        assert!(
            view.contains("array_layer_count: Some(self.layers)"),
            "the view must cover every spec layer"
        );
        assert!(
            view.contains("D2Array"),
            "the view must stay a 2D array so layer indices address it"
        );
    }

    #[test]
    fn upload_layer_writes_the_base_mip_and_the_box_filtered_chain_at_the_requested_layer() {
        let src = include_str!("texture.rs");
        let tests_at = src.find("#[cfg(test)]").expect("tests module exists");
        let production = &src[..tests_at];
        let at = production
            .find("pub fn upload_layer(")
            .expect("upload_layer exists");
        let body = &production[at..];
        let body = &body[..body.find("\n    }\n").expect("method body ends") + 6];

        assert!(
            body.contains("for mip in 0..self.mip_levels"),
            "uploads must write the full spec mip chain"
        );
        assert!(
            body.contains("4 * dim"),
            "each mip upload must use its own row stride"
        );
        assert!(
            body.contains("z: layer"),
            "the upload must target the requested array layer"
        );
        assert!(
            body.contains("downsample_rgba_2x(&current, dim)"),
            "the mip chain must use the shared box-filter rule"
        );
        assert!(
            body.contains("debug_assert") && body.contains("layer < self.layers"),
            "out-of-range layers must be caught in debug builds"
        );
        assert!(
            body.contains("rgba.len()") && body.contains("self.layer_bytes()"),
            "wrong-sized layers must be caught in debug builds"
        );
    }

    #[test]
    fn upload_layers_delegates_to_the_single_layer_rule() {
        let src = include_str!("texture.rs");
        let tests_at = src.find("#[cfg(test)]").expect("tests module exists");
        let production = &src[..tests_at];
        let at = production
            .find("pub fn upload_layers(")
            .expect("upload_layers exists");
        let body = &production[at..];
        let body = &body[..body.find("\n    }\n").expect("method body ends") + 6];

        assert!(
            body.contains("self.upload_layer(texture, queue, layer_idx as u32, base_rgba)"),
            "the bulk path must reuse the per-layer rule"
        );
        assert!(
            !body.contains(concat!("queue.write", "_texture")),
            "the bulk path must not keep a second upload loop"
        );
    }
}
