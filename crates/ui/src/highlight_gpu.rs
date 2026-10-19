// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Highlighter strokes drawn with a "darken" blend.
//!
//! A highlighter is painted in the colour it leaves on the paper, and every
//! pixel keeps the darker of that colour and what is already there (`min`
//! per channel, the PDF Darken blend mode). So:
//!
//! * overlapping highlighter strokes never get darker,
//! * text, ink and the pages of a PDF under them stay fully visible,
//! * on paper the highlighter shows its colour.
//!
//! egui's painter only blends "over", so the strokes go through a paint
//! callback with their own wgpu pipeline, inside egui's render pass and in
//! paint order like any other shape. Antialiased edges fade to white, which
//! `min` leaves unchanged.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use eframe::egui::{epaint::Mesh, PaintCallbackInfo, Painter, Shape};
use eframe::egui_wgpu::{self, wgpu, CallbackResources, CallbackTrait, ScreenDescriptor};
use wgpu::util::DeviceExt;

/// The pipeline is installed (wgpu renderer): highlighters can darken.
static INSTALLED: AtomicBool = AtomicBool::new(false);

const SHADER: &str = r#"
struct Screen {
    size: vec2<f32>,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> screen: Screen;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> VOut {
    var out: VOut;
    out.pos = vec4<f32>(2.0 * pos.x / screen.size.x - 1.0, 1.0 - 2.0 * pos.y / screen.size.y, 0.0, 1.0);
    out.color = color;
    return out;
}

// The (premultiplied) vertex colour over white: what `min` compares with,
// so transparent edges change nothing.
fn over_white(c: vec4<f32>) -> vec3<f32> {
    return c.rgb + vec3<f32>(1.0 - c.a);
}

@fragment
fn fs_gamma(in: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(over_white(in.color), 1.0);
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c < vec3<f32>(0.04045));
}

@fragment
fn fs_linear(in: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(to_linear(over_white(in.color)), 1.0);
}
"#;

struct Resources {
    pipeline: wgpu::RenderPipeline,
    screen: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// Creates the pipeline; call once with eframe's wgpu state. `msaa` must be
/// the sample count of egui's render pass.
pub fn install(render_state: &egui_wgpu::RenderState, msaa: u32) {
    let device = &render_state.device;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("highlighter"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("highlighter"),
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
    let screen = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("highlighter screen"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("highlighter"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: screen.as_entire_binding() }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("highlighter"),
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });
    // egui's vertex: pos (2 x f32), uv (2 x f32), colour (4 x u8, premultiplied).
    let vertex_layout = wgpu::VertexBufferLayout {
        array_stride: 20,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Unorm8x4, offset: 16, shader_location: 1 },
        ],
    };
    let min = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Min,
    };
    let format = render_state.target_format;
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("highlighter"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs",
            compilation_options: Default::default(),
            buffers: &[vertex_layout],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            // An sRGB target expects linear output (egui does the same).
            entry_point: if format.is_srgb() { "fs_linear" } else { "fs_gamma" },
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState { color: min, alpha: min }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
        depth_stencil: None,
        multisample: wgpu::MultisampleState { count: msaa.max(1), ..Default::default() },
        multiview: None,
        cache: None,
    });
    render_state.renderer.write().callback_resources.insert(Resources { pipeline, screen, bind_group });
    INSTALLED.store(true, Ordering::Relaxed);
}

/// One batch of highlighter triangles (screen points), uploaded in `prepare`.
struct Highlight {
    mesh: Mesh,
    buffers: Mutex<Option<(wgpu::Buffer, wgpu::Buffer)>>,
}

impl CallbackTrait for Highlight {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(res) = resources.get::<Resources>() else { return Vec::new() };
        let ppp = screen.pixels_per_point;
        let size = [screen.size_in_pixels[0] as f32 / ppp, screen.size_in_pixels[1] as f32 / ppp, 0.0, 0.0];
        queue.write_buffer(&res.screen, 0, bytemuck::cast_slice(&size));
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("highlighter vertices"),
            contents: bytemuck::cast_slice(&self.mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("highlighter indices"),
            contents: bytemuck::cast_slice(&self.mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        *self.buffers.lock().unwrap_or_else(|e| e.into_inner()) = Some((vertices, indices));
        Vec::new()
    }

    fn paint(&self, _info: PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &CallbackResources) {
        let Some(res) = resources.get::<Resources>() else { return };
        let buffers = self.buffers.lock().unwrap_or_else(|e| e.into_inner());
        let Some((vertices, indices)) = buffers.as_ref() else { return };
        pass.set_pipeline(&res.pipeline);
        pass.set_bind_group(0, &res.bind_group, &[]);
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.mesh.indices.len() as u32, 0, 0..1);
    }
}

/// Paints highlighter triangles (screen points, colours as from
/// `render::paint_rgba`) with the darken blend, clipped like `painter`.
/// Without the wgpu pipeline they are painted as a plain mesh.
pub fn paint(painter: &Painter, mesh: Mesh) {
    if mesh.is_empty() {
        return;
    }
    if !INSTALLED.load(Ordering::Relaxed) {
        painter.add(Shape::mesh(mesh));
        return;
    }
    // The whole screen: the vertex shader maps screen points; the painter's
    // clip rect becomes the scissor.
    let rect = painter.ctx().screen_rect();
    let callback = Highlight { mesh, buffers: Mutex::new(None) };
    painter.add(egui_wgpu::Callback::new_paint_callback(rect, callback));
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::epaint::Vertex;
    use eframe::egui::{pos2, Color32};
    use std::sync::Arc;

    /// Two overlapping highlighter quads over white, black, white and grey
    /// pixels, drawn with the real pipeline on a real GPU (skipped without one).
    #[test]
    fn darkens_without_stacking() {
        let instance = wgpu::Instance::default();
        let Some(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no GPU adapter: skipped");
            return;
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&Default::default(), None)).unwrap();
        let (device, queue) = (Arc::new(device), Arc::new(queue));
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let renderer = egui_wgpu::Renderer::new(&device, format, None, 1, false);
        let state = egui_wgpu::RenderState {
            adapter: Arc::new(adapter),
            available_adapters: Arc::new([]),
            device: device.clone(),
            queue: queue.clone(),
            target_format: format,
            renderer: Arc::new(eframe::egui::mutex::RwLock::new(renderer)),
        };
        install(&state, 1);

        // A 4 x 1 target: white, black, white, grey.
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: 4, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let start: [u8; 16] = [255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255, 128, 128, 128, 255];
        queue.write_texture(
            target.as_image_copy(),
            &start,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(16), rows_per_image: None },
            wgpu::Extent3d { width: 4, height: 1, depth_or_array_layers: 1 },
        );

        // Two identical quads over the whole target, in the highlighter's colour.
        let yellow = Color32::from_rgb(255, 245, 160);
        let mut mesh = Mesh::default();
        for _ in 0..2 {
            let base = mesh.vertices.len() as u32;
            for (x, y) in [(0.0, 0.0), (4.0, 0.0), (4.0, 1.0), (0.0, 1.0)] {
                mesh.vertices.push(Vertex { pos: pos2(x, y), uv: pos2(0.0, 0.0), color: yellow });
            }
            mesh.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        let callback = Highlight { mesh, buffers: Mutex::new(None) };
        let screen = ScreenDescriptor { size_in_pixels: [4, 1], pixels_per_point: 1.0 };
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut renderer = state.renderer.write();
            callback.prepare(&device, &queue, &screen, &mut encoder, &mut renderer.callback_resources);
        }
        let view = target.create_view(&Default::default());
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                })
                .forget_lifetime();
            let renderer = state.renderer.read();
            let info = PaintCallbackInfo {
                viewport: eframe::egui::Rect::from_min_size(pos2(0.0, 0.0), eframe::egui::vec2(4.0, 1.0)),
                clip_rect: eframe::egui::Rect::from_min_size(pos2(0.0, 0.0), eframe::egui::vec2(4.0, 1.0)),
                pixels_per_point: 1.0,
                screen_size_px: [4, 1],
            };
            callback.paint(info, &mut pass, &renderer.callback_resources);
        }
        // Read the pixels back (rows are padded to 256 bytes).
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &readback,
                layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: None },
            },
            wgpu::Extent3d { width: 4, height: 1, depth_or_array_layers: 1 },
        );
        queue.submit([encoder.finish()]);
        readback.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device.poll(wgpu::Maintain::Wait);
        let px: Vec<u8> = readback.slice(..16).get_mapped_range().to_vec();
        assert!(pollster::block_on(device.pop_error_scope()).is_none(), "wgpu validation error");

        let at = |i: usize| [px[i * 4], px[i * 4 + 1], px[i * 4 + 2]];
        // White paper takes the highlighter's colour, once (no stacking).
        assert_eq!(at(0), [255, 245, 160]);
        assert_eq!(at(2), [255, 245, 160]);
        // Black ink stays black.
        assert_eq!(at(1), [0, 0, 0]);
        // Grey keeps its darker channels.
        assert_eq!(at(3), [128, 128, 128]);
    }
}
