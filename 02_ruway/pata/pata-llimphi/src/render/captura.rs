//! El **menú de captura de pantalla** (hapiy) **y de grabación de video**
//! (wf-recorder): imagen (pantalla completa / región / editar en tullpu) arriba,
//! video (grabar pantalla / región / con audio) abajo. La imagen es sin estado —
//! cada botón dispara la captura vía [`crate::CapturaModo`] y cierra el menú—; el
//! video **sí** tiene estado: mientras se graba, el menú muestra un botón rojo de
//! detener con cronómetro. Ver [`crate::grabacion`].

use llimphi_theme::{elevation, radius, Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::{Shadow, View};

use crate::grabacion::GrabModo;
use crate::{CapturaModo, Msg};

/// Rojo de grabación (punto/borde del botón de detener).
const ROJO_REC: Color = Color::from_rgba8(0xE0, 0x3A, 0x3A, 0xFF);

/// `MM:SS` a partir de segundos.
fn mmss(secs: u64) -> String {
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// Ancho del panel (px).
pub(super) const PANEL_W: f32 = 240.0;

/// Una fila-acción del menú de captura de imagen.
fn accion(label: &str, desc: &str, modo: CapturaModo, theme: &Theme) -> View<Msg> {
    fila_accion(label, desc, Msg::Captura(modo), theme)
}

/// Una fila-acción genérica: rótulo + descripción, click dispara un `Msg`.
fn fila_accion(label: &str, desc: &str, msg: Msg, theme: &Theme) -> View<Msg> {
    let titulo = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(18.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(label.to_string(), 13.0, theme.fg_text);
    let sub = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(14.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(desc.to_string(), 11.0, theme.fg_muted);
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: length(38.0_f32) },
        justify_content: Some(JustifyContent::Center),
        padding: TaffyRect { left: length(10.0_f32), right: length(10.0_f32), top: length(0.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .on_click(msg)
    .children(vec![titulo, sub])
}

/// Subtítulo de sección (tenue), p. ej. «Grabar video».
fn seccion(label: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(20.0_f32) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect { left: length(10.0_f32), right: length(0.0_f32), top: length(4.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .text(label.to_string(), 11.0, theme.fg_muted)
}

/// El **botón rojo de detener** la grabación en curso, con cronómetro `MM:SS`.
fn boton_detener(secs: u64, theme: &Theme) -> View<Msg> {
    // Punto rojo (el clásico ● de «grabando»).
    let dot = View::new(Style {
        size: Size { width: length(10.0_f32), height: length(10.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .radius(5.0)
    .fill(ROJO_REC);
    let label = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(18.0_f32) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect { left: length(8.0_f32), right: length(0.0_f32), top: length(0.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .text("Detener grabación".to_string(), 13.0, theme.fg_text);
    let cron = View::new(Style {
        size: Size { width: auto(), height: length(18.0_f32) },
        align_items: Some(AlignItems::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .text(mmss(secs), 13.0, ROJO_REC);
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(40.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexStart),
        padding: TaffyRect { left: length(12.0_f32), right: length(12.0_f32), top: length(0.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .radius(6.0)
    .fill(Color::from_rgba8(0xE0, 0x3A, 0x3A, 0x22))
    .border(1.0, ROJO_REC)
    .hover_fill(Color::from_rgba8(0xE0, 0x3A, 0x3A, 0x38))
    .on_click(Msg::GrabarDetener)
    .children(vec![dot, label, cron])
}

/// El panel enmarcado (fondo + sombra).
/// El panel del menú. `grab = Some(segundos)` mientras hay una grabación en curso:
/// en ese caso el panel colapsa al cronómetro + botón rojo de detener (no tiene
/// sentido ofrecer más capturas mientras grabás). En reposo muestra imagen arriba
/// y video abajo.
pub fn captura_panel(grab: Option<u64>, theme: &Theme) -> View<Msg> {
    let header = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(22.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(
        if grab.is_some() { "Grabando…".to_string() } else { "Captura de pantalla".to_string() },
        13.0,
        theme.fg_muted,
    );
    let (a, blur, dy) = elevation::E4;

    let hijos: Vec<View<Msg>> = if let Some(secs) = grab {
        // Grabando: sólo el cronómetro + detener.
        vec![header, boton_detener(secs, theme)]
    } else {
        // Reposo: imagen (PNG) arriba, video abajo.
        vec![
            header,
            accion("Pantalla completa", "todo el escritorio → PNG", CapturaModo::Completa, theme),
            accion("Región", "elige un rectángulo (slurp)", CapturaModo::Region, theme),
            accion("Editar en tullpu", "captura y abrí para anotar", CapturaModo::Editar, theme),
            seccion("Grabar video", theme),
            fila_accion("Grabar pantalla", "screencast del monitor → MP4", Msg::GrabarIniciar(GrabModo::Pantalla, false), theme),
            fila_accion("Grabar región", "elige un rectángulo (slurp) → MP4", Msg::GrabarIniciar(GrabModo::Region, false), theme),
            fila_accion("Grabar con audio", "pantalla + micrófono → MP4", Msg::GrabarIniciar(GrabModo::Pantalla, true), theme),
        ]
    };

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(PANEL_W), height: auto() },
        padding: TaffyRect { left: length(10.0_f32), right: length(10.0_f32), top: length(10.0_f32), bottom: length(10.0_f32) },
        gap: Size { width: length(0.0_f32), height: length(3.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(radius::LG)
    .shadow(Shadow { color: Color::from_rgba8(0, 0, 0, a), blur, dx: 0.0, dy, spread: 0.0 })
    .children(hijos)
}

/// El overlay para **winit**: scrim (cierra) + panel arriba a la derecha.
pub fn captura_overlay(grab: Option<u64>, bar_h: f32, theme: &Theme) -> View<Msg> {
    let fila = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: auto() },
        justify_content: Some(JustifyContent::FlexEnd),
        padding: TaffyRect { left: length(0.0_f32), right: length(8.0_f32), top: length(8.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![captura_panel(grab, theme)]);
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(0.0_f32), top: length(bar_h), right: length(0.0_f32), bottom: length(0.0_f32) },
        size: Size { width: percent(1.0_f32), height: auto() },
        ..Default::default()
    })
    .on_click(Msg::CapturaPanel)
    .children(vec![fila])
}
