//! Volcado headless del **completado flotante "bonito"** del input de shuma a
//! PNG — el panel de candidatos autónomo que aparece sobre la barra fina cuando
//! se tipea (apps tier 0 con su ícono XDG, tokens/comandos tier 1, líneas y
//! grupos del historial tiers 2/3), con sombra E4, borde, radio y fade.
//!
//! Maneja el path REAL: puebla `State.apps` como lo hace `apps_lanzables`,
//! tipea un prefijo con `Msg::Key` (lo mismo que la barra fina hace por tecla)
//! y pinta lo que produjo `completion_rows` vía `render::completion_panel_preview`.
//! Cierra el hueco de verificación: el autocomplete se escribió pero nunca se
//! había MIRADO renderizado (era "supuestamente bonito").
//!
//! `cargo run -p pata-llimphi --example completion_shot -- [salida.png]`

use std::fs::File;
use std::io::BufWriter;

use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::taffy::prelude::{length, percent, FlexDirection, Size, Style};
use llimphi_ui::llimphi_layout::taffy::Rect;
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::{Alignment, Typesetter};
use llimphi_ui::{Key, KeyEvent, KeyState, Modifiers, View};

use pata_llimphi::{render, Msg};
use shuma_module_shell::{update, LaunchableApp, Msg as SMsg};

const W: u32 = 640;
const H: u32 = 520;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Una tecla de carácter, como la manda la barra fina (`text` con el grafema).
fn key_char(c: char) -> KeyEvent {
    let mut buf = [0u8; 4];
    KeyEvent {
        key: Key::Character(c.to_string().into()),
        state: KeyState::Pressed,
        text: Some(c.encode_utf8(&mut buf).to_string()),
        modifiers: Modifiers::default(),
        repeat: false,
    }
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/completion_shot.png".to_string());
    let theme = llimphi_theme::Theme::dark();

    // Estado real del shell (la misma `inner` que pata hospeda), sembrado con
    // apps del escritorio como `apps_lanzables` las deja: nombre + comando +
    // hint de ícono freedesktop.
    let mut s = pata_llimphi::shuma::ShumaState::default().inner;
    s.apps = vec![
        LaunchableApp::new("Gimp", "gimp").con_icono(Some("gimp".into())),
        LaunchableApp::new("Git Cola", "git-cola").con_icono(Some("git-cola".into())),
        LaunchableApp::new("✶  Pluma", "pluma-app-llimphi").con_icono(Some("accessories-text-editor".into())),
    ];

    // Tipear "gi" — abre el popup: apps (Gimp/Git Cola) tier 0 con ícono,
    // comando `git` tier 1, y lo que el PATH/historial aporte en tiers altos.
    for c in "gi".chars() {
        s = update(s, SMsg::Key(key_char(c)));
    }

    let rows = shuma_module_shell::completion_rows(&s);
    eprintln!(
        "completion_shot: completion={} filas={} extra={} apps={}",
        s.completion.is_some(),
        rows.len(),
        s.completion_extra.len(),
        s.apps.len(),
    );
    if s.completion.is_none() || rows.is_empty() {
        eprintln!("completion_shot: ⚠ el completado quedó vacío — el panel no pintaría nada");
    }

    let panel = render::completion_panel_preview(&s, &theme, 460.0, 1.0)
        .expect("el completado debe producir panel con el estado sembrado");

    let cap = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(20.0_f32) },
        ..Default::default()
    })
    .text_aligned(
        "shuma · completado flotante (tipeado \"gi\"): apps con ícono · comando · historial".to_string(),
        13.0,
        theme.fg_text,
        Alignment::Start,
    );

    let root = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        gap: Size { width: length(0.0), height: length(16.0) },
        padding: Rect { left: length(24.0), right: length(24.0), top: length(22.0), bottom: length(22.0) },
        ..Default::default()
    })
    .fill(theme.bg_app)
    .children(vec![cap, panel]);

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
        label: Some("completion-shot"),
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
    eprintln!("completion_shot: {out} ({W}x{H})");
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
