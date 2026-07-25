//! Widget `session`: el botón de sesión/energía. Un símbolo de power en la barra
//! que abre un menú con bloquear/suspender/reiniciar/apagar/cerrar sesión. Las
//! acciones disruptivas (reiniciar/apagar/cerrar sesión) piden confirmación
//! inline antes de ejecutarse.

use llimphi_theme::{elevation, radius, Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::llimphi_raster::vello::Scene;
use llimphi_ui::{PaintRect, Shadow, View};

use crate::{Msg, SessionAction};

/// Ancho del botón (px).
const BTN_W: f32 = 28.0;
/// Ancho del popup (px).
pub(super) const PANEL_W: f32 = 220.0;
/// Alto de una fila.
const ROW_H: f32 = 32.0;

/// El widget `session`: el botón de power. Click → menú.
pub fn session_view(theme: &Theme) -> View<Msg> {
    let color = theme.fg_text;
    View::new(Style {
        size: Size {
            width: length(BTN_W),
            height: percent(1.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .tooltip("Sesión y energía".to_string())
    .on_click(Msg::SessionToggle)
    .children(vec![View::new(Style {
        size: Size {
            width: length(16.0_f32),
            height: length(16.0_f32),
        },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| dibujar_power(scene, rect, color))])
}

/// Pinta el símbolo de power (arco con hueco arriba + línea vertical) en `rect`.
fn dibujar_power(scene: &mut Scene, rect: PaintRect, color: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Arc, Line, Point, Stroke, Vec2};
    let cx = rect.x as f64 + rect.w as f64 * 0.5;
    let cy = rect.y as f64 + rect.h as f64 * 0.55;
    let r = (rect.w.min(rect.h) as f64) * 0.36;
    let stroke = Stroke::new(1.8);
    // Arco de ~300° dejando un hueco arriba.
    let start = -std::f64::consts::FRAC_PI_2 + 0.6;
    let arc = Arc::new(Point::new(cx, cy), Vec2::new(r, r), start, std::f64::consts::PI * 1.78, 0.0);
    scene.stroke(&stroke, Affine::IDENTITY, color, None, &arc);
    // Línea vertical superior que entra por el hueco.
    let top = rect.y as f64 + rect.h as f64 * 0.12;
    scene.stroke(
        &stroke,
        Affine::IDENTITY,
        color,
        None,
        &Line::new(Point::new(cx, top), Point::new(cx, cy - r * 0.2)),
    );
}

/// El cuerpo del popup: lista de acciones, o un prompt de confirmación si
/// `confirm` está presente.
pub(super) fn session_panel(confirm: Option<SessionAction>, theme: &Theme) -> View<Msg> {
    let hijos: Vec<View<Msg>> = match confirm {
        Some(a) => confirm_rows(a, theme),
        None => {
            let mut filas = vec![titulo("Sesión", theme)];
            for a in SessionAction::ALL {
                filas.push(action_row(a, theme));
            }
            filas
        }
    };

    let (alpha, blur, dy) = elevation::E4;
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: length(PANEL_W),
            height: auto(),
        },
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(8.0_f32),
            bottom: length(8.0_f32),
        },
        gap: Size {
            width: length(0.0_f32),
            height: length(2.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(radius::LG)
    .shadow(Shadow {
        color: Color::from_rgba8(0, 0, 0, alpha),
        blur,
        dx: 0.0,
        dy,
        spread: 0.0,
    })
    .children(hijos)
}

fn titulo(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(22.0_f32),
        },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 12.0, theme.fg_muted)
}

/// Una fila de acción. Las disruptivas (reiniciar/apagar/cerrar sesión) abren la
/// **pantalla de confirmación fullscreen** —la misma del diente de sistema— en vez
/// del confirm inline; el resto (bloquear/suspender) se ejecutan directo.
fn action_row(a: SessionAction, theme: &Theme) -> View<Msg> {
    let msg = if a.needs_confirm() {
        Msg::ConfirmPedir(crate::ConfirmAccion::Session(a))
    } else {
        Msg::SessionRun(a)
    };
    fila(&a.label(), theme.fg_text, theme).on_click(msg)
}

/// El prompt de confirmación para una acción disruptiva.
fn confirm_rows(a: SessionAction, theme: &Theme) -> Vec<View<Msg>> {
    let pregunta = View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text(format!("¿{}?", a.label()), 13.0, theme.fg_text);

    let confirmar = fila(&a.label(), theme.accent, theme).on_click(Msg::SessionRun(a));
    let cancelar = fila("Cancelar", theme.fg_muted, theme).on_click(Msg::SessionCancel);
    vec![pregunta, confirmar, cancelar]
}

/// Una fila clickeable con etiqueta centrada a la izquierda.
fn fila(label: &str, color: Color, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .text(label.to_string(), 13.0, color)
}

/// El overlay completo para **winit**: scrim + panel anclado arriba a la derecha.
pub fn session_overlay(confirm: Option<SessionAction>, bar_h: f32, theme: &Theme) -> View<Msg> {
    let fila = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: percent(1.0_f32),
            height: auto(),
        },
        justify_content: Some(JustifyContent::FlexEnd),
        padding: TaffyRect {
            left: length(0.0_f32),
            right: length(8.0_f32),
            top: length(8.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(vec![session_panel(confirm, theme)]);

    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            top: length(bar_h),
            right: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        size: Size {
            width: percent(1.0_f32),
            height: auto(),
        },
        ..Default::default()
    })
    .on_click(Msg::SessionToggle)
    .children(vec![fila])
}
