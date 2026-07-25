//! El **diálogo de medios extraíbles** (USB): lista las particiones de discos
//! removibles con su etiqueta/tamaño y, según estén montadas o no, ofrece
//! **Montar** o **Abrir** + **Expulsar**. Las acciones van vía `udisksctl` (sin
//! root). El vigía es [`crate::usb`].

use llimphi_theme::{elevation, radius, Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::{Shadow, View};

use crate::usb::{UsbParticion, UsbSnapshot};
use crate::Msg;

/// Ancho del panel (px).
pub(super) const PANEL_W: f32 = 300.0;

/// El cuerpo del diálogo: una fila por partición extraíble.
pub(super) fn usb_body(snap: Option<&UsbSnapshot>, theme: &Theme) -> Vec<View<Msg>> {
    let mut hijos: Vec<View<Msg>> = vec![titulo("Medios extraíbles", theme)];
    let Some(s) = snap.filter(|s| !s.particiones.is_empty()) else {
        hijos.push(nota("No hay medios extraíbles conectados", theme));
        return hijos;
    };
    for p in &s.particiones {
        hijos.push(fila(p, theme));
    }
    hijos
}

/// Una fila de partición: etiqueta + tamaño/estado a la izquierda; botones a la
/// derecha (Montar si no está; Abrir + Expulsar si sí).
fn fila(p: &UsbParticion, theme: &Theme) -> View<Msg> {
    let sub = match &p.montada_en {
        Some(mp) => format!("{} · {}", p.tam, mp),
        None => format!("{} · sin montar", p.tam),
    };
    let nombre = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(18.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(recortar(&p.etiqueta, 24), 13.0, theme.fg_text);
    let detalle = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(14.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(recortar(&sub, 40), 11.0, theme.fg_muted);
    let texto = View::new(Style {
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(38.0_f32) },
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .children(vec![nombre, detalle]);

    let mut botones: Vec<View<Msg>> = Vec::new();
    match &p.montada_en {
        Some(mp) => {
            botones.push(boton("Abrir", Msg::UsbAbrir(mp.clone()), Some(theme.accent), theme));
            botones.push(boton("Expulsar", Msg::UsbExpulsar(p.disco.clone()), None, theme));
        }
        None => {
            botones.push(boton("Montar", Msg::UsbMontar(p.dev.clone()), Some(theme.accent), theme));
        }
    }
    let acciones = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: auto(), height: length(38.0_f32) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(5.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(botones);

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(40.0_f32) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(6.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![texto, acciones])
}

/// Un botón compacto de acción.
fn boton(label: &str, msg: Msg, fondo: Option<Color>, theme: &Theme) -> View<Msg> {
    let v = View::new(Style {
        size: Size { width: auto(), height: length(24.0_f32) },
        padding: TaffyRect { left: length(10.0_f32), right: length(10.0_f32), top: length(0.0_f32), bottom: length(0.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .on_click(msg);
    let fg = if fondo.is_some() { theme.bg_panel } else { theme.fg_text };
    let v = if let Some(bg) = fondo { v.fill(bg) } else { v.fill(theme.bg_button) };
    v.text(label.to_string(), 11.5, fg)
}

fn titulo(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(22.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 13.0, theme.fg_muted)
}

fn nota(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(34.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 12.0, theme.fg_muted)
}

fn recortar(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).chain(std::iter::once('…')).collect()
}

/// El panel enmarcado (fondo + sombra) para el flyout flotante.
pub fn usb_panel(snap: Option<&UsbSnapshot>, theme: &Theme) -> View<Msg> {
    let (a, blur, dy) = elevation::E4;
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(PANEL_W), height: auto() },
        padding: TaffyRect { left: length(12.0_f32), right: length(12.0_f32), top: length(10.0_f32), bottom: length(10.0_f32) },
        gap: Size { width: length(0.0_f32), height: length(3.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(radius::LG)
    .shadow(Shadow { color: Color::from_rgba8(0, 0, 0, a), blur, dx: 0.0, dy, spread: 0.0 })
    .children(usb_body(snap, theme))
}

/// El overlay para **winit**: scrim (cierra) + panel arriba a la derecha.
pub fn usb_overlay(snap: Option<&UsbSnapshot>, bar_h: f32, theme: &Theme) -> View<Msg> {
    let fila = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: auto() },
        justify_content: Some(JustifyContent::FlexEnd),
        padding: TaffyRect { left: length(0.0_f32), right: length(8.0_f32), top: length(8.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![usb_panel(snap, theme)]);
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(0.0_f32), top: length(bar_h), right: length(0.0_f32), bottom: length(0.0_f32) },
        size: Size { width: percent(1.0_f32), height: auto() },
        ..Default::default()
    })
    .on_click(Msg::UsbPanel)
    .children(vec![fila])
}
