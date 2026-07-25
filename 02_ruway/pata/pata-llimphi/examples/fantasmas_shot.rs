//! Volcado headless de los **controles fantasma** del cabezal de shuma, para
//! verlos sin bootear el DM. Tres filas del mismo input:
//!
//! - Arriba (**reposo**): las críticas fijas (aquí, el escudo de ágora) + UNA
//!   leve en turno (música) — los demás salientes esperan su giro.
//! - Abajo (**revelado**): al acercar el puntero se muestran TODOS —cava,
//!   volumen, CPU, batería, luna, signo— clickeables, con los que ya estaban
//!   visibles anclados a la derecha (orden pegajoso).
//!
//! `cargo run -p pata-llimphi --example fantasmas_shot -- [out.png]`

use std::fs::File;
use std::io::BufWriter;

use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::taffy::prelude::{length, FlexDirection, Size, Style};
use llimphi_ui::llimphi_layout::taffy::{AlignItems, Rect as TaffyRect};
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::{Alignment, Typesetter};
use llimphi_ui::View;

use pata_llimphi::mpris::MediaState;
use pata_llimphi::render::BarData;
use pata_llimphi::shuma::{headline_view, ShumaState};
use pata_llimphi::Msg;

const W: u32 = 1120;
const H: u32 = 440;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "fantasmas.png".to_string());
    let theme = llimphi_theme::Theme::default();

    // Estado del cabezal (input vacío → fade pleno; present=true → se pinta).
    let mut state = ShumaState::default();
    state.present = true;

    // Backings de datos (BarData presta slices/refs).
    let cava: Vec<f32> = (0..14).map(|i| 0.25 + 0.55 * ((i as f32 * 0.7).sin().abs())).collect();
    let cpu_alto: Vec<f32> = vec![0.30, 0.72, 0.18, 0.91, 0.44, 0.83, 0.55, 0.62];
    let cpu_bajo: Vec<f32> = vec![0.08, 0.12, 0.05, 0.15, 0.10, 0.07, 0.09, 0.06];
    let media_on = MediaState { has_player: true, playing: true, title: "Aphex Twin — Rhubarb".into() };
    // Red conectada por Wi-Fi con tráfico vivo + un clima lluvioso (animado):
    // alimentan los fantasmas nuevos de red y clima.
    let red_wifi = pata_llimphi::network::NetState {
        status: pata_llimphi::network::NetStatus::Wifi { ssid: "ayllu".into(), signal: 72 },
        wifi_enabled: true,
        networks: Vec::new(),
        saved: Vec::new(),
        details: Default::default(),
    };
    let clima_lluvia = pata_llimphi::weather::Weather {
        temp_c: 14.0,
        sky: pata_llimphi::weather::Sky::Rain,
        desc: "Light rain".into(),
        coords: None,
    };

    // Fila REPOSO: sólo música saliente; el resto en calma (batería cargando,
    // CPU baja, luna a mitad, Sol a mitad de signo) → un solo icono.
    let reposo = BarData {
        media: Some(&media_on),
        cava: &cava,
        cpu: 0.18,
        cpu_cores: &cpu_bajo,
        cpu_temp: Some(45.0),
        bat: Some((0.82, true)), // cargando → no saliente
        volume: 0.6,
        muted: false,
        moon_phase: 0.28,
        sun_longitude: 135.0, // 15° dentro de Leo → sin ingreso
        network: Some(&red_wifi),
        weather: Some(&clima_lluvia),
        net_trafico: (0.35, 0.10),
        brightness: 0.7,
        revelar_alpha: 0.0,
        ..Default::default()
    };

    // Fila REVELADO: puntero en la zona → todos, con señales ricas.
    let revelado = BarData {
        media: Some(&media_on),
        cava: &cava,
        cpu: 0.74,
        cpu_cores: &cpu_alto,
        cpu_temp: Some(84.0), // caliente → titila
        bat: Some((0.16, false)), // baja y descargando
        volume: 0.62,
        muted: false,
        moon_phase: 0.5, // llena
        sun_longitude: 120.4, // recién ingresó a Leo
        network: Some(&red_wifi),
        weather: Some(&clima_lluvia),
        net_trafico: (0.75, 0.30),
        brightness: 0.7,
        vol_evento: Some(true), // rampa creciente: el volumen acaba de subir
        revelar_alpha: 1.0,
        ..Default::default()
    };

    // Fila FUNDIDO: a mitad del fade (aparición/esfumado) — opacidad parcial,
    // el «misterio» animado. Mismas señales ricas, sólo baja el revelar_alpha.
    let medio = BarData {
        media: Some(&media_on),
        cava: &cava,
        cpu: 0.74,
        cpu_cores: &cpu_alto,
        cpu_temp: Some(84.0),
        bat: Some((0.16, false)),
        volume: 0.62,
        muted: false,
        moon_phase: 0.5,
        sun_longitude: 120.4,
        network: Some(&red_wifi),
        weather: Some(&clima_lluvia),
        net_trafico: (0.20, 0.05),
        brightness: 0.7,
        revelar_alpha: 0.4,
        ..Default::default()
    };

    let root = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(W as f32), height: length(H as f32) },
        padding: TaffyRect {
            left: length(24.0_f32),
            right: length(24.0_f32),
            top: length(20.0_f32),
            bottom: length(20.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(20.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_app)
    .children(vec![
        panel("reposo — críticas fijas + una leve en turno", &state, &reposo, &theme),
        panel("fundido — a mitad del fade (opacidad animada)", &state, &medio, &theme),
        panel("revelado — el puntero se acerca → todos, clickeables", &state, &revelado, &theme),
    ]);

    render_png(root, &out);
    eprintln!("escrito {out}");
}

/// Un panel titulado con una barra que hospeda el cabezal del input.
fn panel(titulo: &str, state: &ShumaState, data: &BarData, theme: &llimphi_theme::Theme) -> View<Msg> {
    let rotulo = View::new(Style {
        size: Size { width: length(1040.0_f32), height: length(18.0_f32) },
        ..Default::default()
    })
    .text_aligned(titulo.to_string(), 13.0, theme.fg_muted, Alignment::Start);

    // La "barra": fondo panel + el cabezal del input adentro (alto tipo barra).
    let barra = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: length(1040.0_f32), height: length(52.0_f32) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(12.0_f32),
            right: length(12.0_f32),
            top: length(6.0_f32),
            bottom: length(6.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(8.0)
    .children(vec![headline_view(state, data, theme)]);

    View::new(Style {
        flex_direction: FlexDirection::Column,
        gap: Size { width: length(0.0_f32), height: length(6.0_f32) },
        ..Default::default()
    })
    .children(vec![rotulo, barra])
}

fn render_png(root: View<Msg>, out: &str) {
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

    let hal = pollster::block_on(Hal::new(None)).expect("hal");
    let mut renderer = Renderer::new(&hal).expect("renderer");
    let target = hal.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("fantasmas-shot"),
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
        .render_to_view(&hal, &scene, &view, W, H, Color::from_rgba8(18, 18, 26, 255))
        .expect("render_to_view");
    write_png(&hal, &target, out);
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
