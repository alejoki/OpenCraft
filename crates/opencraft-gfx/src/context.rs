//! wgpu render context over a host-created surface.
//!
//! The host owns the window, the `wgpu::Instance` and the `Surface`; this
//! module owns the adapter, device, queue, surface configuration and depth
//! attachment shared by every render pass.

pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

pub struct RenderContext {
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    pub depth_view: wgpu::TextureView,
    /// Present modes the surface advertises. wgpu panics when a
    /// configuration requests an unsupported mode, so every runtime present
    /// mode change is clamped to this list.
    supported_present_modes: Vec<wgpu::PresentMode>,
}

/// The present mode to request for a vsync state, limited to the modes the
/// surface actually supports.
///
/// Vsync on prefers `Mailbox` over `Fifo`: both are v-synchronized (the
/// presentation engine updates at vblank and never tears), but `Mailbox`
/// does not block `get_current_texture`. That matters on KDE Wayland with
/// direct scanout, where a `Fifo` swapchain can stall to half the display
/// refresh (30 fps on a 60 Hz panel) even when the frame takes only a few
/// milliseconds. The frame loop pairs `Mailbox` with an explicit
/// refresh-rate limiter (see the game's frame loop) so rendering still
/// runs at the display rate instead of spinning uncapped. `Fifo` remains
/// the always-supported fallback.
///
/// Vsync off prefers uncapped `Immediate`, then `Mailbox`, then `Fifo`
/// (which is the only mode guaranteed on every backend).
fn preferred_present_mode(vsync: bool, supported: &[wgpu::PresentMode]) -> wgpu::PresentMode {
    let has = |mode: wgpu::PresentMode| supported.contains(&mode);
    if vsync {
        if has(wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        }
    } else if has(wgpu::PresentMode::Immediate) {
        wgpu::PresentMode::Immediate
    } else if has(wgpu::PresentMode::Mailbox) {
        wgpu::PresentMode::Mailbox
    } else {
        wgpu::PresentMode::Fifo
    }
}

fn create_depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth_texture"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

impl RenderContext {
    /// Build the GPU context over a surface the host created from its own
    /// window. The host keeps ownership of the window, instance and surface;
    /// the adapter, device, queue, surface configuration and depth
    /// attachment are owned here.
    pub fn new(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        vsync: bool,
    ) -> Self {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("Failed to find suitable GPU adapter");

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("voxel_device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
            },
            None,
        ))
        .expect("Failed to create wgpu device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        // Select the requested vsync mode up front so no post-construction
        // reconfigure is needed.
        let present_mode = preferred_present_mode(vsync, &caps.present_modes);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let depth_view = create_depth_view(&device, config.width, config.height);

        Self {
            surface,
            device,
            queue,
            config,
            depth_view,
            supported_present_modes: caps.present_modes,
        }
    }

    /// Apply the requested vsync state to the surface. The mode is chosen
    /// from the adapter's advertised present modes (see
    /// [`preferred_present_mode`]), then the surface is reconfigured with the
    /// same set-present-mode-then-configure mechanism the resize path uses.
    pub fn set_vsync(&mut self, vsync: bool) {
        self.config.present_mode = preferred_present_mode(vsync, &self.supported_present_modes);
        self.surface.configure(&self.device, &self.config);
    }

    /// Reconfigure the surface and rebuild the depth attachment for a new
    /// window size. Zero dimensions are ignored: a minimized window can
    /// report them and a zero-sized surface configuration is invalid.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
            self.depth_view = create_depth_view(&self.device, width, height);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::PresentMode;

    #[test]
    fn vsync_prefers_mailbox_and_falls_back_to_fifo() {
        assert_eq!(
            preferred_present_mode(
                true,
                &[
                    PresentMode::Mailbox,
                    PresentMode::Fifo,
                    PresentMode::Immediate
                ]
            ),
            PresentMode::Mailbox
        );
        assert_eq!(
            preferred_present_mode(true, &[PresentMode::Fifo, PresentMode::Immediate]),
            PresentMode::Fifo,
            "Fifo is the always-supported vsync fallback"
        );
    }

    #[test]
    fn no_vsync_prefers_immediate_then_mailbox_then_fifo() {
        assert_eq!(
            preferred_present_mode(
                false,
                &[
                    PresentMode::Mailbox,
                    PresentMode::Fifo,
                    PresentMode::Immediate
                ]
            ),
            PresentMode::Immediate
        );
        assert_eq!(
            preferred_present_mode(false, &[PresentMode::Mailbox, PresentMode::Fifo]),
            PresentMode::Mailbox
        );
        assert_eq!(
            preferred_present_mode(false, &[PresentMode::Fifo]),
            PresentMode::Fifo
        );
    }
}
