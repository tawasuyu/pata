//! Volcado headless del **chrome del rag sidebar** (refino 2026-07): la barra de
//! chrome (buscador + control mutable), el cabezal uniforme de panel, el buscador
//! jerárquico resaltando un diente, y el popover multiswitch de disposición.
//!
//! Tres drawers lado a lado sobre el mismo sidebar de muestra:
//!  1. Nav con búsqueda «main» → resalta el diente que matchea + expande el árbol.
//!  2. Control center (diente vivo) con su cabezal uniforme.
//!  3. El popover multiswitch DESPLEGADO (opciones excluyentes agrupadas).
//!
//! `cargo run -p pata-llimphi --example sidebar_shot -- [out.png]`

use std::fs::File;
use std::io::BufWriter;

use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::taffy::prelude::{length, percent, FlexDirection, Size, Style};
use llimphi_ui::llimphi_layout::taffy::{AlignItems, Rect as TaffyRect};
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::Typesetter;
use llimphi_ui::View;

use llimphi_widget_navigator::{NavKind, NavNode};
use pata_core::config::{Anchor, SidebarTab, Surface, TabsSource, WidgetSpec};
use pata_core::widget::{ClockReading, WidgetCtx};
use pata_llimphi::render::{sidebar_drawer_view, CentroDatos, ControlExtras};
use pata_llimphi::rag::RagState;
use pata_llimphi::nouser::NavState;
use pata_llimphi::shuma::ShumaState;
use pata_llimphi::toplevel::WindowEntry;
use pata_llimphi::Msg;

const W: u32 = 1380;
const H: u32 = 620;
const DW: f32 = 320.0;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "sidebar_shot.png".to_string());
    let theme = llimphi_theme::Theme::tawa();

    // Sidebar de muestra: dientes Archivos (nav), Correo (rag), Control (vivo).
    let mut surface = Surface::sidebar(Anchor::Left);
    surface.panel_width = DW;
    surface.tabs = vec![
        SidebarTab::new("files", "Archivos", WidgetSpec::new("navigator")),
        SidebarTab::new("rag", "Correo", WidgetSpec::new("rag")),
        SidebarTab::new("control", "Control", WidgetSpec::new("control")),
    ];

    // Contexto de sistema para el control center.
    let mut ctx = WidgetCtx::default();
    ctx.clock = ClockReading { year: 2026, month: 7, day: 7, weekday: 1, hour: 9, minute: 41, second: 0 };
    ctx.volume = 0.6;
    ctx.brightness = 0.8;
    let extras = ControlExtras { battery: Some((80, true)), ..Default::default() };
    // Ventanas de muestra en el escritorio 2 (para el sidebar de workspaces).
    let win = |id: u32, label: &str, app: &str, active: bool| WindowEntry {
        id,
        label: label.to_string(),
        app_id: app.to_string(),
        workspace: 2,
        active,
        minimized: false,
        tab: 0,
    };
    let windows = vec![win(1, "pluma — borrador", "pluma", true), win(2, "cosmos", "cosmos", false)];
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

    // Bosque de muestra para el navegador.
    let roots = || {
        vec![
            NavNode::branch(
                1,
                "src",
                NavKind::Monad,
                vec![
                    NavNode::leaf(11, "lib.rs", NavKind::File),
                    NavNode::leaf(12, "main.rs", NavKind::File),
                    NavNode::leaf(13, "render.rs", NavKind::File),
                ],
            ),
            NavNode::leaf(2, "README.md", NavKind::File),
        ]
    };

    // --- Drawer 1: nav con búsqueda «main» (resalta el diente Archivos) ---
    let mut nav1 = NavState::default();
    nav1.roots = roots();
    nav1.open.insert(String::new(), (0, 0));
    nav1.active.insert(String::new(), (0, 0));
    nav1.search = "main".into();
    nav1.apply_search();
    let d1 = drawer(&surface, 0, &nav1, &shuma, &centro, &theme);

    // --- Drawer 2: control center (diente vivo) con cabezal uniforme ---
    let mut nav2 = NavState::default();
    nav2.roots = roots();
    nav2.open.insert(String::new(), (0, 2));
    nav2.active.insert(String::new(), (0, 2));
    let d2 = drawer(&surface, 2, &nav2, &shuma, &centro, &theme);

    // --- Drawer 3: popover multiswitch desplegado ---
    let mut nav3 = NavState::default();
    nav3.roots = roots();
    nav3.open.insert(String::new(), (0, 0));
    nav3.active.insert(String::new(), (0, 0));
    nav3.control_open = true;
    let d3 = drawer(&surface, 0, &nav3, &shuma, &centro, &theme);

    // --- Drawer 4: sidebar de WORKSPACES (taskbar) — antes se saltaba el header ---
    let mut ws_surface = Surface::sidebar(Anchor::Left);
    ws_surface.panel_width = DW;
    ws_surface.tabs_source = TabsSource::Workspaces;
    let mut nav4 = NavState::default();
    nav4.open.insert(String::new(), (0, 2)); // escritorio 2
    let d4 = drawer(&ws_surface, 2, &nav4, &shuma, &centro, &theme);

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
        col("Búsqueda «main»", d1, &theme),
        col("Control center", d2, &theme),
        col("Multiswitch", d3, &theme),
        col("Workspaces", d4, &theme),
    ]);

    render_png(root, &out);
    eprintln!("sidebar_shot: {out} ({W}x{H})");
}

/// Un drawer del sidebar de muestra (`ti` = diente abierto).
fn drawer(
    surface: &Surface,
    ti: usize,
    nav: &NavState,
    shuma: &ShumaState,
    centro: &CentroDatos,
    theme: &llimphi_theme::Theme,
) -> View<Msg> {
    let rag = RagState::default();
    sidebar_drawer_view(
        surface, 0, "", ti, DW, 540.0, nav, shuma, &rag, centro,
        /*docked*/ true, /*rail_outside*/ false, /*autohide*/ false, /*dos_pasos*/ true,
        /*t*/ 0.55, theme,
    )
}

/// Rotula un drawer con un título arriba.
fn col(titulo: &str, d: View<Msg>, theme: &llimphi_theme::Theme) -> View<Msg> {
    let rot = View::new(Style {
        size: Size { width: length(DW), height: length(24.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(titulo.to_string(), 13.0, theme.fg_muted);
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(DW), height: percent(1.0_f32) },
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
        label: Some("sidebar-shot"),
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
