use std::borrow::Cow;

use glyphon::{Cache, ColorMode, Resolution, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport};

use crate::scene::{Instance, Scene};
use crate::text::TextSystem;

/// Format of the offscreen canvas. Values are sRGB-encoded and blended in
/// sRGB space, which matches how browsers and most toolkits composite.
pub const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const MAX_BLUR_LEVELS: usize = 6;
const BLUR_SLOT: u64 = 256;
const BLUR_SLOTS: u64 = 64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    viewport: [f32; 2],
    scale: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurParams {
    half_pixel: [f32; 2],
    offset: f32,
    _pad: f32,
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

fn make_target(device: &wgpu::Device, label: &str, width: u32, height: u32) -> Target {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: CANVAS_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Target { texture, view, width, height }
}

/// How the window surface wants its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SurfaceTarget {
    pub format: wgpu::TextureFormat,
    /// The compositor expects straight rather than premultiplied alpha.
    pub unpremultiply: bool,
}

/// Draws [`Scene`]s with wgpu.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,

    globals: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    shape_pipeline: wgpu::RenderPipeline,
    backdrop_bgl: wgpu::BindGroupLayout,
    instances: wgpu::Buffer,
    instance_capacity: u64,

    linear: wgpu::Sampler,
    blur_bgl: wgpu::BindGroupLayout,
    blur_down: wgpu::RenderPipeline,
    blur_up: wgpu::RenderPipeline,
    blur_params: wgpu::Buffer,
    blur_levels: Vec<Target>,
    dummy: Target,

    blit_bgl: wgpu::BindGroupLayout,
    blit_shader: wgpu::ShaderModule,
    blit_pipelines: Vec<(SurfaceTarget, wgpu::RenderPipeline)>,

    canvas: Option<Target>,

    text: TextSystem,
    swash: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderers: Vec<TextRenderer>,
    images: crate::image::ImageRenderer,
}

impl Renderer {
    /// `fonts` are the typefaces text is drawn with.
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, fonts: crate::Fonts) -> Self {
        let shape_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("armature shapes"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("shaders/shape.wgsl"))),
        });
        let blur_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("armature blur"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("shaders/blur.wgsl"))),
        });
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("armature blit"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("shaders/blit.wgsl"))),
        });

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("armature globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("armature globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("armature globals"),
            layout: &globals_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }],
        });

        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let samp_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let backdrop_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("armature backdrop"),
            entries: &[tex_entry(0), samp_entry(1)],
        });

        let premultiplied = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };

        let shape_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("armature shapes"),
            bind_group_layouts: &[Some(&globals_bgl), Some(&backdrop_bgl)],
            immediate_size: 0,
        });
        let attrs = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4,
            4 => Float32x4, 5 => Float32x4, 6 => Float32x4
        ];
        let shape_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("armature shapes"),
            layout: Some(&shape_layout),
            vertex: wgpu::VertexState {
                module: &shape_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attrs,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shape_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: CANVAS_FORMAT,
                    blend: Some(premultiplied),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let images = crate::image::ImageRenderer::new(&device, &globals_bgl, premultiplied, CANVAS_FORMAT);

        let instance_capacity = 1024;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("armature instances"),
            size: instance_capacity * std::mem::size_of::<Instance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let linear = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("armature linear"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let blur_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("armature blur"),
            entries: &[
                tex_entry(0),
                samp_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let blur_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("armature blur"),
            bind_group_layouts: &[Some(&blur_bgl)],
            immediate_size: 0,
        });
        let blur_pipeline = |entry: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("armature blur"),
                layout: Some(&blur_layout),
                vertex: wgpu::VertexState {
                    module: &blur_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &blur_shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: CANVAS_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let blur_down = blur_pipeline("fs_down");
        let blur_up = blur_pipeline("fs_up");
        let blur_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("armature blur params"),
            size: BLUR_SLOT * BLUR_SLOTS,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let dummy = make_target(&device, "armature dummy", 1, 1);

        let blit_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("armature blit"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });

        let glyph_cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &glyph_cache);
        let atlas = TextAtlas::with_color_mode(&device, &queue, &glyph_cache, CANVAS_FORMAT, ColorMode::Web);

        Self {
            device,
            queue,
            globals,
            globals_bg,
            shape_pipeline,
            backdrop_bgl,
            instances,
            instance_capacity,
            linear,
            blur_bgl,
            blur_down,
            blur_up,
            blur_params,
            blur_levels: vec![],
            dummy,
            blit_bgl,
            blit_shader,
            blit_pipelines: vec![],
            canvas: None,
            text: TextSystem::new(fonts),
            swash: SwashCache::new(),
            viewport,
            atlas,
            text_renderers: vec![],
            images,
        }
    }

    /// Creates a renderer without a window, for tests and screenshots.
    pub fn headless(fonts: crate::Fonts) -> Result<Self, String> {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
                .map_err(|e| format!("no GPU adapter: {e}"))?;
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .map_err(|e| format!("no GPU device: {e}"))?;
            Ok(Self::new(device, queue, fonts))
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// The font database and text-layout cache.
    pub fn text(&mut self) -> &mut TextSystem {
        &mut self.text
    }

    fn ensure_canvas(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if self.canvas.as_ref().is_some_and(|c| c.width == width && c.height == height) {
            return;
        }
        self.canvas = Some(make_target(&self.device, "armature canvas", width, height));
        self.blur_levels.clear();
        let (mut w, mut h) = (width, height);
        for i in 0..MAX_BLUR_LEVELS {
            w = (w / 2).max(1);
            h = (h / 2).max(1);
            self.blur_levels.push(make_target(&self.device, &format!("armature blur {i}"), w, h));
        }
    }

    /// Renders `scene` into the internal canvas at `width`x`height` physical
    /// pixels with the given scale factor.
    fn draw(&mut self, scene: &Scene, width: u32, height: u32, scale: f32) -> wgpu::CommandEncoder {
        self.ensure_canvas(width, height);
        let scale = scale.max(1e-3);
        let logical = [width as f32 / scale, height as f32 / scale];
        self.queue.write_buffer(
            &self.globals,
            0,
            bytemuck::bytes_of(&Globals { viewport: logical, scale, _pad: 0.0 }),
        );

        // Upload every layer's shapes into one instance buffer.
        let all: Vec<Instance> = scene.layers.iter().flat_map(|l| l.shapes.iter().copied()).collect();
        if all.len() as u64 > self.instance_capacity {
            self.instance_capacity = (all.len() as u64).next_power_of_two();
            self.instances = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("armature instances"),
                size: self.instance_capacity * std::mem::size_of::<Instance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !all.is_empty() {
            self.queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&all));
        }

        self.images.prepare(&self.device, &self.queue, scene);

        // Prepare one text renderer per layer that has text.
        self.viewport.update(&self.queue, Resolution { width, height });
        while self.text_renderers.len() < scene.layers.len() {
            self.text_renderers.push(TextRenderer::new(
                &mut self.atlas,
                &self.device,
                wgpu::MultisampleState::default(),
                None,
            ));
        }
        for (i, layer) in scene.layers.iter().enumerate() {
            if layer.texts.is_empty() {
                continue;
            }
            let areas = layer.texts.iter().map(|t| {
                let bounds = match t.clip {
                    Some(c) => TextBounds {
                        left: (c.x * scale).floor() as i32,
                        top: (c.y * scale).floor() as i32,
                        right: (c.right() * scale).ceil() as i32,
                        bottom: (c.bottom() * scale).ceil() as i32,
                    },
                    None => TextBounds { left: 0, top: 0, right: width as i32, bottom: height as i32 },
                };
                let [r, g, b, a] = t.color.to_rgba8();
                TextArea {
                    buffer: &t.layout.buffer,
                    left: (t.pos.x * scale).round(),
                    top: (t.pos.y * scale).round(),
                    scale,
                    bounds,
                    default_color: glyphon::Color::rgba(r, g, b, a),
                    custom_glyphs: &[],
                }
            });
            if let Err(e) = self.text_renderers[i].prepare(
                &self.device,
                &self.queue,
                &mut self.text.fonts,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash,
            ) {
                eprintln!("armature: text prepare failed: {e:?}");
            }
        }

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("armature frame") });
        let clear = {
            let c = scene.clear;
            wgpu::Color { r: (c.r * c.a) as f64, g: (c.g * c.a) as f64, b: (c.b * c.a) as f64, a: c.a as f64 }
        };

        let mut first = 0u32;
        let mut blur_slot = 0u64;
        let mut backdrop_bg = self.bind_backdrop(None);
        for (i, layer) in scene.layers.iter().enumerate() {
            if layer.backdrop_blur > 0.0 {
                let level = self.blur(&mut encoder, layer.backdrop_blur * scale, &mut blur_slot);
                backdrop_bg = self.bind_backdrop(Some(level));
            }
            let canvas = self.canvas.as_ref().unwrap();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("armature layer"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &canvas.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: if i == 0 { wgpu::LoadOp::Clear(clear) } else { wgpu::LoadOp::Load },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let n = layer.shapes.len() as u32;
            if n > 0 {
                pass.set_pipeline(&self.shape_pipeline);
                pass.set_bind_group(0, &self.globals_bg, &[]);
                pass.set_bind_group(1, &backdrop_bg, &[]);
                pass.set_vertex_buffer(0, self.instances.slice(..));
                pass.draw(0..4, first..first + n);
            }
            first += n;
            self.images.render(&mut pass, &self.globals_bg, i);
            if !layer.texts.is_empty()
                && let Err(e) = self.text_renderers[i].render(&self.atlas, &self.viewport, &mut pass) {
                    eprintln!("armature: text render failed: {e:?}");
                }
        }
        encoder
    }

    fn bind_backdrop(&self, level: Option<usize>) -> wgpu::BindGroup {
        let view = match level {
            Some(l) => &self.blur_levels[l].view,
            None => &self.dummy.view,
        };
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("armature backdrop"),
            layout: &self.backdrop_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.linear) },
            ],
        })
    }

    /// Blurs the current canvas with a dual Kawase filter. Returns the blur
    /// level holding the result.
    fn blur(&self, encoder: &mut wgpu::CommandEncoder, radius_px: f32, slot: &mut u64) -> usize {
        // Each down/up pair roughly doubles the blur radius.
        let levels = (radius_px.max(2.0).log2().floor() as usize).saturating_sub(1).clamp(1, MAX_BLUR_LEVELS);
        let offset = (radius_px / (1u32 << (levels + 1)) as f32).clamp(1.0, 4.0);

        let mut pass = |src: &wgpu::TextureView, src_size: (u32, u32), dst: &Target, down: bool, slot: &mut u64| {
            if *slot >= BLUR_SLOTS {
                return;
            }
            let at = *slot * BLUR_SLOT;
            *slot += 1;
            let params = BlurParams {
                half_pixel: [0.5 / src_size.0 as f32, 0.5 / src_size.1 as f32],
                offset,
                _pad: 0.0,
            };
            self.queue.write_buffer(&self.blur_params, at, bytemuck::bytes_of(&params));
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("armature blur pass"),
                layout: &self.blur_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(src) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.linear) },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.blur_params,
                            offset: at,
                            size: wgpu::BufferSize::new(std::mem::size_of::<BlurParams>() as u64),
                        }),
                    },
                ],
            });
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("armature blur"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_pipeline(if down { &self.blur_down } else { &self.blur_up });
            rp.set_bind_group(0, &bg, &[]);
            rp.draw(0..3, 0..1);
        };

        let canvas = self.canvas.as_ref().unwrap();
        pass(&canvas.view, (canvas.width, canvas.height), &self.blur_levels[0], true, slot);
        for i in 1..levels {
            let src = &self.blur_levels[i - 1];
            pass(&src.view, (src.width, src.height), &self.blur_levels[i], true, slot);
        }
        for i in (1..levels).rev() {
            let src = &self.blur_levels[i];
            pass(&src.view, (src.width, src.height), &self.blur_levels[i - 1], false, slot);
        }
        0
    }

    fn blit_pipeline(&mut self, target: SurfaceTarget) -> usize {
        if let Some(i) = self.blit_pipelines.iter().position(|(t, _)| *t == target) {
            return i;
        }
        let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("armature blit"),
            bind_group_layouts: &[Some(&self.blit_bgl)],
            immediate_size: 0,
        });
        let constants = [
            ("TO_LINEAR", if target.format.is_srgb() { 1.0 } else { 0.0 }),
            ("UNPREMULTIPLY", if target.unpremultiply { 1.0 } else { 0.0 }),
        ];
        let pipeline = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("armature blit"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &self.blit_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &self.blit_shader,
                entry_point: Some("fs"),
                compilation_options: wgpu::PipelineCompilationOptions { constants: &constants, ..Default::default() },
                targets: &[Some(wgpu::ColorTargetState { format: target.format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        self.blit_pipelines.push((target, pipeline));
        self.blit_pipelines.len() - 1
    }

    /// Renders `scene` onto a window surface texture.
    pub fn render(&mut self, scene: &Scene, target: &wgpu::TextureView, surface: SurfaceTarget, width: u32, height: u32, scale: f32) {
        let mut encoder = self.draw(scene, width, height, scale);
        let pi = self.blit_pipeline(surface);
        let canvas = self.canvas.as_ref().unwrap();
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("armature blit"),
            layout: &self.blit_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&canvas.view) }],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("armature blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.blit_pipelines[pi].1);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
        self.atlas.trim();
        self.text.end_frame();
    }

    /// Renders `scene` offscreen and returns straight-alpha RGBA8 pixels.
    pub fn render_to_rgba(&mut self, scene: &Scene, width: u32, height: u32, scale: f32) -> Vec<u8> {
        let mut encoder = self.draw(scene, width, height, scale);
        let canvas = self.canvas.as_ref().unwrap();
        let row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("armature readback"),
            size: (row * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &canvas.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(height) },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(encoder.finish()));
        buffer.map_async(wgpu::MapMode::Read, .., |r| r.expect("map readback buffer"));
        self.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll device");
        let data = buffer.get_mapped_range(..).expect("mapped range");
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            let start = (y * row) as usize;
            for px in data[start..start + (width * 4) as usize].chunks_exact(4) {
                let a = px[3];
                let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8 };
                out.extend_from_slice(&[un(px[0]), un(px[1]), un(px[2]), a]);
            }
        }
        drop(data);
        buffer.unmap();
        self.atlas.trim();
        self.text.end_frame();
        out
    }

    /// Renders `scene` and writes it to a PNG file.
    pub fn save_png(&mut self, scene: &Scene, width: u32, height: u32, scale: f32, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        let pixels = self.render_to_rgba(scene, width, height, scale);
        let file = std::io::BufWriter::new(std::fs::File::create(path)?);
        let mut enc = png::Encoder::new(file, width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        let mut w = enc.write_header().map_err(std::io::Error::other)?;
        w.write_image_data(&pixels).map_err(std::io::Error::other)?;
        Ok(())
    }
}
