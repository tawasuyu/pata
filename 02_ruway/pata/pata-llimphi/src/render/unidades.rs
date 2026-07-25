//! Render del diente **«Unidades»** (sandokan): estado + telemetría de las
//! unidades vivas del plano de control, en una lista read-only. Los datos llegan
//! por [`crate::unidades`] (arje-bus). Sin plano de control → aviso.

use llimphi_theme::{Color, Theme};
use rimay_localize::{t, t_args};
use llimphi_ui::llimphi_layout::taffy::prelude::{
    auto, length, percent, AlignItems, FlexDirection, JustifyContent, Size, Style,
};
use llimphi_ui::llimphi_layout::taffy::Rect as TaffyRect;
use llimphi_ui::llimphi_raster::vello::Scene;
use llimphi_ui::{PaintRect, View};

use sandokan_lifecycle::LifecycleState;
use sandokan_monitor_core::{MonitorSnapshot, UnitObservation};

use super::panels::panel_box_flow;
use crate::Msg;

/// Cuántos puntos de estado dibuja el diente vivo (las unidades de más se omiten).
const MAX_DOTS: usize = 9;

/// El panel de unidades, de alto completo: resumen (activas/total) + una fila por
/// unidad (punto de estado + etiqueta + telemetría). Aviso si no hay snapshot.
pub fn unidades_view(
    snap: Option<&MonitorSnapshot>,
    scroll: f32,
    panel_h: f32,
    theme: &Theme,
) -> View<Msg> {
    let titulo = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(24.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t("pata-units"), 14.0, theme.fg_text);

    let mut hijos = vec![titulo];
    match snap {
        Some(s) if !s.is_empty() => {
            let resumen = View::new(Style {
                size: Size { width: percent(1.0_f32), height: length(18.0_f32) },
                align_items: Some(AlignItems::Center),
                ..Default::default()
            })
            .text(
                t_args(
                    "pata-units-summary",
                    &[
                        ("active", s.running().to_string().into()),
                        ("total", s.len().to_string().into()),
                    ],
                ),
                12.0,
                theme.fg_muted,
            );
            hijos.push(resumen);

            let filas: Vec<View<Msg>> = s.units.iter().map(|u| fila(u, theme)).collect();
            hijos.push(panel_box_flow(filas, theme));
        }
        _ => {
            hijos.push(aviso(theme));
        }
    }

    // Alto estimado (para el scroll): título + resumen + una fila por unidad.
    let n = snap.map(|s| s.len()).unwrap_or(0);
    let content_len = 60.0 + n as f32 * 24.0;
    let inner = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: auto() },
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(10.0_f32),
            top: length(10.0_f32),
            bottom: length(10.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(10.0_f32) },
        ..Default::default()
    })
    .children(hijos);
    super::scroll_panel(inner, scroll, content_len, panel_h, theme)
}

fn rgba(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgba8(r, g, b, 255)
}

// =====================================================================
// Diente «Unidades» vivo: grilla de puntos de estado en el rail
// =====================================================================

/// El icono **vivo** del diente «Unidades»: una grilla de puntos (uno por unidad,
/// coloreado por su [`LifecycleState`]) con énfasis **inteligente** — halo rojo
/// que late si alguna unidad falló/murió, ámbar si hay pendientes, calmo si todas
/// corren. Sin snapshot, puntos tenues de marcador de posición. `t` es el reloj
/// monotónico del rail.
pub fn unidades_vivo_view(
    snap: Option<&MonitorSnapshot>,
    t: f64,
    size: f32,
    theme: &Theme,
) -> View<Msg> {
    let mut dots: Vec<Color> = Vec::new();
    let mut hay_falla = false;
    let mut hay_pend = false;
    if let Some(s) = snap {
        for u in s.units.iter().take(MAX_DOTS) {
            dots.push(estado_visual(&u.state).0);
        }
        hay_falla = s
            .units
            .iter()
            .any(|u| matches!(u.state, LifecycleState::Failed { .. } | LifecycleState::Killed));
        hay_pend = s.units.iter().any(|u| {
            matches!(u.state, LifecycleState::Pending | LifecycleState::Parked { .. })
        });
    }
    let muted = theme.fg_muted;
    let accent = theme.accent;
    View::new(Style {
        size: Size { width: length(size), height: length(size) },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| {
        pintar_unidades(scene, rect, &dots, hay_falla, hay_pend, t, accent, muted)
    })
}

#[allow(clippy::too_many_arguments)]
fn pintar_unidades(
    scene: &mut Scene,
    rect: PaintRect,
    dots: &[Color],
    hay_falla: bool,
    hay_pend: bool,
    t: f64,
    accent: Color,
    muted: Color,
) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle, RoundedRect};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return;
    }
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);

    // Halo de énfasis: rojo+rápido si hay falla, ámbar si hay pendientes, calmo si todo ok.
    let (col, vel, base_a, amp_a) = if hay_falla {
        (rgba(0xF8, 0x71, 0x71), 7.0, 0.16, 0.26)
    } else if hay_pend {
        (rgba(0xFB, 0xBF, 0x24), 4.5, 0.10, 0.16)
    } else {
        (accent, 1.6, 0.04, 0.07)
    };
    let breath = 0.5 + 0.5 * (t * vel).sin();
    let pad = h * 0.08;
    let halo = RoundedRect::new(x + pad, y + pad, x + w - pad, y + h - pad, h * 0.22);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        col.with_alpha(base_a + amp_a * breath as f32),
        None,
        &halo,
    );

    // Grilla 3×3 de puntos. Sin datos, 9 puntos tenues de marcador.
    let cols = 3usize;
    let n = if dots.is_empty() { MAX_DOTS } else { dots.len() };
    let rows = n.div_ceil(cols);
    let cell = (w.min(h) * 0.74) / cols as f64;
    let r = cell * 0.26;
    let grid_w = cols as f64 * cell;
    let grid_h = rows as f64 * cell;
    let ox = x + (w - grid_w) * 0.5 + cell * 0.5;
    let oy = y + (h - grid_h) * 0.5 + cell * 0.5;
    for i in 0..n {
        let c = i % cols;
        let row = i / cols;
        let cx = ox + c as f64 * cell;
        let cy = oy + row as f64 * cell;
        let color = dots.get(i).copied().unwrap_or_else(|| muted.with_alpha(0.35));
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &Circle::new((cx, cy), r));
    }
}

/// `(color del punto, rótulo)` del estado de ciclo de vida.
fn estado_visual(s: &LifecycleState) -> (Color, String) {
    let (color, key) = match s {
        LifecycleState::Running => (rgba(0x4A, 0xDE, 0x80), "pata-unit-running"),
        LifecycleState::Pending => (rgba(0xFB, 0xBF, 0x24), "pata-unit-pending"),
        LifecycleState::Parked { .. } => (rgba(0xFB, 0xBF, 0x24), "pata-unit-parked"),
        LifecycleState::Exited { .. } => (rgba(0x94, 0xA3, 0xB8), "pata-unit-exited"),
        LifecycleState::Failed { .. } => (rgba(0xF8, 0x71, 0x71), "pata-unit-failed"),
        LifecycleState::Killed => (rgba(0xF8, 0x71, 0x71), "pata-unit-killed"),
    };
    (color, t(key))
}

/// Una fila: punto de estado + etiqueta a la izquierda, telemetría a la derecha.
fn fila(u: &UnitObservation, theme: &Theme) -> View<Msg> {
    let (col, rotulo) = estado_visual(&u.state);
    let punto = View::new(Style {
        size: Size { width: length(8.0_f32), height: length(8.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(col)
    .radius(4.0);
    let etiqueta = View::new(Style {
        size: Size { width: auto(), height: length(20.0_f32) },
        align_items: Some(AlignItems::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .text(u.label.clone(), 12.0, theme.fg_text);
    // Telemetría: mem en MiB + cpu% (o el rótulo del estado si no hay corriendo).
    let detalle = match &u.telemetry {
        Some(tel) => format!("{} MiB · {:.0}%", tel.mem_bytes / (1024 * 1024), tel.cpu_pct),
        None => rotulo,
    };
    let der = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: percent(0.0_f32), height: length(20.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexEnd),
        ..Default::default()
    })
    .text(detalle, 11.0, theme.fg_muted);
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(22.0_f32) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(8.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![punto, etiqueta, der])
}

fn aviso(theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(60.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        padding: TaffyRect {
            left: length(12.0_f32),
            right: length(12.0_f32),
            top: length(8.0_f32),
            bottom: length(8.0_f32),
        },
        ..Default::default()
    })
    .text(t("pata-units-no-control"), 12.0, theme.fg_muted)
}
