//! Sonda headless del bug "vim en el drawer sale NEGRO" (metal, 2026-07-14):
//! arranca la shuma COMPLETA como la hospeda pata (live-wire, chromeless,
//! hosted_bar), tipea `vim` + Enter como el press_key de pata (`on_key` →
//! `update`), drena los efectos unos segundos y pinta `drawer_body_view_full`
//! a una textura. NO escribe imagen: imprime **estadísticas de píxeles**
//! (colores distintos, % del color dominante) + los gates del modelo
//! (pty vivo / alt-screen). Si el modelo dice alt-screen pero el render es
//! ~uniforme, el bug está en la VISTA; si pinta variado, el bug está en el
//! loop vivo de pata (no aquí).
//!
//! `cargo run -p pata-llimphi --example vim_drawer_probe --release`

use std::collections::HashMap;
use std::time::{Duration, Instant};

use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::taffy::prelude::{percent, FlexDirection, Size, Style};
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::Typesetter;
use llimphi_ui::{Key, KeyEvent, KeyState, Modifiers, NamedKey, View};

use pata_llimphi::shuma::drawer_body_view_full;

const W: u32 = 1280;
const H: u32 = 800;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn tecla_char(c: &str) -> KeyEvent {
    KeyEvent {
        key: Key::Character(c.into()),
        state: KeyState::Pressed,
        text: Some(c.to_string()),
        modifiers: Modifiers::default(),
        repeat: false,
    }
}

fn main() {
    let theme = llimphi_theme::Theme::dark();

    // ── 1. Modelo full como en pata ─────────────────────────────────────
    let (tx, rx) = std::sync::mpsc::channel::<shuma_shell_llimphi::Msg>();
    let tx = std::sync::Mutex::new(tx);
    let handle: llimphi_ui::Handle<shuma_shell_llimphi::Msg> =
        llimphi_ui::Handle::<()>::for_test().lift(move |m| {
            let _ = tx.lock().unwrap().send(m);
        });
    let mut full = shuma_shell_llimphi::new_model();
    full.chromeless = true;
    full.hosted_bar = true;
    if full.active_shell_state().is_none() {
        for i in 0..64 {
            full.active_session = i;
            if full.active_shell_state().is_some() {
                break;
            }
        }
    }
    assert!(full.active_shell_state().is_some(), "sin sesión shell");
    shuma_shell_llimphi::spawn_host_effects(&mut full, &handle);

    // ── 2. Tipear el comando (arg1, default `vim`) + Enter y drenar 6 s ─
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "vim".to_string());
    for c in cmd.chars() {
        let s = c.to_string();
        if let Some(m) = shuma_shell_llimphi::on_key(&full, &tecla_char(&s)) {
            full = shuma_shell_llimphi::update(full, m, &handle);
        }
    }
    let enter = KeyEvent {
        key: Key::Named(NamedKey::Enter),
        state: KeyState::Pressed,
        text: None,
        modifiers: Modifiers::default(),
        repeat: false,
    };
    if let Some(m) = shuma_shell_llimphi::on_key(&full, &enter) {
        full = shuma_shell_llimphi::update(full, m, &handle);
    }
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(6) {
        if let Ok(m) = rx.recv_timeout(Duration::from_millis(50)) {
            full = shuma_shell_llimphi::update(full, m, &handle);
        }
    }
    // `PROBE_HOLA=1`: ciclo interactivo completo — tipear «di hola» + Enter
    // por el MODO CONSOLA, esperar la respuesta, y volcar qué quedó en el
    // block (cosecha + asentamiento + secciones), con timestamps.
    if std::env::var("PROBE_HOLA").is_ok() {
        for c in "di hola".chars() {
            let s = c.to_string();
            if let Some(m) = shuma_shell_llimphi::on_key(&full, &tecla_char(&s)) {
                full = shuma_shell_llimphi::update(full, m, &handle);
            }
        }
        let enter2 = KeyEvent {
            key: Key::Named(NamedKey::Enter),
            state: KeyState::Pressed,
            text: None,
            modifiers: Modifiers::default(),
            repeat: false,
        };
        if let Some(m) = shuma_shell_llimphi::on_key(&full, &enter2) {
            full = shuma_shell_llimphi::update(full, m, &handle);
        }
        eprintln!("[{:?}] «di hola» enviado por consola", t0.elapsed());
        let mut ultimo_len = 0usize;
        let fin = Instant::now() + Duration::from_secs(20);
        while Instant::now() < fin {
            if let Ok(m) = rx.recv_timeout(Duration::from_millis(50)) {
                full = shuma_shell_llimphi::update(full, m, &handle);
            }
            let n = full
                .active_shell_state()
                .map(|s| s.output.len())
                .unwrap_or(0);
            if n != ultimo_len {
                eprintln!("[{:?}] block lines: {n}", t0.elapsed());
                ultimo_len = n;
            }
        }
        // Sonda directa del TuiSession: skin, screen vivo y cosecha manual —
        // para ver de qué eslabón cuelga el "no cosecha nada".
        if let Some(arc) = full.active_shell_state().and_then(|s| s.running.clone()) {
            if let Ok(mut g) = arc.lock() {
                if let Some(tui) = g.tui.as_mut() {
                    let screen = tui.parser.screen();
                    let (rows, cols) = screen.size();
                    eprintln!(
                        "── TUI skin={:?} {}x{} altscreen={} ──",
                        tui.skin,
                        rows,
                        cols,
                        screen.alternate_screen()
                    );
                    let mut caja = None;
                    for r in 0..rows {
                        if let Some(c) = screen.cell(r, 0) {
                            if c.contents().starts_with('╭') {
                                caja = Some(r);
                            }
                        }
                    }
                    eprintln!("   caja ╭ en fila: {caja:?} · cosechadas={} pre={}", tui.cosechadas, tui.pre_cosechadas);
                    eprintln!("   screen (últimas 8 filas):");
                    for r in rows.saturating_sub(8)..rows {
                        let t: String = (0..cols)
                            .filter_map(|c| screen.cell(r, c).map(|x| {
                                let s = x.contents();
                                if s.is_empty() { " ".into() } else { s }
                            }))
                            .collect();
                        eprintln!("   {r:2}|{}", t.trim_end());
                    }
                    let pre = tui.pre_cosechar_asentado();
                    let cos = tui.cosechar();
                    eprintln!("   pre_cosechar_asentado → {} filas · cosechar → {} filas", pre.len(), cos.len());
                }
            }
        }
        if let Some(st) = full.active_shell_state() {
            eprintln!("── output del block ({} líneas) ──", st.output.len());
            for l in st.output.iter().rev().take(25).collect::<Vec<_>>().into_iter().rev() {
                eprintln!("| {}", l.text);
            }
            let lineas: Vec<String> = st
                .output
                .iter()
                .filter(|l| l.stage.is_none())
                .map(|l| l.text.clone())
                .collect();
            match shuma_module_shell::sections::detect_claude(&lineas) {
                Some(secs) => {
                    eprintln!("── secciones ──");
                    for sec in secs {
                        eprintln!(
                            "  [{}] {} ({} items)",
                            if sec.title.is_empty() { "prosa" } else { "PLEG" },
                            if sec.title.is_empty() { "(visible)" } else { &sec.title },
                            sec.kind.count()
                        );
                    }
                }
                None => eprintln!("── sin secciones (render plano) ──"),
            }
        }
    }
    let st = full.active_shell_state().expect("shell");
    eprintln!(
        "modelo: pty_vivo={} altscreen={}",
        st.tiene_pty_vivo(),
        st.is_fullscreen_tui()
    );

    // ── 3. Pintar el cuerpo del drawer y sacar stats ────────────────────
    // `PROBE_WRAP=1` replica el envoltorio EXACTO de `shuma_open_view` (bar
    // arriba + body con alto fijo, clip y alpha de reveal + titlebar +
    // canvas_col + scrim) para bisecar contra el metal; sin la env queda el
    // cuerpo pelado (el caso que ya pintaba bien).
    let wrap = std::env::var("PROBE_WRAP").is_ok();
    let body_view: View<pata_llimphi::Msg> = if wrap {
        let bar_px = 40.0_f32;
        let drawer_h = (H as f32) - bar_px - 6.0;
        let reveal: f32 = 1.0;
        let titlebar = pata_llimphi::shuma::drawer_titlebar(
            &pata_llimphi::shuma::ShumaState { present: true, open: true, ..Default::default() },
            &theme,
        );
        let canvas_col = View::new(Style {
            flex_direction: FlexDirection::Column,
            flex_grow: 1.0,
            size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
            min_size: Size {
                width: llimphi_ui::llimphi_layout::taffy::prelude::auto(),
                height: llimphi_ui::llimphi_layout::taffy::prelude::length(0.0_f32),
            },
            ..Default::default()
        })
        .children(vec![drawer_body_view_full(&full, &theme)]);
        let body_inner = View::new(Style {
            flex_direction: FlexDirection::Column,
            size: Size {
                width: percent(1.0_f32),
                height: llimphi_ui::llimphi_layout::taffy::prelude::length(drawer_h),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .children(vec![titlebar, canvas_col]);
        let body = View::new(Style {
            size: Size {
                width: percent(1.0_f32),
                height: llimphi_ui::llimphi_layout::taffy::prelude::length(drawer_h * reveal),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .clip(true)
        .alpha(reveal)
        .children(vec![body_inner]);
        let bar = View::new(Style {
            size: Size {
                width: percent(1.0_f32),
                height: llimphi_ui::llimphi_layout::taffy::prelude::length(bar_px),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .fill(theme.bg_panel);
        let handle = View::new(Style {
            size: Size {
                width: percent(1.0_f32),
                height: llimphi_ui::llimphi_layout::taffy::prelude::length(6.0_f32),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        let scrim = {
            let mut st = Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: llimphi_ui::llimphi_layout::taffy::prelude::length(0.0_f32),
                },
                ..Default::default()
            };
            st.flex_grow = 1.0;
            View::new(st)
        };
        View::new(Style {
            flex_direction: FlexDirection::Column,
            size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
            ..Default::default()
        })
        .children(vec![bar, body, handle, scrim])
    } else {
        drawer_body_view_full(&full, &theme)
    };
    // `PROBE_CLIPS=N` anida N contenedores con clip(true) extra alrededor del
    // cuerpo — sonda del límite de profundidad de blend/clip de vello (la
    // hipótesis del "escena decapitada" en vivo: el árbol real anida más capas
    // que el probe y cruza el límite).
    let clips: usize = std::env::var("PROBE_CLIPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body_view = body_view;
    for _ in 0..clips {
        body_view = View::new(Style {
            size: Size {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            },
            ..Default::default()
        })
        .clip(true)
        .children(vec![body_view]);
    }
    let root: View<pata_llimphi::Msg> = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        ..Default::default()
    })
    .children(vec![body_view]);

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
        label: Some("vim-drawer-probe"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
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
    // `PROBE_TRANSPARENT=1` limpia con TRANSPARENT como el render vivo de pata
    // (surface Wayland); sin la env, clear opaco como el pantallazo clásico.
    let bg = if std::env::var("PROBE_TRANSPARENT").is_ok() {
        Color::from_rgba8(0, 0, 0, 0)
    } else {
        let [r, g, b, _] = theme.bg_app.components;
        Color::from_rgba8((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, 255)
    };
    renderer
        .render_to_view(&hal, &scene, &view, W, H, bg)
        .expect("render_to_view");

    // Readback + histograma.
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
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    hal.queue.submit(std::iter::once(enc.finish()));
    let slice = buf.slice(..);
    let (ptx, prx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = ptx.send(res);
    });
    let _ = hal.device.poll(wgpu::PollType::wait_indefinitely());
    prx.recv().expect("map").expect("map ok");
    let data = slice.get_mapped_range();
    let mut hist: HashMap<[u8; 4], u64> = HashMap::new();
    for row in 0..H as usize {
        let base = row * padded;
        for px in 0..W as usize {
            let o = base + px * 4;
            *hist
                .entry([data[o], data[o + 1], data[o + 2], data[o + 3]])
                .or_insert(0) += 1;
        }
    }
    let total = (W as u64) * (H as u64);
    let mut top: Vec<(_, _)> = hist.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1));
    eprintln!("pixeles: {total} · colores distintos: {}", hist.len());
    for (c, n) in top.iter().take(5) {
        eprintln!(
            "  color {:?}: {:.1}%",
            c,
            (**n as f64) * 100.0 / total as f64
        );
    }
    // Matar el PTY de vim para no dejar huérfanos.
    if let Some(arc) = full.active_shell_state().and_then(|s| s.running.clone()) {
        if let Ok(g) = arc.lock() {
            g.handle.kill();
        }
    }
}
