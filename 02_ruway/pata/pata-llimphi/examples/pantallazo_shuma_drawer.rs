//! Volcado headless del **drawer B4** (la shuma COMPLETA hospedada en pata) a
//! PNG: la barra con el input vivo de la sesión activa arriba + el cuerpo del
//! drawer con dientes/sesiones/canvas de la shuma debajo. Sirve para validar a
//! ojo la integración `PATA_SHUMA_FULL` (live-wire) sin bootear el DM —el gate
//! que tenía frenado a B4 (ver `project_pata_shuma_paridad`).
//!
//! `cargo run -p pata-llimphi --example pantallazo_shuma_drawer -- [salida.png]`

use std::fs::File;
use std::io::BufWriter;

use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::taffy::prelude::{length, percent, FlexDirection, Size, Style};
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::Typesetter;
use llimphi_ui::View;

use pata_llimphi::render::BarData;
use pata_llimphi::shuma::{drawer_body_view_full, headline_view, ShumaState};
use pata_llimphi::Msg;

const W: u32 = 1280;
const H: u32 = 800;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/shuma_drawer.png".to_string());
    let theme = llimphi_theme::Theme::dark();

    // La shuma COMPLETA hospedada en barra (mismo Model que el standalone, con
    // el input de la sesión activa delegado a la barra de pata).
    let mut full = shuma_shell_llimphi::new_model();
    shuma_shell_llimphi::set_hosted_in_bar(&mut full, true);

    // Estado del cabezal/drawer: presente y desplegado.
    let mut state = ShumaState {
        present: true,
        open: true,
        ..Default::default()
    };
    state.anim = llimphi_motion::Tween::idle(1.0);

    // Barra superior con el input vivo de la sesión activa (live-wire).
    // `headline_view` toma un `&BarData`; para el pantallazo alcanza con los
    // defaults + la shuma completa hospedada (lo único que el cabezal usa aquí).
    let bar_data = BarData {
        shuma_full: Some(&full),
        ..Default::default()
    };
    let bar = View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(40.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .children(vec![headline_view(&state, &bar_data, &theme)]);

    // Cuerpo del drawer: la shuma entera (dientes/sesiones/menubar/canvas).
    let body = View::new(Style {
        flex_grow: 1.0,
        size: Size {
            width: percent(1.0_f32),
            height: percent(0.0_f32),
        },
        ..Default::default()
    })
    .children(vec![drawer_body_view_full(&full, &theme)]);

    let root = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_app)
    .children(vec![bar, body]);

    let hal = pollster::block_on(Hal::new(None)).expect("hal");
    let mut renderer = Renderer::new(&hal).expect("renderer");

    let mut layout = LayoutTree::new();
    let mounted = mount(&mut layout, root);
    let mut ts = Typesetter::new();
    let computed = {
        let tmap = &mounted.text_measures;
        layout
            .compute_with_measure(mounted.root, (W as f32, H as f32), |nid, known, avail| {
                match tmap.get(&nid) {
                    Some(tm) => measure_text_node(&mut ts, tm, known, avail),
                    None => taffy::Size::ZERO,
                }
            })
            .expect("layout")
    };
    let mut scene = vello::Scene::new();
    paint(&mut scene, &mounted, &computed, &mut ts, None, None);

    let target = hal.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shuma-drawer-shot"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FMT,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let [r, g, b, _] = theme.bg_app.components;
    let bg = Color::from_rgba8((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, 255);
    renderer
        .render_to_view(&hal, &scene, &view, W, H, bg)
        .expect("render_to_view");
    write_png(&hal, &target, &out);
    eprintln!("pantallazo_shuma_drawer: {out} ({W}x{H})");
    let _ = |m: Msg| m; // marca de tipo: el View es View<Msg>
}

fn write_png(hal: &Hal, target: &wgpu::Texture, path: &str) {
    let unpadded = (W * 4) as usize;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded = unpadded.div_ceil(align) * align;
    let buf = hal.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * H as usize) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = hal
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded as u32),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    hal.queue.submit(std::iter::once(enc.finish()));
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    hal.device.poll(wgpu::PollType::wait_indefinitely());
    rx.recv().unwrap().unwrap();
    let data = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((W * H * 4) as usize);
    for row in 0..H as usize {
        let s = row * padded;
        pixels.extend_from_slice(&data[s..s + unpadded]);
    }
    drop(data);
    buf.unmap();
    let file = File::create(path).expect("png");
    let mut enc = png::Encoder::new(BufWriter::new(file), W, H);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().unwrap();
    w.write_image_data(&pixels).unwrap();
}
