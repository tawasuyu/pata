//! Certificación headless del **navegador de ventanas en el rail** (widgets
//! `workspaces` + `window_tabs` en el slot `start` de un sidebar): renderiza el
//! rail DOS veces —con y sin los widgets— y reporta el conteo de píxeles que
//! difieren como **stats de texto** (Regla 8: la evidencia es numérica; el PNG
//! sólo queda en disco por si hace falta mirarlo).
//!
//! `cargo run -p pata-llimphi --example rail_zen_shot -- [salida.png]`

use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::Typesetter;
use llimphi_ui::View;

use pata_core::atencion::Manifestacion;
use pata_core::config::{Anchor, Prop, Surface, SidebarTab, WidgetSpec};
use pata_core::widget::WidgetCtx;
use pata_llimphi::nouser::NavState;
use pata_llimphi::render;
use pata_llimphi::shuma::ShumaState;
use pata_llimphi::toplevel::WindowEntry;
use pata_llimphi::Msg;

const W: u32 = 64;
const H: u32 = 600;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn ventana(id: u32, ws: u8, tab: usize, app_id: &str, label: &str, active: bool) -> WindowEntry {
    WindowEntry {
        id,
        label: label.into(),
        app_id: app_id.into(),
        workspace: ws,
        active,
        minimized: false,
        tab,
    }
}

fn rail_surface(con_zen: bool) -> Surface {
    let mut s = Surface::sidebar(Anchor::Left);
    s.tabs.push(SidebarTab::new(
        "monads",
        "Mónadas",
        WidgetSpec::new("navigator").with("source", Prop::Str("nouser".into())),
    ));
    s.tabs.push(SidebarTab::new(
        "files",
        "Archivos",
        WidgetSpec::new("navigator").with("source", Prop::Str("home".into())),
    ));
    if con_zen {
        s.start = vec![WidgetSpec::new("workspaces"), WidgetSpec::new("window_tabs")];
    }
    s
}

fn render_rail(hal: &Hal, renderer: &mut Renderer, con_zen: bool, windows: &[WindowEntry]) -> Vec<u8> {
    let theme = llimphi_theme::Theme::dark();
    let surface = rail_surface(con_zen);
    let mut ctx = WidgetCtx::default();
    ctx.active_workspace = 1;
    ctx.workspace_count = 9;
    ctx.workspace_occupied = 0b0000_0101; // 1 y 3 ocupados; 2 es hueco libre
    let vivo = render::DienteVivo {
        manifest: Manifestacion::Reposo,
        cava_frame: &[],
        ctx: &ctx,
        unidades: None,
        flota_remoto: None,
        windows,
        terminal_sessions: &[],
        t: 0.0,
    };
    let nav = NavState::default();
    let shuma = ShumaState::default();
    let root: View<Msg> = render::sidebar_surface_view(
        &surface, 0, "", W as f32, H as f32, &nav, &[], "", None, &shuma, &vivo, &theme, false,
    );

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
        .render_to_view(hal, &scene, &view, W, H, Color::from_rgba8(0, 0, 0, 255))
        .expect("render_to_view");
    leer_pixels(hal, &target)
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/rail_zen.png".to_string());
    let hal = pollster::block_on(Hal::new(None)).expect("hal");
    let mut renderer = Renderer::new(&hal).expect("renderer");

    let wins = [
        // Tab 0: un split de dos ventanas (Tullpu + Firefox) — mismo (ws, tab).
        ventana(10, 1, 0, "tullpu-app-llimphi", "Tullpu — lienzo", true),
        ventana(11, 1, 0, "firefox", "Mozilla Firefox", false),
        // Tab 1: una ventana sola.
        ventana(13, 1, 1, "nada", "Nada — editor", false),
        // Otro escritorio (no se pinta en el rail del activo).
        ventana(12, 3, 0, "org.kde.konsole", "Konsole (otro escritorio)", false),
    ];

    let sin = render_rail(&hal, &mut renderer, false, &wins);
    let con = render_rail(&hal, &mut renderer, true, &wins);

    let diff = sin
        .chunks_exact(4)
        .zip(con.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count();
    let total = (W * H) as usize;
    println!("rail_zen_shot: {diff} px distintos de {total} ({:.1}%)", 100.0 * diff as f64 / total as f64);
    // Umbral: 2 celdas de escritorio (30×26) + «+» (30×18) + 2 chips de tab
    // (~32×32) rondan >3000 px. Si el grupo no se pinta, el diff es 0.
    assert!(diff > 1500, "el grupo zen no aparece en el rail (diff {diff} px)");
    println!("rail_zen_shot: OK — el rail pinta escritorios ocupados + «+» + tabs");

    escribir_png(&con, &out);
    println!("rail_zen_shot: {out} ({W}x{H})");
}

/// Lee la textura a un buffer RGBA plano (sin padding de filas).
fn leer_pixels(hal: &Hal, target: &wgpu::Texture) -> Vec<u8> {
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
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rb") });
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
    hal.queue.submit(Some(enc.finish()));
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = hal.device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range();
    let mut out = Vec::with_capacity(unpadded * H as usize);
    for fila in data.chunks_exact(padded) {
        out.extend_from_slice(&fila[..unpadded]);
    }
    out
}

fn escribir_png(rgba: &[u8], path: &str) {
    let file = std::fs::File::create(path).expect("crear png");
    let w = std::io::BufWriter::new(file);
    let mut enc = png::Encoder::new(w, W, H);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("header png");
    writer.write_image_data(rgba).expect("data png");
}
