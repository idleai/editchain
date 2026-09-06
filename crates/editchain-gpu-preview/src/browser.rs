//! Browser-facing `wgpu` surface and wasm-bindgen API.

use std::{borrow::Cow, fmt};

use wasm_bindgen::prelude::*;
use wgpu::util::DeviceExt as _;

use super::{build_vertices, scissor_rect, RenderFrame, FLOATS_PER_VERTEX};

const SHADER: &str = include_str!("shader.wgsl");

/// A browser canvas renderer backed by WebGPU or WebGL2.
#[wasm_bindgen]
pub struct GpuRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    config: wgpu::SurfaceConfiguration,
    backend: String,
    canvas: web_sys::HtmlCanvasElement,
}

impl fmt::Debug for GpuRenderer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuRenderer")
            .field("backend", &self.backend)
            .field("width", &self.config.width)
            .field("height", &self.config.height)
            .finish_non_exhaustive()
    }
}

fn js_error(message: impl fmt::Display) -> JsValue {
    JsValue::from_str(&message.to_string())
}

fn set_stage(canvas: &web_sys::HtmlCanvasElement, stage: &str) {
    drop(canvas.set_attribute("data-gpu-stage", stage));
}

fn instance_descriptor(backends: wgpu::Backends) -> wgpu::InstanceDescriptor {
    wgpu::InstanceDescriptor {
        backends,
        flags: wgpu::InstanceFlags::default(),
        backend_options: wgpu::BackendOptions::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    }
}

async fn create_instance(requested: &str) -> Result<wgpu::Instance, JsValue> {
    match requested {
        "webgl" => Ok(wgpu::Instance::new(instance_descriptor(wgpu::Backends::GL))),
        "webgpu" => Ok(wgpu::Instance::new(instance_descriptor(
            wgpu::Backends::BROWSER_WEBGPU,
        ))),
        "auto" => Ok(
            wgpu::util::new_instance_with_webgpu_detection(instance_descriptor(
                wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL,
            ))
            .await,
        ),
        value => Err(js_error(format!("unsupported GPU backend: {value}"))),
    }
}

fn pipeline(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("EditChain GPU preview shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
    });
    let attributes = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4];
    let buffers = [Some(wgpu::VertexBufferLayout {
        array_stride: 24,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attributes,
    })];
    let targets = [Some(wgpu::ColorTargetState {
        format,
        // Premultiplied blending matches the compositing alpha mode requested
        // below and stays correct for the fully opaque geometry this renderer
        // emits.
        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        write_mask: wgpu::ColorWrites::ALL,
    })];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("EditChain GPU preview pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &buffers,
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &targets,
        }),
        multiview_mask: None,
        cache: None,
    })
}

#[wasm_bindgen]
impl GpuRenderer {
    /// Create a renderer for `canvas_id` using `webgl`, `webgpu`, or `auto`.
    ///
    /// # Errors
    ///
    /// Returns an error when the DOM canvas is missing, the requested backend
    /// is invalid or unavailable, or `wgpu` cannot create a surface/device.
    #[wasm_bindgen(js_name = create)]
    pub async fn create(canvas_id: String, requested_backend: String) -> Result<Self, JsValue> {
        let window = web_sys::window().ok_or_else(|| js_error("browser window is unavailable"))?;
        let document = window
            .document()
            .ok_or_else(|| js_error("browser document is unavailable"))?;
        let canvas = document
            .get_element_by_id(&canvas_id)
            .ok_or_else(|| js_error(format!("canvas #{canvas_id} is missing")))?
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .map_err(|_element| js_error(format!("element #{canvas_id} is not a canvas")))?;

        set_stage(&canvas, "instance");
        let instance = create_instance(&requested_backend).await?;
        set_stage(&canvas, "surface");
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(js_error)?;
        set_stage(&canvas, "adapter");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: true,
            })
            .await
            .map_err(js_error)?;
        let backend = if adapter.get_info().backend == wgpu::Backend::BrowserWebGpu {
            "webgpu"
        } else {
            "webgl"
        }
        .to_owned();
        set_stage(&canvas, "device");
        let required_limits = if backend == "webgpu" {
            // A true WebGPU adapter is not limited to the WebGL2 downlevel set;
            // request standard limits while clamping resolution fields to the
            // adapter's reported values instead of the WebGL2 ceiling.
            wgpu::Limits::default().using_resolution(adapter.limits())
        } else {
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits())
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("EditChain GPU preview device"),
                required_limits,
                ..wgpu::DeviceDescriptor::default()
            })
            .await
            .map_err(js_error)?;
        let width = canvas.width().max(1);
        let height = canvas.height().max(1);
        let mut config = surface
            .get_default_config(&adapter, width, height)
            .ok_or_else(|| js_error("adapter has no compatible canvas configuration"))?;
        // Prefer premultiplied compositing when the surface supports it so an
        // alpha-0 clear composites as transparent and the DOM theme shows
        // through the canvas.
        if surface
            .get_capabilities(&adapter)
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            config.alpha_mode = wgpu::CompositeAlphaMode::PreMultiplied;
        }
        set_stage(&canvas, "configure");
        surface.configure(&device, &config);
        set_stage(&canvas, "pipeline");
        let pipeline = pipeline(&device, config.format);
        set_stage(&canvas, "ready");

        Ok(Self {
            surface,
            device,
            queue,
            pipeline,
            config,
            backend,
            canvas,
        })
    }

    /// Return the backend selected by `wgpu` (`webgl` or `webgpu`).
    #[must_use]
    pub fn backend(&self) -> String {
        self.backend.clone()
    }

    /// Parse the frame contract, submit its geometry to the GPU, and return
    /// the vertex count.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed frame JSON, invalid dimensions,
    /// excessive geometry, or a canvas surface that requires another frame.
    pub fn render(
        &mut self,
        frame_json: &str,
        width: f32,
        height: f32,
        scale: f32,
    ) -> Result<u32, JsValue> {
        if !width.is_finite()
            || !height.is_finite()
            || width <= 0.0
            || height <= 0.0
            || !scale.is_finite()
            || scale <= 0.0
        {
            return Err(js_error("canvas dimensions and scale must be positive"));
        }
        let frame: RenderFrame = serde_json::from_str(frame_json).map_err(js_error)?;
        let vertices = build_vertices(&frame, width, height, scale);
        let vertex_total = vertices
            .len()
            .checked_div(FLOATS_PER_VERTEX)
            .ok_or_else(|| js_error("invalid GPU vertex stride"))?;
        let vertex_count = u32::try_from(vertex_total)
            .map_err(|error| js_error(format!("GPU vertex count overflow: {error}")))?;
        if vertex_count == 0 {
            // Empty frames (loading/empty states before any history rows) draw
            // nothing. `wgpu::util::DeviceExt::create_buffer_init` panics on an
            // empty slice ("buffer slice can not be empty"), which would abort
            // the wasm module; skip the submission instead.
            return Ok(0);
        }
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("EditChain GPU preview vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });

        let pixel_width = self.canvas.width().max(1);
        let pixel_height = self.canvas.height().max(1);
        if self.config.width != pixel_width || self.config.height != pixel_height {
            self.config.width = pixel_width;
            self.config.height = pixel_height;
            self.surface.configure(&self.device, &self.config);
        }
        let surface_frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(surface_frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(surface_frame) => surface_frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(0);
            }
            wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost
            | wgpu::CurrentSurfaceTexture::Validation => {
                self.surface.configure(&self.device, &self.config);
                return Err(js_error(
                    "GPU surface must be redrawn after reconfiguration",
                ));
            }
        };
        let view = surface_frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("EditChain GPU preview encoder"),
            });
        let attachments = [Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                // Transparent clear: the DOM layer owns theme/background/
                // selection, and the canvas must composite through it.
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        })];
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("EditChain GPU preview pass"),
                color_attachments: &attachments,
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            render_pass.set_pipeline(&self.pipeline);
            render_pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            let (scissor_x, scissor_y, scissor_width, scissor_height) =
                scissor_rect(&frame.graph, scale, width, height);
            render_pass.set_scissor_rect(scissor_x, scissor_y, scissor_width, scissor_height);
            render_pass.draw(0..vertex_count, 0..1);
        }
        let _submission_index = self.queue.submit([encoder.finish()]);
        self.queue.present(surface_frame);
        Ok(vertex_count)
    }
}
