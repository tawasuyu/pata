//! Certificación headless del despliegue de progreso: arma varias
//! [`Transferencia`] en distintos estados, alimenta muestras para que tengan
//! velocidad/ETA/historial reales, y renderiza sus tarjetas a un PNG.
//!
//!   `cargo run -p pata-notify --example progreso_shot -- out.png`
//!
//! No abre ventana ni toca D-Bus: sólo `view → layout → raster → PNG`, el mismo
//! camino que certifica los demos de llimphi.

use std::fs::File;
use std::io::BufWriter;

use llimphi_theme::Theme;
use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::taffy::prelude::{length, percent, FlexDirection, Size, Style};
use llimphi_ui::llimphi_layout::taffy::Rect;
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::Typesetter;
use llimphi_ui::View;
use pata_core::progreso::{EstadoTransferencia, Transferencia};
use pata_notify::progreso_view::progreso_card;
use pata_notify::Msg;

const W: u32 = 380;
const H: u32 = 460;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Una transferencia corriendo a ~`vel` B/s hasta `frac` del total, con historial.
fn corriendo(id: u32, titulo: &str, total: u64, frac: f64, vel: f64) -> Transferencia {
    let mut t = Transferencia::nueva(id, titulo, Some(total));
    // Alimenta ~24 muestras (1 cada 0.25 s) subiendo a `vel`, con una leve
    // ondulación para que el sparkline no sea una recta.
    let objetivo = (total as f64 * frac) as u64;
    let pasos = 24u64;
    for i in 0..=pasos {
        let tt = i as f64 * 0.25;
        let onda = 1.0 + 0.25 * (i as f64 * 0.7).sin();
        let bytes = ((objetivo as f64) * (i as f64 / pasos as f64) * onda)
            .min(objetivo as f64) as u64;
        let _ = vel; // la velocidad emerge de las muestras
        t.muestrear(bytes, tt);
    }
    t
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "progreso_shot.png".into());

    let mut t1 = corriendo(1, "Copiando 42 archivos → Documentos", 3_400_000_000, 0.62, 0.0);
    let _ = &mut t1;
    let t2 = corriendo(2, "Moviendo vídeo.mkv → Externo", 1_200_000_000, 0.28, 0.0);
    let mut t3 = corriendo(3, "Copiando informe.pdf", 8_400_000, 1.0, 0.0);
    t3.finalizar(EstadoTransferencia::Hecha);
    let mut t4 = Transferencia::nueva(4, "Descargando actualización", None);
    t4.muestrear(50_000_000, 0.0);
    t4.muestrear(120_000_000, 1.0);

    let transfers = [t1, t2, t3, t4];

    let hal = pollster::block_on(Hal::new(None)).expect("hal");
    let mut renderer = Renderer::new(&hal).expect("renderer");
    let theme = Theme::dark();

    let cards: Vec<View<Msg>> = transfers.iter().map(|t| progreso_card(t, &theme)).collect();
    let root = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0), height: percent(1.0) },
        gap: Size { width: length(0.0), height: length(8.0) },
        padding: Rect {
            left: length(10.0),
            right: length(10.0),
            top: length(10.0),
            bottom: length(10.0),
        },
        ..Default::default()
    })
    .fill(theme.bg_app)
    .children(cards);

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
        label: Some("dump-progreso"),
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
    let bg = Color::from_rgba8(6, 8, 12, 255);
    renderer
        .render_to_view(&hal, &scene, &view, W, H, bg)
        .expect("render_to_view");
    write_png(&hal, &target, &out);
    eprintln!("progreso_shot: escrito {out} ({W}x{H})");
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
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
    );
    hal.queue.submit(std::iter::once(enc.finish()));
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    let _ = hal.device.poll(wgpu::PollType::wait_indefinitely());
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
