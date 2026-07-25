//! El panel del diente de **sistema/sesión** (el del footer del rail): un resumen
//! de usuario+sistema, el cambio de contexto (`pacha`), el panel del sistema
//! (`wawa-panel`) y las acciones de energía (cerrar sesión / reiniciar / apagar).
//!
//! La frontera es la de siempre (Regla 2): pata NO reimplementa lógica de sistema —
//! las acciones disruptivas emiten [`Msg::ConfirmPedir`] (que abre la pantalla de
//! confirmación fullscreen) y de ahí van por su CLI (`systemctl`/`loginctl`/`pacha`),
//! y el panel del sistema lanza `wawa-panel`. Aquí va SÓLO la vista.
//!
//! La info **estática** (usuario/host/SO/kernel) se lee UNA vez y se cachea
//! ([`sistema_info`]); la **dinámica** (RAM/CPU) sale del `WidgetCtx` de cada frame.

use std::sync::OnceLock;

use llimphi_theme::{Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::llimphi_text::Alignment;
use llimphi_ui::View;

use llimphi_widget_scroll::{clamp_offset, scroll_y, ScrollPalette};

use pata_core::widget::WidgetCtx;

use crate::nouser::NavState;
use crate::perfil::PachaInfo;
use crate::{ConfirmAccion, Msg, SessionAction};

/// Padding interno del panel (px).
const PAD: f32 = 10.0;
/// Alto de una fila clave/valor.
const KV_H: f32 = 22.0;
/// Alto de un rótulo de sección.
const SEC_H: f32 = 24.0;
/// Alto de una fila-acción clickeable.
const ROW_H: f32 = 36.0;

/// Info estática del equipo, leída una sola vez.
struct SistemaInfo {
    usuario: String,
    host: String,
    os: String,
    kernel: String,
}

/// Devuelve (y cachea) la info estática del sistema: usuario (`$USER`), hostname,
/// SO (`PRETTY_NAME` de `/etc/os-release`) y kernel (`/proc/sys/kernel/osrelease`).
/// Tolerante: lo que no se pueda leer queda en «—».
fn sistema_info() -> &'static SistemaInfo {
    static CACHE: OnceLock<SistemaInfo> = OnceLock::new();
    CACHE.get_or_init(|| {
        let usuario = std::env::var("USER")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "—".to_string());
        let host = std::fs::read_to_string("/etc/hostname")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "—".to_string());
        let os = std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "—".to_string());
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "—".to_string());
        SistemaInfo { usuario, host, os, kernel }
    })
}

/// El cuerpo del diente sistema/sesión, autocontenido a `body_h` px. El cabezal
/// uniforme lo antepone el marco; aquí va sólo lo propio del panel, scrolleable.
pub fn sistema_panel(
    pachas: &[PachaInfo],
    ctx: &WidgetCtx,
    nav: &NavState,
    body_h: f32,
    theme: &Theme,
) -> View<Msg> {
    let info = sistema_info();
    let mut rows: Vec<View<Msg>> = Vec::new();
    let mut h = 0.0_f32;
    let push = |v: View<Msg>, alto: f32, rows: &mut Vec<View<Msg>>, h: &mut f32| {
        rows.push(v);
        *h += alto;
    };

    // --- Usuario ---
    push(seccion("Usuario", theme), SEC_H, &mut rows, &mut h);
    push(kv("Nombre", &info.usuario, theme), KV_H, &mut rows, &mut h);
    push(kv("Equipo", &info.host, theme), KV_H, &mut rows, &mut h);

    // --- Sistema ---
    push(seccion("Sistema", theme), SEC_H, &mut rows, &mut h);
    push(kv("SO", &info.os, theme), KV_H, &mut rows, &mut h);
    push(kv("Kernel", &info.kernel, theme), KV_H, &mut rows, &mut h);
    let ram = format!(
        "{} / {} MiB ({:.0}%)",
        ctx.ram_used_mb,
        ctx.ram_total_mb,
        ctx.ram * 100.0
    );
    push(kv("Memoria", &ram, theme), KV_H, &mut rows, &mut h);
    push(kv("CPU", &format!("{:.0}%", ctx.cpu * 100.0), theme), KV_H, &mut rows, &mut h);

    // --- Contexto (pacha): una fila por contexto; el activo se rotula, el resto
    // pide confirmación para cambiar. ---
    if !pachas.is_empty() {
        push(seccion("Contexto (pacha)", theme), SEC_H, &mut rows, &mut h);
        for p in pachas {
            push(pacha_row(p, theme), ROW_H, &mut rows, &mut h);
        }
    }

    // --- Acciones ---
    push(seccion("Acciones", theme), SEC_H, &mut rows, &mut h);
    push(
        accion_row("⚙", "Panel del sistema", theme.fg_text, Msg::Spawn("wawa-panel".to_string()), theme),
        ROW_H,
        &mut rows,
        &mut h,
    );
    push(
        accion_row(
            "⇦",
            "Cerrar sesión",
            theme.fg_text,
            Msg::ConfirmPedir(ConfirmAccion::Session(SessionAction::Logout)),
            theme,
        ),
        ROW_H,
        &mut rows,
        &mut h,
    );
    push(
        accion_row(
            "⟳",
            "Reiniciar",
            theme.fg_text,
            Msg::ConfirmPedir(ConfirmAccion::Session(SessionAction::Reboot)),
            theme,
        ),
        ROW_H,
        &mut rows,
        &mut h,
    );
    push(
        accion_row(
            "⏻",
            "Apagar",
            peligro(),
            Msg::ConfirmPedir(ConfirmAccion::Session(SessionAction::Shutdown)),
            theme,
        ),
        ROW_H,
        &mut rows,
        &mut h,
    );

    // Sumar los gaps de 4 px entre filas.
    let content_len = h + (rows.len().saturating_sub(1)) as f32 * 4.0 + PAD * 2.0;
    let inner = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: auto() },
        gap: Size { width: length(0.0_f32), height: length(4.0_f32) },
        ..Default::default()
    })
    .children(rows);

    let viewport = (body_h - PAD * 2.0).max(0.0);
    let offset = clamp_offset(nav.scroll, content_len, viewport);
    let scrolled = scroll_y(
        offset,
        content_len,
        viewport,
        inner,
        Msg::NavScroll,
        &ScrollPalette::from_theme(theme),
    );
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: length(body_h) },
        padding: TaffyRect {
            left: length(PAD),
            right: length(PAD),
            top: length(PAD),
            bottom: length(PAD),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .children(vec![scrolled])
}

/// Rojo suave para las acciones destructivas (apagar) y el marcador de peligro.
fn peligro() -> Color {
    Color::from_rgba8(248, 113, 113, 255)
}

/// Verde vivo del contexto activo.
fn verde() -> Color {
    Color::from_rgba8(52, 211, 153, 255)
}

/// Un rótulo de sección.
fn seccion(titulo: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(SEC_H) },
        align_items: Some(AlignItems::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .text(titulo.to_string(), 11.0, theme.accent)
}

/// Una fila clave/valor (clave atenuada, valor a la derecha).
fn kv(clave: &str, valor: &str, theme: &Theme) -> View<Msg> {
    let k = View::new(Style {
        size: Size { width: length(84.0_f32), height: length(KV_H) },
        align_items: Some(AlignItems::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .text(clave.to_string(), 11.0, theme.fg_muted);
    let v = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: length(0.0_f32), height: length(KV_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text_aligned(valor.to_string(), 11.0, theme.fg_text, Alignment::Start);
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(KV_H) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(vec![k, v])
}

/// Una fila de contexto `pacha`: marcador (activo ● verde, si no ○) + nombre; el
/// activo se rotula «activo» y NO es clickeable, el resto pide confirmación para
/// cambiar ([`Msg::ConfirmPedir`]).
fn pacha_row(p: &PachaInfo, theme: &Theme) -> View<Msg> {
    let dot_color = if p.active { verde() } else { theme.fg_muted };
    let dot = View::new(Style {
        size: Size { width: length(18.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .text(if p.active { "●" } else { "○" }.to_string(), 11.0, dot_color);
    let nombre = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: length(0.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text_aligned(p.label.clone(), 13.0, theme.fg_text, Alignment::Start);
    let estado = View::new(Style {
        size: Size { width: length(64.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::End),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .text(
        if p.active { "activo".to_string() } else { "cambiar".to_string() },
        10.0,
        if p.active { verde() } else { theme.fg_muted },
    );
    let mut fila = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(4.0_f32),
            right: length(8.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(theme.bg_panel_alt)
    .radius(6.0)
    .children(vec![dot, nombre, estado]);
    if !p.active {
        fila = fila.hover_fill(theme.bg_button_hover).on_click(Msg::ConfirmPedir(
            ConfirmAccion::Pacha { id: p.id.clone(), label: p.label.clone() },
        ));
    }
    fila
}

/// Una fila-acción clickeable: glifo + etiqueta, con hover. `color` tiñe la
/// etiqueta (rojo para apagar). `msg` es lo que dispara al click.
fn accion_row(glifo: &str, label: &str, color: Color, msg: Msg, theme: &Theme) -> View<Msg> {
    let ic = View::new(Style {
        size: Size { width: length(24.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .text(glifo.to_string(), 14.0, color);
    let txt = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: length(0.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text_aligned(label.to_string(), 13.0, color, Alignment::Start);
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(4.0_f32),
            right: length(8.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(theme.bg_panel_alt)
    .hover_fill(theme.bg_button_hover)
    .radius(6.0)
    .on_click(msg)
    .children(vec![ic, txt])
}
