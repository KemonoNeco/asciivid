use super::{Layout, Renderer};
use crate::atlas::Atlas;
use crate::glitch::{Tear, row_shift};
use crate::grid::Cell;
use anyhow::{Context, Result};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    cols: u32,
    rows: u32,
    cw: u32,
    ch: u32,
    out_w: u32,
    out_h: u32,
    _pad: [u32; 2],
    bg: [f32; 4],
}

/// wgpu compute renderer. Cells are composited on the GPU and the frame is read back for
/// the encoder.
pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    cells: wgpu::Buffer,
    glyphs: wgpu::Buffer,
    shifts: wgpu::Buffer,
    out: wgpu::Buffer,
    readback: wgpu::Buffer,
    layout: Layout,
    // Staging for each frame's uploads, kept to avoid reallocating.
    cell_data: Vec<[f32; 4]>,
    glyph_data: Vec<u32>,
    shift_data: Vec<u32>,
    /// Adapter name and backend, for the startup line.
    pub name: String,
}

impl GpuRenderer {
    pub fn new(atlas: &Atlas, layout: Layout, bg: [f32; 3]) -> Result<GpuRenderer> {
        // WGPU_BACKEND, WGPU_ADAPTER_NAME etc. can steer adapter choice.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .context("no GPU adapter found")?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("asciivid"),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .context("failed to open GPU device")?;

        let Layout { cols, rows, cw, ch, out_w, out_h } = layout;
        let frame_bytes = layout.frame_bytes() as u64;
        let max = device.limits().max_storage_buffer_binding_size;
        anyhow::ensure!(
            frame_bytes <= max,
            "{out_w}x{out_h} frame needs {frame_bytes} bytes, over this GPU's {max}-byte storage limit"
        );

        let mut atlas_bytes: Vec<u8> = atlas.glyphs.concat();
        atlas_bytes.resize(atlas_bytes.len().next_multiple_of(4), 0);
        let params = Params {
            cols: cols as u32,
            rows: rows as u32,
            cw: cw as u32,
            ch: ch as u32,
            out_w: out_w as u32,
            out_h: out_h as u32,
            _pad: [0; 2],
            bg: [bg[0], bg[1], bg[2], 0.0],
        };

        let init = |label, contents: &[u8], usage| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents, usage })
        };
        let empty = |label, size: u64, usage| {
            device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false })
        };
        use wgpu::BufferUsages as U;
        let params = init("params", bytemuck::bytes_of(&params), U::UNIFORM);
        let atlas_buf = init("atlas", &atlas_bytes, U::STORAGE);
        let n = (cols * rows) as u64;
        let cells = empty("cells", n * 16, U::STORAGE | U::COPY_DST);
        let glyphs = empty("glyphs", n * 4, U::STORAGE | U::COPY_DST);
        let shifts = empty("shifts", out_h as u64 * 4, U::STORAGE | U::COPY_DST);
        let out = empty("out", frame_bytes, U::STORAGE | U::COPY_SRC);
        let readback = empty("readback", frame_bytes, U::MAP_READ | U::COPY_DST);

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("render"),
            source: wgpu::ShaderSource::Wgsl(include_str!("render.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("render"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("render"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[&params, &atlas_buf, &cells, &glyphs, &shifts, &out]
                .iter()
                .enumerate()
                .map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32, resource: b.as_entire_binding() })
                .collect::<Vec<_>>(),
        });

        Ok(GpuRenderer {
            device,
            queue,
            pipeline,
            bind_group,
            cells,
            glyphs,
            shifts,
            out,
            readback,
            layout,
            cell_data: Vec::with_capacity(n as usize),
            glyph_data: Vec::with_capacity(n as usize),
            shift_data: Vec::with_capacity(out_h),
            name: format!("{} ({:?})", info.name, info.backend),
        })
    }
}

impl Renderer for GpuRenderer {
    fn render(&mut self, cells: &[Cell], tears: &[Tear], out: &mut [u8]) -> Result<()> {
        let Layout { out_w, out_h, .. } = self.layout;
        self.cell_data.clear();
        self.cell_data.extend(cells.iter().map(|c| [c.color[0], c.color[1], c.color[2], c.fill]));
        self.glyph_data.clear();
        self.glyph_data.extend(cells.iter().map(|c| c.glyph as u32));
        self.shift_data.clear();
        self.shift_data.extend((0..out_h).map(|y| row_shift(tears, y, out_w) as u32));
        self.queue.write_buffer(&self.cells, 0, bytemuck::cast_slice(&self.cell_data));
        self.queue.write_buffer(&self.glyphs, 0, bytemuck::cast_slice(&self.glyph_data));
        self.queue.write_buffer(&self.shifts, 0, bytemuck::cast_slice(&self.shift_data));

        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.dispatch_workgroups(out_w.div_ceil(8) as u32, out_h.div_ceil(8) as u32, 1);
        }
        enc.copy_buffer_to_buffer(&self.out, 0, &self.readback, 0, None);
        self.queue.submit([enc.finish()]);

        let (tx, rx) = std::sync::mpsc::channel();
        self.readback.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .context("waiting for GPU")?;
        rx.recv()?.context("mapping GPU frame")?;
        out.copy_from_slice(&self.readback.get_mapped_range(..)?);
        self.readback.unmap();
        Ok(())
    }
}
