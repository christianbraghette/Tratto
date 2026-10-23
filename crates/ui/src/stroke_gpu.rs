// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Strokes drawn by our own wgpu pipelines, with their triangles kept on
//! the GPU.
//!
//! The ink of a page is a lot of triangles, and through egui's painter
//! every one of them would be checked, copied and uploaded again on every
//! frame. Here a page's strokes are a [`Batch`]: its triangles are uploaded
//! once, again only when they change (just the new ones when strokes were
//! only added), and drawn each frame at the page's current position and,
//! while zooming, scaled from the zoom they were made at: both are uniforms. They are drawn through paint callbacks, inside egui's render
//! pass and in paint order like any other shape, clipped like the painter.
//!
//! Two blends:
//!
//! * [`Blend::Over`], ink: exactly what egui does with a mesh (premultiplied
//!   colours, "over"), so ink looks the same as before.
//! * [`Blend::Darken`], highlighters: painted in the colour they leave on the
//!   paper, every pixel keeps the darker of that colour and what is already
//!   there (`min` per channel, the PDF Darken blend mode). Overlapping
//!   highlighter strokes never get darker, and text, ink and the pages of a
//!   PDF under them stay fully visible. Antialiased edges fade to white,
//!   which `min` leaves unchanged.
//!
//! Without the pipelines (they could not be created) everything is painted
//! as plain egui meshes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{PaintCallbackInfo, Painter, Shape, Vec2, epaint::Mesh};
use eframe::egui_wgpu::{self, CallbackResources, CallbackTrait, ScreenDescriptor, wgpu};
use wgpu::util::DeviceExt;

/// The pipelines are installed (wgpu renderer).
static INSTALLED: AtomicBool = AtomicBool::new(false);
static NEXT_BATCH: AtomicU64 = AtomicU64::new(1);
/// Batches dropped since the last frame: their GPU buffers are freed.
static DROPPED: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// egui's vertex: pos (2 x f32), uv (2 x f32), colour (4 x u8, premultiplied).
const VERTEX_SIZE: u64 = 20;
/// Smallest GPU buffer of a batch, in bytes.
const MIN_BUFFER: u64 = 64 * 1024;

const SHADER: &str = r#"
struct Locals {
    // The screen in points, where the batch's (0, 0) is on it, and how much
    // larger than made the batch is drawn (zooming).
    screen: vec2<f32>,
    offset: vec2<f32>,
    scale: f32,
};
@group(0) @binding(0) var<uniform> locals: Locals;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> VOut {
    let p = pos * locals.scale + locals.offset;
    var out: VOut;
    out.pos = vec4<f32>(2.0 * p.x / locals.screen.x - 1.0, 1.0 - 2.0 * p.y / locals.screen.y, 0.0, 1.0);
    out.color = color;
    return out;
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c < vec3<f32>(0.04045));
}

// Ink: egui's own output for a mesh on its white texel.
@fragment
fn fs_over_gamma(in: VOut) -> @location(0) vec4<f32> {
    return in.color;
}

@fragment
fn fs_over_linear(in: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(to_linear(in.color.rgb), in.color.a);
}

// Highlighters: the (premultiplied) vertex colour over white, which is what
// `min` compares with, so transparent edges change nothing.
fn over_white(c: vec4<f32>) -> vec3<f32> {
    return c.rgb + vec3<f32>(1.0 - c.a);
}

@fragment
fn fs_darken_gamma(in: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(over_white(in.color), 1.0);
}

@fragment
fn fs_darken_linear(in: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(to_linear(over_white(in.color)), 1.0);
}
"#;

/// How triangles mix with what is under them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    /// Ink: painted over.
    Over,
    /// Highlighters: the darker of both, per channel.
    Darken,
}

struct Resources {
    over: wgpu::RenderPipeline,
    darken: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// The batches on the GPU, by id.
    batches: HashMap<u64, GpuBatch>,
}

impl Resources {
    fn pipeline(&self, blend: Blend) -> &wgpu::RenderPipeline {
        match blend {
            Blend::Over => &self.over,
            Blend::Darken => &self.darken,
        }
    }
}

/// A uniform with the screen size, an offset and a scale, and its bind group.
struct Locals {
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl Locals {
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stroke locals"),
            // `Locals` of the shader: 5 floats, rounded up to its alignment.
            size: 24,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stroke locals"),
            layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
        });
        Self { buffer, bind_group }
    }

    fn write(&self, queue: &wgpu::Queue, screen: &ScreenDescriptor, offset: Vec2, scale: f32) {
        let ppp = screen.pixels_per_point;
        let [w, h] = screen.size_in_pixels.map(|v| v as f32 / ppp);
        let data = [w, h, offset.x, offset.y, scale, 0.0];
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&data));
    }
}

/// Creates the pipelines; call once with eframe's wgpu state. `msaa` must be
/// the sample count of egui's render pass.
pub fn install(render_state: &egui_wgpu::RenderState, msaa: u32) {
    let device = &render_state.device;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("strokes"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("stroke locals"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("strokes"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let vertex_layout = wgpu::VertexBufferLayout {
        array_stride: VERTEX_SIZE,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Unorm8x4, offset: 16, shader_location: 1 },
        ],
    };
    let format = render_state.target_format;
    // An sRGB target expects linear output (egui does the same).
    let linear = format.is_srgb();
    let pipeline = |label: &str, fragment: &str, blend: wgpu::BlendState| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(vertex_layout.clone())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(fragment),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState { count: msaa.max(1), ..Default::default() },
            multiview_mask: None,
            cache: None,
        })
    };
    // egui's blend.
    let over = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
    };
    let min = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Min,
    };
    let over = pipeline("ink", if linear { "fs_over_linear" } else { "fs_over_gamma" }, over);
    let darken = pipeline(
        "highlighter",
        if linear { "fs_darken_linear" } else { "fs_darken_gamma" },
        wgpu::BlendState { color: min, alpha: min },
    );
    let resources = Resources { over, darken, layout, batches: HashMap::new() };
    render_state.renderer.write().callback_resources.insert(resources);
    INSTALLED.store(true, Ordering::Relaxed);
}

/// Benchmarks: paint as with the pipelines, without a GPU.
#[cfg(test)]
pub fn pretend_installed() {
    INSTALLED.store(true, Ordering::Relaxed);
}

// ---------------------------------------------------------------- batches

/// Triangles kept on the GPU: uploaded when they change, drawn at any
/// offset. Built at `page space * scale` (see `render::StrokeCache`).
pub struct Batch {
    id: u64,
    mesh: Arc<Mesh>,
    /// Changes when the mesh is replaced; within one generation only its end
    /// changes (see [`Batch::append`], [`Batch::edit`]), so only that is uploaded.
    generation: u64,
    /// Vertices and indices unchanged at the start of the mesh since it was
    /// last uploaded (`usize::MAX`: all of them); the GPU keeps those.
    unchanged: Arc<[AtomicUsize; 2]>,
}

impl Default for Batch {
    fn default() -> Self {
        Self {
            id: NEXT_BATCH.fetch_add(1, Ordering::Relaxed),
            mesh: Arc::default(),
            generation: 0,
            unchanged: Arc::new([AtomicUsize::new(usize::MAX), AtomicUsize::new(usize::MAX)]),
        }
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        DROPPED.lock().unwrap_or_else(|e| e.into_inner()).push(self.id);
    }
}

impl Batch {
    pub fn mesh(&self) -> &Mesh {
        &self.mesh
    }

    /// Adds triangles after the ones already there.
    pub fn append(&mut self, add: impl FnOnce(&mut Mesh)) {
        add(Arc::make_mut(&mut self.mesh));
    }

    /// Changes the triangles from `vertices` and `indices` on (those before
    /// are left as they are): only the rest is uploaded again.
    pub fn edit(&mut self, vertices: usize, indices: usize, change: impl FnOnce(&mut Mesh)) {
        self.unchanged[0].fetch_min(vertices, Ordering::Relaxed);
        self.unchanged[1].fetch_min(indices, Ordering::Relaxed);
        change(Arc::make_mut(&mut self.mesh));
    }

    /// Replaces all the triangles.
    pub fn replace(&mut self, mesh: Mesh) {
        self.mesh = Arc::new(mesh);
        self.generation += 1;
    }
}

/// A batch on the GPU.
struct GpuBatch {
    generation: u64,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    /// What of the batch's mesh is uploaded.
    vertex_count: usize,
    index_count: usize,
    locals: Locals,
}

fn buffer(device: &wgpu::Device, label: &str, usage: wgpu::BufferUsages, needed: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: needed.next_power_of_two().max(MIN_BUFFER),
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

impl GpuBatch {
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, mesh: &Mesh, generation: u64) -> Self {
        let vertex_bytes = mesh.vertices.len() as u64 * VERTEX_SIZE;
        let index_bytes = mesh.indices.len() as u64 * 4;
        Self {
            generation,
            vertices: buffer(device, "stroke vertices", wgpu::BufferUsages::VERTEX, vertex_bytes),
            indices: buffer(device, "stroke indices", wgpu::BufferUsages::INDEX, index_bytes),
            vertex_count: 0,
            index_count: 0,
            locals: Locals::new(device, layout),
        }
    }

    /// Uploads what the GPU does not have of `mesh` yet.
    fn sync(&mut self, queue: &wgpu::Queue, mesh: &Mesh) {
        let (vertices, indices) = (&mesh.vertices[self.vertex_count..], &mesh.indices[self.index_count..]);
        if !vertices.is_empty() {
            queue.write_buffer(&self.vertices, self.vertex_count as u64 * VERTEX_SIZE, bytemuck::cast_slice(vertices));
        }
        if !indices.is_empty() {
            queue.write_buffer(&self.indices, self.index_count as u64 * 4, bytemuck::cast_slice(indices));
        }
        (self.vertex_count, self.index_count) = (mesh.vertices.len(), mesh.indices.len());
    }

    fn fits(&self, mesh: &Mesh) -> bool {
        mesh.vertices.len() as u64 * VERTEX_SIZE <= self.vertices.size()
            && mesh.indices.len() as u64 * 4 <= self.indices.size()
    }
}

/// Draws a batch, its (0, 0) at `offset` (screen points), `scale` times
/// as large as made.
struct DrawBatch {
    id: u64,
    generation: u64,
    mesh: Arc<Mesh>,
    /// `Batch::unchanged`.
    unchanged: Arc<[AtomicUsize; 2]>,
    offset: Vec2,
    scale: f32,
    blend: Blend,
}

impl CallbackTrait for DrawBatch {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(res) = resources.get_mut::<Resources>() else { return Vec::new() };
        for id in std::mem::take(&mut *DROPPED.lock().unwrap_or_else(|e| e.into_inner())) {
            res.batches.remove(&id);
        }
        let mesh = &*self.mesh;
        let unchanged = [0, 1].map(|i| self.unchanged[i].swap(usize::MAX, Ordering::Relaxed));
        if let Some(gpu) = res.batches.get_mut(&self.id) {
            gpu.vertex_count = gpu.vertex_count.min(unchanged[0]);
            gpu.index_count = gpu.index_count.min(unchanged[1]);
        }
        let gpu = match res.batches.get_mut(&self.id) {
            // Changed at its end within the same generation: just that part.
            Some(gpu)
                if gpu.generation == self.generation
                    && gpu.vertex_count <= mesh.vertices.len()
                    && gpu.index_count <= mesh.indices.len()
                    && gpu.fits(mesh) =>
            {
                gpu
            }
            _ => {
                let gpu = GpuBatch::new(device, &res.layout, mesh, self.generation);
                res.batches.insert(self.id, gpu);
                res.batches.get_mut(&self.id).expect("just inserted")
            }
        };
        gpu.sync(queue, mesh);
        gpu.locals.write(queue, screen, self.offset, self.scale);
        Vec::new()
    }

    fn paint(&self, _info: PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &CallbackResources) {
        let Some(res) = resources.get::<Resources>() else { return };
        let Some(gpu) = res.batches.get(&self.id) else { return };
        pass.set_pipeline(res.pipeline(self.blend));
        pass.set_bind_group(0, &gpu.locals.bind_group, &[]);
        pass.set_vertex_buffer(0, gpu.vertices.slice(..));
        pass.set_index_buffer(gpu.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..gpu.index_count as u32, 0, 0..1);
    }
}

/// Paints `batch` with its (0, 0) at `offset` (screen points), `scale`
/// times as large as it was made (`1.0` but while zooming), clipped like
/// `painter`. Uploads it first if it changed. A batch is drawn once per frame.
pub fn paint_batch(painter: &Painter, batch: &Batch, offset: Vec2, scale: f32, blend: Blend) {
    if batch.mesh.is_empty() {
        return;
    }
    if !INSTALLED.load(Ordering::Relaxed) {
        let mut mesh = (*batch.mesh).clone();
        for v in &mut mesh.vertices {
            v.pos = (v.pos.to_vec2() * scale + offset).to_pos2();
        }
        painter.add(Shape::mesh(mesh));
        return;
    }
    let (id, generation, mesh, unchanged) = (batch.id, batch.generation, batch.mesh.clone(), batch.unchanged.clone());
    let callback = DrawBatch { id, generation, mesh, unchanged, offset, scale, blend };
    // The whole screen: the vertex shader maps screen points; the painter's
    // clip rect becomes the scissor.
    painter.add(egui_wgpu::Callback::new_paint_callback(painter.ctx().viewport_rect(), callback));
}

// -------------------------------------------------------------- one frame

/// Triangles drawn once (screen points), uploaded in `prepare`.
struct DrawMesh {
    mesh: Mesh,
    blend: Blend,
    gpu: Mutex<Option<(wgpu::Buffer, wgpu::Buffer, Locals)>>,
}

impl CallbackTrait for DrawMesh {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(res) = resources.get::<Resources>() else { return Vec::new() };
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("stroke vertices"),
            contents: bytemuck::cast_slice(&self.mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("stroke indices"),
            contents: bytemuck::cast_slice(&self.mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let locals = Locals::new(device, &res.layout);
        locals.write(queue, screen, Vec2::ZERO, 1.0);
        *self.gpu.lock().unwrap_or_else(|e| e.into_inner()) = Some((vertices, indices, locals));
        Vec::new()
    }

    fn paint(&self, _info: PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &CallbackResources) {
        let Some(res) = resources.get::<Resources>() else { return };
        let gpu = self.gpu.lock().unwrap_or_else(|e| e.into_inner());
        let Some((vertices, indices, locals)) = gpu.as_ref() else { return };
        pass.set_pipeline(res.pipeline(self.blend));
        pass.set_bind_group(0, &locals.bind_group, &[]);
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.mesh.indices.len() as u32, 0, 0..1);
    }
}

/// Paints triangles for this frame only (screen points, colours as from
/// `render::paint_rgba`), clipped like `painter`.
pub fn paint_mesh(painter: &Painter, mesh: Mesh, blend: Blend) {
    if mesh.is_empty() {
        return;
    }
    if !INSTALLED.load(Ordering::Relaxed) {
        painter.add(Shape::mesh(mesh));
        return;
    }
    let callback = DrawMesh { mesh, blend, gpu: Mutex::new(None) };
    painter.add(egui_wgpu::Callback::new_paint_callback(painter.ctx().viewport_rect(), callback));
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::epaint::Vertex;
    use eframe::egui::{Color32, pos2, vec2};

    /// A GPU, egui's renderer with our pipelines, and a 4 x 1 target.
    struct Gpu {
        device: wgpu::Device,
        queue: wgpu::Queue,
        state: egui_wgpu::RenderState,
        target: wgpu::Texture,
    }

    impl Gpu {
        /// `None` without a GPU (the test is skipped).
        fn new() -> Option<Self> {
            let instance = wgpu::Instance::default();
            let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
            let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
            let format = wgpu::TextureFormat::Rgba8Unorm;
            let renderer = egui_wgpu::Renderer::new(&device, format, Default::default());
            let state = egui_wgpu::RenderState {
                adapter,
                available_adapters: Vec::new(),
                instance,
                device: device.clone(),
                queue: queue.clone(),
                target_format: format,
                renderer: Arc::new(eframe::egui::mutex::RwLock::new(renderer)),
                surface_config: egui_wgpu::SurfaceConfig::LOW_LATENCY,
            };
            install(&state, 1);
            let target = device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: 4, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            Some(Self { device, queue, state, target })
        }

        /// Fills the target with `start`, runs the callbacks as egui would and
        /// returns the pixels (RGB).
        fn draw(&self, start: [u8; 16], callbacks: &[&dyn CallbackTrait]) -> Vec<[u8; 3]> {
            let errors = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let size = wgpu::Extent3d { width: 4, height: 1, depth_or_array_layers: 1 };
            self.queue.write_texture(
                self.target.as_image_copy(),
                &start,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(16), rows_per_image: None },
                size,
            );
            let screen = ScreenDescriptor { size_in_pixels: [4, 1], pixels_per_point: 1.0 };
            let mut encoder = self.device.create_command_encoder(&Default::default());
            {
                let mut renderer = self.state.renderer.write();
                for c in callbacks {
                    c.prepare(&self.device, &self.queue, &screen, &mut encoder, &mut renderer.callback_resources);
                }
            }
            let view = self.target.create_view(&Default::default());
            {
                let mut pass = encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    })
                    .forget_lifetime();
                let renderer = self.state.renderer.read();
                let rect = eframe::egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(4.0, 1.0));
                for c in callbacks {
                    let info = PaintCallbackInfo {
                        viewport: rect,
                        clip_rect: rect,
                        pixels_per_point: 1.0,
                        screen_size_px: [4, 1],
                    };
                    c.paint(info, &mut pass, &renderer.callback_resources);
                }
            }
            // Read the pixels back (rows are padded to 256 bytes).
            let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 256,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                self.target.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: None },
                },
                size,
            );
            self.queue.submit([encoder.finish()]);
            readback.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
            self.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let px: Vec<u8> = readback.slice(..16).get_mapped_range().unwrap().to_vec();
            assert!(pollster::block_on(errors.pop()).is_none(), "wgpu validation error");
            px.chunks(4).map(|p| [p[0], p[1], p[2]]).collect()
        }

        fn batches(&self) -> usize {
            self.state.renderer.read().callback_resources.get::<Resources>().unwrap().batches.len()
        }
    }

    /// A quad from x0 to x1 (screen points), full height.
    fn quad(mesh: &mut Mesh, x0: f32, x1: f32, color: Color32) {
        let base = mesh.vertices.len() as u32;
        for (x, y) in [(x0, 0.0), (x1, 0.0), (x1, 1.0), (x0, 1.0)] {
            mesh.vertices.push(Vertex { pos: pos2(x, y), uv: pos2(0.0, 0.0), color });
        }
        mesh.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    const WHITE_BLACK_WHITE_GREY: [u8; 16] = [255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255, 128, 128, 128, 255];

    /// The pipelines on a real GPU (skipped without one). One test: freed
    /// batches are process-wide, other renderers would take them.
    #[test]
    fn strokes_on_the_gpu() {
        let Some(gpu) = Gpu::new() else { return eprintln!("no GPU adapter: skipped") };
        highlighters_darken_without_stacking(&gpu);
        ink_is_painted_over(&gpu);
        batches_are_uploaded_when_they_change(&gpu);
    }

    /// Two overlapping highlighter quads over white, black, white and grey pixels.
    fn highlighters_darken_without_stacking(gpu: &Gpu) {
        let yellow = Color32::from_rgb(255, 245, 160);
        let mut mesh = Mesh::default();
        quad(&mut mesh, 0.0, 4.0, yellow);
        quad(&mut mesh, 0.0, 4.0, yellow);
        let draw = DrawMesh { mesh, blend: Blend::Darken, gpu: Mutex::new(None) };
        let px = gpu.draw(WHITE_BLACK_WHITE_GREY, &[&draw]);
        // White paper takes the highlighter's colour, once (no stacking);
        // black ink stays black; grey keeps its darker channels.
        assert_eq!(px, [[255, 245, 160], [0, 0, 0], [255, 245, 160], [128, 128, 128]]);
    }

    /// Ink is painted over, like egui paints a mesh: opaque covers, half
    /// transparent (premultiplied) mixes.
    fn ink_is_painted_over(gpu: &Gpu) {
        let mut mesh = Mesh::default();
        quad(&mut mesh, 0.0, 2.0, Color32::from_rgb(10, 20, 30));
        quad(&mut mesh, 2.0, 3.0, Color32::from_rgba_premultiplied(0, 0, 0, 128));
        let draw = DrawMesh { mesh, blend: Blend::Over, gpu: Mutex::new(None) };
        let px = gpu.draw(WHITE_BLACK_WHITE_GREY, &[&draw]);
        assert_eq!(px, [[10, 20, 30], [10, 20, 30], [127, 127, 127], [128, 128, 128]]);
    }

    /// A batch stays on the GPU: drawn at an offset, then grown (only the new
    /// triangles are sent), replaced, and freed once dropped.
    fn batches_are_uploaded_when_they_change(gpu: &Gpu) {
        let red = Color32::from_rgb(255, 0, 0);
        let blue = Color32::from_rgb(0, 0, 255);
        let mut batch = Batch::default();
        batch.append(|m| quad(m, 0.0, 1.0, red));
        let call = |b: &Batch, x: f32| DrawBatch {
            id: b.id,
            generation: b.generation,
            mesh: b.mesh.clone(),
            unchanged: b.unchanged.clone(),
            offset: vec2(x, 0.0),
            scale: 1.0,
            blend: Blend::Over,
        };
        let px = gpu.draw(WHITE_BLACK_WHITE_GREY, &[&call(&batch, 1.0)]);
        assert_eq!(px[1], [255, 0, 0], "drawn at its offset");
        assert_eq!(px[0], [255, 255, 255]);
        // Zooming: drawn larger than made, from its offset.
        let zoomed = DrawBatch { scale: 2.0, ..call(&batch, 1.0) };
        let px = gpu.draw(WHITE_BLACK_WHITE_GREY, &[&zoomed]);
        assert_eq!(px[..3], [[255, 255, 255], [255, 0, 0], [255, 0, 0]], "drawn scaled");

        batch.append(|m| quad(m, 1.0, 2.0, blue));
        let px = gpu.draw(WHITE_BLACK_WHITE_GREY, &[&call(&batch, 0.0)]);
        assert_eq!(px[..2], [[255, 0, 0], [0, 0, 255]], "the appended part was uploaded");
        let res = gpu.state.renderer.read();
        let gpu_batch = &res.callback_resources.get::<Resources>().unwrap().batches[&batch.id];
        assert_eq!((gpu_batch.vertex_count, gpu_batch.generation), (8, 0));
        drop(res);

        // Its end changed (a stroke erased, another written): only that part
        // is sent again.
        batch.edit(4, 6, |m| {
            m.vertices.truncate(4);
            m.indices.truncate(6);
            quad(m, 2.0, 3.0, red);
        });
        let px = gpu.draw(WHITE_BLACK_WHITE_GREY, &[&call(&batch, 0.0)]);
        assert_eq!(px, [[255, 0, 0], [0, 0, 0], [255, 0, 0], [128, 128, 128]], "its end was uploaded again");
        // Only made shorter: nothing to send, fewer triangles drawn.
        batch.edit(4, 6, |m| {
            m.vertices.truncate(4);
            m.indices.truncate(6);
        });
        let px = gpu.draw(WHITE_BLACK_WHITE_GREY, &[&call(&batch, 0.0)]);
        assert_eq!(px, [[255, 0, 0], [0, 0, 0], [255, 255, 255], [128, 128, 128]], "made shorter");
        let res = gpu.state.renderer.read();
        let gpu_batch = &res.callback_resources.get::<Resources>().unwrap().batches[&batch.id];
        assert_eq!((gpu_batch.vertex_count, gpu_batch.index_count, gpu_batch.generation), (4, 6, 0));
        drop(res);

        let mut replaced = Mesh::default();
        quad(&mut replaced, 3.0, 4.0, blue);
        batch.replace(replaced);
        let px = gpu.draw(WHITE_BLACK_WHITE_GREY, &[&call(&batch, 0.0)]);
        assert_eq!(px, [[255, 255, 255], [0, 0, 0], [255, 255, 255], [0, 0, 255]], "replaced as a whole");

        assert_eq!(gpu.batches(), 1);
        let mut other = Batch::default();
        other.append(|m| quad(m, 0.0, 1.0, red));
        drop(batch);
        gpu.draw(WHITE_BLACK_WHITE_GREY, &[&call(&other, 0.0)]);
        assert_eq!(gpu.batches(), 1, "the dropped batch was freed");
    }
}
