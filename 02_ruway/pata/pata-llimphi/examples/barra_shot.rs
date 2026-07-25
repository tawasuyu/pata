//! Volcado headless de la **barra de mando real de pata** (el `shuma_input` en
//! live-wire + PS1 chakana + el ticker viejo) a PNG. Cierra el hueco de
//! verificación: rompe la norma «pata no se verifica headless» a propósito, para
//! poder MIRAR que la marquesina cae en el placeholder del input real (no en el
//! inner bare) y que la chakana reacciona.
//!
//! Renderiza dos filas: (1) input vacío con una marquesina inyectada en la
//! **sesión activa del modelo full** (lo que live-wire pinta); (2) el ticker
//! viejo `marquesina_view` al lado, para ver la duplicación que hay que sacar.
//!
//! `cargo run -p pata-llimphi --example barra_shot -- [salida.png]`

use std::fs::File;
use std::io::BufWriter;

use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::taffy::prelude::{length, percent, AlignItems, FlexDirection, Size, Style};
use llimphi_ui::llimphi_layout::taffy::Rect;
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::{Alignment, Typesetter};
use llimphi_ui::View;

use pata_llimphi::{render, shuma, shuma_app, Msg};

const W: u32 = 1100;
const H: u32 = 150;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/barra_shot.png".to_string());
    let theme = llimphi_theme::Theme::dark();

    // Modelo full (live-wire), como lo hospeda pata. Le inyecto una marquesina en
    // la SESIÓN ACTIVA — el input que live-wire realmente pinta.
    let mut full = shuma_app::new();
    shuma_app::set_active_marquesina(
        &mut full,
        Some(shuma_module_shell::Marquesina::urgente("CI · build failed en main")),
        0,
    );
    let ultimo = shuma_app::active_ultimo_resultado(&full); // None (fresh) → chakana idle
    let (ck_c, ck_t) = render::chakana_vista(true, true, ultimo, None, &theme);

    let shuma_state = shuma::ShumaState::default();
    // Datos que disparan los TRES iconos fugaces (música + CPU mini-cava +
    // batería) — sin esto el overlay devuelve None y el shot no los verifica.
    let media = pata_llimphi::mpris::MediaState {
        has_player: true,
        playing: true,
        title: "artista — pista".to_string(),
    };
    let cava: Vec<f32> = vec![0.2, 0.6, 0.9, 0.4, 0.7, 0.3, 0.8, 0.5];
    let cores: Vec<f32> = vec![0.15, 0.85, 0.4, 0.95, 0.05, 0.6, 0.3, 0.75];
    let data = render::BarData {
        shuma_full: Some(&full),
        chakana_color: Some(ck_c),
        chakana_titila: ck_t,
        chakana_forma: Default::default(),
        sys_alert: Some("CPU 95% · batería 12%".to_string()), // para el ticker viejo
        anim_t: 1.0,
        media: Some(&media),
        cava: &cava,
        cpu: 0.5,
        cpu_cores: &cores,
        cpu_temp: Some(55.0),
        bat: Some((0.42, false)), // descargando → el icono sale
        // Acción larga en curso (copiar/mover): la línea finísima a lo largo del
        // input muestra el 62 %.
        progreso: Some(0.62),
        ..Default::default()
    };

    // Barra de mando: PS1 chakana + input (live-wire, con label flotante pwd/git
    // + marquesina) + reloj grande a la derecha. (El ticker viejo ya no va.)
    let ps1 = render::start_button_view(
        "chakana",
        None,
        1.0,
        ck_c,
        ck_t,
        render::ChakanaForma::Chakana,
        &theme,
    );
    let input = shuma::headline_view(&shuma_state, &data, &theme);
    let reloj = render::clock_big_view(&theme); // el widget REAL (hora local)

    let bar = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(40.0_f32) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(12.0_f32), height: length(0.0_f32) },
        padding: Rect { left: length(10.0), right: length(14.0), top: length(0.0), bottom: length(0.0) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .children(vec![ps1, input, reloj]);

    let cap = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(20.0_f32) },
        ..Default::default()
    })
    .text_aligned(
        "pata · barra de mando (live-wire): PS1 chakana · label pwd/git flotante · input+marquesina · reloj".to_string(),
        13.0,
        theme.fg_text,
        Alignment::Start,
    );

    let root = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        gap: Size { width: length(0.0), height: length(14.0) },
        padding: Rect { left: length(24.0), right: length(24.0), top: length(22.0), bottom: length(22.0) },
        ..Default::default()
    })
    .fill(theme.bg_app)
    .children(vec![cap, bar]);

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
        label: Some("barra-shot"),
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
        .render_to_view(&hal, &scene, &view, W, H, Color::from_rgba8(6, 8, 12, 255))
        .expect("render_to_view");
    write_png(&hal, &target, &out);
    eprintln!("barra_shot: {out} ({W}x{H})");
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
    let mut enc = hal.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded as u32), rows_per_image: Some(H) },
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
    let mut enc = png::Encoder::new(BufWriter::new(file), W, H);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().unwrap();
    w.write_image_data(&pixels).unwrap();
}

#[allow(dead_code)]
fn _msg_marker(_: Msg) {}
