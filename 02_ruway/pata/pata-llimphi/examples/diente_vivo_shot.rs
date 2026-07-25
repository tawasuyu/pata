//! Volcado headless del **diente vivo** + el **control center**, para verlos sin
//! bootear el DM.
//!
//! - Izquierda: el panel del control center (reloj, «sonando ahora», volumen/
//!   brillo/batería, listas Wi-Fi/Bluetooth, perfil de energía, luz nocturna).
//! - Derecha: las manifestaciones del diente (reposo+halo, volumen, música, CPU
//!   caliente, batería) rendereadas grandes para ver el canvas.
//!
//! `cargo run -p pata-llimphi --example diente_vivo_shot -- [out.png]`

use std::fs::File;
use std::io::BufWriter;

use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::taffy::prelude::{length, percent, FlexDirection, Size, Style};
use llimphi_ui::llimphi_layout::taffy::{AlignItems, JustifyContent, Rect as TaffyRect};
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::Typesetter;
use llimphi_ui::View;

use pata_core::atencion::{EstadoBat, Manifestacion};
use pata_core::widget::{ClockReading, WidgetCtx};
use pata_llimphi::bluetooth::{BtDevice, BtState};
use pata_llimphi::mpris::MediaState;
use pata_llimphi::network::{NetState, NetStatus, WifiAp};
use matilda_core::{Container, Host, Inventory, RestartPolicy, VHost};
use llimphi_icons::{icon_view, Icon};
use llimphi_widget_dock_rail::{
    dock_rail_view_badged, BadgeKind, DockBadge, DockRailItem, DockRailPalette, DockRailSide,
};
use matilda_discover::{ContainerStatus, ObservedService, RunState};
use pata_llimphi::flota_discover::HostObs;
use pata_llimphi::render::{
    control_center_view, diente_vivo_view, flota_view, monitor_vivo_view, paint_reposo_halo,
    sistema_monitor_view, unidades_view, unidades_vivo_view, CentroDatos, ControlExtras, DienteVivo,
};
use pata_llimphi::Msg;
use sandokan_core::TelemetryFrame;
use sandokan_lifecycle::LifecycleState;
use sandokan_monitor_core::{MonitorSnapshot, UnitObservation};

const W: u32 = 1800;
const H: u32 = 820;
const SZ: f32 = 56.0;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "diente_vivo.png".to_string());
    let theme = llimphi_theme::Theme::default();

    // ---- Snapshot del sistema (alimenta control center + monitor) ----
    let mut ctx = WidgetCtx::default();
    ctx.clock = ClockReading { year: 2026, month: 6, day: 27, weekday: 5, hour: 14, minute: 32, second: 0 };
    ctx.volume = 0.55;
    ctx.muted = false;
    ctx.brightness = 0.80;
    ctx.cpu = 0.42;
    ctx.cpu_cores_n = 8;
    let cargas = [0.30_f32, 0.62, 0.18, 0.91, 0.44, 0.27, 0.55, 0.12];
    for (i, c) in cargas.iter().enumerate() {
        ctx.cpu_cores[i] = *c;
    }
    ctx.ram = 0.63;
    ctx.ram_used_mb = 10_320;
    ctx.ram_total_mb = 16_384;

    let extras = ControlExtras {
        battery: Some((72, false)),
        wifi: true,
        bt: true,
        power_profile: Some("balanced".to_string()),
        night: false,
        ..Default::default()
    };
    let media = MediaState {
        has_player: true,
        playing: true,
        title: "Mac DeMarco — Chamber of Reflection".to_string(),
    };
    let net = NetState {
        status: NetStatus::Wifi { ssid: "Hogar".to_string(), signal: 78 },
        wifi_enabled: true,
        networks: vec![
            WifiAp { ssid: "Hogar".to_string(), signal: 78, secure: true, active: true },
            WifiAp { ssid: "Vecino-5G".to_string(), signal: 47, secure: true, active: false },
            WifiAp { ssid: "Cafe_Libre".to_string(), signal: 30, secure: false, active: false },
        ],
        saved: Vec::new(),
        details: Default::default(),
    };
    let bt = BtState {
        available: true,
        powered: true,
        devices: vec![
            BtDevice { mac: "AA:BB".to_string(), name: "Auriculares".to_string(), connected: true, paired: true, battery: Some(70) },
            BtDevice { mac: "CC:DD".to_string(), name: "Mouse".to_string(), connected: false, paired: true, battery: None },
        ],
    };
    // Inventario de flota de muestra (matilda).
    let mut inv = Inventory::new();
    inv.add_host(Host::new("edge-1", "10.0.0.1").with_tag("prod").with_tag("edge"));
    inv.add_host(Host::new("db-1", "10.0.0.2").with_tag("prod").with_tag("db"));
    inv.add_container(
        Container::new("web", "nginx:1.27").with_port(8080, 80).with_restart(RestartPolicy::Always),
    );
    inv.add_container(
        Container::new("api", "ghcr.io/jl/api:1.0")
            .with_port(9000, 9000)
            .with_restart(RestartPolicy::UnlessStopped),
    );
    inv.add_container(Container::new("pg", "postgres:16").with_restart(RestartPolicy::Always));
    inv.add_vhost(VHost::to_container("jlsoltech.com", "web", 80).with_tls());

    // Estado real observado (discover SSH) de muestra.
    let cs = |name: &str, state: RunState, status: &str| ContainerStatus {
        name: name.to_string(),
        image: String::new(),
        state,
        status: status.to_string(),
        ports: String::new(),
    };
    let flota_remoto = vec![
        HostObs {
            name: "edge-1".to_string(),
            reachable: true,
            containers: vec![
                cs("web", RunState::Running, "Up 3 hours"),
                cs("api", RunState::Exited, "Exited (1) 2 min ago"),
            ],
            vhosts: vec!["jlsoltech.com".to_string()],
            services: vec![
                ObservedService { unit: "nginx.service".to_string(), enabled: true, active: true },
                ObservedService { unit: "docker.service".to_string(), enabled: true, active: true },
            ],
        },
        HostObs {
            name: "db-1".to_string(),
            reachable: false,
            containers: vec![],
            vhosts: vec![],
            services: vec![],
        },
    ];

    // Snapshot de unidades de muestra (sandokan).
    let tf = |mib: u64, cpu: f64| TelemetryFrame {
        card_id: ulid::Ulid::nil(),
        at: std::time::SystemTime::UNIX_EPOCH,
        mem_bytes: mib * 1024 * 1024,
        nproc: 4,
        cpu_pct: cpu,
        restarts: 0,
    };
    let uo = |label: &str, state: LifecycleState, tel: Option<TelemetryFrame>| UnitObservation {
        card_id: ulid::Ulid::nil(),
        label: label.to_string(),
        state,
        telemetry: tel,
        restarts: 0,
    };
    let unidades = MonitorSnapshot {
        units: vec![
            uo("mirada", LifecycleState::Running, Some(tf(248, 11.0))),
            uo("pata", LifecycleState::Running, Some(tf(36, 2.0))),
            uo("rimay-verbo", LifecycleState::Running, Some(tf(512, 0.4))),
            uo("paloma", LifecycleState::Pending, None),
            uo("roto", LifecycleState::Failed { reason: "exit 1".into() }, None),
        ],
    };

    let centro = CentroDatos {
        ctx: &ctx,
        extras: &extras,
        media: Some(&media),
        net: Some(&net),
        net_password: None,
        bt: Some(&bt),
        flota: Some(&inv),
        flota_remoto: Some(&flota_remoto),
        movil: None,
        matilda: None,
        unidades: Some(&unidades),
        windows: &[],
        willay: &[],
    };
    let panel = View::new(Style {
        size: Size { width: length(300.0_f32), height: length(H as f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(vec![control_center_view(H as f32, &centro, &theme)]);

    // ---- Monitor de sistema (centro) ----
    let monitor = View::new(Style {
        size: Size { width: length(340.0_f32), height: length(H as f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(vec![sistema_monitor_view(&ctx, H as f32, &theme)]);

    // ---- Unidades (sandokan) ----
    let unidades_col = View::new(Style {
        size: Size { width: length(300.0_f32), height: length(H as f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(vec![unidades_view(Some(&unidades), 0.0, H as f32, &theme)]);

    // ---- Flota (matilda) ----
    let flota_col = View::new(Style {
        size: Size { width: length(300.0_f32), height: length(H as f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(vec![flota_view(Some(&inv), Some(&flota_remoto), 0.0, H as f32, &theme)]);

    // ---- Manifestaciones del diente (derecha) ----
    let tiles = vec![
        tile("Reposo (halo)", reposo_view(&theme), &theme),
        tile("Volumen", manifest_view(Manifestacion::Volumen { frac: 0.6, muted: false }, &ctx, &theme), &theme),
        tile("Volumen mute", manifest_view(Manifestacion::Volumen { frac: 0.4, muted: true }, &ctx, &theme), &theme),
        tile("Música", manifest_view(Manifestacion::Musica, &ctx, &theme), &theme),
        tile("CPU caliente", manifest_view(Manifestacion::Cpu { carga: 0.92 }, &ctx, &theme), &theme),
        tile(
            "Batería baja",
            manifest_view(Manifestacion::Bateria { frac: 0.12, cargando: false, estado: EstadoBat::Baja }, &ctx, &theme),
            &theme,
        ),
        tile(
            "Cargando",
            manifest_view(Manifestacion::Bateria { frac: 0.85, cargando: true, estado: EstadoBat::Enchufada }, &ctx, &theme),
            &theme,
        ),
        // El diente monitor (vivo): ecualizador de cores + RAM + énfasis inteligente.
        tile("Monitor (diente)", monitor_vivo_view(&ctx, 0.55, SZ, &theme), &theme),
        // El diente unidades (vivo): grilla de puntos, late rojo porque «roto» falló.
        tile("Unidades (diente)", unidades_vivo_view(Some(&unidades), 0.55, SZ, &theme), &theme),
    ];
    let galeria = View::new(Style {
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        size: Size { width: percent(0.0_f32), height: percent(1.0_f32) },
        padding: TaffyRect {
            left: length(20.0_f32),
            right: length(16.0_f32),
            top: length(16.0_f32),
            bottom: length(16.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(12.0_f32) },
        ..Default::default()
    })
    .children(tiles);

    // ---- Rail derecho ESPEJADO con badges (Sistema/Unidades/Flota) ----
    let rail_items = vec![
        DockRailItem { id: 0, active: false },
        DockRailItem { id: 1, active: true },
        DockRailItem { id: 2, active: false },
    ];
    let rail = dock_rail_view_badged(
        &rail_items,
        46.0,
        DockRailSide::InnerRight,
        &DockRailPalette::from_theme(&theme),
        |id, size, _color| match id {
            0 => monitor_vivo_view(&ctx, 0.55, size, &theme),
            1 => unidades_vivo_view(Some(&unidades), 0.55, size, &theme),
            _ => View::new(Style {
                size: Size { width: length(size), height: length(size) },
                align_items: Some(AlignItems::Center),
                justify_content: Some(JustifyContent::Center),
                ..Default::default()
            })
            .children(vec![icon_view::<Msg>(Icon::Globe, theme.accent, 1.9)]),
        },
        |id| match id {
            1 => Some(DockBadge::Count(1, BadgeKind::Error)),   // 1 unidad fallada
            2 => Some(DockBadge::Count(1, BadgeKind::Warning)), // 1 host no alcanzable
            _ => None,
        },
        |_| Msg::DienteTick,
        |_| None,
    );
    let rail_col = View::new(Style {
        size: Size { width: length(60.0_f32), height: length(H as f32) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(0.0_f32),
            right: length(0.0_f32),
            top: length(6.0_f32),
            bottom: length(0.0_f32),
        },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(vec![rail]);

    let root = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_app)
    .children(vec![rail_col, panel, monitor, unidades_col, flota_col, galeria]);

    render_png(root, &out);
    eprintln!("diente_vivo_shot: {out} ({W}x{H})");
}

/// Una tarjeta: el canvas de la manifestación (en una caja) + su rótulo a la derecha.
fn tile(label: &str, canvas: View<Msg>, theme: &llimphi_theme::Theme) -> View<Msg> {
    let caja = View::new(Style {
        size: Size { width: length(SZ + 16.0), height: length(SZ + 16.0) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(theme.bg_panel_alt)
    .radius(10.0)
    .children(vec![canvas]);
    let rotulo = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: percent(0.0_f32), height: length(SZ + 16.0) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(label.to_string(), 14.0, theme.fg_text);
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(SZ + 16.0) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(14.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![caja, rotulo])
}

/// El canvas de una manifestación (no-reposo).
fn manifest_view(m: Manifestacion, ctx: &WidgetCtx, theme: &llimphi_theme::Theme) -> View<Msg> {
    let vivo = DienteVivo { manifest: m, cava_frame: &[], ctx, unidades: None, flota_remoto: None, windows: &[], terminal_sessions: &[], t: 0.55 };
    diente_vivo_view(&vivo, SZ, theme).unwrap_or_else(|| View::new(Style::default()))
}

/// El canvas de reposo: halo que respira detrás del icono Gauge.
fn reposo_view(theme: &llimphi_theme::Theme) -> View<Msg> {
    let accent = theme.accent;
    View::new(Style {
        size: Size { width: length(SZ), height: length(SZ) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| paint_reposo_halo(scene, rect, 1.0, accent))
    .children(vec![llimphi_icons::icon_view::<Msg>(llimphi_icons::Icon::Gauge, accent, 2.6)])
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
        label: Some("diente-vivo-shot"),
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
        .render_to_view(&hal, &scene, &view, W, H, Color::from_rgba8(20, 20, 28, 255))
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
