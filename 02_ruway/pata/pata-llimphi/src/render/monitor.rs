//! Render del **monitor de sistema** (diente `monitor`/`sistema` del sidebar):
//! CPU (promedio + por core) y RAM, reusando los panels del quick-settings
//! (`cpu_panel`/`ram_panel`). Es el primer paso del control center de sistema +
//! flota; a futuro suma las unidades de sandokan y la flota de matilda.

use llimphi_theme::{Color, Theme};
use llimphi_ui::llimphi_layout::taffy::prelude::{length, percent, AlignItems, FlexDirection, Size, Style};
use llimphi_ui::llimphi_layout::taffy::Rect as TaffyRect;
use llimphi_ui::llimphi_raster::vello::Scene;
use llimphi_ui::{PaintRect, View};

use pata_core::widget::{WidgetCtx, MAX_CORES};

use super::panels::{cpu_panel_body, panel_box_flow, ram_panel_body};
use crate::Msg;

/// Cuántas barras dibuja el diente monitor (los cores se reparten en cubetas).
const MON_BARS: usize = 5;
/// Umbral de CPU caliente para el modo "alarma" del diente.
const MON_HOT: f32 = 0.85;
/// Umbral de presión de RAM.
const MON_RAM_HI: f32 = 0.85;

/// El panel del monitor de sistema, de alto completo. Apila el panel de CPU
/// (promedio + cores) y el de RAM (barra + total/usado/libre), ambos reusados del
/// quick-settings de la barra.
pub fn sistema_monitor_view(ctx: &WidgetCtx, panel_h: f32, theme: &Theme) -> View<Msg> {
    let titulo = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(24.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text("Sistema".to_string(), 14.0, theme.fg_text);

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: length(panel_h) },
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(8.0_f32),
            top: length(10.0_f32),
            bottom: length(10.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(10.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .children(vec![
        titulo,
        panel_box_flow(cpu_panel_body(ctx, theme), theme),
        panel_box_flow(ram_panel_body(ctx, theme), theme),
    ])
}

// =====================================================================
// Diente monitor: canvas animado e inteligente en el rail
// =====================================================================

/// El icono **vivo** del diente monitor: un ecualizador de carga por core (los
/// cores se reparten en [`MON_BARS`] cubetas), coloreado verde→ámbar→rojo, con un
/// strip de RAM abajo y un énfasis **inteligente** según el estado: halo rojo que
/// late si la CPU está caliente (carga alta), ámbar si la RAM está bajo presión,
/// y calmo (respiración lenta) en reposo. `t` es el reloj monotónico del rail.
pub fn monitor_vivo_view(ctx: &WidgetCtx, t: f64, size: f32, theme: &Theme) -> View<Msg> {
    // Reparte los cores en MON_BARS cubetas (promedio de cada cubeta).
    let n = (ctx.cpu_cores_n as usize).min(MAX_CORES).max(1);
    let mut barras = [0.0_f32; MON_BARS];
    for (b, slot) in barras.iter_mut().enumerate() {
        let lo = b * n / MON_BARS;
        let hi = ((b + 1) * n / MON_BARS).max(lo + 1).min(n);
        let mut acc = 0.0;
        let mut cnt = 0;
        for c in lo..hi {
            acc += ctx.cpu_cores[c].clamp(0.0, 1.0);
            cnt += 1;
        }
        *slot = if cnt > 0 { acc / cnt as f32 } else { ctx.cpu };
    }
    let cpu = ctx.cpu.clamp(0.0, 1.0);
    let ram = ctx.ram.clamp(0.0, 1.0);
    let accent = theme.accent;
    View::new(Style {
        size: Size { width: length(size), height: length(size) },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| pintar_monitor(scene, rect, &barras, cpu, ram, t, accent))
}

fn rgba(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgba8(r, g, b, 255)
}

/// Lerp RGB de dos colores (`k` en 0..1).
fn lerp(a: Color, b: Color, k: f32) -> Color {
    let k = k.clamp(0.0, 1.0);
    let (ca, cb) = (a.components, b.components);
    Color::from_rgba8(
        ((ca[0] + (cb[0] - ca[0]) * k) * 255.0) as u8,
        ((ca[1] + (cb[1] - ca[1]) * k) * 255.0) as u8,
        ((ca[2] + (cb[2] - ca[2]) * k) * 255.0) as u8,
        255,
    )
}

/// Color de una carga `f` 0..1: verde → ámbar → rojo.
fn color_carga(f: f32) -> Color {
    let verde = rgba(0x4A, 0xDE, 0x80);
    let ambar = rgba(0xFB, 0xBF, 0x24);
    let rojo = rgba(0xF8, 0x71, 0x71);
    if f < 0.5 {
        lerp(verde, ambar, f / 0.5)
    } else {
        lerp(ambar, rojo, (f - 0.5) / 0.5)
    }
}

#[allow(clippy::too_many_arguments)]
fn pintar_monitor(scene: &mut Scene, rect: PaintRect, barras: &[f32], cpu: f32, ram: f32, t: f64, accent: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, RoundedRect};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return;
    }
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    let hot = cpu >= MON_HOT;
    let ram_hi = ram >= MON_RAM_HI;

    // Énfasis inteligente: halo que late detrás. Rojo+rápido si caliente, ámbar si
    // la RAM aprieta, y un latido lento y tenue (acento) en reposo.
    let (halo_col, vel, base_a, amp_a) = if hot {
        (rgba(0xF8, 0x71, 0x71), 7.0, 0.16, 0.24)
    } else if ram_hi {
        (rgba(0xFB, 0xBF, 0x24), 5.0, 0.12, 0.18)
    } else {
        (accent, 1.6, 0.04, 0.07)
    };
    let breath = 0.5 + 0.5 * (t * vel).sin();
    let pad = h * 0.08;
    let halo = RoundedRect::new(x + pad, y + pad, x + w - pad, y + h - pad, h * 0.22);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        halo_col.with_alpha(base_a + amp_a * breath as f32),
        None,
        &halo,
    );

    // Ecualizador de cores (las barras laten con un shimmer suave por `t` para que
    // se lea vivo aún a carga estable).
    let n = barras.len().max(1);
    let inner = w * 0.74;
    let x0 = x + (w - inner) * 0.5;
    let gap = 1.5_f64;
    let bw = ((inner - gap * (n as f64 - 1.0)) / n as f64).max(1.0);
    let top = y + h * 0.16;
    let bot = y + h * 0.74; // deja lugar abajo para el strip de RAM
    let full = bot - top;
    for (i, &f) in barras.iter().enumerate() {
        let shimmer = 0.04 * (t * 3.0 + i as f64 * 1.3).sin() as f32;
        let v = (f + shimmer).clamp(0.05, 1.0) as f64;
        let bh = (v * full).max(1.5);
        let bx = x0 + i as f64 * (bw + gap);
        let by = bot - bh;
        let rr = RoundedRect::new(bx, by, bx + bw, bot, 1.0);
        scene.fill(Fill::NonZero, Affine::IDENTITY, color_carga(f), None, &rr);
    }

    // Strip de RAM abajo: ancho = fracción usada; ámbar/rojo si aprieta.
    let ry0 = y + h * 0.82;
    let ry1 = y + h * 0.90;
    let track = RoundedRect::new(x0, ry0, x0 + inner, ry1, (ry1 - ry0) * 0.5);
    scene.fill(Fill::NonZero, Affine::IDENTITY, accent.with_alpha(0.18), None, &track);
    let rw = inner * ram as f64;
    if rw > 0.5 {
        let col = if ram_hi { rgba(0xF8, 0x71, 0x71) } else { accent };
        let fill = RoundedRect::new(x0, ry0, x0 + rw, ry1, (ry1 - ry0) * 0.5);
        scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &fill);
    }
}
