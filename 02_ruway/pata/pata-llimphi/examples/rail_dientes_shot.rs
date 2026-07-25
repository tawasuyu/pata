//! Volcado headless del **rail de dientes** del sidebar (preset, rail izquierdo)
//! a PNG, sobre un fondo oscuro. Sirve para certificar que los íconos de los
//! dientes salen **a color** (Mónadas violeta, Archivos ámbar, Buscar azul) sin
//! bootear el DM — además de imprimir un histograma de colores vivos como texto.
//!
//! `cargo run -p pata-llimphi --example rail_dientes_shot -- [salida.png]`

use std::collections::HashMap;
use std::fs::File;
use std::io::BufWriter;

use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::taffy::prelude::{length, FlexDirection, Size, Style};
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::Typesetter;
use llimphi_ui::View;

use pata_llimphi::{render, Msg};

const W: u32 = 64;
const H: u32 = 240;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/rail_dientes.png".to_string());
    let theme = llimphi_theme::Theme::dark();

    // Rail izquierdo del preset: dientes Mónadas / Archivos / Buscar.
    let cfg = pata_core::Config::preset();
    let surface = cfg.surfaces[1].clone();
    let nav = pata_llimphi::nouser::NavState::default();
    let shuma = pata_llimphi::shuma::ShumaState::default();
    // Diente vivo en reposo: el rail cae a los iconos normales a color (que es
    // justo lo que este pantallazo certifica).
    let ctx = pata_core::widget::WidgetCtx::default();
    let vivo = render::DienteVivo {
        manifest: pata_core::atencion::Manifestacion::Reposo,
        cava_frame: &[],
        ctx: &ctx,
        unidades: None,
        flota_remoto: None,
        windows: &[],
        terminal_sessions: &[],
        t: 0.0,
    };

    let rail = render::sidebar_surface_view(
        &surface, 1, "", W as f32, H as f32, &nav, &[], "", None, &shuma, &vivo, &theme, false,
    );
    let root: View<Msg> = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(W as f32), height: length(H as f32) },
        ..Default::default()
    })
    .children(vec![rail]);

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
        label: Some("rail-shot"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
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
    renderer
        .render_to_view(&hal, &scene, &view, W, H, Color::from_rgba8(20, 22, 28, 255))
        .expect("render_to_view");
    let pixels = read_png(&hal, &target, &out);

    // Histograma de colores vivos: certifica color sin mirar el PNG.
    let mut hues: HashMap<(u8, u8, u8), u32> = HashMap::new();
    for px in pixels.chunks_exact(4) {
        let (r, g, b) = (px[0], px[1], px[2]);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        // Píxel "vivo": saturación alta y brillo medio-alto (descarta fondo/gris).
        if max as i32 - min as i32 > 50 && max > 120 {
            *hues.entry((r / 32 * 32, g / 32 * 32, b / 32 * 32)).or_insert(0) += 1;
        }
    }
    let mut top: Vec<_> = hues.into_iter().filter(|(_, c)| *c > 8).collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!("rail_dientes_shot: {out} → {} cubos de color vivo distintos", top.len());
    for ((r, g, b), c) in top.iter().take(8) {
        eprintln!("  #{r:02x}{g:02x}{b:02x} × {c}");
    }
    assert!(
        top.len() >= 3,
        "se esperaban ≥3 colores vivos distintos (un diente por color), hubo {}",
        top.len()
    );
}

fn read_png(hal: &Hal, target: &wgpu::Texture, path: &str) -> Vec<u8> {
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
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
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
    let mut penc = png::Encoder::new(BufWriter::new(file), W, H);
    penc.set_color(png::ColorType::Rgba);
    penc.set_depth(png::BitDepth::Eight);
    penc.write_header().unwrap().write_image_data(&pixels).unwrap();
    pixels
}
