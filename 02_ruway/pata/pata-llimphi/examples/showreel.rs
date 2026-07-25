//! **Showreel** de pata — para r/rust · r/unixporn. NO es eye-candy abstracto:
//! es una vitrina de la **barra REAL** y sus **widgets vivos** del toolkit, en
//! acción. Cada frame reconstruye un árbol `View<Msg>` con las MISMAS funciones
//! de render que pinta la barra en producción (`pata_llimphi::render`): los
//! medidores (`widget_view_kinded`), el reloj, el switcher de escritorios
//! (`workspaces_view`), el visualizador de audio (`cava_view`), el clima
//! (`weather_view`), la fase lunar y el botón de inicio. El **estado** de cada
//! widget se deriva del tiempo normalizado `t∈[0,1]` — el espectro cava se
//! mueve, el switcher salta de escritorio, los medidores suben y bajan, la luna
//! cambia de fase. No se dibuja una barra falsa: si existe el render, se usa.
//!
//! Beats (timeline):
//!   1. cold-open: trazo bezier draw-on (firma).
//!   2. la barra superior hace slide-in con sus widgets vivos.
//!   3. widgets respiran (cava/medidores/switcher/luna por `t`).
//!   4. el menú de inicio (GNOME) se despliega; luego el control panel.
//!   5. cierre: wordmark «pata» + subtítulo, frame limpio para screenshot.
//!
//! Render headless y determinista (sin reloj, sin runtime, sin winit): frame
//! `i` de `N` → `t = i/(N-1)` → View → layout (taffy + parley) → vello::Scene →
//! wgpu → PNG. Idéntico al eventloop.
//!
//! ```text
//! cargo run -p pata-llimphi --example showreel --release -- \
//!     [out_dir] [n_frames] [W] [H]
//! ```
//! Defaults: `out_dir=showreel_frames_pata`, `n_frames=300`, `W=1600`, `H=900`.

use std::fs::{create_dir_all, File};
use std::io::BufWriter;

use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint, PaintRect};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::taffy::prelude::{
    auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style,
};
use llimphi_ui::llimphi_layout::taffy::Rect as TaffyRect;
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::{self, Color, Gradient};
use llimphi_ui::llimphi_raster::vello::kurbo::{Affine, BezPath, Circle, Point, Stroke};
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::{draw_layout_brush_xf, measurement, Alignment, Typesetter};
use llimphi_ui::View;

use llimphi_theme::{motion, Theme};

use app_bus::AppRegistry;
use pata_core::widget::{MeterOrient, MeterSize, WidgetView};
use pata_llimphi::render::{
    cava_view, control_overlay, start_button_view, start_menu_gnome_overlay, weather_view,
    widget_view_kinded, workspaces_view, ControlExtras, WsMonitores,
};
use pata_llimphi::weather::{Sky, Weather};
use pata_llimphi::Msg;

const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

// ───────────────────────── utilidades ─────────────────────────

fn with_alpha(c: Color, a: f32) -> Color {
    let [r, g, b, _] = c.components;
    Color::new([r, g, b, a.clamp(0.0, 1.0)])
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Reescala `t` desde el subintervalo `[lo,hi]` a `[0,1]`, clampado.
fn seg(t: f32, lo: f32, hi: f32) -> f32 {
    ((t - lo) / (hi - lo)).clamp(0.0, 1.0)
}

// ───────────────────────── tema / skin ─────────────────────────

#[derive(Clone)]
struct Skin {
    theme: Theme,
    accent: Color,
    bg: Color,
    bg_hi: Color,
    fg: Color,
    fg_muted: Color,
}

// ───────────────────────── estado vivo de los widgets ─────────────────────────

/// Un seno respirando entre `lo` y `hi` con `phase` (0..1) de desfase. `t` en
/// segundos-equivalente (lo derivamos de la timeline para que sea suave).
fn breathe(tt: f32, lo: f32, hi: f32, freq: f32, phase: f32) -> f32 {
    let s = ((tt * freq + phase) * std::f32::consts::TAU).sin() * 0.5 + 0.5;
    lo + (hi - lo) * s
}

/// El frame de cava (16 barras) derivado de `t`: un espectro que se mueve, con
/// graves más altos a la izquierda y un poco de ruido determinista.
fn cava_frame(t: f32) -> Vec<f32> {
    let n = 16usize;
    let tt = t * 6.0; // varios ciclos a lo largo del reel
    (0..n)
        .map(|i| {
            let f = i as f32 / (n as f32 - 1.0);
            // envolvente: graves a la izquierda decaen hacia agudos.
            let env = (1.0 - f * 0.55).powf(1.4);
            let a = breathe(tt, 0.1, 1.0, 0.9 + f * 1.7, f * 2.3);
            let b = breathe(tt, 0.0, 1.0, 1.7 + f * 0.6, f * 5.1 + 0.7);
            (env * (a * 0.7 + b * 0.3)).clamp(0.02, 1.0)
        })
        .collect()
}

/// Un medidor vivo (cpu/ram/vol/brillo) cuya fracción respira por `t`. Se usa
/// `Small` para un cluster apretado (sin la leyenda de ancho fijo que abría
/// huecos): glifo de cabecera + barrita, estilo waybar compacto.
fn meter(kind: &str, label: &str, frac: f32, s: &Skin) -> View<Msg> {
    let caption = format!("{:>2.0}%", frac * 100.0);
    let v = WidgetView::Meter {
        label: Some(label.to_string()),
        fraction: frac,
        caption,
        size: MeterSize::Small,
        orient: MeterOrient::Vertical,
    };
    widget_view_kinded(&v, Some(kind), &s.theme)
        .radius(6.0)
        .hover_fill(s.theme.bg_button_hover)
}

/// El racimo de cores (CpuCores), 8 núcleos, cada uno respira distinto.
fn cores(t: f32, s: &Skin) -> View<Msg> {
    let tt = t * 5.0;
    let fr: Vec<f32> = (0..8)
        .map(|i| breathe(tt, 0.08, 0.95, 1.0 + i as f32 * 0.35, i as f32 * 0.6))
        .collect();
    let avg = fr.iter().sum::<f32>() / fr.len() as f32;
    let v = WidgetView::Cores {
        label: Some("CPU".into()),
        fractions: fr,
        caption: format!("{:.0}% (8)", avg * 100.0),
        size: MeterSize::Small,
        orient: MeterOrient::Horizontal,
    };
    widget_view_kinded(&v, Some("cpu_cores"), &s.theme)
}

/// El reloj (avanza un minuto cada par de frames — legible, no real).
fn clock(t: f32, s: &Skin) -> View<Msg> {
    let total = (14 * 60 + 32) + (t * 18.0) as i32; // de 14:32 en adelante
    let hh = (total / 60) % 24;
    let mm = total % 60;
    let v = WidgetView::Text(format!("{hh:02}:{mm:02}"));
    widget_view_kinded(&v, Some("clock"), &s.theme)
}

/// La fase lunar (recorre el ciclo a lo largo del reel).
fn moon(t: f32, s: &Skin) -> View<Msg> {
    let phase = (0.18 + t as f32 * 0.6).fract();
    let v = WidgetView::Moon { phase, name: "Gibosa".into() };
    widget_view_kinded(&v, Some("moon"), &s.theme)
}

/// El switcher de escritorios: el activo salta 1→2→3→4 a lo largo del beat.
fn workspaces(t: f32, s: &Skin) -> View<Msg> {
    let count = 4u8;
    let active = ((seg(t, 0.0, 1.0) * 4.0).floor() as u8).clamp(0, 3) + 1;
    // ocupados: todos menos uno, para que se vea el realce tenue.
    let occupied = 0b0000_1011u16;
    // `others` (ocupados en otro monitor) = 0 y sin animación de cometa: el
    // showreel sólo muestra el realce plano del switcher.
    workspaces_view(active, count, occupied, 0, None, 4.0, FlexDirection::Row, &WsMonitores::default(), &s.theme)
}

// ───────────────────────── la barra real ─────────────────────────

/// Construye el cuerpo de la barra superior (slots start/center/end) con los
/// widgets REALES del render de pata, vivos por `t`.
fn bar_body(t: f32, cw: f64, s: &Skin) -> View<Msg> {
    let registry = AppRegistry::with_defaults();
    let apps = registry.all();
    let start_label = "⊞ pata";

    // START: botón de inicio + switcher de escritorios.
    let start = View::new(Style {
        flex_direction: FlexDirection::Row,
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(10.0), height: length(0.0) },
        flex_grow: 1.0,
        size: Size { width: percent(0.0), height: percent(1.0_f32) },
        justify_content: Some(JustifyContent::FlexStart),
        ..Default::default()
    })
    .children(vec![
        start_button_view(
            start_label,
            None,
            t,
            s.theme.accent,
            false,
            pata_llimphi::render::ChakanaForma::Chakana,
            &s.theme,
        ),
        workspaces(t, s),
    ]);

    // CENTER: el reloj, grande y centrado.
    let center = View::new(Style {
        flex_direction: FlexDirection::Row,
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        flex_grow: 1.0,
        size: Size { width: percent(0.0), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(vec![clock(t, s)]);

    // END: cluster de indicadores vivos — cava, cores, cpu/ram/vol/bri, clima, luna.
    let tt = t * 4.0;
    let cpu = breathe(tt, 0.12, 0.78, 0.7, 0.0);
    let ram = breathe(tt, 0.30, 0.66, 0.4, 1.3);
    let vol = breathe(tt, 0.20, 0.90, 0.9, 2.1);
    let bri = breathe(tt, 0.40, 0.95, 0.3, 3.4);
    let weather = Weather { temp_c: 17.0, sky: Sky::PartlyCloudy, desc: "Parcialmente nublado".into(), coords: None };
    let _ = &apps; // registro tocado (carga real), las apps van en el menú beat.

    let end = View::new(Style {
        flex_direction: FlexDirection::Row,
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexEnd),
        gap: Size { width: length(10.0), height: length(0.0) },
        flex_grow: 1.0,
        size: Size { width: percent(0.0), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(vec![
        cava_view(&cava_frame(t), &s.theme),
        cores(t, s),
        meter("cpu_meter", "CPU", cpu, s),
        meter("ram_meter", "RAM", ram, s),
        meter("volume", "VOL", vol, s),
        meter("brightness", "BRI", bri, s),
        weather_view(Some(&weather), None, &s.theme),
        moon(t, s),
    ]);

    // Cuerpo de la barra: fondo de panel con gradiente sutil + borde inferior.
    let body = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(16.0),
            right: length(16.0),
            top: length(0.0),
            bottom: length(0.0),
        },
        ..Default::default()
    })
    .fill_gradient(
        Gradient::new_linear(Point::new(0.0, 0.0), Point::new(0.0, 1.0))
            .with_stops([s.bg_hi, s.theme.bg_panel_alt].as_slice()),
    )
    .children(vec![start, center, end]);
    let _ = cw;
    body
}

// ───────────────────────── overlays vector (cold-open + wordmark) ─────────────────────────

fn signature_path(cw: f64, ch: f64) -> BezPath {
    let cx = cw / 2.0;
    let cy = ch / 2.0;
    let mut p = BezPath::new();
    p.move_to((cx - 360.0, cy + 40.0));
    p.curve_to(
        (cx - 150.0, cy - 220.0),
        (cx + 150.0, cy + 220.0),
        (cx + 360.0, cy - 40.0),
    );
    p
}

fn trim_path(full: &BezPath, prog: f64) -> (BezPath, Point) {
    use vello::kurbo::ParamCurve;
    let prog = prog.clamp(0.0, 1.0);
    let mut cubic = None;
    let mut start = Point::ZERO;
    for el in full.elements() {
        match el {
            vello::kurbo::PathEl::MoveTo(p) => start = *p,
            vello::kurbo::PathEl::CurveTo(c1, c2, p) => {
                cubic = Some(vello::kurbo::CubicBez::new(start, *c1, *c2, *p));
            }
            _ => {}
        }
    }
    let mut out = BezPath::new();
    let mut head = start;
    if let Some(cb) = cubic {
        out.move_to(cb.p0);
        let steps = 96;
        for i in 1..=steps {
            let u = (i as f64 / steps as f64) * prog;
            let pt = cb.eval(u);
            out.line_to(pt);
            head = pt;
        }
    }
    (out, head)
}

fn draw_overlays(scene: &mut vello::Scene, ts: &mut Typesetter, t: f32, cw: f64, ch: f64, s: &Skin) {
    // ── COLD OPEN (0–11%) ──────────────────────────────────────────
    let b1 = seg(t, 0.0, 0.11);
    let line_vis = 1.0 - seg(t, 0.11, 0.18);
    if line_vis > 0.001 {
        let path = signature_path(cw, ch);
        let draw_on = motion::ease_out_cubic(seg(t, 0.01, 0.12)) as f64;
        let (trimmed, head) = trim_path(&path, draw_on);
        let line_col = with_alpha(s.accent, 0.9 * line_vis);
        scene.stroke(&Stroke::new(2.0), Affine::IDENTITY, line_col, None, &trimmed);
        let pop = motion::ease_out_back(b1);
        let r = (4.0 + 7.0 * pop as f64).max(0.0);
        let dot_a = (b1 * line_vis).clamp(0.0, 1.0);
        scene.fill(
            peniko::Fill::NonZero,
            Affine::IDENTITY,
            with_alpha(s.accent, 0.18 * dot_a),
            None,
            &Circle::new(head, r * 3.2),
        );
        scene.fill(
            peniko::Fill::NonZero,
            Affine::IDENTITY,
            with_alpha(s.accent, dot_a),
            None,
            &Circle::new(head, r),
        );
    }

    // ── WORDMARK (84–100%) ─────────────────────────────────────────
    let word_in = seg(t, 0.86, 0.96);
    let word_a = motion::ease_out_cubic(word_in);
    if word_a > 0.001 {
        let size = 150.0_f32;
        let layout = ts.layout(
            "pata", size, None, Alignment::Start, 1.0, false, None, 800.0, false, false, 0.0, 0.0,
        );
        let m = measurement(&layout);
        let rise = lerp(24.0, 0.0, word_a as f64);
        let ox = (cw - m.width as f64) / 2.0;
        let oy = (ch - m.height as f64) / 2.0 - 18.0 + rise;
        let brush = peniko::Brush::Solid(with_alpha(s.fg, word_a));
        draw_layout_brush_xf(scene, &layout, &brush, Affine::translate((ox, oy)));

        let sub_a = motion::ease_out_cubic(seg(t, 0.90, 1.0));
        if sub_a > 0.001 {
            let ssz = 26.0_f32;
            let sub = ts.layout(
                "the Llimphi desktop bar", ssz, None, Alignment::Start, 1.0, false, None, 400.0,
                false, false, 0.0, 0.0,
            );
            let sm = measurement(&sub);
            let dot_r = 6.0;
            let block_w = sm.width as f64 + dot_r * 2.0 + 14.0;
            let sx = (cw - block_w) / 2.0;
            let sy = oy + m.height as f64 + 18.0;
            scene.fill(
                peniko::Fill::NonZero,
                Affine::IDENTITY,
                with_alpha(s.accent, sub_a),
                None,
                &Circle::new(Point::new(sx + dot_r, sy + ssz as f64 * 0.42), dot_r as f64),
            );
            let sbrush = peniko::Brush::Solid(with_alpha(s.fg_muted, sub_a));
            draw_layout_brush_xf(
                scene,
                &sub,
                &sbrush,
                Affine::translate((sx + dot_r * 2.0 + 14.0, sy)),
            );
        }
    }

    // ── punto teal de firma (esquina inf-der) ───────
    let corner_a = seg(t, 0.04, 0.12) * (1.0 - seg(t, 0.82, 0.88));
    if corner_a > 0.001 {
        let cx = cw - 54.0;
        let cy = ch - 54.0;
        scene.fill(
            peniko::Fill::NonZero,
            Affine::IDENTITY,
            with_alpha(s.accent, 0.16 * corner_a),
            None,
            &Circle::new(Point::new(cx, cy), 18.0),
        );
        scene.fill(
            peniko::Fill::NonZero,
            Affine::IDENTITY,
            with_alpha(s.accent, 0.9 * corner_a),
            None,
            &Circle::new(Point::new(cx, cy), 6.0),
        );
    }
}

// ───────────────────────── la escena por frame ─────────────────────────

fn build_view(t: f32, cw: f64, ch: f64, s: &Skin) -> View<Msg> {
    let bar_h = 48.0_f32;

    // Slide-in de la barra (12–22%) y fade-out antes del wordmark (82–88%).
    let slide = motion::ease_out_cubic(seg(t, 0.12, 0.24));
    let bar_alpha = (slide * (1.0 - seg(t, 0.82, 0.88))).clamp(0.0, 1.0);
    let bar_dy = lerp(-(bar_h as f64) - 8.0, 0.0, slide as f64);

    let mut children: Vec<View<Msg>> = Vec::new();

    if bar_alpha > 0.001 {
        let bar = View::new(Style {
            position: Position::Absolute,
            inset: TaffyRect {
                left: length(0.0),
                top: length(0.0),
                right: auto(),
                bottom: auto(),
            },
            size: Size { width: length(cw as f32), height: length(bar_h) },
            ..Default::default()
        })
        .transform(Affine::translate((0.0, bar_dy)))
        .alpha(bar_alpha)
        .children(vec![bar_body(t, cw, s)]);
        children.push(bar);
    }

    // ── BEAT MENÚ DE INICIO (GNOME) (32–52%) ───────────────────────
    let menu_in = motion::ease_out_cubic(seg(t, 0.32, 0.40));
    let menu_out = 1.0 - motion::ease_in_out_cubic(seg(t, 0.48, 0.53));
    let menu_a = (menu_in * menu_out).clamp(0.0, 1.0);
    if menu_a > 0.001 {
        let registry = AppRegistry::with_defaults();
        let apps = registry.all();
        let overlay = start_menu_gnome_overlay(apps, "", bar_h, (cw as f32, ch as f32), &s.theme);
        let scrim = View::new(Style {
            position: Position::Absolute,
            inset: TaffyRect {
                left: length(0.0),
                top: length(0.0),
                right: length(0.0),
                bottom: length(0.0),
            },
            size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
            ..Default::default()
        })
        .fill(with_alpha(s.theme.bg_app, 0.78 * menu_a as f32))
        .alpha(menu_a)
        .transform(Affine::translate((0.0, lerp(18.0, 0.0, menu_in as f64))))
        .children(vec![overlay]);
        children.push(scrim);
    }

    // ── BEAT CONTROL PANEL (quick settings) (58–78%) ───────────────
    let ctrl_in = motion::ease_out_cubic(seg(t, 0.58, 0.66));
    let ctrl_out = 1.0 - motion::ease_in_out_cubic(seg(t, 0.74, 0.79));
    let ctrl_a = (ctrl_in * ctrl_out).clamp(0.0, 1.0);
    if ctrl_a > 0.001 {
        let extras = ControlExtras { battery: Some((72, false)), wifi: true, bt: false, ..Default::default() };
        // Valores vivos (mismas ondas que la barra) para que el flyout respire.
        let tt = t * 4.0;
        let vol = breathe(tt, 0.20, 0.90, 0.9, 2.1);
        let bri = breathe(tt, 0.40, 0.95, 0.3, 3.4);
        let overlay = control_overlay(vol, false, bri, &extras, bar_h, (cw as f32, ch as f32), &s.theme);
        let wrap = View::new(Style {
            position: Position::Absolute,
            inset: TaffyRect {
                left: length(0.0),
                top: length(0.0),
                right: length(0.0),
                bottom: length(0.0),
            },
            size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
            ..Default::default()
        })
        .alpha(ctrl_a)
        .transform(Affine::translate((lerp(28.0, 0.0, ctrl_in as f64), 0.0)))
        .children(vec![overlay]);
        children.push(wrap);
    }

    // Overlay full-screen del vector (cold-open + wordmark).
    let overlay = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0),
            top: length(0.0),
            right: length(0.0),
            bottom: length(0.0),
        },
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .paint_with({
        let s = s.clone();
        move |scene, ts, _rect: PaintRect| {
            draw_overlays(scene, ts, t, cw, ch, &s);
        }
    });
    children.push(overlay);

    View::new(Style {
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        position: Position::Relative,
        ..Default::default()
    })
    .fill(s.bg)
    .children(children)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out_dir = args.next().unwrap_or_else(|| "showreel_frames_pata".to_string());
    let n: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(300);
    let w: u32 = args.next().and_then(|v| v.parse().ok()).unwrap_or(1600);
    let h: u32 = args.next().and_then(|v| v.parse().ok()).unwrap_or(900);
    create_dir_all(&out_dir).expect("mkdir out_dir");

    let theme = Theme::by_name("Tawa").expect("tema Tawa");
    let accent = theme.accent;
    let skin = Skin {
        accent,
        bg: theme.bg_app,
        bg_hi: {
            let [r, g, b, a] = theme.bg_panel_alt.components;
            Color::new([(r + 0.06).min(1.0), (g + 0.06).min(1.0), (b + 0.06).min(1.0), a])
        },
        fg: theme.fg_text,
        fg_muted: theme.fg_muted,
        theme,
    };
    let [br, bg, bb, _] = skin.bg.components;
    let base = Color::from_rgba8((br * 255.0) as u8, (bg * 255.0) as u8, (bb * 255.0) as u8, 255);

    let hal = pollster::block_on(Hal::new(None)).expect("hal");
    let mut renderer = Renderer::new(&hal).expect("renderer");
    let target = hal.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("showreel-pata"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
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

    let mut ts = Typesetter::new();
    let cw = w as f64;
    let ch = h as f64;

    for i in 0..n {
        let t = if n <= 1 { 0.0 } else { i as f32 / (n as f32 - 1.0) };
        let root = build_view(t, cw, ch, &skin);

        let mut layout = LayoutTree::new();
        let mounted = mount(&mut layout, root);
        let computed = {
            let tmap = &mounted.text_measures;
            layout
                .compute_with_measure(mounted.root, (w as f32, h as f32), |nid, known, avail| {
                    match tmap.get(&nid) {
                        Some(tm) => measure_text_node(&mut ts, tm, known, avail),
                        None => taffy::Size::ZERO,
                    }
                })
                .expect("layout")
        };
        let mut scene = vello::Scene::new();
        paint(&mut scene, &mounted, &computed, &mut ts, None, None);

        renderer
            .render_to_view(&hal, &scene, &view, w, h, base)
            .expect("render_to_view");
        let path = format!("{out_dir}/frame_{i:04}.png");
        write_png(&hal, &target, &path, w, h);
        if i % 30 == 0 || i == n - 1 {
            eprintln!("showreel-pata: frame {}/{} (t={:.3})", i + 1, n, t);
        }
    }
    eprintln!("showreel-pata: {n} frames en {out_dir}/ ({w}x{h})");
}

fn write_png(hal: &Hal, target: &wgpu::Texture, path: &str, w: u32, h: u32) {
    let unpadded = (w * 4) as usize;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded = unpadded.div_ceil(align) * align;
    let buf = hal.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * h as usize) as u64,
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
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
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
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for r in 0..h as usize {
        let sidx = r * padded;
        pixels.extend_from_slice(&data[sidx..sidx + unpadded]);
    }
    drop(data);
    buf.unmap();
    let file = File::create(path).expect("png");
    let mut enc = png::Encoder::new(BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().unwrap();
    wr.write_image_data(&pixels).unwrap();
}
