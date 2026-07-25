//! Certificación headless del **rail de dientes-workspace** (`TabsSource::Workspaces`,
//! DISENO-SHELL-NAVEGADOR §4-§5): renderiza el rail de un sidebar izquierdo con la
//! fuente de dientes en `workspaces` y confirma que pinta un diente por escritorio
//! ocupado. Reporta el conteo de píxeles distintos vs. un rail vacío como **stat de
//! texto** (Regla 8). El PNG queda en disco por si hace falta mirarlo.
//!
//! `cargo run -p pata-llimphi --example rail_workspaces_shot -- [salida.png]`

use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::Typesetter;
use llimphi_ui::View;

use pata_core::atencion::Manifestacion;
use pata_core::config::{Anchor, Surface, TabsSource};
use pata_core::widget::WidgetCtx;
use pata_llimphi::nouser::NavState;
use pata_llimphi::render;
use pata_llimphi::shuma::ShumaState;
use pata_llimphi::toplevel::WindowEntry;
use pata_llimphi::Msg;

const W: u32 = 64;
const H: u32 = 600;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn ventana(id: u32, ws: u8, app_id: &str, label: &str, active: bool) -> WindowEntry {
    WindowEntry { id, label: label.into(), app_id: app_id.into(), workspace: ws, active, minimized: false, tab: 0 }
}

fn render_rail(hal: &Hal, renderer: &mut Renderer, source: TabsSource, windows: &[WindowEntry]) -> Vec<u8> {
    let theme = llimphi_theme::Theme::dark();
    let mut surface = Surface::sidebar(Anchor::Left);
    surface.tabs_source = source; // el `tabs` estático queda vacío: si es Config el rail va pelado
    let mut ctx = WidgetCtx::default();
    ctx.active_workspace = 3;
    ctx.workspace_count = 9;
    ctx.workspace_occupied = 0b0010_0100; // escritorios 3 y 6 ocupados (bits 2 y 5)
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
            .compute_with_measure(mounted.root, (W as f32, H as f32), |nid, known, avail| match tmap.get(&nid) {
                Some(tm) => measure_text_node(&mut ts, tm, known, avail),
                None => taffy::Size::ZERO,
            })
            .expect("layout")
    };
    let mut scene = vello::Scene::new();
    paint(&mut scene, &mounted, &computed, &mut ts, None, None);

    let target = hal.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rail-ws-shot"),
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
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/rail_workspaces.png".to_string());
    let hal = pollster::block_on(Hal::new(None)).expect("hal");
    let mut renderer = Renderer::new(&hal).expect("renderer");

    let wins = [
        ventana(10, 3, "tullpu-app-llimphi", "Tullpu — lienzo", true),
        ventana(11, 3, "firefox", "Mozilla Firefox", false),
        ventana(12, 6, "org.kde.konsole", "Konsole (esc. 6)", false),
    ];

    // Config = tabs estáticos vacíos → rail pelado; Workspaces → dientes 1 y 3.
    let pelado = render_rail(&hal, &mut renderer, TabsSource::Config, &wins);
    let ws = render_rail(&hal, &mut renderer, TabsSource::Workspaces, &wins);

    let diff = pelado
        .chunks_exact(4)
        .zip(ws.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count();
    let total = (W * H) as usize;
    println!("rail_workspaces_shot: {diff} px distintos de {total} ({:.1}%)", 100.0 * diff as f64 / total as f64);
    // Dos dientes-workspace (~44×44 c/u con su nº + badge) rondan >2000 px. Si la
    // fuente `workspaces` no genera dientes, el diff es 0.
    assert!(diff > 800, "el rail de workspaces no pintó dientes (diff {diff} px)");
    println!("rail_workspaces_shot: OK — dientes de 3 y 6 + «+» intermedio (→4) + «+» final (→7)");

    escribir_png(&ws, &out);
    println!("rail_workspaces_shot: {out} ({W}x{H})");
}

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
    let mut enc = hal.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rb") });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded as u32), rows_per_image: Some(H) },
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
    enc.write_header().expect("png header").write_image_data(rgba).expect("png data");
}
