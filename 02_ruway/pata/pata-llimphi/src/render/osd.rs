//! OSD (on-screen display): el cartel transitorio que aparece al cambiar el
//! volumen o el brillo —un ícono + una barra de nivel— y se desvanece solo.
//!
//! Se dispara desde las interacciones que pata **conoce** (rueda/slider/mute del
//! volumen y brillo), con el valor optimista (pata sabe el paso de 5%; el próximo
//! muestreo corrige). Las teclas multimedia globales las maneja el compositor, no
//! pata (Regla 2), así que ésas quedan fuera de alcance.
//!
//! En winit es un overlay absoluto centrado abajo; en layer-shell, una surface
//! `Overlay` dedicada (como el tooltip).

use std::time::{Duration, Instant};

use llimphi_theme::{Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::llimphi_raster::vello::Scene;
use llimphi_ui::{PaintRect, View};

use crate::Msg;

/// Cuánto dura el cartel en pantalla.
const VIDA: Duration = Duration::from_millis(1300);
/// Ancho/alto del cartel (px).
pub const OSD_W: u32 = 240;
pub const OSD_H: u32 = 60;

/// Qué controla el OSD vigente.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsdKind {
    /// Volumen del sink por defecto.
    Volume,
    /// Brillo de la pantalla.
    Brightness,
}

/// El cartel vigente: qué, qué nivel, y hasta cuándo mostrarlo.
#[derive(Clone, Copy, Debug)]
pub struct Osd {
    /// Qué controla.
    pub kind: OsdKind,
    /// Nivel `0..1`.
    pub level: f32,
    /// `true` si está silenciado (sólo volumen).
    pub muted: bool,
    /// Instante hasta el que se muestra.
    pub until: Instant,
}

impl Osd {
    /// Un cartel nuevo, visible por [`VIDA`] desde ahora.
    pub fn flash(kind: OsdKind, level: f32, muted: bool) -> Self {
        Self {
            kind,
            level: level.clamp(0.0, 1.0),
            muted,
            until: Instant::now() + VIDA,
        }
    }

    /// `true` si ya pasó su tiempo (hay que ocultarlo).
    pub fn expired(&self) -> bool {
        Instant::now() >= self.until
    }
}

/// El cuerpo del cartel: ícono + barra de nivel, en una pastilla redondeada.
pub(super) fn osd_body(osd: &Osd, theme: &Theme) -> View<Msg> {
    let acento = theme.accent;
    let tenue = theme.bg_button;
    let kind = osd.kind;
    let muted = osd.muted;
    let nivel = if muted && kind == OsdKind::Volume { 0.0 } else { osd.level };

    let icono = View::new(Style {
        size: Size { width: length(22.0_f32), height: length(22.0_f32) },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| dibujar_icono(scene, rect, kind, muted, acento));

    let relleno = View::new(Style {
        size: Size { width: percent(nivel), height: length(8.0_f32) },
        ..Default::default()
    })
    .fill(acento)
    .radius(4.0);
    let pista = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: percent(1.0_f32), height: length(8.0_f32) },
        ..Default::default()
    })
    .fill(tenue)
    .radius(4.0)
    .children(vec![relleno]);
    let pista_wrap = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: percent(1.0_f32), height: length(22.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .children(vec![pista]);

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        gap: Size { width: length(12.0_f32), height: length(0.0_f32) },
        padding: TaffyRect {
            left: length(16.0_f32),
            right: length(18.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(12.0)
    .children(vec![icono, pista_wrap])
}

/// Pinta el ícono: ♪ (volumen) o ☀ (brillo), a mano. El volumen mudo lleva una
/// onda tachada.
fn dibujar_icono(scene: &mut Scene, rect: PaintRect, kind: OsdKind, muted: bool, color: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, BezPath, Circle, Line, Point, Stroke};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    let cx = x + w * 0.5;
    let cy = y + h * 0.5;
    match kind {
        OsdKind::Brightness => {
            // Sol: disco + rayos.
            let r = h * 0.20;
            scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &Circle::new(Point::new(cx, cy), r));
            let stroke = Stroke::new(1.6);
            for i in 0..8 {
                let a = std::f64::consts::PI * 2.0 * i as f64 / 8.0;
                let (c, s) = (a.cos(), a.sin());
                scene.stroke(
                    &stroke,
                    Affine::IDENTITY,
                    color,
                    None,
                    &Line::new(
                        Point::new(cx + c * r * 1.5, cy + s * r * 1.5),
                        Point::new(cx + c * r * 2.1, cy + s * r * 2.1),
                    ),
                );
            }
        }
        OsdKind::Volume => {
            // Altavoz: rectángulo + triángulo.
            let mut p = BezPath::new();
            p.move_to(Point::new(x + w * 0.22, cy - h * 0.12));
            p.line_to(Point::new(x + w * 0.34, cy - h * 0.12));
            p.line_to(Point::new(x + w * 0.5, cy - h * 0.26));
            p.line_to(Point::new(x + w * 0.5, cy + h * 0.26));
            p.line_to(Point::new(x + w * 0.34, cy + h * 0.12));
            p.line_to(Point::new(x + w * 0.22, cy + h * 0.12));
            p.close_path();
            scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &p);
            if muted {
                scene.stroke(
                    &Stroke::new(1.8),
                    Affine::IDENTITY,
                    color,
                    None,
                    &Line::new(Point::new(x + w * 0.58, y + h * 0.3), Point::new(x + w * 0.82, y + h * 0.7)),
                );
            } else {
                // Dos ondas.
                let stroke = Stroke::new(1.6);
                for k in 1..=2 {
                    let rr = h * 0.14 * k as f64;
                    let arc = llimphi_ui::llimphi_raster::kurbo::Arc::new(
                        Point::new(x + w * 0.56, cy),
                        llimphi_ui::llimphi_raster::kurbo::Vec2::new(rr, rr),
                        -std::f64::consts::FRAC_PI_3,
                        std::f64::consts::FRAC_PI_3 * 2.0,
                        0.0,
                    );
                    scene.stroke(&stroke, Affine::IDENTITY, color, None, &arc);
                }
            }
        }
    }
}

/// El cuerpo del OSD llenando su surface dedicada (**layer-shell**).
pub fn osd_surface_view(osd: &Osd, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(vec![osd_body(osd, theme)])
}

/// El overlay del OSD para **winit**: la pastilla centrada en el cuarto inferior.
pub fn osd_overlay(osd: &Osd, screen: (f32, f32), theme: &Theme) -> View<Msg> {
    let (sw, sh) = screen;
    let left = ((sw - OSD_W as f32) * 0.5).max(0.0);
    let top = (sh - OSD_H as f32 - 80.0).max(0.0);
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(left),
            top: length(top),
            right: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        size: Size { width: length(OSD_W as f32), height: length(OSD_H as f32) },
        ..Default::default()
    })
    .children(vec![osd_body(osd, theme)])
}
