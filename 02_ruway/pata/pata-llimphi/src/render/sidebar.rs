//! Render del **sidebar navegador** (Fase 11c): el rail de dientes pegado al
//! borde y, cuando un diente está activo, el panel con el navegador de
//! Mónadas/archivos.
//!
//! - El **rail** reusa [`llimphi_widget_dock_rail`]: una franja vertical con un
//!   diente por `SidebarTab`. El diente activo (su panel desplegado) va
//!   resaltado. Clic → [`Msg::NavTabActivate`].
//! - El **panel** ([`panel_inner`]) lleva un cabezal con el toggle Árbol/Grafo +
//!   el navegador ([`llimphi_widget_navigator`]) dentro de un área de scroll. El
//!   plano de datos lo provee [`crate::nouser`].
//!
//! Dos backends montan estas piezas distinto:
//! - **winit** ([`sidebar_rail_view`] + [`nav_panel_view`]): cada superficie vive
//!   en la ventana única, posicionada en absoluto sobre la pantalla; el panel
//!   flota como un drawer.
//! - **layer-shell** ([`sidebar_surface_view`]): el rail es su propia layer
//!   surface anclada al borde; al abrir un diente la surface **crece** en ancho y
//!   el panel se pinta junto al rail (el eje libre lo estira el compositor).

use llimphi_theme::Theme;
use rimay_localize::{t, t_args};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::{DragPhase, View};

// El rail lo pinta el widget `rag-sidebar` (dock-rail encapsulado adentro); pata sólo
// arma los badges de los dientes.
use llimphi_widget_dock_rail::{BadgeKind, DockBadge};
use sandokan_lifecycle::LifecycleState;
use pata_host::HostedTooth;
use llimphi_widget_navigator::{
    navigator_view, NavId, NavMode, NavNode, NavPalette, NavSpec,
};
use llimphi_widget_rag_sidebar::{
    rag_multiselect_view, rag_panel_view, RagOptions, RagSidebarMsg,
    RagSidebarPalette, RagSidebarState, RagSide, RagTooth, BAR_H,
};
use std::sync::Arc;
use llimphi_widget_scroll::{clamp_offset, scroll_y, ScrollPalette};
use llimphi_widget_segmented::{segmented_view, SegmentedPalette};

use std::collections::HashSet;

use pata_core::config::{Anchor, Surface, TabsSource};
use pata_core::layout::Rect;

use super::diente::DienteVivo;
use super::{control_center_view, CentroDatos};
use crate::nouser::NavState;
use crate::rag::RagState;
use crate::shuma::ShumaState;
use crate::Msg;

/// Ancho FIJO de la layer surface del drawer, en px. Es **mayor** que cualquier
/// `panel_width` posible (slider 120..600): así el panel se puede redimensionar
/// arrastrando su divisor SIN redimensionar la surface (lo que crashea Iris Xe) —
/// solo se repinta el panel al ancho nuevo y se reajusta la input-region. El resto
/// de la surface queda transparente y click-through.
pub const DRAWER_SURFACE_W: f32 = 640.0;
/// Alto del cabezal del panel (título + toggle de modo), en px.
const HEADER_H: f32 = 40.0;
/// Padding interno del panel, en px.
const PAD: f32 = 8.0;
/// Alto estimado de una fila del navegador en modo árbol (igual al `ROW_H`
/// interno del widget). Para dimensionar el scroll.
const TREE_ROW_H: f32 = 24.0;
/// Alto estimado de un nodo del navegador en modo grafo (nodo + separación).
const GRAPH_ROW_H: f32 = 60.0;

// =====================================================================
// Piezas compartidas por ambos backends
// =====================================================================


/// Decide el badge de un diente-sesión de terminal a partir de su estado —el
/// **árbitro de notificación** del `</>`—. Puro para poder certificarlo:
/// - la sesión **activa** nunca badgea (la estás mirando; espeja el A6 bare);
/// - un **comando largo** terminado (no-activa) → badge ámbar con el conteo;
/// - el último comando **falló** (`exit≠0`, sin comando largo pendiente) → punto
///   rojo;
/// - en reposo/ok → sin badge (el LED de actividad ya distingue quieto/mov/claude).
fn terminal_badge(active: bool, long_alerts: usize, ultimo_ok: Option<bool>) -> Option<DockBadge> {
    if active {
        return None;
    }
    if long_alerts > 0 {
        return Some(DockBadge::Count(long_alerts as u32, BadgeKind::Warning));
    }
    if ultimo_ok == Some(false) {
        return Some(DockBadge::Dot(BadgeKind::Error));
    }
    None
}

// Esquema de **ids codificados** del rail unificado: un solo `Vec<RagTooth>` mezcla
// grupos heterogéneos (escritorios, tabs de config, terminal, hospedados, shuma). El
// id codifica de QUÉ grupo viene cada diente para (a) rutear su click y (b) decodificar
// qué panel abre. Los tabs de config quedan en su índice CRUDO (`< WS_BASE`); el resto
// sobre bases altas. `nav.open` (y por ende el panel del drawer) guarda el id CODIFICADO
// (los handlers `WorkspaceTooth`/`NavTabActivate` lo hacen), así todo el flujo —highlight
// del rail, decode del panel— usa la misma identidad sin ambigüedad.
pub(crate) const WS_BASE: u64 = 1_000_000;
pub(crate) const TERM_BASE: u64 = 2_000_000;
pub(crate) const HOST_BASE: u64 = 3_000_000;
/// Base de los dientes de **footer** (`surface.footer_tabs`): el diente de sistema/
/// sesión anclado al fondo del rail. `FOOTER_BASE + índice` en `footer_tabs`. Va
/// arriba de `SHUMA_ID` para que el decode del panel lo distinga de un diente-escritorio
/// (`WS_BASE`) o un tab de config (`< WS_BASE`).
pub(crate) const FOOTER_BASE: u64 = 5_000_000;
pub(crate) const SHUMA_ID: u64 = 9_000_000;

/// Icono estático de diente por nombre (glifo). Las animaciones de canvas vivo
/// (monitor/unidades/atención) se reponen luego —probablemente sobre la barra shuma—;
/// por ahora el rail unificado usa iconos estáticos.
fn tooth_ic(name: &str) -> Arc<dyn Fn(f32, Color) -> View<Msg> + Send + Sync> {
    let name = name.to_string();
    Arc::new(move |size, color| tooth_icon(&name, size, color))
}

/// Color del **distintivo de otro monitor**: un ámbar cálido que contrasta con el
/// acento (azul) del escritorio local. No tiñe el número ni pone una bola: sólo
/// dibuja un **aro** discreto alrededor del diente para leer de un vistazo «ese
/// escritorio vive en la otra pantalla».
const WS_OTRO_MONITOR: Color = Color::from_rgba8(0xE8, 0x9A, 0x3C, 0xFF);

/// Icono de un diente-escritorio: el número, **grande y en negrita** (el rail es
/// el navegador de escritorios; el número es su protagonista, no un rótulo
/// tímido). Si `en_otro`, se le añade un **aro** sutil en [`WS_OTRO_MONITOR`]
/// (borde de otro color, sin bola ni tinte del número) — así los escritorios de
/// otra pantalla se distinguen discretamente, sin gritar.
fn ws_ic(n: u8, en_otro: bool) -> Arc<dyn Fn(f32, Color) -> View<Msg> + Send + Sync> {
    Arc::new(move |size, color| {
        let v = View::new(Style {
            size: Size { width: length(size), height: length(size) },
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        })
        .text(n.to_string(), (size * 0.66).max(15.0), color)
        .bold();
        if en_otro {
            // Aro discreto de otro color: el diente pertenece a otro monitor.
            v.radius((size * 0.30) as f64).border(1.5, WS_OTRO_MONITOR)
        } else {
            v
        }
    })
}

/// Badge inteligente de un diente de config según su `kind`: nº de unidades falladas
/// (rojo) en «Unidades», nº de hosts no alcanzables (ámbar) en «Flota». `None` si no
/// aplica. Se computa eager (tenemos `vivo`), no como cierre.
fn badge_for_kind(kind: &str, vivo: &DienteVivo) -> Option<DockBadge> {
    if crate::es_unidades(kind) {
        let n = vivo
            .unidades
            .map(|s| {
                s.units
                    .iter()
                    .filter(|u| matches!(u.state, LifecycleState::Failed { .. } | LifecycleState::Killed))
                    .count()
            })
            .unwrap_or(0);
        return (n > 0).then(|| DockBadge::Count(n as u32, BadgeKind::Error));
    }
    if crate::es_flota(kind) {
        let n = vivo.flota_remoto.map(|o| o.iter().filter(|h| !h.reachable).count()).unwrap_or(0);
        return (n > 0).then(|| DockBadge::Count(n as u32, BadgeKind::Warning));
    }
    None
}

/// Arma el `Vec<RagTooth>` COMPLETO del rail de un sidebar —grupos concatenados con
/// separador entre cada uno, cada uno con su encía— más la encía «+» del grupo
/// escritorios. Es el modelo unificado: workspaces + herramientas + dientes absorbidos
/// (hospedados) + shuma conviven en UN rail que pinta el widget. Los iconos son
/// estáticos (los canvas vivos se reponen luego). Devuelve `(teeth, grow)`.
#[allow(clippy::too_many_arguments)]
fn build_rail_teeth(
    surface: &Surface,
    nav: &NavState,
    hosted: &[HostedTooth],
    hosted_active: Option<u32>,
    shuma: &ShumaState,
    vivo: &DienteVivo,
) -> (Vec<RagTooth<Msg>>, Option<(String, String, Msg)>) {
    let _ = nav;
    let mut teeth: Vec<RagTooth<Msg>> = Vec::new();
    // El «+» de escritorios ya no va como encía única al FINAL del rail: se intercala
    // en cada hueco vía `RagTooth::grow_after` (ver abajo). El `grow` del final queda
    // libre para otros usos (hoy ninguno en pata).
    let grow: Option<(String, String, Msg)> = None;
    // ¿algún grupo ya empezó? (para poner separador_before al primero del siguiente).
    let mut hay_grupo = false;

    // GRUPO **escritorios**: si el sidebar lo declara en su slot start (los escritorios
    // pasan a ser dientes-con-sidebar, no botones sueltos).
    let quiere_ws = surface
        .start
        .iter()
        .any(|w| matches!(w.kind.as_str(), "workspaces" | "escritorios"));
    if quiere_ws || surface.tabs_source == TabsSource::Workspaces {
        let (celdas, _libre) = super::task_manager::ws_rail_model(
            vivo.ctx.active_workspace,
            vivo.ctx.workspace_count,
            vivo.ctx.workspace_occupied,
            vivo.ctx.workspace_others,
        );
        let count = vivo.ctx.workspace_count as u16;
        for (i, &n) in celdas.iter().enumerate() {
            // ¿Este escritorio se ve en OTRO monitor (no el enfocado)? Lleva un aro
            // discreto (ws_ic) y el tooltip lo aclara. `workspace_others` es la máscara
            // de escritorios visibles en otra pantalla (bit `n-1`); nunca coincide con
            // el activo de aquí (un escritorio vive en un solo monitor).
            let en_otro = n != vivo.ctx.active_workspace
                && (vivo.ctx.workspace_others & (1u16 << (n as u16 - 1))) != 0;
            // El diente del escritorio ACTIVO se marca igual que el `</>` de la
            // sesión activa: antes ninguna celda de workspace llevaba `.active`,
            // así que el único diente resaltado del rail acababa siendo el `</>`
            // —parecía que ESE era el workspace activo—.
            let etiqueta = if en_otro {
                format!("Escritorio {n} · en otro monitor (clic = ir allá)")
            } else {
                format!("Escritorio {n}")
            };
            // Sin badge de conteo de ventanas: el número del diente ya es el
            // protagonista; una bola encima sólo ensuciaba el rail.
            let mut t = RagTooth::new(WS_BASE + n as u64, etiqueta, ws_ic(n, en_otro))
                .active(n == vivo.ctx.active_workspace);
            // Encía «+» en CADA hueco: si el siguiente escritorio mostrado (o el fin
            // de la grilla) deja un slot libre justo después de éste, un botón crea
            // ESE escritorio, pegado a su lugar en la fila —no un único «+» al final
            // del rail—. Ej: mostrados 1,3,5 → [1][+2][3][+4][5][+6].
            let siguiente = celdas.get(i + 1).map(|&x| x as u16).unwrap_or(count + 1);
            let hueco = n as u16 + 1;
            if hueco < siguiente && hueco <= count {
                t = t.grow_after(
                    "+",
                    format!("Nuevo escritorio (el {hueco})"),
                    Msg::SwitchWorkspace(hueco as u8),
                );
            }
            teeth.push(t);
        }
        hay_grupo = hay_grupo || !celdas.is_empty();
    }

    // GRUPO **dientes** por `tabs_source` (config = tabs estáticos; terminal = sesiones).
    match surface.tabs_source {
        TabsSource::Config => {
            for (i, tab) in surface.tabs.iter().enumerate() {
                let mut t = RagTooth::new(i as u64, tab.label.clone(), tooth_ic(&tab.icon));
                t.no_header = tab.no_header;
                t.badge = badge_for_kind(&tab.content.kind, vivo);
                if i == 0 && hay_grupo {
                    t.separator_before = true;
                }
                teeth.push(t);
            }
            hay_grupo = hay_grupo || !surface.tabs.is_empty();
        }
        TabsSource::Workspaces => { /* ya pintado arriba */ }
        TabsSource::Terminal => {
            for (idx, s) in vivo.terminal_sessions.iter().enumerate() {
                // El diente de sesión sólo se resalta cuando el drawer de shuma está
                // ABIERTO (estás mirando la terminal); si no, el `</>` no debe parecer
                // "activo" sólo porque una sesión es la última usada.
                let mut t = RagTooth::new(TERM_BASE + s.index as u64, format!("Terminal {}", s.index), tooth_ic("shell"))
                    .active(s.active && shuma.open);
                t.badge = terminal_badge(s.active, s.long_alerts, s.ultimo_ok);
                if idx == 0 && hay_grupo {
                    t.separator_before = true;
                }
                teeth.push(t);
            }
            hay_grupo = hay_grupo || !vivo.terminal_sessions.is_empty();
        }
    }

    // GRUPO **hospedados** (dientes absorbidos de la app enfocada).
    for (i, h) in hosted.iter().enumerate() {
        let mut t = RagTooth::new(HOST_BASE + h.id as u64, h.label.clone(), tooth_ic(&h.icon))
            .active(hosted_active == Some(h.id));
        if i == 0 {
            t.separator_before = true;
        }
        teeth.push(t);
    }
    hay_grupo = hay_grupo || !hosted.is_empty();

    // Diente **shuma** (toggle del drawer Quake) — salvo en un sidebar que YA es de
    // Terminal (sus dientes ya son las sesiones). Con sesiones vivas, el `</>` se
    // materializa como el grupo de sesiones; si no, el diente-toggle simple.
    if shuma.present && surface.tabs_source != TabsSource::Terminal {
        if vivo.terminal_sessions.is_empty() {
            let t = RagTooth::new(SHUMA_ID, "Shell", tooth_ic("shell"))
                .active(shuma.open);
            teeth.push(if hay_grupo { t.separator_before() } else { t });
        } else {
            for (idx, s) in vivo.terminal_sessions.iter().enumerate() {
                // El diente de sesión sólo se resalta cuando el drawer de shuma está
                // ABIERTO (estás mirando la terminal); si no, el `</>` no debe parecer
                // "activo" sólo porque una sesión es la última usada.
                let mut t = RagTooth::new(TERM_BASE + s.index as u64, format!("Terminal {}", s.index), tooth_ic("shell"))
                    .active(s.active && shuma.open);
                t.badge = terminal_badge(s.active, s.long_alerts, s.ultimo_ok);
                if idx == 0 && hay_grupo {
                    t.separator_before = true;
                }
                teeth.push(t);
            }
        }
    }

    (teeth, grow)
}

/// Arma los dientes de **footer** del rail (anclados al fondo) desde
/// `surface.footer_tabs`. Hoy es sólo el diente de sistema/sesión (`kind = "sesion"`),
/// pero es genérico: cada `footer_tab` es un `RagTooth` con id `FOOTER_BASE + índice`,
/// que el rail resalta si su panel está abierto y cuyo click abre su panel de sidebar
/// (decodificado en [`panel_body`]). Vacío si el sidebar no declara footer.
fn build_footer_teeth(surface: &Surface) -> Vec<RagTooth<Msg>> {
    surface
        .footer_tabs
        .iter()
        .enumerate()
        .map(|(i, tab)| {
            let mut t = RagTooth::new(FOOTER_BASE + i as u64, tab.label.clone(), tooth_ic(&tab.icon));
            t.no_header = tab.no_header;
            t
        })
        .collect()
}

/// El estado del widget para el RAIL (surface aparte del panel): el highlight sale de
/// `nav.open` codificado; disposición fija (docked, sin autohide) porque pata gobierna
/// esconder/mostrar a nivel de surface. `si` filtra el diente abierto de ESE sidebar.
fn rail_state(surface: &Surface, si: usize, nav: &NavState, out: &str) -> RagSidebarState {
    // El abierto de ESTE monitor: cada rail resalta lo desplegado en su pantalla.
    let open = match nav.open_en(out) {
        Some((s, ti)) if s == si => Some(ti as u64),
        _ => None,
    };
    RagSidebarState {
        rail_outside: false,
        docked: true,
        autohide: false,
        dos_pasos: false,
        open,
        selected: open,
        panel_w: surface.panel_width,
        control_open: nav.control_open,
        search: nav.search.clone(),
        search_focused: nav.search_focused,
        revealed: true,
    }
}

/// Enruta los `RagSidebarMsg` del rail unificado al `Msg` de pata, decodificando el id
/// del diente clickeado según el esquema de grupos. `hosted_app` es la app enfocada
/// (para rutear un diente hospedado a su `HostToothActivate`).
fn rail_map(si: usize, hosted_app: String) -> Arc<dyn Fn(RagSidebarMsg) -> Msg + Send + Sync> {
    Arc::new(move |rm| match rm {
        RagSidebarMsg::Activate(id) => {
            if id >= SHUMA_ID {
                Msg::ShumaToggle
            } else if id >= FOOTER_BASE {
                // Diente de footer (sistema/sesión): abre su panel como cualquier tab,
                // con el id CODIFICADO (`panel_body` lo decodifica contra `footer_tabs`).
                Msg::NavTabActivate(si, id as usize)
            } else if id >= HOST_BASE {
                Msg::HostToothActivate(hosted_app.clone(), (id - HOST_BASE) as u32)
            } else if id >= TERM_BASE {
                Msg::TerminalSession((id - TERM_BASE) as usize)
            } else if id >= WS_BASE {
                Msg::WorkspaceTooth { si, ws: (id - WS_BASE) as u8 }
            } else {
                Msg::NavTabActivate(si, id as usize)
            }
        }
        RagSidebarMsg::SetRailOutside(b) => Msg::SidebarSetRailOutside(si, b),
        RagSidebarMsg::SetDocked(b) => Msg::SidebarSetDocked(si, b),
        RagSidebarMsg::SetAutohide(b) => Msg::SidebarSetAutohide(si, b),
        RagSidebarMsg::SetDosPasos(b) => Msg::SidebarSetDienteDosPasos(b),
        RagSidebarMsg::Resize(dx) => Msg::SidebarResize(si, dx),
        RagSidebarMsg::ControlToggle => Msg::SidebarControlToggle(si),
        RagSidebarMsg::SearchFocus(f) => Msg::SearchFocus(f),
        RagSidebarMsg::SearchSet(_) | RagSidebarMsg::Reveal(_) | RagSidebarMsg::Blur => Msg::NahualAnim,
    })
}

#[allow(clippy::too_many_arguments)]
fn rail_strip(
    surface: &Surface,
    si: usize,
    out: &str,
    thickness: f32,
    nav: &NavState,
    hosted: &[HostedTooth],
    hosted_app: &str,
    hosted_active: Option<u32>,
    shuma: &ShumaState,
    vivo: &DienteVivo,
    theme: &Theme,
    transparente: bool,
) -> View<Msg> {
    // Rail UNIFICADO: pata NO pinta rail propio — el widget `rag-sidebar` es dueño del
    // rail (dock-rail encapsulado adentro). Se le pasan los dientes como `RagTooth` y
    // el widget pinta la franja + botón de opciones + encía. Es el invariante «las dos
    // barras SON ragsidebar y nada más».
    let (teeth, grow) = build_rail_teeth(surface, nav, hosted, hosted_active, shuma, vivo);
    let footer = build_footer_teeth(surface);
    let state = rail_state(surface, si, nav, out);
    let side = if surface.anchor == Anchor::Right { RagSide::Right } else { RagSide::Left };
    llimphi_widget_rag_sidebar::rag_rail_view(
        &state,
        teeth,
        thickness,
        side,
        &rail_palette(theme, transparente),
        rail_map(si, hosted_app.to_string()),
        None,
        None,
        grow,
        footer,
    )
}

/// Paleta del rail. En modo **transparente** (config `rail_transparente`) el rail no
/// pinta fondo (`bg_panel_alt`/`bg_rail` a alpha 0) y los pills de los dientes quedan
/// translúcidos, para que se vea a través el frost que mirada aplica SÓLO bajo cada
/// diente (ver `over_layer_rects`/`input_frost_subrects` en mirada + la input-region
/// por-diente que setea `set_rail_teeth_input_region`). El icono/glifo del diente
/// sigue opaco. En modo normal, la paleta del tema tal cual.
fn rail_palette(theme: &Theme, transparente: bool) -> RagSidebarPalette {
    let mut p = RagSidebarPalette::from_theme(theme);
    if transparente {
        let clear = p.bg_panel_alt.with_alpha(0.0);
        p.bg_panel_alt = clear;
        p.rail.bg_rail = p.rail.bg_rail.with_alpha(0.0);
        // Pills translúcidos: el activo tiñe el frost con el acento; el hover, apenas.
        p.rail.bg_active = p.rail.bg_active.with_alpha(0.45);
        p.rail.bg_hover = p.rail.bg_hover.with_alpha(0.30);
    }
    p
}

/// El contenido del panel (cabezal con toggle de modo + navegador con scroll),
/// dimensionado para llenar su contenedor de alto `panel_h` px. Trae su propio
/// fondo y padding. `ti` es el diente desplegado.
#[allow(clippy::too_many_arguments)]
fn panel_body(
    surface: &Surface,
    si: usize,
    ti: usize,
    drawer_h: f32,
    nav: &NavState,
    shuma: &ShumaState,
    rag: &RagState,
    centro: &CentroDatos,
    theme: &Theme,
) -> (String, bool, Option<View<Msg>>, View<Msg>) {
    // UN solo lugar arma el cuerpo de CUALQUIER diente (tabs Y escritorios), así
    // ninguno se salta el chrome. `ti` es el id CODIFICADO del diente abierto: `>=
    // WS_BASE` = un diente-escritorio (su nº = `ti - WS_BASE`); `< WS_BASE` = el índice
    // de un tab de config. El cabezal uniforme lo pone el widget `rag-sidebar` con el
    // `(titulo, no_header, accessory)` que devolvemos. (Los dientes shuma/terminal/
    // hospedados NO abren panel de sidebar — nunca llegan aquí.)
    // `ti >= FOOTER_BASE` = un diente de FOOTER (`footer_tabs`), decodificado ANTES que
    // `WS_BASE` porque su base es mayor. `WS_BASE..FOOTER_BASE` = escritorio; `< WS_BASE`
    // = tab de config.
    let es_footer = ti as u64 >= FOOTER_BASE;
    let footer_tab = if es_footer {
        surface.footer_tabs.get((ti as u64 - FOOTER_BASE) as usize)
    } else {
        None
    };
    let es_ws = !es_footer && ti as u64 >= WS_BASE;
    let ws_n = (ti as u64).saturating_sub(WS_BASE) as u8;
    let tab = if es_ws || es_footer { footer_tab } else { surface.tabs.get(ti) };
    let titulo = if es_ws {
        format!("Escritorio {ws_n}")
    } else {
        tab.map(|t| t.label.clone()).unwrap_or_default()
    };
    let no_header = tab.map(|t| t.no_header).unwrap_or(false);
    // Despacho por el `kind` del CONTENIDO del diente. shuma se conecta como
    // diente: su contenido es el shell completo (drawer_body_view).
    let kind = tab.map(|t| t.content.kind.as_str()).unwrap_or("");
    let body_h = llimphi_widget_rag_sidebar::body_height(drawer_h, no_header);

    // Accesorio del header (derecha) + cuerpo autocontenido, según el `kind`. Todos
    // los cuerpos se dimensionan a `body_h` (traen su propio fondo/padding).
    let (accessory, body): (Option<View<Msg>>, View<Msg>) = if crate::es_sesion(kind) {
        // Diente de sistema/sesión (footer): info usuario+sistema, cambio de contexto
        // (pacha), wawa-panel y acciones de energía (con confirmación fullscreen).
        (None, super::sistema::sistema_panel(&centro.extras.pacha_cat, centro.ctx, nav, body_h, theme))
    } else if es_ws {
        // Taskbar del escritorio `ws_n` (las ventanas que viven ahí). `si` viaja
        // para que el menú contextual de cada pestaña se ancle a este drawer.
        (None, super::task_manager::workspace_taskbar_panel(centro.windows, si, ws_n, body_h, theme))
    } else if crate::es_diente_vivo(kind) {
        // Control center: reloj + «sonando ahora» + volumen/brillo/batería + redes.
        (None, control_center_view(body_h, centro, theme))
    } else if crate::es_monitor(kind) {
        // Monitor de sistema: CPU (promedio + cores) + RAM.
        (None, super::sistema_monitor_view(centro.ctx, body_h, theme))
    } else if crate::es_flota(kind) {
        // Flota (matilda): inventario read-only de hosts/contenedores/vhosts.
        (None, super::flota_view(centro.flota, centro.flota_remoto, nav.scroll, body_h, theme))
    } else if crate::es_unidades(kind) {
        // Unidades (sandokan): estado + telemetría del plano de control.
        (None, super::unidades_view(centro.unidades, nav.scroll, body_h, theme))
    } else if crate::es_actividad(kind) {
        // Actividad (willay): timeline cronológico + clips re-copiables.
        (None, super::actividad_view(centro.willay, nav.scroll, body_h, theme))
    } else if crate::es_pacha(kind) {
        // Perfil `pacha` (contextos de usuario): el panel es un SIDEBAR de
        // instancias (tabs, una activa) + el contenido de la seleccionada.
        (None, pacha_panel(&centro.extras.pacha_cat, nav, body_h, theme))
    } else if crate::rag::is_rag_kind(kind) {
        // Panel RAG (preguntale a tu correo): buscador semántico + respuesta.
        (None, crate::rag::body_view(rag, body_h, theme))
    } else if kind == "shuma" {
        (None, body_pane(crate::shuma::drawer_body_view(shuma, theme, true), body_h, theme))
    } else {
        // Navegador Árbol/Grafo: el toggle de modo va como accesorio del cabezal.
        let toggle = View::new(Style {
            size: Size { width: length(140.0_f32), height: length(HEADER_H - 1.0) },
            align_items: Some(AlignItems::Center),
            ..Default::default()
        })
        .children(vec![segmented_view(
            &NavMode::LABELS,
            nav.mode.index(),
            |i| Msg::NavSetMode(NavMode::from_index(i)),
            &SegmentedPalette::from_theme(theme),
        )]);
        (Some(toggle), nav_body(nav, body_h, theme))
    };
    (titulo, no_header, accessory, body)
}

/// Traduce el estado vivo de pata (config de la surface + [`NavState`]) al modelo del
/// **widget unificado** [`RagSidebarState`]: `open`/`selected` fijados al diente `ti`
/// desplegado, los 4 ejes desde el estado efectivo. pata sigue pintando su propio rail
/// (dientes vivos, workspaces, hospedados, shuma), así que el panel se dibuja en modo
/// **DELEGADO** con [`rag_panel_view`] (sin rail del widget).
fn sidebar_state(
    surface: &Surface,
    ti: usize,
    nav: &NavState,
    docked: bool,
    rail_outside: bool,
    autohide: bool,
    dos_pasos: bool,
) -> RagSidebarState {
    RagSidebarState {
        rail_outside,
        docked,
        autohide,
        dos_pasos,
        open: Some(ti as u64),
        selected: Some(ti as u64),
        panel_w: surface.panel_width,
        control_open: nav.control_open,
        search: nav.search.clone(),
        search_focused: nav.search_focused,
        revealed: true,
    }
}

/// Enruta los [`RagSidebarMsg`] del widget unificado (panel + multiselect) al `Msg` de
/// pata para el sidebar `si`, reusando los handlers existentes. Los mensajes que el
/// panel/multiselect no emiten en modo delegado (Reveal/Blur/SearchSet) caen a un no-op
/// ([`Msg::NahualAnim`]).
fn sidebar_map(si: usize) -> Arc<dyn Fn(RagSidebarMsg) -> Msg + Send + Sync> {
    Arc::new(move |rm| match rm {
        RagSidebarMsg::Activate(id) => Msg::NavTabActivate(si, id as usize),
        RagSidebarMsg::SetRailOutside(b) => Msg::SidebarSetRailOutside(si, b),
        RagSidebarMsg::SetDocked(b) => Msg::SidebarSetDocked(si, b),
        RagSidebarMsg::SetAutohide(b) => Msg::SidebarSetAutohide(si, b),
        RagSidebarMsg::SetDosPasos(b) => Msg::SidebarSetDienteDosPasos(b),
        RagSidebarMsg::Resize(dx) => Msg::SidebarResize(si, dx),
        RagSidebarMsg::ControlToggle => Msg::SidebarControlToggle(si),
        RagSidebarMsg::SearchFocus(f) => Msg::SearchFocus(f),
        RagSidebarMsg::SearchSet(_) | RagSidebarMsg::Reveal(_) | RagSidebarMsg::Blur => {
            Msg::NahualAnim
        }
    })
}

/// El **panel** del diente `ti` con el chrome unificado (buscador + control + cabezal),
/// SIN rail — modo DELEGADO. Reemplaza al viejo `drawer_spec` + `rag_drawer_view`. El
/// popover de disposición NO va aquí: lo saca [`multiselect_overlay`] a una capa externa
/// (así `panel_width` angosto no lo clipea). `t` alimenta el halo del control idle.
#[allow(clippy::too_many_arguments)]
fn panel_view(
    surface: &Surface,
    si: usize,
    ti: usize,
    drawer_h: f32,
    nav: &NavState,
    shuma: &ShumaState,
    rag: &RagState,
    centro: &CentroDatos,
    state: &RagSidebarState,
    t: f64,
    theme: &Theme,
) -> View<Msg> {
    let (title, no_header, accessory, body) =
        panel_body(surface, si, ti, drawer_h, nav, shuma, rag, centro, theme);
    // El widget deriva título/no_header del diente ABIERTO en `teeth`; basta con el
    // diente desplegado (su icono no se dibuja en delegado: pata pinta su propio rail).
    let teeth = vec![RagTooth {
        id: ti as u64,
        title,
        no_header,
        icon: Arc::new(|_size, _color| View::new(Style::default())),
        badge: None,
        separator_before: false,
        force_active: None,
        grow_after: None,
    }];
    rag_panel_view(
        state,
        &teeth,
        body,
        accessory,
        nav.search_hits_roots(),
        t,
        &RagSidebarPalette::from_theme(theme),
        sidebar_map(si),
    )
}

/// El popover de **disposición** (los 4 ejes) como capa EXTERNA: la card del widget
/// unificado ([`rag_multiselect_view`]) bajo la barra, arrimada al borde interno del
/// panel, MÁS un backdrop de click-away que la cierra. Va en la surface del drawer (de
/// ancho `w`, más ancha que el panel), FUERA del nodo del panel — así `panel_width`
/// angosto no la clipea (el bug del popover inline). `None` si el control está cerrado.
fn multiselect_overlay(
    surface: &Surface,
    si: usize,
    state: &RagSidebarState,
    w: f32,
    theme: &Theme,
) -> Option<View<Msg>> {
    if !state.control_open {
        return None;
    }
    let pal = RagSidebarPalette::from_theme(theme);
    let card = rag_multiselect_view(state, RagOptions::default(), sidebar_map(si), &pal);
    let right = surface.anchor == Anchor::Right;
    // Card (238 px de ancho) bajo la barra, arrimada al borde INTERNO del panel.
    let inset = if right {
        TaffyRect { left: auto(), right: length(8.0_f32), top: length(BAR_H + 2.0), bottom: auto() }
    } else {
        TaffyRect {
            left: length((state.panel_w - 238.0 - 8.0).max(8.0)),
            right: auto(),
            top: length(BAR_H + 2.0),
            bottom: auto(),
        }
    };
    let card_pos = View::new(Style { position: Position::Absolute, inset, ..Default::default() })
        .children(vec![card]);
    // Backdrop sobre toda la surface del drawer: un click fuera de la card la cierra.
    let backdrop = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(0.0_f32), right: auto(), top: length(0.0_f32), bottom: auto() },
        size: Size { width: length(w), height: percent(1.0_f32) },
        ..Default::default()
    })
    .on_click(Msg::SidebarControlToggle(si));
    Some(
        View::new(Style {
            position: Position::Absolute,
            inset: TaffyRect { left: length(0.0_f32), right: auto(), top: length(0.0_f32), bottom: auto() },
            size: Size { width: length(w), height: percent(1.0_f32) },
            ..Default::default()
        })
        .children(vec![backdrop, card_pos]),
    )
}

/// El **menú contextual de una ventana** como capa externa en la surface del
/// drawer (path layer-shell): la card [`super::task_manager::win_context_menu_card`]
/// anclada donde se hizo clic-derecho + un backdrop de click-away que la cierra
/// ([`Msg::WinMenuClose`]). Vive en la surface del drawer (640 px), como el popover
/// de disposición, así el panel angosto no la clipea. `None` si no hay menú abierto
/// para ESTE sidebar. `h` es el alto de la surface (para clampear el ancla al
/// borde); `ws_count` alimenta el submenú «Mover a escritorio…».
fn win_menu_overlay(
    nav: &NavState,
    si: usize,
    w: f32,
    h: f32,
    ws_count: u8,
    theme: &Theme,
) -> Option<View<Msg>> {
    let menu = nav.win_menu.as_ref().filter(|m| m.si == si)?;
    let card = super::task_manager::win_context_menu_card(menu, ws_count, theme);
    // Clamp del ancla dentro de la surface para que el popup no se corte. `est_h` es
    // un alto conservador de la card (cabecera + acciones + submenú de mover).
    let est_h = 340.0_f32;
    let x = menu.x.clamp(4.0, (w - super::task_manager::WIN_MENU_W - 4.0).max(4.0));
    let y = menu.y.clamp(4.0, (h - est_h).max(4.0));
    let card_pos = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(x), right: auto(), top: length(y), bottom: auto() },
        ..Default::default()
    })
    .children(vec![card]);
    let backdrop = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(0.0_f32), right: auto(), top: length(0.0_f32), bottom: auto() },
        size: Size { width: length(w), height: percent(1.0_f32) },
        ..Default::default()
    })
    .on_click(Msg::WinMenuClose);
    Some(
        View::new(Style {
            position: Position::Absolute,
            inset: TaffyRect { left: length(0.0_f32), right: auto(), top: length(0.0_f32), bottom: auto() },
            size: Size { width: length(w), height: percent(1.0_f32) },
            ..Default::default()
        })
        .children(vec![backdrop, card_pos]),
    )
}

/// El menú contextual de ventana (path **winit**, dev): la misma card anclada en
/// **coords de pantalla** (en winit todo vive en la ventana única) + un backdrop a
/// pantalla completa. `None` si no hay menú abierto para ESTE sidebar.
pub fn nav_win_menu_overlay(
    nav: &NavState,
    si: usize,
    screen: (i32, i32),
    ws_count: u8,
    theme: &Theme,
) -> Option<View<Msg>> {
    let menu = nav.win_menu.as_ref().filter(|m| m.si == si)?;
    let (sw, sh) = screen;
    let card = super::task_manager::win_context_menu_card(menu, ws_count, theme);
    let x = menu.x.clamp(4.0, (sw as f32 - super::task_manager::WIN_MENU_W - 4.0).max(4.0));
    let y = menu.y.clamp(4.0, (sh as f32 - 340.0).max(4.0));
    let card_pos = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(x), top: length(y), right: auto(), bottom: auto() },
        ..Default::default()
    })
    .children(vec![card]);
    let backdrop = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(0.0_f32), top: length(0.0_f32), right: auto(), bottom: auto() },
        size: Size { width: length(sw as f32), height: length(sh as f32) },
        ..Default::default()
    })
    .on_click(Msg::WinMenuClose);
    Some(
        View::new(Style {
            position: Position::Absolute,
            inset: TaffyRect { left: length(0.0_f32), top: length(0.0_f32), right: auto(), bottom: auto() },
            size: Size { width: length(sw as f32), height: length(sh as f32) },
            ..Default::default()
        })
        .children(vec![backdrop, card_pos]),
    )
}

/// Envuelve un contenido cualquiera en un **pane autocontenido** (fondo del panel
/// + padding), de alto `h`. Lo usan los cuerpos que no traían fondo propio (el
/// navegador y el diente-shuma); los demás content-views ya son autocontenidos.
fn body_pane(child: View<Msg>, h: f32, theme: &Theme) -> View<Msg> {
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: length(h) },
        padding: TaffyRect {
            left: length(PAD),
            right: length(PAD),
            top: length(PAD),
            bottom: length(PAD),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .children(vec![child])
}

/// El cuerpo del **navegador** (menú "Abrir con…" / aviso / árbol-grafo con
/// scroll), como pane autocontenido de alto `body_h`. La localización jerárquica
/// del buscador ya está reflejada en `nav.expanded`/`nav.selected` (vía
/// [`NavState::apply_search`]), así que aquí sólo se pinta el estado.
fn nav_body(nav: &NavState, body_h: f32, theme: &Theme) -> View<Msg> {
    let viewport = (body_h - PAD * 2.0).max(0.0);
    let cuerpo = if let Some(mid) = nav.menu {
        open_with_menu(nav, mid, theme)
    } else if nav.roots.is_empty() {
        aviso_view(nav, theme, viewport)
    } else {
        let row_h = match nav.mode {
            NavMode::Tree => TREE_ROW_H,
            NavMode::Graph => GRAPH_ROW_H,
        };
        let visibles = count_visible(&nav.roots, &nav.expanded);
        let content_len = visibles as f32 * row_h + 16.0;
        let offset = clamp_offset(nav.scroll, content_len, viewport);

        let navv = navigator_view(
            NavSpec {
                roots: &nav.roots,
                mode: nav.mode,
                selected: nav.selected,
                palette: NavPalette::from_theme(theme),
                guides: true,
            },
            |id| nav.expanded.contains(&id),
            Msg::NavToggle,
            Msg::NavSelect,
            // Right-click sobre un archivo → menú "Abrir con…".
            Some(Msg::NavContextMenu),
        );

        scroll_y(
            offset,
            content_len,
            viewport,
            navv,
            Msg::NavScroll,
            &ScrollPalette::from_theme(theme),
        )
    };
    body_pane(cuerpo, body_h, theme)
}

// =====================================================================
// Panel del diente PERFIL `pacha` (contextos de usuario): un sidebar de
// instancias (tabs, una activa) + el contenido de la seleccionada.
// =====================================================================

/// Alto de una fila-tab de instancia.
const PACHA_TAB_H: f32 = 34.0;
/// Alto de una fila clave/valor del detalle.
const KV_H: f32 = 22.0;
/// Verde vivo para el marcador de la instancia activa.
fn pacha_verde() -> Color {
    Color::from_rgba8(52, 211, 153, 255)
}

/// Resuelve qué instancia se ve: la pedida por UI si existe, si no la activa, si
/// no la primera. Cadena para que el panel nunca quede sin selección con datos.
fn pacha_sel_id(infos: &[crate::perfil::PachaInfo], want: Option<&str>) -> String {
    if let Some(w) = want {
        if infos.iter().any(|p| p.id == w) {
            return w.to_string();
        }
    }
    infos
        .iter()
        .find(|p| p.active)
        .or_else(|| infos.first())
        .map(|p| p.id.clone())
        .unwrap_or_default()
}

/// El panel del diente perfil: tabs de instancias arriba + detalle de la
/// seleccionada abajo, todo scrolleable. Autocontenido a `body_h`.
fn pacha_panel(
    infos: &[crate::perfil::PachaInfo],
    nav: &NavState,
    body_h: f32,
    theme: &Theme,
) -> View<Msg> {
    if infos.is_empty() {
        let aviso = View::new(Style {
            size: Size { width: percent(1.0_f32), height: length((body_h - PAD * 2.0).max(40.0)) },
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            padding: TaffyRect {
                left: length(12.0_f32),
                right: length(12.0_f32),
                top: length(0.0_f32),
                bottom: length(0.0_f32),
            },
            ..Default::default()
        })
        .text(
            "Sin contextos definidos.\nDefinilos en ~/.config/pacha/pachas.ron".to_string(),
            12.0,
            theme.fg_muted,
        );
        return body_pane(aviso, body_h, theme);
    }

    let sel_id = pacha_sel_id(infos, nav.pacha_sel.as_deref());
    // Tabs (una fila por instancia).
    let tabs: Vec<View<Msg>> = infos
        .iter()
        .map(|p| pacha_tab_row(p, p.id == sel_id, theme))
        .collect();
    let tabs_h = infos.len() as f32 * (PACHA_TAB_H + 4.0);
    let tabs_col = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: length(tabs_h) },
        gap: Size { width: length(0.0_f32), height: length(4.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(tabs);

    // Detalle de la instancia seleccionada.
    let (det_rows, det_h) = infos
        .iter()
        .find(|p| p.id == sel_id)
        .map(|p| pacha_detalle(p, theme))
        .unwrap_or((Vec::new(), 0.0));
    let detalle = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: length(det_h) },
        gap: Size { width: length(0.0_f32), height: length(4.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(det_rows);

    let sep = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(1.0_f32) },
        margin: TaffyRect {
            left: length(0.0_f32),
            right: length(0.0_f32),
            top: length(8.0_f32),
            bottom: length(8.0_f32),
        },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(theme.fg_muted);

    let inner = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: auto() },
        ..Default::default()
    })
    .children(vec![tabs_col, sep, detalle]);

    let content_len = tabs_h + 17.0 + det_h;
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
    body_pane(scrolled, body_h, theme)
}

/// Una fila-tab de instancia: marcador (activa ●, si no ○) + nombre + ciclo de
/// vida. La seleccionada va resaltada; al click manda [`Msg::PachaSelect`].
fn pacha_tab_row(p: &crate::perfil::PachaInfo, selected: bool, theme: &Theme) -> View<Msg> {
    let dot_color = if p.active { pacha_verde() } else { theme.fg_muted };
    let dot = View::new(Style {
        size: Size { width: length(16.0_f32), height: length(PACHA_TAB_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text(if p.active { "●" } else { "○" }.to_string(), 11.0, dot_color);
    let nombre = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: length(0.0_f32), height: length(PACHA_TAB_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(
        p.label.clone(),
        13.0,
        if selected { theme.accent } else { theme.fg_text },
    );
    let ciclo = View::new(Style {
        size: Size { width: length(66.0_f32), height: length(PACHA_TAB_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::End),
        ..Default::default()
    })
    .text(p.lifecycle.clone(), 10.0, theme.fg_muted);
    let bg = if selected { theme.bg_button_hover } else { theme.bg_panel_alt };
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(PACHA_TAB_H) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(6.0_f32),
            right: length(8.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(bg)
    .hover_fill(theme.bg_button_hover)
    .radius(6.0)
    .on_click(Msg::PachaSelect(Some(p.id.clone())))
    .children(vec![dot, nombre, ciclo])
}

/// Las filas del **detalle** de una instancia + su alto total. Resume el contexto:
/// estado, política al dejarlo, vista, dotfiles, apps de la receta, overlay de
/// config y recursos; más el botón «Activar» si no es el contexto en uso.
fn pacha_detalle(p: &crate::perfil::PachaInfo, theme: &Theme) -> (Vec<View<Msg>>, f32) {
    let mut rows: Vec<View<Msg>> = Vec::new();
    let mut h = 0.0_f32;

    let estado = if p.active { "activo (en uso)".to_string() } else { p.lifecycle.clone() };
    rows.push(pacha_kv("Estado", &estado, theme));
    h += KV_H;
    rows.push(pacha_kv("Al dejarlo", &p.on_leave, theme));
    h += KV_H;
    if p.persist {
        rows.push(pacha_kv("Al reabrir", "recuerda las apps abiertas", theme));
        h += KV_H;
    }
    if let Some(v) = &p.vista {
        rows.push(pacha_kv("Vista", v, theme));
        h += KV_H;
    }
    if p.dotfiles > 0 {
        rows.push(pacha_kv("Dotfiles", &format!("{} sets", p.dotfiles), theme));
        h += KV_H;
    }

    if !p.apps.is_empty() {
        rows.push(pacha_section("Apps", theme));
        h += KV_H;
        for a in &p.apps {
            rows.push(pacha_app_row(a, theme));
            h += KV_H;
        }
    }
    if !p.overlay.is_empty() {
        rows.push(pacha_section("Config", theme));
        h += KV_H;
        for (k, v) in &p.overlay {
            rows.push(pacha_kv(k, v, theme));
            h += KV_H;
        }
    }
    if !p.resources.is_empty() {
        rows.push(pacha_section("Recursos", theme));
        h += KV_H;
        for (k, v) in &p.resources {
            rows.push(pacha_kv(k, v, theme));
            h += KV_H;
        }
    }

    if !p.active {
        rows.push(pacha_activar(&p.id, theme));
        h += 34.0 + 8.0;
    }
    // Sumar los gaps de 4 px entre filas.
    h += (rows.len().saturating_sub(1)) as f32 * 4.0;
    (rows, h)
}

/// Una fila clave/valor del detalle (clave atenuada a la izquierda, valor a la
/// derecha ocupando el resto).
fn pacha_kv(clave: &str, valor: &str, theme: &Theme) -> View<Msg> {
    let k = View::new(Style {
        size: Size { width: length(84.0_f32), height: length(KV_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(clave.to_string(), 11.0, theme.fg_muted);
    let v = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: length(0.0_f32), height: length(KV_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(valor.to_string(), 11.0, theme.fg_text);
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(KV_H) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(vec![k, v])
}

/// Un rótulo de sección del detalle (Apps / Config / Recursos).
fn pacha_section(titulo: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(KV_H) },
        align_items: Some(AlignItems::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .text(titulo.to_string(), 11.0, theme.accent)
}

/// Una fila de app de la receta: el comando (+ una marca «aislada» si corre con
/// `$HOME` propio).
fn pacha_app_row(a: &crate::perfil::AppLinea, theme: &Theme) -> View<Msg> {
    let cmd = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: length(0.0_f32), height: length(KV_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(a.command.clone(), 11.0, theme.fg_text);
    let mut hijos = vec![cmd];
    if a.aislada {
        hijos.push(
            View::new(Style {
                size: Size { width: length(52.0_f32), height: length(KV_H) },
                align_items: Some(AlignItems::Center),
                justify_content: Some(JustifyContent::End),
                ..Default::default()
            })
            .text("· aislada".to_string(), 10.0, theme.fg_muted),
        );
    }
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(KV_H) },
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(0.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(hijos)
}

/// Botón «Activar este contexto» → [`Msg::SwitchPacha`] (`pacha switch <id>`).
fn pacha_activar(id: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(34.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        margin: TaffyRect {
            left: length(0.0_f32),
            right: length(0.0_f32),
            top: length(8.0_f32),
            bottom: length(0.0_f32),
        },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(theme.accent)
    .hover_fill(theme.bg_button_hover)
    .radius(8.0)
    .on_click(Msg::SwitchPacha(id.to_string()))
    .text("Activar este contexto".to_string(), 12.0, theme.bg_panel)
}

// =====================================================================
// Backend winit: superficies en absoluto sobre la ventana única
// =====================================================================

/// El rail de un `SurfaceKind::Sidebar`, posicionado en el rect que el layout
/// reservó para él (path winit). `si` es el índice de la superficie.
pub fn sidebar_rail_view(
    surface: &Surface,
    si: usize,
    rect: Rect,
    nav: &NavState,
    shuma: &ShumaState,
    vivo: &DienteVivo,
    theme: &Theme,
) -> View<Msg> {
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(rect.x as f32),
            top: length(rect.y as f32),
            right: auto(),
            bottom: auto(),
        },
        size: Size {
            width: length(rect.w as f32),
            height: length(rect.h as f32),
        },
        ..Default::default()
    })
    // El path winit no conoce el foco (no hay toplevels) → sin dientes hospedados,
    // pero el diente de shuma es in-process y sí aparece.
    // El backend winit (dev, ventana anidada) no tiene glass de mirada → rail opaco.
    .children(vec![rail_strip(surface, si, "", rect.w as f32, nav, &[], "", None, shuma, vivo, theme, false)])
}

/// El panel flotante del diente `ti` desplegado (path winit): flota junto al
/// `rail_rect` (a su derecha si el sidebar está a la izquierda, a su izquierda si
/// está a la derecha). `si` es el índice de la surface (para enrutar sus `Msg`).
#[allow(clippy::too_many_arguments)]
pub fn nav_panel_view(
    surface: &Surface,
    si: usize,
    ti: usize,
    rail_rect: Rect,
    screen: (i32, i32),
    nav: &NavState,
    shuma: &ShumaState,
    rag: &RagState,
    centro: &CentroDatos,
    theme: &Theme,
) -> View<Msg> {
    let pw = surface.panel_width;
    let (_, sh) = screen;
    let h = (rail_rect.h as f32).min(sh as f32);
    let y = rail_rect.y as f32;
    let x = match surface.anchor {
        Anchor::Right => (rail_rect.x as f32 - pw).max(0.0),
        _ => (rail_rect.x + rail_rect.w) as f32,
    };

    // Path winit (dev): monta el mismo widget unificado en modo DELEGADO (solo el
    // panel; el rail lo pinta `sidebar_rail_view` aparte). La disposición sale de los
    // overrides del `Surface` (o defaults sensatos), sin reloj de animación.
    let state = sidebar_state(
        surface,
        ti,
        nav,
        surface.reserve.unwrap_or(true),
        surface.rail_outside.unwrap_or(false),
        surface.autohide,
        false,
    );
    let panel = panel_view(surface, si, ti, h, nav, shuma, rag, centro, &state, 0.0, theme);
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(x),
            top: length(y),
            right: auto(),
            bottom: auto(),
        },
        size: Size {
            width: length(pw),
            height: length(h),
        },
        ..Default::default()
    })
    .children(vec![panel])
}

/// El popover de disposición del sidebar `si` (path winit): la card
/// [`rag_multiselect_view`] posicionada en pantalla junto al control del panel, más un
/// backdrop de click-away a pantalla completa. `None` si el control está cerrado. En
/// winit todo vive en la ventana única, así que la card se ubica en coords absolutas.
pub fn nav_multiselect_overlay(
    surface: &Surface,
    si: usize,
    rail_rect: Rect,
    screen: (i32, i32),
    nav: &NavState,
    theme: &Theme,
) -> Option<View<Msg>> {
    if !nav.control_open {
        return None;
    }
    let pw = surface.panel_width;
    let (sw, sh) = screen;
    let y = rail_rect.y as f32;
    let panel_x = match surface.anchor {
        Anchor::Right => (rail_rect.x as f32 - pw).max(0.0),
        _ => (rail_rect.x + rail_rect.w) as f32,
    };
    let right = surface.anchor == Anchor::Right;
    let card_x = if right {
        panel_x + 8.0
    } else {
        panel_x + (pw - 238.0 - 8.0).max(8.0)
    };
    let state = sidebar_state(
        surface,
        0,
        nav,
        surface.reserve.unwrap_or(true),
        surface.rail_outside.unwrap_or(false),
        surface.autohide,
        false,
    );
    let card = rag_multiselect_view(
        &state,
        RagOptions::default(),
        sidebar_map(si),
        &RagSidebarPalette::from_theme(theme),
    );
    let card_pos = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(card_x),
            top: length(y + BAR_H + 2.0),
            right: auto(),
            bottom: auto(),
        },
        ..Default::default()
    })
    .children(vec![card]);
    let backdrop = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(0.0_f32), top: length(0.0_f32), right: auto(), bottom: auto() },
        size: Size { width: length(sw as f32), height: length(sh as f32) },
        ..Default::default()
    })
    .on_click(Msg::SidebarControlToggle(si));
    Some(
        View::new(Style {
            position: Position::Absolute,
            inset: TaffyRect { left: length(0.0_f32), top: length(0.0_f32), right: auto(), bottom: auto() },
            size: Size { width: length(sw as f32), height: length(sh as f32) },
            ..Default::default()
        })
        .children(vec![backdrop, card_pos]),
    )
}

// =====================================================================
// Backend layer-shell: el rail y el drawer viven en surfaces SEPARADAS
// =====================================================================

/// La vista que llena la layer surface del **rail** de un `SurfaceKind::Sidebar`
/// de tamaño `(w, h)` px (con `w == thickness`). Es SÓLO la franja de dientes: el
/// panel desplegado lo pinta [`sidebar_drawer_view`] en su propia surface aparte.
///
/// (Antes esta surface crecía en ancho para alojar el panel; ese resize por-diente
/// fallaba al reconfigurar el swapchain en Iris Xe y dejaba el panel sin clicks —
/// por eso el panel es ahora una surface de tamaño fijo independiente.)
#[allow(clippy::too_many_arguments)]
pub fn sidebar_surface_view(
    surface: &Surface,
    si: usize,
    out: &str,
    w: f32,
    h: f32,
    nav: &NavState,
    hosted: &[HostedTooth],
    hosted_app: &str,
    hosted_active: Option<u32>,
    shuma: &ShumaState,
    vivo: &DienteVivo,
    theme: &Theme,
    transparente: bool,
) -> View<Msg> {
    let thickness = surface.thickness;
    let rail = rail_strip(
        surface,
        si,
        out,
        thickness,
        nav,
        hosted,
        hosted_app,
        hosted_active,
        shuma,
        vivo,
        theme,
        transparente,
    );
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: length(w),
            height: length(h),
        },
        ..Default::default()
    })
    .children(vec![rail])
}

/// La vista que llena la layer surface del **drawer** del sidebar `si`: la barrita
/// de control (una línea) arriba + el contenido del diente `ti` debajo, a ancho
/// fijo `w == panel_width`. Es una surface aparte del rail, pegada a su borde
/// interno. `docked`/`rail_outside` son el estado efectivo de los dos ejes (para
/// los switches de la barrita).
#[allow(clippy::too_many_arguments)]
/// Vista del drawer CERRADO: un contenedor vacío del tamaño de la surface, SIN
/// fill. El `render` limpia a transparente, así el drawer queda invisible (y con
/// input-region vacía, el puntero lo atraviesa). La surface sigue mapeada — nunca
/// se destruye — para no perder el `VkSurface` en Iris Xe.
pub fn sidebar_drawer_hidden(w: f32, h: f32) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: length(w),
            height: length(h),
        },
        ..Default::default()
    })
}

#[allow(clippy::too_many_arguments)]
pub fn sidebar_drawer_view(
    surface: &Surface,
    si: usize,
    out: &str,
    ti: usize,
    w: f32,
    h: f32,
    nav: &NavState,
    shuma: &ShumaState,
    rag: &RagState,
    centro: &CentroDatos,
    docked: bool,
    rail_outside: bool,
    autohide: bool,
    dos_pasos: bool,
    t: f64,
    theme: &Theme,
) -> View<Msg> {
    // Todo el chrome (barra buscador+control, cabezal uniforme) lo pinta el widget
    // unificado en modo DELEGADO ([`rag_panel_view`]); pata aporta el cuerpo, los datos
    // y su propio rail (surface aparte). El popover de disposición sale a una capa
    // externa ([`multiselect_overlay`]) para no clipearlo con `panel_width`.
    let state = sidebar_state(surface, ti, nav, docked, rail_outside, autohide, dos_pasos);
    // ¿Hay un diente REALMENTE desplegado en ESTE sidebar? Si no, el drawer se está
    // mostrando SÓLO para hostear la ventanita de opciones (abierta con la línea
    // punteada del rail colapsado): en ese caso se pinta únicamente el overlay, sin
    // panel ni divisor (la surface queda transparente salvo la card).
    let diente_abierto = nav.open_en(out).filter(|(s, _)| *s == si).is_some();
    if !diente_abierto {
        return match multiselect_overlay(surface, si, &state, w, theme) {
            Some(ov) => View::new(Style {
                position: Position::Relative,
                size: Size { width: length(w), height: length(h) },
                ..Default::default()
            })
            .children(vec![ov]),
            None => sidebar_drawer_hidden(w, h),
        };
    }
    // La surface `w` es más ancha que el panel (ver `DRAWER_SURFACE_W`): el panel se
    // pinta a `panel_width` pegado al borde INTERNO (el que toca el rail) y un divisor
    // arrastrable en su borde hacia el centro cambia el ancho en vivo.
    let panel_w = surface.panel_width.clamp(1.0, w);
    let panel = panel_view(surface, si, ti, h, nav, shuma, rag, centro, &state, t, theme);
    let right = surface.anchor == Anchor::Right;
    // Panel pegado al borde interno de la surface.
    let panel_inset = if right {
        TaffyRect { left: auto(), right: length(0.0_f32), top: length(0.0_f32), bottom: auto() }
    } else {
        TaffyRect { left: length(0.0_f32), right: auto(), top: length(0.0_f32), bottom: auto() }
    };
    let panel_abs = View::new(Style {
        position: Position::Absolute,
        inset: panel_inset,
        size: Size { width: length(panel_w), height: length(h) },
        ..Default::default()
    })
    .children(vec![panel]);
    // Divisor: 6 px en el borde del panel que da al centro, DENTRO del área del panel
    // (para que la input-region lo cubra). Izquierdo: a x = panel_w-6, arrastrar a la
    // derecha agranda (+dx). Derecho: a x = w-panel_w, arrastrar a la izquierda agranda
    // (se invierte el signo).
    let (div_left, sign) = if right {
        ((w - panel_w).max(0.0), -1.0_f32)
    } else {
        ((panel_w - 6.0).max(0.0), 1.0_f32)
    };
    let divider = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(div_left), right: auto(), top: length(0.0_f32), bottom: length(0.0_f32) },
        size: Size { width: length(6.0_f32), height: auto() },
        ..Default::default()
    })
    .hover_fill(theme.accent)
    .draggable(move |phase, dx, _dy| match phase {
        DragPhase::Move => Some(Msg::SidebarResize(si, sign * dx)),
        _ => None,
    });
    let mut hijos = vec![panel_abs, divider];
    // Popover de disposición: capa externa en la surface del drawer (más ancha que el
    // panel), fuera del nodo del panel → `panel_width` angosto no lo clipea. La
    // input-region de la surface se ensancha a toda ella mientras está abierto (ver
    // `Msg::SidebarControlToggle` en el backend layer-shell).
    if let Some(ov) = multiselect_overlay(surface, si, &state, w, theme) {
        hijos.push(ov);
    }
    // Menú contextual de una pestaña del taskbar (por encima de todo): popup anclado
    // al cursor + backdrop de click-away. Sólo si el menú es de ESTE sidebar.
    if let Some(ov) = win_menu_overlay(nav, si, w, h, centro.ctx.workspace_count, theme) {
        hijos.push(ov);
    }
    View::new(Style {
        position: Position::Relative,
        size: Size { width: length(w), height: length(h) },
        ..Default::default()
    })
    .children(hijos)
}

// =====================================================================
// Auxiliares
// =====================================================================

/// El menú "Abrir con…" sobre el archivo `id`: una fila por app nativa que
/// declare su mime (de `nav.menu_options`), más "el sistema" (`xdg-open`) y
/// "Cancelar". Se pinta en el cuerpo del panel (sin overlay flotante: así
/// funciona idéntico en winit y layer-shell sin necesitar coords del cursor).
fn open_with_menu(nav: &NavState, id: NavId, theme: &Theme) -> View<Msg> {
    let path = nav.file_path(id).unwrap_or("");
    let name = path.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(path);

    let mut rows: Vec<View<Msg>> = Vec::new();
    rows.push(
        View::new(Style {
            size: Size {
                width: percent(1.0_f32),
                height: length(28.0_f32),
            },
            align_items: Some(AlignItems::Center),
            ..Default::default()
        })
        .text(
            t_args("pata-open-with-title", &[("name", name.to_string().into())]),
            12.0,
            theme.fg_muted,
        ),
    );
    for (app_id, label) in &nav.menu_options {
        let aid = app_id.clone();
        rows.push(menu_button(label, theme).on_click(Msg::NavOpenWith(id, Some(aid))));
    }
    rows.push(menu_button(&t("pata-open-with-system"), theme).on_click(Msg::NavOpenWith(id, None)));
    rows.push(menu_button(&t("cancel"), theme).on_click(Msg::NavMenuCancel));

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: auto(),
        },
        gap: Size {
            width: length(0.0_f32),
            height: length(4.0_f32),
        },
        ..Default::default()
    })
    .children(rows)
}

/// Una fila clickeable del menú "Abrir con…". El caller le cuelga el `on_click`.
fn menu_button(label: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(30.0_f32),
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
    .fill(theme.bg_panel_alt)
    .hover_fill(theme.bg_button_hover)
    .radius(6.0)
    .text(label.to_string(), 13.0, theme.fg_text)
}

/// Un aviso centrado cuando no hay Mónadas que mostrar (conectando, o error).
fn aviso_view(nav: &NavState, theme: &Theme, viewport: f32) -> View<Msg> {
    let (texto, color) = match &nav.error {
        Some(e) => (e.clone(), theme.fg_muted),
        None => (t("pata-connecting-nouser"), theme.fg_muted),
    };
    View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(viewport.max(40.0)),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text(texto, 12.0, color)
}

/// Cuenta los nodos visibles del bosque dado el conjunto de expandidos — para
/// dimensionar el alto del contenido del scroll.
fn count_visible(roots: &[NavNode], expanded: &HashSet<u64>) -> usize {
    fn walk(node: &NavNode, expanded: &HashSet<u64>, acc: &mut usize) {
        *acc += 1;
        if node.has_children() && expanded.contains(&node.id) {
            for c in &node.children {
                walk(c, expanded, acc);
            }
        }
    }
    let mut acc = 0;
    for r in roots {
        walk(r, expanded, &mut acc);
    }
    acc
}

/// El icono de un diente del rail: un **icono vectorial coloreado** (Lucide-like,
/// `llimphi-icons`) según el nombre declarado en el `SidebarTab`. Cada tipo trae
/// su propio color vivo y distintivo, así el rail se lee de un vistazo (Mónadas
/// violeta, Archivos ámbar, Buscar azul, etc.) en vez de glifos monocromos.
///
/// El `_color` que el rail resuelve (acento/atenuado) se ignora a propósito: el
/// estado activo ya lo marca el fondo del diente (pastilla + barra de acento),
/// así que el icono puede quedarse siempre a todo color.
fn tooth_icon(name: &str, size: f32, _color: Color) -> View<Msg> {
    let (icon, color) = tooth_icon_kind(name);
    // Contenedor de tamaño fijo (`size`×`size`): `icon_view` se pinta en
    // posición absoluta llenando a su padre, así que necesita una caja acotada.
    View::new(Style {
        size: Size {
            width: length(size),
            height: length(size),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .children(vec![llimphi_icons::icon_view::<Msg>(icon, color, 1.9)])
}

/// Mapea el nombre del diente a `(icono, color vivo)`. Tolerante a sinónimos
/// es/en y a los nombres que usan los dientes hospedados / shuma.
fn tooth_icon_kind(name: &str) -> (llimphi_icons::Icon, Color) {
    use llimphi_icons::Icon;
    match name {
        "monads" | "monadas" | "monad" | "astro" => (Icon::Link, Color::from_rgba8(167, 139, 250, 255)), // violeta
        "files" | "archivos" | "file" | "dir" | "folder" | "tree" => {
            (Icon::Folder, Color::from_rgba8(251, 191, 36, 255)) // ámbar
        }
        "search" | "buscar" | "find" => (Icon::Search, Color::from_rgba8(96, 165, 250, 255)), // azul
        "rag" | "ask" | "ai" | "correo" | "mail" => {
            (Icon::Search, Color::from_rgba8(167, 139, 250, 255)) // violeta: preguntale a tu correo
        }
        "home" | "inicio" => (Icon::Home, Color::from_rgba8(52, 211, 153, 255)),              // verde
        "control" | "vivo" => (Icon::Gauge, Color::from_rgba8(45, 212, 191, 255)), // teal: diente vivo
        "monitor" | "sistema" | "sysmon" => (Icon::Equalizer, Color::from_rgba8(96, 165, 250, 255)), // azul: monitor
        "flota" | "fleet" | "matilda" | "server" | "servers" => {
            (Icon::Globe, Color::from_rgba8(45, 212, 191, 255)) // teal: flota baremetal
        }
        "unidades" | "units" | "sandokan" => (Icon::Grid, Color::from_rgba8(167, 139, 250, 255)), // violeta: plano de control
        "actividad" | "activity" | "timeline" | "willay" => {
            (Icon::Bell, Color::from_rgba8(52, 211, 153, 255)) // verde: centro de actividad
        }
        "pacha" | "perfil" | "contexto" | "contextos" => {
            (Icon::User, Color::from_rgba8(251, 146, 60, 255)) // naranja: contextos de usuario
        }
        "tools" | "herramientas" | "settings" | "config" => {
            (Icon::Settings, Color::from_rgba8(45, 212, 191, 255)) // teal
        }
        "sesion" | "session" | "power" | "apagar" | "energia" | "system" => {
            (Icon::Power, Color::from_rgba8(248, 113, 113, 255)) // rojo suave: sistema/sesión
        }
        "shell" | "terminal" | "consola" => (Icon::Code, Color::from_rgba8(74, 222, 128, 255)), // verde lima
        "image" | "imagen" | "gallery" | "galeria" => (Icon::Image, Color::from_rgba8(244, 114, 182, 255)), // rosa
        "music" | "audio" | "musica" => (Icon::Music, Color::from_rgba8(232, 121, 249, 255)), // fucsia
        "film" | "video" | "media" => (Icon::Film, Color::from_rgba8(248, 113, 113, 255)),    // rojo
        "code" | "codigo" | "dev" => (Icon::Code, Color::from_rgba8(125, 211, 252, 255)),     // celeste
        "info" => (Icon::Info, Color::from_rgba8(96, 165, 250, 255)),
        _ => (Icon::File, Color::from_rgba8(148, 163, 184, 255)), // gris azulado neutro
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llimphi_widget_navigator::{NavKind, NavNode};

    fn forest() -> Vec<NavNode> {
        vec![
            NavNode::branch(
                1,
                "m1",
                NavKind::Monad,
                vec![NavNode::leaf(11, "a", NavKind::File), NavNode::leaf(12, "b", NavKind::File)],
            ),
            NavNode::leaf(2, "m2", NavKind::Monad),
        ]
    }

    #[test]
    fn count_visible_respeta_expansion() {
        let roots = forest();
        // Colapsado: sólo las 2 raíces.
        let none = HashSet::new();
        assert_eq!(count_visible(&roots, &none), 2);
        // Expandida la primera: 2 raíces + 2 hijos.
        let mut exp = HashSet::new();
        exp.insert(1u64);
        assert_eq!(count_visible(&roots, &exp), 4);
    }

    #[test]
    fn terminal_badge_arbitro() {
        // La sesión activa nunca badgea, aunque tenga alertas o haya fallado.
        assert_eq!(terminal_badge(true, 3, Some(false)), None);
        // Comando largo terminado en sesión no-activa → chip ámbar con el conteo.
        assert_eq!(
            terminal_badge(false, 2, None),
            Some(DockBadge::Count(2, BadgeKind::Warning))
        );
        // El comando largo gana al fallo (aviso más informativo con conteo).
        assert_eq!(
            terminal_badge(false, 1, Some(false)),
            Some(DockBadge::Count(1, BadgeKind::Warning))
        );
        // Sin alertas pero el último comando falló → punto rojo.
        assert_eq!(
            terminal_badge(false, 0, Some(false)),
            Some(DockBadge::Dot(BadgeKind::Error))
        );
        // Reposo / último comando ok → sin badge (el LED distingue actividad).
        assert_eq!(terminal_badge(false, 0, Some(true)), None);
        assert_eq!(terminal_badge(false, 0, None), None);
    }

    /// CANDADO anti-regresión («fijo y eterno») del NUEVO diseño: el sidebar
    /// IZQUIERDO del preset REAL es el **navegador de escritorios** — sus dientes son
    /// SÓLO los workspaces vivos (rango WS) más el diente de shuma; NINGÚN diente de
    /// herramienta (pacha/mónadas/archivos/correo…, id < WS_BASE) vive aquí: todos se
    /// mudaron al sidebar DERECHO. Si alguien vuelve a mezclar herramientas en el rail
    /// izquierdo, vuelve a los «botones sin sidebar» del slot start, o saca los
    /// escritorios, esto se pone ROJO. Es el mecanismo que corta el patrón «se ve bien
    /// en el PNG y en metal no está».
    #[test]
    fn candado_preset_izquierdo_es_navegador_de_escritorios_puro() {
        use pata_core::config::{Anchor, Config, SurfaceKind};
        let cfg = Config::preset();
        let izq = cfg
            .surfaces
            .iter()
            .find(|s| s.kind == SurfaceKind::Sidebar && s.anchor == Anchor::Left)
            .expect("el preset trae un sidebar izquierdo");

        let mut ctx = pata_core::widget::WidgetCtx::default();
        ctx.active_workspace = 1;
        ctx.workspace_count = 4;
        ctx.workspace_occupied = 0b0101; // escritorios 1 y 3 ocupados
        let vivo = DienteVivo {
            manifest: pata_core::atencion::Manifestacion::Reposo,
            cava_frame: &[],
            ctx: &ctx,
            unidades: None,
            flota_remoto: None,
            windows: &[],
            terminal_sessions: &[],
            t: 0.0,
        };
        let nav = NavState::default();
        let shuma = ShumaState::default();
        let (teeth, _grow) = build_rail_teeth(izq, &nav, &[], None, &shuma, &vivo);

        // Hay dientes-ESCRITORIO (id codificado en el rango WS) …
        assert!(
            teeth.iter().any(|t| (WS_BASE..TERM_BASE).contains(&t.id)),
            "los escritorios deben ser DIENTES del rail (no botones del slot start)"
        );
        // … y NINGÚN diente de herramienta (tab de config, id < WS_BASE): las
        // herramientas se mudaron al sidebar derecho.
        assert!(
            !teeth.iter().any(|t| t.id < WS_BASE),
            "el rail izquierdo NO lleva herramientas — sólo escritorios (+ shuma)"
        );
        // … y la ENCÍA «+» del grupo escritorios (intercalada en el hueco tras un
        // escritorio ocupado) crea uno nuevo: con 1 y 3 ocupados, el 2 tiene su «+».
        assert!(
            teeth.iter().any(|t| t.grow_after.is_some()),
            "la encía «+» crece un escritorio nuevo en el hueco"
        );

        // Contraparte: las herramientas mudadas viven en el sidebar DERECHO.
        let der = cfg
            .surfaces
            .iter()
            .find(|s| s.kind == SurfaceKind::Sidebar && s.anchor == Anchor::Right)
            .expect("el preset trae un sidebar derecho");
        for herramienta in ["pacha", "monads", "eventos", "monitor"] {
            assert!(
                der.tabs.iter().any(|t| t.icon == herramienta),
                "la herramienta «{herramienta}» vive en el sidebar derecho"
            );
        }
    }
}
