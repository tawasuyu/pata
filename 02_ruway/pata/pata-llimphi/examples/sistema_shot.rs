//! Volcado headless del diente de **sistema/sesión**: el panel del diente (info
//! usuario+sistema, contexto pacha, acciones de energía) y la **pantalla de
//! confirmación fullscreen** que intercepta apagar/reiniciar/cerrar sesión.
//!
//! `cargo run -p pata-llimphi --example sistema_shot -- [out.png]`

use std::fs::File;
use std::io::BufWriter;

use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::taffy::prelude::{length, percent, FlexDirection, Position, Size, Style};
use llimphi_ui::llimphi_layout::taffy::{AlignItems, Rect as TaffyRect};
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::Typesetter;
use llimphi_ui::View;

use pata_core::config::{Anchor, SidebarTab, Surface, TabsSource, WidgetSpec};
use pata_core::widget::{ClockReading, WidgetCtx};
use pata_llimphi::nouser::NavState;
use pata_llimphi::rag::RagState;
use pata_llimphi::render::{confirm_overlay_view, sidebar_drawer_view, CentroDatos, ControlExtras};
use pata_llimphi::shuma::ShumaState;
use pata_llimphi::{ConfirmAccion, Msg, SessionAction};

const W: u32 = 1120;
const H: u32 = 560;
const DW: f32 = 320.0;
/// Espejo de `render::sidebar::FOOTER_BASE` (pub(crate)): el id del diente de footer.
const FOOTER_BASE: usize = 5_000_000;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "sistema_shot.png".to_string());
    let theme = llimphi_theme::Theme::tawa();

    // Sidebar izquierdo estilo mirada: escritorios arriba + diente sistema al fondo.
    let mut surface = Surface::sidebar(Anchor::Left);
    surface.panel_width = DW;
    surface.tabs_source = TabsSource::Workspaces;
    surface.footer_tabs = vec![SidebarTab::new("sesion", "Sistema", WidgetSpec::new("sesion"))];

    let mut ctx = WidgetCtx::default();
    ctx.clock = ClockReading { year: 2026, month: 7, day: 12, weekday: 0, hour: 9, minute: 41, second: 0 };
    ctx.cpu = 0.23;
    ctx.ram = 0.58;
    ctx.ram_used_mb = 9_412;
    ctx.ram_total_mb = 16_384;
    let extras = ControlExtras::default();
    let windows: Vec<pata_llimphi::toplevel::WindowEntry> = Vec::new();
    let centro = CentroDatos {
        ctx: &ctx,
        extras: &extras,
        media: None,
        net: None,
        net_password: None,
        bt: None,
        flota: None,
        flota_remoto: None,
        movil: None,
        matilda: None,
        unidades: None,
        windows: &windows,
        willay: &[],
    };
    let shuma = ShumaState::default();

    // Panel del diente sistema abierto (id de footer).
    let mut nav = NavState::default();
    // `open`/`active` son por-monitor (conector → diente); winit usa la clave "".
    nav.open.insert(String::new(), (0, FOOTER_BASE));
    nav.active.insert(String::new(), (0, FOOTER_BASE));
    let rag = RagState::default();
    let panel = sidebar_drawer_view(
        &surface, 0, "", FOOTER_BASE, DW, 500.0, &nav, &shuma, &rag, &centro,
        /*docked*/ true, /*rail_outside*/ false, /*autohide*/ false, /*dos_pasos*/ true,
        /*t*/ 0.55, &theme,
    );

    // Overlay de confirmación (apagar) sobre una caja del tamaño de la surface.
    let accion = ConfirmAccion::Session(SessionAction::Shutdown);
    let overlay_box = View::new(Style {
        position: Position::Relative,
        size: Size { width: length(560.0_f32), height: length(500.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_app)
    .children(vec![confirm_overlay_view(&accion, 560.0, 500.0, &theme)]);

    let root = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        align_items: Some(AlignItems::Start),
        padding: TaffyRect {
            left: length(16.0_f32),
            right: length(16.0_f32),
            top: length(16.0_f32),
            bottom: length(16.0_f32),
        },
        gap: Size { width: length(20.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_app)
    .children(vec![
        col("Diente sistema/sesión", panel, DW, &theme),
        col("Confirmación fullscreen", overlay_box, 560.0, &theme),
    ]);

    render_png(root, &out);
    eprintln!("sistema_shot: {out} ({W}x{H})");
}

fn col(titulo: &str, d: View<Msg>, w: f32, theme: &llimphi_theme::Theme) -> View<Msg> {
    let rot = View::new(Style {
        size: Size { width: length(w), height: length(24.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(titulo.to_string(), 13.0, theme.fg_muted);
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(w), height: percent(1.0_f32) },
        gap: Size { width: length(0.0_f32), height: length(8.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(vec![rot, d])
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
        label: Some("sistema-shot"),
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
        .render_to_view(&hal, &scene, &view, W, H, Color::from_rgba8(18, 17, 15, 255))
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
    hal.queue.submit(Some(enc.finish()));

    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = hal.device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((W * H * 4) as usize);
    for row in 0..H as usize {
        let start = row * padded;
        pixels.extend_from_slice(&data[start..start + unpadded]);
    }
    drop(data);
    buf.unmap();

    let file = File::create(path).expect("create png");
    let w = BufWriter::new(file);
    let mut enc = png::Encoder::new(w, W, H);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().expect("png header").write_image_data(&pixels).expect("png data");
}
