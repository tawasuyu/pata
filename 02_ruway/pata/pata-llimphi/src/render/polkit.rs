//! Diálogo de autenticación del agente **polkit**: muestra el mensaje que arma
//! polkit y un campo de contraseña (enmascarado) con Autenticar/Cancelar. Reusa
//! el patrón del campo de contraseña del applet de red (foco de teclado).

use llimphi_theme::{elevation, radius, Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::{Shadow, View};

use crate::Msg;

/// Ancho del diálogo (px).
pub(super) const PANEL_W: f32 = 360.0;
const ROW_H: f32 = 32.0;

/// El cuerpo del diálogo: mensaje + campo de contraseña + acciones.
pub(super) fn polkit_panel(message: &str, typed: &str, theme: &Theme) -> View<Msg> {
    let titulo = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(22.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text("Autenticación requerida".to_string(), 12.0, theme.fg_muted);

    let msg = View::new(Style {
        size: Size { width: percent(1.0_f32), height: auto() },
        align_items: Some(AlignItems::FlexStart),
        ..Default::default()
    })
    .text(recortar(message, 120), 13.0, theme.fg_text);

    let mostrado = if typed.is_empty() {
        "contraseña…".to_string()
    } else {
        "•".repeat(typed.chars().count())
    };
    let color = if typed.is_empty() { theme.fg_muted } else { theme.fg_text };
    let campo = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_button)
    .radius(6.0)
    .text(mostrado, 13.0, color);

    let autenticar = accion("Autenticar", theme.accent, theme).on_click(Msg::PolkitSubmit);
    let cancelar = accion("Cancelar", theme.fg_muted, theme).on_click(Msg::PolkitCancel);
    let botones = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        justify_content: Some(JustifyContent::FlexEnd),
        gap: Size { width: length(8.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![cancelar, autenticar]);

    let (a, blur, dy) = elevation::E4;
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(PANEL_W), height: auto() },
        padding: TaffyRect {
            left: length(16.0_f32),
            right: length(16.0_f32),
            top: length(14.0_f32),
            bottom: length(14.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(10.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(radius::LG)
    .shadow(Shadow {
        color: Color::from_rgba8(0, 0, 0, a),
        blur,
        dx: 0.0,
        dy,
        spread: 0.0,
    })
    .children(vec![titulo, msg, campo, botones])
}

fn accion(label: &str, color: Color, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: length(96.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .text(label.to_string(), 13.0, color)
}

fn recortar(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// El overlay del diálogo para **winit**: scrim oscuro a pantalla completa
/// (modal: el click afuera cancela) + el diálogo centrado.
pub fn polkit_overlay(message: &str, typed: &str, screen: (f32, f32), theme: &Theme) -> View<Msg> {
    let (sw, sh) = screen;
    let panel = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(((sw - PANEL_W) * 0.5).max(0.0)),
            top: length((sh * 0.3).max(0.0)),
            right: auto(),
            bottom: auto(),
        },
        size: Size { width: length(PANEL_W), height: auto() },
        ..Default::default()
    })
    .children(vec![polkit_panel(message, typed, theme)]);

    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            top: length(0.0_f32),
            right: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .fill(Color::from_rgba8(0, 0, 0, 120))
    .on_click(Msg::PolkitCancel)
    .children(vec![panel])
}
