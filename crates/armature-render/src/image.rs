//! Images: pixels an app hands to the renderer, and the textures made from them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::geometry::Rect;
use crate::scene::Scene;

static NEXT_SLOT: AtomicU64 = AtomicU64::new(1);

struct Pixels {
    /// Names the texture this image lives in. Frames of one video share a
    /// slot, so each new frame is written into the same texture.
    slot: u64,
    version: u64,
    width: u32,
    height: u32,
    /// Level 0 is the full image; each further level is half the size.
    levels: Vec<Vec<u8>>,
}

/// A picture to draw with [`Scene::image`]: sRGB, straight-alpha RGBA
/// pixels, top row first. Cloning is cheap; the pixels are shared.
#[derive(Clone)]
pub struct Image(Arc<Pixels>);

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Image({}×{}, slot {})", self.0.width, self.0.height, self.0.slot)
    }
}

impl PartialEq for Image {
    fn eq(&self, other: &Self) -> bool {
        self.0.slot == other.0.slot && self.0.version == other.0.version
    }
}

/// Halves an image, weighting colour by alpha so see-through edges do not
/// pick up the colour of fully transparent pixels.
fn halve(src: &[u8], w: u32, h: u32) -> (Vec<u8>, u32, u32) {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = vec![0u8; nw as usize * nh as usize * 4];
    for y in 0..nh {
        for x in 0..nw {
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let sx = (x * 2 + dx).min(w - 1) as usize;
                let sy = (y * 2 + dy).min(h - 1) as usize;
                let p = &src[(sy * w as usize + sx) * 4..][..4];
                let pa = p[3] as u32;
                r += p[0] as u32 * pa;
                g += p[1] as u32 * pa;
                b += p[2] as u32 * pa;
                a += pa;
            }
            let o = &mut out[(y as usize * nw as usize + x as usize) * 4..][..4];
            // Fully transparent blocks keep a colour of zero.
            let average = |sum: u32| (sum + a / 2).checked_div(a).unwrap_or(0) as u8;
            o[0] = average(r);
            o[1] = average(g);
            o[2] = average(b);
            o[3] = ((a + 2) / 4) as u8;
        }
    }
    (out, nw, nh)
}

impl Image {
    /// An image that stays sharp when drawn smaller than its size. Building
    /// the smaller copies takes a moment for large photos, so call this off
    /// the main thread. `rgba` must hold `width × height × 4` bytes.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        assert_eq!(rgba.len(), width as usize * height as usize * 4, "image data does not match its size");
        let mut levels = vec![rgba];
        let (mut w, mut h) = (width, height);
        while w > 1 || h > 1 {
            let (next, nw, nh) = halve(levels.last().expect("starts with level 0"), w, h);
            levels.push(next);
            (w, h) = (nw, nh);
        }
        Self(Arc::new(Pixels { slot: NEXT_SLOT.fetch_add(1, Ordering::Relaxed), version: 0, width, height, levels }))
    }

    /// An image with no smaller copies: quick to make, for video frames and
    /// pictures drawn near their own size.
    pub fn frame(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        assert_eq!(rgba.len(), width as usize * height as usize * 4, "image data does not match its size");
        Self(Arc::new(Pixels { slot: NEXT_SLOT.fetch_add(1, Ordering::Relaxed), version: 0, width, height, levels: vec![rgba] }))
    }

    /// The next frame of the same picture, such as a video. It reuses this
    /// image's texture when the size is unchanged.
    pub fn next_frame(&self, width: u32, height: u32, rgba: Vec<u8>) -> Self {
        assert_eq!(rgba.len(), width as usize * height as usize * 4, "image data does not match its size");
        Self(Arc::new(Pixels { slot: self.0.slot, version: self.0.version + 1, width, height, levels: vec![rgba] }))
    }

    pub fn width(&self) -> u32 {
        self.0.width
    }

    pub fn height(&self) -> u32 {
        self.0.height
    }
}

pub(crate) struct ImageItem {
    pub image: Image,
    pub rect: Rect,
    pub uv: [f32; 4],
    pub clip: [f32; 4],
    pub opacity: f32,
    /// Quarter turns clockwise.
    pub turns: u8,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    rect: [f32; 4],
    uv: [f32; 4],
    clip: [f32; 4],
    params: [f32; 4],
}

struct Texture {
    bind_group: wgpu::BindGroup,
    texture: wgpu::Texture,
    version: u64,
    size: (u32, u32),
    last_used: u64,
}

/// Textures unused for this many frames are freed. Photos are large, so
/// this is short; drawing one again just uploads it again.
const KEEP_FRAMES: u64 = 3;

/// Uploads images and draws them. Owned by the renderer.
pub(crate) struct ImageRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    instances: wgpu::Buffer,
    capacity: u64,
    textures: HashMap<u64, Texture>,
    frame: u64,
    max_side: u32,
    /// For each layer of the scene being drawn: the texture slot and
    /// instance index of each image, in drawing order.
    draws: Vec<Vec<(u64, u32)>>,
}

impl ImageRenderer {
    pub fn new(device: &wgpu::Device, globals_layout: &wgpu::BindGroupLayout, blend: wgpu::BlendState, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("armature images"), source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("shaders/image.wgsl"))) });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("armature image"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("armature images"), bind_group_layouts: &[Some(globals_layout), Some(&layout)], immediate_size: 0 });
        let attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("armature images"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Instance>() as u64, step_mode: wgpu::VertexStepMode::Instance, attributes: &attrs })],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(blend), write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        // Trilinear: blends between the half-size copies when drawn small.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("armature image"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let capacity = 16;
        let instances = device.create_buffer(&wgpu::BufferDescriptor { label: Some("armature image instances"), size: capacity * std::mem::size_of::<Instance>() as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        Self { pipeline, layout, sampler, instances, capacity, textures: HashMap::new(), frame: 0, max_side: device.limits().max_texture_dimension_2d, draws: vec![] }
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, image: &Image) {
        let p = &image.0;
        // Photos larger than the GPU allows start from a smaller copy.
        let mut first = 0;
        let (mut w, mut h) = (p.width, p.height);
        while (w > self.max_side || h > self.max_side) && first + 1 < p.levels.len() {
            first += 1;
            (w, h) = ((w / 2).max(1), (h / 2).max(1));
        }
        if w > self.max_side || h > self.max_side || w == 0 || h == 0 {
            return;
        }
        let levels = &p.levels[first..];
        let reuse = self.textures.get(&p.slot).is_some_and(|t| t.size == (w, h) && t.texture.mip_level_count() == levels.len() as u32);
        if !reuse {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("armature image"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: levels.len() as u32,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // Not an sRGB format: the canvas blends sRGB-encoded values as they are.
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("armature image"),
                layout: &self.layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) }],
            });
            self.textures.insert(p.slot, Texture { bind_group, texture, version: u64::MAX, size: (w, h), last_used: self.frame });
        }
        let entry = self.textures.get_mut(&p.slot).expect("inserted above");
        if entry.version == p.version {
            return;
        }
        entry.version = p.version;
        let (mut lw, mut lh) = (w, h);
        for (i, data) in levels.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &entry.texture, mip_level: i as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(lw * 4), rows_per_image: Some(lh) },
                wgpu::Extent3d { width: lw, height: lh, depth_or_array_layers: 1 },
            );
            (lw, lh) = ((lw / 2).max(1), (lh / 2).max(1));
        }
    }

    /// Uploads what the scene draws and frees textures it has stopped drawing.
    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, scene: &Scene) {
        self.frame += 1;
        self.draws.clear();
        let mut instances = Vec::new();
        for layer in &scene.layers {
            let mut list = Vec::new();
            for item in &layer.images {
                self.upload(device, queue, &item.image);
                let slot = item.image.0.slot;
                let Some(texture) = self.textures.get_mut(&slot) else { continue };
                texture.last_used = self.frame;
                list.push((slot, instances.len() as u32));
                instances.push(Instance { rect: [item.rect.x, item.rect.y, item.rect.w, item.rect.h], uv: item.uv, clip: item.clip, params: [item.opacity, (item.turns % 4) as f32, 0.0, 0.0] });
            }
            self.draws.push(list);
        }
        if instances.len() as u64 > self.capacity {
            self.capacity = (instances.len() as u64).next_power_of_two();
            self.instances = device.create_buffer(&wgpu::BufferDescriptor { label: Some("armature image instances"), size: self.capacity * std::mem::size_of::<Instance>() as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        }
        if !instances.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&instances));
        }
        let frame = self.frame;
        self.textures.retain(|_, t| frame - t.last_used < KEEP_FRAMES);
    }

    /// Draws one layer's images. Call between its shapes and its text.
    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>, globals: &wgpu::BindGroup, layer: usize) {
        let Some(list) = self.draws.get(layer).filter(|l| !l.is_empty()) else { return };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, globals, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        for (slot, index) in list {
            if let Some(texture) = self.textures.get(slot) {
                pass.set_bind_group(1, &texture.bind_group, &[]);
                pass.draw(0..4, *index..*index + 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_half_size_copies_down_to_one_pixel() {
        let image = Image::new(8, 4, vec![255; 8 * 4 * 4]);
        let sizes: Vec<usize> = image.0.levels.iter().map(|l| l.len() / 4).collect();
        assert_eq!(sizes, [32, 8, 2, 1]);
        assert!(image.0.levels.iter().all(|l| l.iter().all(|b| *b == 255)), "a solid image stays solid");
    }

    #[test]
    fn transparent_pixels_do_not_tint_their_neighbours() {
        // One opaque red pixel beside three fully transparent green ones.
        let px = [255, 0, 0, 255, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0];
        let (half, w, h) = halve(&px, 2, 2);
        assert_eq!((w, h), (1, 1));
        assert_eq!(half, [255, 0, 0, 64], "still red, at a quarter of the coverage");
    }

    #[test]
    fn frames_of_one_video_share_a_texture() {
        let a = Image::frame(2, 2, vec![0; 16]);
        let b = a.next_frame(2, 2, vec![9; 16]);
        assert_eq!(a.0.slot, b.0.slot);
        assert_ne!(a, b, "but they are different pictures");
        assert_ne!(Image::frame(2, 2, vec![0; 16]).0.slot, a.0.slot);
    }
}
