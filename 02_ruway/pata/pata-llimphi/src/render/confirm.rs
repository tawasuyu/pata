//! La **pantalla de confirmación fullscreen** de las acciones disruptivas
//! (apagar / reiniciar / cerrar sesión / cambiar de contexto). Un scrim traslúcido
//! «sobre todo» + una tarjeta centrada con la pregunta y dos botones. Es render-only:
//! los botones emiten [`Msg::ConfirmAceptar`] / [`Msg::ConfirmCancelar`]; el scrim
//! (click fuera de la tarjeta) también cancela.

use llimphi_theme::{radius, Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::{Shadow, View};

use crate::{ConfirmAccion, Msg, SessionAction};

/// Ancho de la tarjeta (px).
const CARD_W: f32 = 380.0;
/// Alto de un botón (px).
const BTN_H: f32 = 40.0;

/// El overlay completo: scrim fullscreen (cancela al click) + tarjeta centrada. `w`/`h`
/// son el tamaño de la superficie sobre la que se pinta (pantalla en winit, surface del
/// menú en layer). El scrim cubre todo y atenúa el fondo — la confirmación es modal.
pub fn confirm_overlay_view(accion: &ConfirmAccion, w: f32, h: f32, theme: &Theme) -> View<Msg> {
    let card = tarjeta(accion, theme);
    // Scrim: cubre toda la superficie, atenúa, y al clickearse (fuera de la tarjeta)
    // cancela. La tarjeta va centrada encima; su propio `on_click` no burbujea a cancelar.
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            top: length(0.0_f32),
            right: auto(),
            bottom: auto(),
        },
        size: Size { width: length(w), height: length(h) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .fill(Color::from_rgba8(0, 0, 0, 150))
    .on_click(Msg::ConfirmCancelar)
    .children(vec![card])
}

/// La tarjeta: pregunta grande + línea de detalle + fila de botones [Cancelar][Verbo].
fn tarjeta(accion: &ConfirmAccion, theme: &Theme) -> View<Msg> {
    let destructiva = matches!(
        accion,
        ConfirmAccion::Session(SessionAction::Shutdown) | ConfirmAccion::Session(SessionAction::Reboot)
    );
    let acento = if destructiva {
        Color::from_rgba8(248, 113, 113, 255) // rojo: apagar/reiniciar
    } else {
        theme.accent
    };

    let pregunta = View::new(Style {
        size: Size { width: percent(1.0_f32), height: auto() },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text(accion.pregunta(), 18.0, theme.fg_text)
    .bold();

    let mut hijos = vec![pregunta];
    let detalle = accion.detalle();
    if !detalle.is_empty() {
        hijos.push(
            View::new(Style {
                size: Size { width: percent(1.0_f32), height: auto() },
                align_items: Some(AlignItems::Center),
                justify_content: Some(JustifyContent::Center),
                margin: TaffyRect {
                    left: length(0.0_f32),
                    right: length(0.0_f32),
                    top: length(6.0_f32),
                    bottom: length(0.0_f32),
                },
                ..Default::default()
            })
            .text(detalle.to_string(), 12.0, theme.fg_muted),
        );
    }
    hijos.push(botones(accion, acento, theme));

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(CARD_W), height: auto() },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(24.0_f32),
            right: length(24.0_f32),
            top: length(24.0_f32),
            bottom: length(20.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(radius::LG)
    .shadow(Shadow {
        color: Color::from_rgba8(0, 0, 0, 160),
        blur: 40.0,
        dx: 0.0,
        dy: 16.0,
        spread: 0.0,
    })
    // Un click sobre la tarjeta NO cancela (sólo el scrim): re-emitimos un no-op para
    // que el evento no burbujee al scrim de atrás.
    .on_click(Msg::NahualAnim)
    .children(hijos)
}

/// La fila de botones: [Cancelar] (secundario) + [Verbo] (acento/rojo).
fn botones(accion: &ConfirmAccion, acento: Color, theme: &Theme) -> View<Msg> {
    let cancelar = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: length(0.0_f32), height: length(BTN_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .fill(theme.bg_panel_alt)
    .hover_fill(theme.bg_button_hover)
    .radius(8.0)
    .on_click(Msg::ConfirmCancelar)
    .text("Cancelar".to_string(), 14.0, theme.fg_text);

    let aceptar = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: length(0.0_f32), height: length(BTN_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .fill(acento)
    .hover_fill(theme.bg_button_hover)
    .radius(8.0)
    .on_click(Msg::ConfirmAceptar)
    .text(accion.verbo(), 14.0, theme.bg_panel);

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(BTN_H) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(10.0_f32), height: length(0.0_f32) },
        margin: TaffyRect {
            left: length(0.0_f32),
            right: length(0.0_f32),
            top: length(20.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(vec![cancelar, aceptar])
}
