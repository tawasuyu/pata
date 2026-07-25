//! El **diálogo del común** (tampu): qué tienes en custodia y qué aportaste, con
//! sus devoluciones vencidas y anomalías. Sólo lectura — los gestos (tomar,
//! devolver, asentar una falta) viven en la app tampu; la barra narra y alerta.

use llimphi_theme::{elevation, radius, Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::llimphi_text::Alignment;
use llimphi_ui::{Shadow, View};

use crate::tampu::{Lado, ObjetoVista, TampuSnapshot};
use crate::Msg;

/// Ancho del panel (px).
pub(super) const PANEL_W: f32 = 300.0;
/// Alto de una fila.
const ROW_H: f32 = 34.0;

/// El cuerpo del diálogo: sección «Tengo en custodia» + «Aporté al común».
pub(super) fn tampu_body(snap: Option<&TampuSnapshot>, theme: &Theme) -> Vec<View<Msg>> {
    let mut hijos: Vec<View<Msg>> = vec![titulo("El común", theme)];
    let Some(s) = snap.filter(|s| !s.objetos.is_empty()) else {
        hijos.push(nota("Nada tuyo en juego en el común", theme));
        return hijos;
    };
    let tengo: Vec<&ObjetoVista> = s.objetos.iter().filter(|o| o.lado == Lado::Tengo).collect();
    let aporte: Vec<&ObjetoVista> = s.objetos.iter().filter(|o| o.lado == Lado::Aporte).collect();
    if !tengo.is_empty() {
        hijos.push(seccion("Tengo en custodia", theme));
        for o in tengo {
            hijos.push(fila(o, theme));
        }
    }
    if !aporte.is_empty() {
        hijos.push(seccion("Aporté al común", theme));
        for o in aporte {
            hijos.push(fila(o, theme));
        }
    }
    hijos
}

/// Una fila de objeto: descripción + detalle (plazo / quién lo tiene / alarma).
/// La descripción va en acento si está vencido, en rojo si hay manipulación.
fn fila(o: &ObjetoVista, theme: &Theme) -> View<Msg> {
    let rojo = Color::from_rgb8(0xE0, 0x5A, 0x5A);
    let ambar = Color::from_rgb8(0xFB, 0xBF, 0x24);
    let (col_titulo, col_detalle) = if o.anomalia {
        (rojo, rojo)
    } else if o.vencido {
        (theme.fg_text, ambar)
    } else {
        (theme.fg_text, theme.fg_muted)
    };
    let desc = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(18.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(recortar(&o.descripcion, 34), 12.5, col_titulo);
    let det = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(14.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text_aligned(o.detalle.clone(), 11.0, col_detalle, Alignment::Start);
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        justify_content: Some(JustifyContent::Center),
        gap: Size { width: length(0.0_f32), height: length(1.0_f32) },
        ..Default::default()
    })
    .children(vec![desc, det])
}

fn titulo(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(22.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 13.0, theme.fg_muted)
}

fn seccion(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(20.0_f32) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect { left: length(0.0_f32), right: length(0.0_f32), top: length(4.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .text(t.to_string(), 11.0, theme.accent)
}

fn nota(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 12.0, theme.fg_muted)
}

/// Recorta `s` a `max` caracteres con elipsis.
fn recortar(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).chain(std::iter::once('…')).collect()
}

/// El panel enmarcado (fondo + sombra) para el flyout flotante.
pub fn tampu_panel(snap: Option<&TampuSnapshot>, theme: &Theme) -> View<Msg> {
    let (a, blur, dy) = elevation::E4;
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(PANEL_W), height: auto() },
        padding: TaffyRect { left: length(12.0_f32), right: length(12.0_f32), top: length(10.0_f32), bottom: length(10.0_f32) },
        gap: Size { width: length(0.0_f32), height: length(2.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(radius::LG)
    .shadow(Shadow { color: Color::from_rgba8(0, 0, 0, a), blur, dx: 0.0, dy, spread: 0.0 })
    .children(tampu_body(snap, theme))
}

/// El overlay completo para **winit**: scrim (cierra) + panel arriba a la derecha.
pub fn tampu_overlay(snap: Option<&TampuSnapshot>, bar_h: f32, theme: &Theme) -> View<Msg> {
    let fila = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: auto() },
        justify_content: Some(JustifyContent::FlexEnd),
        padding: TaffyRect { left: length(0.0_f32), right: length(8.0_f32), top: length(8.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![tampu_panel(snap, theme)]);
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(0.0_f32), top: length(bar_h), right: length(0.0_f32), bottom: length(0.0_f32) },
        size: Size { width: percent(1.0_f32), height: auto() },
        ..Default::default()
    })
    .on_click(Msg::TampuPanel)
    .children(vec![fila])
}
