//! Probeta headless del **diálogo de volumen** (el mezclador que abre el icono
//! fugaz / el widget de volumen): renderiza `volume_menu_view` —la vista que la
//! barra layer-shell pinta con el menú `Volume` abierto— y certifica con
//! **stats numéricas** (sin mirar PNGs) que el flyout pinta de verdad:
//!
//! - a `open_t = 0` el cuerpo bajo la barra queda vacío (sólo fondo);
//! - a `open_t = 1` la card del mezclador cubre miles de píxeles.
//!
//! Sirve para partir el bug "el diálogo del volumen no se ve" en dos: si esto
//! pinta, la vista está sana y el problema es de mecánica de surface/anclaje
//! (configure/menu_reveal_at); si no pinta, el bug está en la vista.
//!
//! `cargo run -p pata-llimphi --example volumen_shot [-- salida.png]`

use std::fs::File;
use std::io::BufWriter;

use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::Typesetter;
use llimphi_ui::View;

use pata_llimphi::render::BarData;
use pata_llimphi::shuma::ShumaState;
use pata_llimphi::{render, Model, Msg, VolumeTab};

const W: u32 = 1280;
const H: u32 = 420;
const BAR_PX: f32 = 40.0;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn main() {
    let out = std::env::args().nth(1);
    let theme = llimphi_theme::Theme::default();

    // La barra REAL del preset nativo (chakana + shuma_input + reloj): misma
    // Surface y SurfaceWidgets que la sesión viva, no un mock.
    let cfg = pata_core::config::Config::preset();
    let surfaces = Model::construir_surfaces(&cfg);
    let surface = &cfg.surfaces[0];
    let widgets = &surfaces[0];
    let shuma_state = ShumaState::default();

    let data = BarData { volume: 0.62, muted: false, ..Default::default() };
    let ctx = pata_core::widget::WidgetCtx::default();

    // Mezclador con datos de mentira pero realistas (una salida + una app).
    let sinks = vec![pata_llimphi::sampler::Sink {
        name: "alsa_output.pci".into(),
        description: "Built-in Audio Analog Stereo".into(),
        is_default: true,
        volume: 0.62,
        muted: false,
    }];
    let sink_inputs = Vec::new();
    let sources = Vec::new();
    let source_outputs = Vec::new();

    let vista = |open_t: f32| -> View<Msg> {
        render::volume_menu_view(
            surface,
            widgets,
            &shuma_state,
            &data,
            &theme,
            BAR_PX,
            &ctx,
            &sinks,
            &sink_inputs,
            &sources,
            &source_outputs,
            VolumeTab::Reproduccion,
            640.0, // anchor_x: centro
            W as f32,
            open_t,
        )
    };

    let hal = pollster::block_on(Hal::new(None)).expect("hal");
    let mut renderer = Renderer::new(&hal).expect("renderer");

    let cerrado = render_pixels(&hal, &mut renderer, vista(0.0));
    let abierto = render_pixels(&hal, &mut renderer, vista(1.0));

    // Stats numéricas: píxeles distintos entre cerrado y abierto, separando la
    // franja de la barra (arriba) del cuerpo (donde debe posarse la card).
    let bar_rows = BAR_PX as usize;
    let (mut diff_bar, mut diff_cuerpo) = (0usize, 0usize);
    for row in 0..H as usize {
        for col in 0..W as usize {
            let i = (row * W as usize + col) * 4;
            if cerrado[i..i + 4] != abierto[i..i + 4] {
                if row < bar_rows {
                    diff_bar += 1;
                } else {
                    diff_cuerpo += 1;
                }
            }
        }
    }
    // Y cuántos píxeles del cuerpo abierto NO son el fondo de limpieza (o sea,
    // pintura real de la card — no dependemos sólo del diff).
    let fondo = [18u8, 18, 26, 255];
    let (mut pintados_bar, mut pintados_cuerpo) = (0usize, 0usize);
    for row in 0..H as usize {
        for col in 0..W as usize {
            let i = (row * W as usize + col) * 4;
            if abierto[i..i + 4] != fondo {
                if row < bar_rows {
                    pintados_bar += 1;
                } else {
                    pintados_cuerpo += 1;
                }
            }
        }
    }
    println!(
        "volumen_shot: diff barra={diff_bar}px · diff cuerpo={diff_cuerpo}px · \
         pintados barra={pintados_bar}px · pintados cuerpo={pintados_cuerpo}px"
    );
    let ok = diff_cuerpo > 5_000 && pintados_cuerpo > 5_000;
    println!(
        "volumen_shot: {}",
        if ok {
            "OK — la card del mezclador PINTA con open_t=1 (la vista está sana)"
        } else {
            "FALLA — el cuerpo quedó vacío con open_t=1 (el bug está en la vista)"
        }
    );

    if let Some(out) = out {
        write_png(&abierto, &out);
        eprintln!("escrito {out}");
    }
    std::process::exit(if ok { 0 } else { 1 });
}

/// Renderiza la vista a un buffer RGBA (readback de la textura wgpu).
fn render_pixels(hal: &Hal, renderer: &mut Renderer, root: View<Msg>) -> Vec<u8> {
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
        label: Some("volumen-shot"),
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
        .render_to_view(hal, &scene, &view, W, H, Color::from_rgba8(18, 18, 26, 255))
        .expect("render_to_view");

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
            texture: &target,
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
    let mapped = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((W * H * 4) as usize);
    for row in 0..H as usize {
        let s = row * padded;
        pixels.extend_from_slice(&mapped[s..s + unpadded]);
    }
    drop(mapped);
    buf.unmap();
    pixels
}

fn write_png(pixels: &[u8], path: &str) {
    let file = File::create(path).expect("png");
    let mut enc = png::Encoder::new(BufWriter::new(file), W, H);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().unwrap();
    w.write_image_data(pixels).unwrap();
}
