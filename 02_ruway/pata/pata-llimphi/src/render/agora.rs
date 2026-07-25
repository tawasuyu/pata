//! El **diálogo «¿Le creo?»** (ágora): la cara de barra del sustrato de
//! confianza. Muestra un resumen de tu red (a cuánta gente le pusiste nombre,
//! cuántos avales conoces) y, sobre todo, las **revocaciones** resueltas a
//! nombre — con la clave **comprometida** marcada en rojo, que es la alerta que
//! amerita el fantasma. El veredicto per-claim interactivo vive en la app ágora;
//! el botón «Abrir ágora» la lanza. El vigía es [`crate::agora`].

use llimphi_theme::{elevation, radius, Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::{Shadow, View};

use crate::agora::{AgoraSnapshot, RevocacionVista};
use crate::Msg;

/// Ancho del panel (px).
pub(super) const PANEL_W: f32 = 320.0;

/// El cuerpo del diálogo: resumen de la red + revocaciones + botón a la app.
pub(super) fn agora_body(snap: Option<&AgoraSnapshot>, theme: &Theme) -> Vec<View<Msg>> {
    let mut hijos: Vec<View<Msg>> = vec![titulo("¿Le creo?", theme)];
    let s = match snap {
        Some(s) => s,
        None => {
            hijos.push(nota("Ágora no está en uso todavía.", theme));
            hijos.push(boton("Abrir ágora", Msg::AgoraAbrir, Some(theme.accent), theme));
            return hijos;
        }
    };

    // Resumen de la red de confianza, en llano.
    hijos.push(resumen(s, theme));

    // Revocaciones: si hay una comprometida vigente, encabezado en rojo.
    let alerta = s.hay_alerta;
    if s.revocaciones.is_empty() {
        hijos.push(nota("Sin revocaciones en tu red.", theme));
    } else {
        let color = if alerta { theme.fg_destructive } else { theme.fg_muted };
        hijos.push(seccion("Revocaciones", color, theme));
        for r in &s.revocaciones {
            hijos.push(fila_revocacion(r, theme));
        }
    }

    hijos.push(espacio(6.0));
    hijos.push(boton("Abrir ágora", Msg::AgoraAbrir, Some(theme.accent), theme));
    hijos
}

/// La línea de resumen: «N personas · M avales · K con nombre».
fn resumen(s: &AgoraSnapshot, theme: &Theme) -> View<Msg> {
    let personas = |n: usize, sing: &str, plur: &str| if n == 1 { format!("{n} {sing}") } else { format!("{n} {plur}") };
    let txt = format!(
        "{} · {} · {}",
        personas(s.personas, "persona", "personas"),
        personas(s.avales, "aval", "avales"),
        personas(s.conocidos, "con nombre", "con nombre"),
    );
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(20.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(txt, 12.0, theme.fg_text)
}

/// Una fila de revocación: nombre + motivo. Un punto rojo si comprometida y
/// vigente (la alerta); atenuada si ya no rige (venció la suspensión).
fn fila_revocacion(r: &RevocacionVista, theme: &Theme) -> View<Msg> {
    let rojo = r.comprometida && r.vigente;
    let punto_color = if rojo {
        theme.fg_destructive
    } else if r.vigente {
        theme.fg_muted
    } else {
        theme.fg_muted.with_alpha(0.5)
    };
    let punto = View::new(Style {
        size: Size { width: length(8.0_f32), height: length(8.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(punto_color)
    .radius(4.0);

    let nombre_txt = if r.conocido { r.nombre.clone() } else { format!("{} (sin nombre)", r.nombre) };
    let nombre = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(16.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(recortar(&nombre_txt, 30), 13.0, if r.vigente { theme.fg_text } else { theme.fg_muted });
    let sub = if r.vigente {
        r.motivo.to_string()
    } else {
        format!("{} · ya no rige", r.motivo)
    };
    let detalle = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(14.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(sub, 11.0, if rojo { theme.fg_destructive } else { theme.fg_muted });
    let texto = View::new(Style {
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(34.0_f32) },
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .children(vec![nombre, detalle]);

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(34.0_f32) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(8.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![punto, texto])
}

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
    .text(t.to_string(), 14.0, theme.fg_text)
}

fn seccion(t: &str, color: Color, _theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(18.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 11.0, color)
}

fn nota(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(30.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 12.0, theme.fg_muted)
}

fn espacio(h: f32) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(h) },
        ..Default::default()
    })
}

fn recortar(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).chain(std::iter::once('…')).collect()
}

/// El panel enmarcado (fondo + sombra) para el flyout flotante.
pub fn agora_panel(snap: Option<&AgoraSnapshot>, theme: &Theme) -> View<Msg> {
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
    .children(agora_body(snap, theme))
}

/// El overlay para **winit**: scrim (cierra) + panel arriba a la derecha.
pub fn agora_overlay(snap: Option<&AgoraSnapshot>, bar_h: f32, theme: &Theme) -> View<Msg> {
    let fila = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: auto() },
        justify_content: Some(JustifyContent::FlexEnd),
        padding: TaffyRect { left: length(0.0_f32), right: length(8.0_f32), top: length(8.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![agora_panel(snap, theme)]);
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(0.0_f32), top: length(bar_h), right: length(0.0_f32), bottom: length(0.0_f32) },
        size: Size { width: percent(1.0_f32), height: auto() },
        ..Default::default()
    })
    .on_click(Msg::AgoraPanel)
    .children(vec![fila])
}
