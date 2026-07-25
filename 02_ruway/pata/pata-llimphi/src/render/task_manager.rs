//! Task manager, workspace switcher, tray, portapapeles y botón de inicio:
//! todos los widgets de la barra que muestran estado dinámico del host.

use llimphi_theme::Theme;
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{
        auto, length, percent, AlignItems, FlexDirection, JustifyContent, Size, Style,
    },
    AlignContent, FlexWrap, Rect as TaffyRect,
};
use llimphi_ui::llimphi_raster::peniko::{
    Blob, Color, ImageAlphaType, ImageBrush as Image, ImageData, ImageFormat,
};
use llimphi_ui::llimphi_compositor::DragPhase;
use llimphi_ui::View;

use crate::tray::{TrayIcon, TrayItem};
use crate::nouser::WinMenu;
use crate::{Msg, WinAct};

use super::widgets::chip;

/// Largo máximo de la etiqueta de una ventana antes de recortar con `…`.
const WINDOW_LABEL_MAX: usize = 22;

/// Largo máximo de la etiqueta de un item del tray antes de recortar con `…`.
const TRAY_LABEL_MAX: usize = 14;

/// Largo máximo del preview del portapapeles antes de recortar con `…`.
const CLIPBOARD_PREVIEW_MAX: usize = 28;

/// Lado del ícono-badge (cuadrado) de una ventana en el task manager, en px.
const WIN_BADGE_PX: f32 = 18.0;
/// Ancho fijo de cada botón de tarea (taskbar): todos miden lo mismo.
const TASK_W: f32 = 170.0;

/// Tamaño del ícono del tray en la barra (px).
const TRAY_ICON_PX: f32 = 18.0;

/// Ancho del popup del historial de portapapeles (px).
const CLIP_MENU_W: f32 = 360.0;
/// Largo máximo de cada fila del historial antes de recortar.
const CLIP_ROW_MAX: usize = 48;

use crate::toplevel::WindowEntry;

/// El **task manager** (estilo KDE): un botón por ventana abierta.
pub(super) fn window_list_view(
    windows: &[WindowEntry],
    gap: f32,
    dir: FlexDirection,
    reorderable: bool,
    theme: &Theme,
) -> View<Msg> {
    let chips: Vec<View<Msg>> = windows
        .iter()
        .map(|w| window_button(w, reorderable, theme))
        .collect();

    View::new(Style {
        flex_direction: dir,
        align_items: Some(AlignItems::Center),
        // Alineados al inicio (a la izquierda en barra horizontal), estilo
        // taskbar — no centrados. El slot que lo hospeda ya le da el ancho.
        justify_content: Some(JustifyContent::FlexStart),
        flex_grow: 1.0,
        // Gap CHICO entre botones de tarea (no el `surface.gap` de entre widgets
        // grandes, que se veía exagerado). Capado a 4 px.
        gap: Size {
            width: length(gap.min(4.0)),
            height: length(gap.min(4.0)),
        },
        ..Default::default()
    })
    .children(chips)
}

/// Un botón de ventana del task manager: badge (inicial) + título recortado.
///
/// Interacción: clic izquierdo activa (o, si `reorderable`, arrastra para
/// reordenar y el click "sin movimiento" lo reinterpreta el backend como
/// activación); clic derecho y clic medio cierran la ventana.
/// Resuelve el `AppIcon` de marca desde el `app_id` de Wayland, tolerando los
/// sufijos de binario de la suite (`tullpu-app-llimphi`→tullpu,
/// `nakui-sheet-llimphi`→nakui, `nahual-shell-llimphi`→nahual…).
fn app_icon_de(app_id: &str) -> Option<llimphi_icons::app_icons::AppIcon> {
    use llimphi_icons::app_icons::AppIcon;
    if let Some(i) = AppIcon::from_app_id(app_id) {
        return Some(i);
    }
    for suf in ["-app-llimphi", "-sheet-llimphi", "-shell-llimphi", "-llimphi", "-app"] {
        if let Some(base) = app_id.strip_suffix(suf) {
            if let Some(i) = AppIcon::from_app_id(base) {
                return Some(i);
            }
        }
    }
    None
}

fn window_button(w: &WindowEntry, reorderable: bool, theme: &Theme) -> View<Msg> {
    let (fg, fill, badge_bg, badge_fg) = if w.active {
        (theme.fg_text, theme.bg_panel, theme.accent, theme.bg_panel)
    } else if w.minimized {
        (theme.fg_muted, theme.bg_panel_alt, theme.bg_panel, theme.fg_muted)
    } else {
        (theme.fg_text, theme.bg_panel_alt, theme.bg_panel, theme.fg_muted)
    };

    let badge_box = View::new(Style {
        size: Size {
            width: length(WIN_BADGE_PX),
            height: length(WIN_BADGE_PX),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .fill(badge_bg)
    .radius(4.0);
    // Ícono de marca de la app (vectorial, determinista): prefiere el nombre de
    // ícono que el cliente declaró vía `xdg_toplevel_icon` (traído por el puente
    // `mirada_toplevel_icon`), cayendo al `app_id`; si nada resuelve (apps de
    // terceros), cae a la inicial como antes.
    let icono = w
        .icon_name
        .as_deref()
        .and_then(app_icon_de)
        .or_else(|| app_icon_de(&w.app_id));
    let badge = match icono {
        Some(icon) => badge_box.children(vec![View::new(Style {
            size: Size {
                width: length(WIN_BADGE_PX - 6.0),
                height: length(WIN_BADGE_PX - 6.0),
            },
            ..Default::default()
        })
        .children(vec![llimphi_icons::app_icons::app_icon_view_colored(icon, badge_fg, 1.6)])]),
        None => badge_box.text(w.inicial(), 11.0, badge_fg),
    };

    let titulo = View::new(Style {
        // Crece para llenar el ancho fijo del botón; el texto va a la izquierda
        // (estilo botón de tarea: ícono + título alineado, no centrado).
        flex_grow: 1.0,
        size: Size {
            width: auto(),
            height: length(22.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexStart),
        ..Default::default()
    })
    .text(super::recortar(&w.label, WINDOW_LABEL_MAX), 12.0, fg);

    let boton = View::new(Style {
        flex_direction: FlexDirection::Row,
        // Ancho FIJO: todos los botones de tarea miden lo mismo (estilo taskbar),
        // no se encogen/crecen según el largo del título.
        flex_shrink: 0.0,
        size: Size {
            width: length(TASK_W),
            height: length(26.0_f32),
        },
        padding: TaffyRect {
            left: length(6.0_f32),
            right: length(8.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(6.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    })
    .fill(fill)
    .radius(4.0)
    .border(1.0, theme.border)
    .hover_fill(theme.bg_button_hover)
    .tooltip(w.label.clone())
    // Clic derecho y clic medio cierran la ventana (estándar de taskbar).
    .on_right_click(Msg::CloseWindow(w.id))
    .on_middle_click(Msg::CloseWindow(w.id))
    .children(vec![badge, titulo]);

    if reorderable {
        // Arrastrable para reordenar (backend layer-shell). `draggable` sustituye
        // al `on_click`: por eso el click izquierdo se resuelve vía `TaskDragEnd`
        // (el backend distingue click de arrastre real por la distancia movida).
        let id = w.id;
        boton.draggable(move |fase, dx, _dy| match fase {
            DragPhase::Move => Some(Msg::TaskDragMove(id, dx)),
            DragPhase::End => Some(Msg::TaskDragEnd(id)),
        })
    } else {
        // Backend winit (dev): click izquierdo activa directo, sin arrastre.
        boton.on_click(Msg::ActivateWindow(w.id))
    }
}

/// Una **cometa** del resaltado del switcher: durante el cambio de escritorio,
/// el color activo viaja de la celda vieja a la nueva. `head` es la posición de
/// la cabeza en espacio de índice de celda (0-based, ya interpolada); `dir` el
/// sentido del viaje (`+1` derecha, `-1` izquierda) para dejar una cola detrás.
#[derive(Clone, Copy)]
pub struct WsComet {
    pub head: f32,
    pub dir: f32,
}

/// Interpola dos colores componente a componente (`t` en `0..1`).
fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    use llimphi_ui::llimphi_raster::peniko::color::AlphaColor;
    let [ar, ag, ab, aa] = a.components;
    let [br, bg, bb, ba] = b.components;
    let t = t.clamp(0.0, 1.0);
    AlphaColor::new([
        ar + (br - ar) * t,
        ag + (bg - ag) * t,
        ab + (bb - ab) * t,
        aa + (ba - aa) * t,
    ])
}

/// Resplandor `0..1` de la celda `i` (0-based) bajo la cometa: agudo en la
/// cabeza, con **cola** larga por detrás (sentido contrario a `dir`) — el sello
/// del cometa. `0` si la celda queda fuera del alcance.
fn cell_glow(i: f32, c: &WsComet) -> f32 {
    let d = i - c.head; // distancia con signo en celdas
    let ahead = d * c.dir > 0.0; // por delante de la cabeza en el sentido del viaje
    let width = if ahead { 0.55 } else { 1.9 }; // frente nítido, cola larga
    (1.0 - d.abs() / width).clamp(0.0, 1.0)
}

/// Pertenencia de escritorios vista **desde un monitor**: lo que el switcher de
/// una barra necesita para pintar *de quién es* cada escritorio (su hogar), no
/// sólo cuál se ve dónde. Derivada de `mirada-ctl workspaces`
/// (`homes=` / `outputs=` / `focus=`). Default vacío = sin datos de pertenencia
/// (WM viejo / preview) → las celdas pintan como siempre.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WsMonitores {
    /// Conector del monitor donde vive esta barra (vacío = desconocido).
    pub panel: String,
    /// Hogar de cada escritorio con dueño: `(ws 1-based, conector)`.
    pub homes: Vec<(u8, String)>,
    /// Escritorio activo por monitor **conectado**: `(conector, ws 1-based)`.
    pub outputs: Vec<(String, u8)>,
    /// Conector del monitor con el foco del sistema.
    pub foco: String,
}

impl WsMonitores {
    /// El dueño del escritorio `n`, si tiene.
    fn home_de(&self, n: u8) -> Option<&str> {
        self.homes.iter().find(|(w, _)| *w == n).map(|(_, c)| c.as_str())
    }

    /// ¿El conector `name` está conectado ahora?
    fn conectado(&self, name: &str) -> bool {
        self.outputs.iter().any(|(o, _)| o == name)
    }

    /// El monitor que está mostrando el escritorio `n`, si alguno.
    fn visible_en(&self, n: u8) -> Option<&str> {
        self.outputs.iter().find(|(_, w)| *w == n).map(|(o, _)| o.as_str())
    }
}

/// El **workspace switcher**: una celda por escritorio virtual. Durante un
/// cambio, `comet` hace viajar el resaltado activo de la celda vieja a la nueva.
/// `mon` aporta la **pertenencia** por-monitor: los escritorios de otro monitor
/// salen atenuados y el tooltip dice de quién son y dónde se ven.
#[allow(clippy::too_many_arguments)]
pub fn workspaces_view(
    active: u8,
    count: u8,
    occupied: u16,
    others: u16,
    comet: Option<WsComet>,
    gap: f32,
    dir: FlexDirection,
    mon: &WsMonitores,
    theme: &Theme,
) -> View<Msg> {
    let celdas: Vec<View<Msg>> = (1..=count)
        .map(|n| {
            let bit = 1u16 << (n as u16 - 1);
            let ocupado = occupied & bit != 0;
            // En otro monitor (y no es el escritorio enfocado): se muestra en una
            // pantalla distinta; cliquearlo lleva el foco a ese monitor.
            let en_otro = (others & bit != 0) && n != active;
            // Con cometa en vuelo, el resaltado activo NO se pinta estático: lo
            // lleva el glow del cometa (que aterriza en la celda nueva al final).
            let glow = comet.map(|c| cell_glow((n - 1) as f32, &c)).unwrap_or(0.0);
            let activo = comet.is_none() && n == active;
            workspace_cell(n, activo, ocupado, en_otro, glow, mon, theme)
        })
        .collect();
    // Gap CHICO entre celdas del switcher (capado a 4 px): el `surface.gap` de
    // entre widgets grandes se veía exagerado para estos cuadraditos.
    let g = gap.clamp(2.0, 4.0);
    View::new(Style {
        flex_direction: dir,
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(g),
            height: length(g),
        },
        ..Default::default()
    })
    .children(celdas)
}

/// Una celda del workspace switcher: cuadradito con número y estado visual.
/// Cuatro estados bien diferenciados:
/// - **activo**: relleno con el acento (el escritorio que ves en este monitor).
/// - **en otro monitor**: borde grueso en `border_focus` (tono distinto del
///   acento) + relleno medio — está abierto en otra pantalla; cliquearlo lleva
///   el foco a ese monitor.
/// - **ocupado** (tiene ventanas): borde de acento grueso + número fuerte.
/// - **vacío**: apagado, borde tenue, número atenuado.
///
/// La **pertenencia** (`mon`) matiza: un escritorio cuyo hogar es otro monitor
/// conectado sale con número atenuado y borde `border_focus` fino («es de
/// allá»); uno cuyo hogar está desconectado anda *prestado* y el tooltip lo
/// dice. Sin datos de pertenencia, nada cambia.
fn workspace_cell(
    n: u8,
    active: bool,
    occupied: bool,
    on_other: bool,
    glow: f32,
    mon: &WsMonitores,
    theme: &Theme,
) -> View<Msg> {
    let home = mon.home_de(n);
    // Ajeno: pertenece a OTRO monitor conectado (se vea o no allá ahora).
    let ajeno = !mon.panel.is_empty()
        && home.is_some_and(|h| h != mon.panel && mon.conectado(h));
    // Prestado: su monitor está desconectado — vive de prestado por aquí.
    let prestado = home.is_some_and(|h| !mon.conectado(h)) && !mon.outputs.is_empty();
    // Rellenos bien separados — robusto en cualquier theme (no depende de que el
    // borde sea visible): activo = acento; en-otro/ocupado = tono medio
    // (hover/botón); vacío = el más apagado. El borde discrimina en-otro
    // (border_focus) de ocupado (accent).
    let (mut fill, mut fg, mut borde_w, mut borde_col) = if active {
        (theme.accent, theme.bg_panel, 1.0_f64, theme.accent)
    } else if on_other {
        (theme.bg_button_hover, theme.fg_text, 2.0_f64, theme.border_focus)
    } else if occupied {
        (theme.bg_button_hover, theme.fg_text, 2.0_f64, theme.accent)
    } else {
        (theme.bg_panel_alt, theme.fg_muted, 1.0_f64, theme.border)
    };
    // Ajeno y no activo aquí: número atenuado + borde fino en `border_focus` —
    // se ve que existe y de quién es, sin competir con los propios.
    if ajeno && !active && !on_other {
        fg = theme.fg_muted;
        borde_w = 1.0;
        borde_col = theme.border_focus;
    }
    // Cometa de transición: el resaltado activo viaja por las celdas. El glow
    // tiñe la celda hacia el acento (cabeza intensa, cola que se desvanece).
    if glow > 0.0 {
        let g = glow.clamp(0.0, 1.0);
        fill = lerp_color(fill, theme.accent, g);
        fg = lerp_color(fg, theme.bg_panel, g);
        borde_col = lerp_color(borde_col, theme.accent, g);
        borde_w = borde_w.max(1.0 + g as f64);
    }
    let base = if on_other {
        match mon.visible_en(n) {
            Some(o) => format!("Escritorio {n} · visible en {o} (clic = ir allá)"),
            None => format!("Escritorio {n} · en otro monitor (clic = ir allá)"),
        }
    } else if occupied {
        format!("Escritorio {n} · con ventanas")
    } else {
        format!("Escritorio {n} · vacío")
    };
    let tooltip = match home {
        Some(h) if prestado => format!("{base} · de {h} (desconectado, prestado)"),
        Some(h) if ajeno => format!("{base} · de {h}"),
        _ => base,
    };
    View::new(Style {
        // Más grande para acertar el click cómodo (antes 22×20 = chico).
        size: Size {
            width: length(30.0_f32),
            height: length(26.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .fill(fill)
    .radius(5.0)
    .border(borde_w, borde_col)
    .hover_fill(theme.bg_button_hover)
    .tooltip(tooltip)
    .on_click(Msg::SwitchWorkspace(n))
    .text(n.to_string(), 13.0, fg)
}

/// Qué pinta el widget `workspaces` montado en el RAIL de un sidebar: los
/// escritorios **visibles** (ocupados, el activo aunque esté vacío, y los que se
/// ven en otro monitor) y el destino del botón «+» — el primer escritorio libre,
/// rellenando huecos antes que estirar al final. `None` = no queda ninguno libre
/// y el «+» no se pinta. Pura, para certificarla por test.
pub(crate) fn ws_rail_model(active: u8, count: u8, occupied: u16, others: u16) -> (Vec<u8>, Option<u8>) {
    let mut celdas = Vec::new();
    let mut libre = None;
    for n in 1..=count.min(16) {
        let bit = 1u16 << (n as u16 - 1);
        if occupied & bit != 0 || others & bit != 0 || n == active {
            celdas.push(n);
        } else if libre.is_none() {
            libre = Some(n);
        }
    }
    (celdas, libre)
}

/// El widget `window_tabs` en el **rail** de un sidebar: las pestañas
/// verticales del navegador de ventanas — un chip cuadrado por ventana del
/// escritorio ACTIVO (1 tab = 1 ventana, DISENO-SHELL-NAVEGADOR §3), con el
/// ícono de marca de la app (o su inicial). La activa lleva borde de acento;
/// las minimizadas van atenuadas. Clic activa, clic derecho/medio cierra —
/// siempre por la CLI del WM ([`Msg::RailTabActivate`]/[`Msg::RailTabClose`]:
/// los ids son de mirada, no del backend). `active_ws == 0` (sin dato del WM)
/// no pinta nada.
pub fn window_tabs_rail_view(
    windows: &[WindowEntry],
    active_ws: u8,
    thickness: f32,
    theme: &Theme,
) -> View<Msg> {
    // Agrupar por `tab`: varias ventanas con el mismo tab son un **split** y se
    // pintan como UN chip (§3/§9 del diseño). Se preserva el orden de aparición.
    let del_ws: Vec<&WindowEntry> = windows
        .iter()
        .filter(|w| active_ws != 0 && w.workspace == active_ws)
        .collect();
    let mut tabs: Vec<(usize, Vec<&WindowEntry>)> = Vec::new();
    for w in del_ws {
        match tabs.iter_mut().find(|(t, _)| *t == w.tab) {
            Some((_, miembros)) => miembros.push(w),
            None => tabs.push((w.tab, vec![w])),
        }
    }
    let chips: Vec<View<Msg>> = tabs
        .into_iter()
        .map(|(_, miembros)| {
            if miembros.len() == 1 {
                rail_tab_chip(miembros[0], thickness, theme)
            } else {
                rail_split_chip(&miembros, thickness, theme)
            }
        })
        .collect();
    View::new(Style {
        flex_direction: FlexDirection::Column,
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(0.0_f32),
            right: length(0.0_f32),
            top: length(2.0_f32),
            bottom: length(2.0_f32),
        },
        gap: Size {
            width: length(4.0_f32),
            height: length(4.0_f32),
        },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(chips)
}

/// El **taskbar** de un diente-workspace (`TabsSource::Workspaces`): el panel que
/// se despliega al activar el diente del escritorio `ws_n` — una **pestaña por
/// ventana** de ESE escritorio, con la familiaridad de un navegador (ícono de
/// marca + título + botón «×» de cerrar) y de un taskbar (la activa resaltada, las
/// minimizadas atenuadas; clic izquierdo enfoca, clic medio cierra, clic derecho
/// abre el menú contextual con todas las acciones). Es la "sidebar propia" de cada
/// workspace (DISENO-SHELL-NAVEGADOR §4-§5). `si` es el sidebar del drawer, para
/// que el menú contextual sepa a qué surface anclarse.
pub fn workspace_taskbar_panel(
    windows: &[WindowEntry],
    si: usize,
    ws_n: u8,
    panel_h: f32,
    theme: &Theme,
) -> View<Msg> {
    // El título "Escritorio N" lo pone el cabezal uniforme del sidebar
    // (`panel_inner`), no este cuerpo — así no se duplica.
    let mut hijos: Vec<View<Msg>> = Vec::new();
    let mut filas: Vec<View<Msg>> = windows
        .iter()
        .filter(|w| ws_n != 0 && w.workspace == ws_n)
        .map(|w| taskbar_tab_row(w, si, ws_n, theme))
        .collect();
    if filas.is_empty() {
        filas.push(
            View::new(Style {
                size: Size { width: percent(1.0), height: length(22.0_f32) },
                align_items: Some(AlignItems::Center),
                ..Default::default()
            })
            .text("Sin ventanas".to_string(), 12.0, theme.fg_muted),
        );
    }
    hijos.extend(filas);
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0), height: length(panel_h) },
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(8.0_f32),
            bottom: length(8.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(6.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// Una **pestaña** del taskbar de un workspace: ícono de marca + título + «×».
/// Estilo navegador/taskbar — fila alta y clickeable en todo su ancho. Clic
/// izquierdo enfoca; el «×» y el clic medio cierran; el clic **derecho** abre el
/// menú contextual anclado al cursor ([`Msg::WinMenuOpen`], con coords de pantalla
/// que resuelve el backend). La activa lleva fondo + borde de acento; la
/// minimizada va atenuada e itálica-de-facto (fg_muted).
fn taskbar_tab_row(w: &WindowEntry, si: usize, ws_n: u8, theme: &Theme) -> View<Msg> {
    let (fg, fill, borde_w, borde_col) = if w.active {
        (theme.fg_text, theme.bg_panel, 1.5_f64, theme.accent)
    } else if w.minimized {
        (theme.fg_muted, theme.bg_panel_alt, 1.0_f64, theme.border)
    } else {
        (theme.fg_text, theme.bg_panel_alt, 1.0_f64, theme.border)
    };

    // Badge de marca (ícono de la app, o la inicial).
    let badge_box = View::new(Style {
        size: Size { width: length(WIN_BADGE_PX), height: length(WIN_BADGE_PX) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(4.0);
    let badge = match app_icon_de(&w.app_id) {
        Some(icon) => badge_box.children(vec![View::new(Style {
            size: Size { width: length(WIN_BADGE_PX - 6.0), height: length(WIN_BADGE_PX - 6.0) },
            ..Default::default()
        })
        .children(vec![llimphi_icons::app_icons::app_icon_view_colored(icon, fg, 1.6)])]),
        None => badge_box.text(w.inicial(), 11.0, theme.fg_muted),
    };

    let titulo = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(22.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexStart),
        ..Default::default()
    })
    .text(super::recortar(&w.label, WINDOW_LABEL_MAX), 12.5, fg);

    // Botón «×» de cerrar (estilo pestaña de browser): a la derecha, propio click.
    let cerrar = View::new(Style {
        size: Size { width: length(18.0_f32), height: length(18.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .radius(4.0)
    .hover_fill(theme.bg_button_hover)
    .tooltip("Cerrar".to_string())
    // Los ids del taskbar son de MIRADA (la lista sale de `mirada-ctl windows`), no
    // de los toplevels foreign: se activan/cierran por la CLI del WM
    // (`RailTabActivate`/`RailTabClose`), igual que las pestañas del rail y que el
    // menú contextual (`focus-window`/`close-window`). NO `ActivateWindow`/
    // `CloseWindow`, que resuelven por id de foreign-toplevel (otro espacio).
    .on_click(Msg::RailTabClose(w.id))
    .text("✕".to_string(), 11.0, theme.fg_muted);

    let id = w.id;
    let title = w.label.clone();
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0), height: length(30.0_f32) },
        padding: TaffyRect {
            left: length(7.0_f32),
            right: length(6.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(7.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .fill(fill)
    .radius(6.0)
    .border(borde_w, borde_col)
    .hover_fill(theme.bg_button_hover)
    .tooltip(w.label.clone())
    // Enfocar/cerrar por la CLI del WM (ids de mirada), no por foreign-toplevel.
    .on_click(Msg::RailTabActivate(w.id))
    .on_middle_click(Msg::RailTabClose(w.id))
    // Clic derecho → menú contextual anclado al cursor (coords de pantalla del
    // backend). El taskbar es de ESTE sidebar (`si`) y ESTE escritorio (`ws_n`).
    .on_right_click_screen(move |x, y, _w, _h| {
        Some(Msg::WinMenuOpen { si, ws: ws_n, id, title: title.clone(), x, y })
    })
    .children(vec![badge, titulo, cerrar])
}

/// Ancho del popup del menú contextual de una ventana (px).
pub(crate) const WIN_MENU_W: f32 = 232.0;

/// Una fila de acción del menú contextual: glifo + etiqueta, alto de toque
/// cómodo, hover resaltado, emite `on` al clic.
fn win_menu_row(glifo: &str, label: &str, on: Msg, theme: &Theme) -> View<Msg> {
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0), height: length(28.0_f32) },
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(9.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .radius(5.0)
    .hover_fill(theme.bg_button_hover)
    .on_click(on)
    .children(vec![
        View::new(Style {
            size: Size { width: length(16.0_f32), height: length(18.0_f32) },
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            flex_shrink: 0.0,
            ..Default::default()
        })
        .text(glifo.to_string(), 12.0, theme.fg_muted),
        View::new(Style {
            flex_grow: 1.0,
            size: Size { width: auto(), height: length(18.0_f32) },
            align_items: Some(AlignItems::Center),
            ..Default::default()
        })
        .text(label.to_string(), 12.5, theme.fg_text),
    ])
}

/// Un separador fino entre grupos de acciones del menú.
fn win_menu_sep(theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0), height: length(1.0_f32) },
        margin: TaffyRect {
            left: length(6.0_f32),
            right: length(6.0_f32),
            top: length(3.0_f32),
            bottom: length(3.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.border)
}

/// La **card** del menú contextual de una ventana: cabecera con el título + las
/// acciones de taskbar. `ws_count` alimenta el grupo «Mover a escritorio…» (un
/// destino por escritorio, saltando el actual). Todas las filas emiten
/// [`Msg::WinMenuDo`]. La posiciona el overlay del backend (anclada al cursor).
pub fn win_context_menu_card(menu: &WinMenu, ws_count: u8, theme: &Theme) -> View<Msg> {
    let id = menu.win_id;
    let mut filas: Vec<View<Msg>> = Vec::new();
    // Cabecera: título de la ventana (recortado), atenuado.
    filas.push(
        View::new(Style {
            size: Size { width: percent(1.0), height: length(24.0_f32) },
            padding: TaffyRect {
                left: length(10.0_f32),
                right: length(10.0_f32),
                top: length(0.0_f32),
                bottom: length(0.0_f32),
            },
            align_items: Some(AlignItems::Center),
            ..Default::default()
        })
        .text(super::recortar(&menu.title, 26), 11.5, theme.fg_muted),
    );
    filas.push(win_menu_sep(theme));
    filas.push(win_menu_row("⧉", "Traer al frente", Msg::WinMenuDo(id, WinAct::Focus), theme));
    filas.push(win_menu_row("▭", "Minimizar", Msg::WinMenuDo(id, WinAct::Minimize), theme));
    filas.push(win_menu_row("⤢", "Maximizar", Msg::WinMenuDo(id, WinAct::Maximize), theme));
    filas.push(win_menu_row("⛶", "Pantalla completa", Msg::WinMenuDo(id, WinAct::Fullscreen), theme));
    filas.push(win_menu_row("◈", "Flotante / teselada", Msg::WinMenuDo(id, WinAct::ToggleFloat), theme));
    filas.push(win_menu_row("⊙", "Visible en todos los escritorios", Msg::WinMenuDo(id, WinAct::Sticky), theme));
    filas.push(win_menu_row("▣", "Picture-in-picture", Msg::WinMenuDo(id, WinAct::Pip), theme));

    // Grupo «Mover a escritorio…»: un destino por escritorio (1..=count), saltando
    // el actual. Filas compactas con el número.
    let destinos: Vec<u8> = (1..=ws_count.min(9)).filter(|&n| n != menu.ws).collect();
    if !destinos.is_empty() {
        filas.push(win_menu_sep(theme));
        filas.push(
            View::new(Style {
                size: Size { width: percent(1.0), height: length(20.0_f32) },
                padding: TaffyRect {
                    left: length(10.0_f32),
                    right: length(10.0_f32),
                    top: length(0.0_f32),
                    bottom: length(0.0_f32),
                },
                align_items: Some(AlignItems::Center),
                ..Default::default()
            })
            .text("Mover a escritorio".to_string(), 10.5, theme.fg_placeholder),
        );
        // Los números en una rejilla horizontal que envuelve.
        let celdas: Vec<View<Msg>> = destinos
            .iter()
            .map(|&n| {
                View::new(Style {
                    size: Size { width: length(28.0_f32), height: length(26.0_f32) },
                    align_items: Some(AlignItems::Center),
                    justify_content: Some(JustifyContent::Center),
                    ..Default::default()
                })
                .fill(theme.bg_panel)
                .radius(5.0)
                .border(1.0, theme.border)
                .hover_fill(theme.bg_button_hover)
                .tooltip(format!("Mover al escritorio {n}"))
                .on_click(Msg::WinMenuDo(id, WinAct::MoveTo(n)))
                .text(n.to_string(), 12.0, theme.fg_text)
            })
            .collect();
        filas.push(
            View::new(Style {
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                size: Size { width: percent(1.0), height: auto() },
                padding: TaffyRect {
                    left: length(8.0_f32),
                    right: length(8.0_f32),
                    top: length(2.0_f32),
                    bottom: length(2.0_f32),
                },
                gap: Size { width: length(5.0_f32), height: length(5.0_f32) },
                ..Default::default()
            })
            .children(celdas),
        );
    }

    filas.push(win_menu_sep(theme));
    filas.push(win_menu_row("✕", "Cerrar", Msg::WinMenuDo(id, WinAct::Close), theme));
    filas.push(win_menu_row("✕", "Cerrar las demás", Msg::WinMenuDo(id, WinAct::CloseOthers), theme));

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(WIN_MENU_W), height: auto() },
        padding: TaffyRect {
            left: length(4.0_f32),
            right: length(4.0_f32),
            top: length(5.0_f32),
            bottom: length(5.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(1.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(9.0)
    .border(1.0, theme.border)
    .children(filas)
}

/// Un chip de pestaña vertical del rail: cuadrado de lado `thickness - 12`,
/// ícono de marca centrado (cae a la inicial si el `app_id` no resuelve).
fn rail_tab_chip(w: &WindowEntry, thickness: f32, theme: &Theme) -> View<Msg> {
    let lado = (thickness - 12.0).max(24.0);
    let (fill, fg, borde_w, borde_col) = if w.active {
        (theme.bg_button_hover, theme.fg_text, 2.0_f64, theme.accent)
    } else if w.minimized {
        (theme.bg_panel, theme.fg_muted, 1.0_f64, theme.border)
    } else {
        (theme.bg_panel_alt, theme.fg_text, 1.0_f64, theme.border)
    };
    let caja = View::new(Style {
        size: Size {
            width: length(lado),
            height: length(lado),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(fill)
    .radius(6.0)
    .border(borde_w, borde_col)
    .hover_fill(theme.bg_button_hover)
    .tooltip(w.label.clone())
    .on_click(Msg::RailTabActivate(w.id))
    .on_right_click(Msg::RailTabClose(w.id))
    .on_middle_click(Msg::RailTabClose(w.id));
    match app_icon_de(&w.app_id) {
        Some(icon) => caja.children(vec![View::new(Style {
            size: Size {
                width: length(lado - 10.0),
                height: length(lado - 10.0),
            },
            ..Default::default()
        })
        .children(vec![llimphi_icons::app_icons::app_icon_view_colored(icon, fg, 1.6)])]),
        None => caja.text(w.inicial(), 13.0, fg),
    }
}

/// Un chip de **split tab** del rail: un tab que contiene 2+ ventanas teseladas
/// (§9: «split como par de mini-iconos»). Se pinta como el chip normal pero con
/// los íconos/iniciales de sus miembros en una mini-rejilla (hasta 4; el resto
/// se resume como «+N»), y borde de acento si alguno está activo — el tab activo.
/// Clic activa el tab (su primer miembro → el Cerebro lo trae al frente); clic
/// derecho cierra ese primer miembro.
fn rail_split_chip(miembros: &[&WindowEntry], thickness: f32, theme: &Theme) -> View<Msg> {
    let lado = (thickness - 12.0).max(24.0);
    let activo = miembros.iter().any(|m| m.active);
    let (fill, fg, borde_w, borde_col) = if activo {
        (theme.bg_button_hover, theme.fg_text, 2.0_f64, theme.accent)
    } else {
        (theme.bg_panel_alt, theme.fg_text, 1.0_f64, theme.border)
    };
    // Objetivo del clic: la ventana activa del split, o la primera.
    let objetivo = miembros.iter().find(|m| m.active).unwrap_or(&miembros[0]);
    let tip = format!("Split · {} ventanas: {}", miembros.len(),
        miembros.iter().map(|m| m.label.as_str()).collect::<Vec<_>>().join(" · "));

    // Mini-celdas: hasta 4 miembros en rejilla 2×N; el resto resumido en la última.
    let vistos = miembros.len().min(4);
    let mut celdas: Vec<View<Msg>> = Vec::with_capacity(vistos);
    for (i, m) in miembros.iter().take(vistos).enumerate() {
        let ultima_con_resto = i == vistos - 1 && miembros.len() > vistos;
        let etiqueta = if ultima_con_resto {
            format!("+{}", miembros.len() - (vistos - 1))
        } else {
            m.inicial()
        };
        celdas.push(
            View::new(Style {
                size: Size { width: length((lado - 8.0) / 2.0), height: length((lado - 8.0) / 2.0) },
                align_items: Some(AlignItems::Center),
                justify_content: Some(JustifyContent::Center),
                flex_shrink: 0.0,
                ..Default::default()
            })
            .fill(theme.bg_panel)
            .radius(3.0)
            .text(etiqueta, 9.0, fg),
        );
    }
    let rejilla = View::new(Style {
        flex_direction: FlexDirection::Row,
        flex_wrap: FlexWrap::Wrap,
        align_content: Some(AlignContent::Center),
        justify_content: Some(JustifyContent::Center),
        align_items: Some(AlignItems::Center),
        size: Size { width: length(lado - 6.0), height: length(lado - 6.0) },
        gap: Size { width: length(2.0_f32), height: length(2.0_f32) },
        ..Default::default()
    })
    .children(celdas);

    View::new(Style {
        size: Size { width: length(lado), height: length(lado) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(fill)
    .radius(6.0)
    .border(borde_w, borde_col)
    .hover_fill(theme.bg_button_hover)
    .tooltip(tip)
    .on_click(Msg::RailTabActivate(objetivo.id))
    .on_right_click(Msg::RailTabClose(objetivo.id))
    .on_middle_click(Msg::RailTabClose(objetivo.id))
    .children(vec![rejilla])
}

/// El **botón de inicio**: chip con su label/ícono. Clic → menú de apps.
/// Si el label es `"chakana"`, pinta la **marca** (cruz andina escalonada) viva
/// (respira con `anim_t`) — el PS1/botón de inicio del navegador de ventanas.
/// Forma del PS1 transformable (la chakana). Cambia según el MODO del input:
/// el botón de inicio muta para señalar qué está pasando, con animación.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChakanaForma {
    /// La cruz andina escalonada — reposo / estado normal.
    #[default]
    Chakana,
    /// Modo CONSOLA (el input le habla a un PTY vivo): un chevron ❯ que late.
    Consola,
}

pub fn start_button_view(
    label: &str,
    exec: Option<&str>,
    anim_t: f32,
    chakana_color: Color,
    chakana_titila: bool,
    forma: ChakanaForma,
    theme: &Theme,
) -> View<Msg> {
    let click = match exec {
        Some(cmd) => Msg::Spawn(cmd.to_string()),
        None => Msg::StartToggle,
    };
    let es_chakana = label == "chakana";
    let width = if es_chakana { length(38.0_f32) } else { auto() };
    let pad = if es_chakana { 3.0_f32 } else { 16.0_f32 };
    let base = View::new(Style {
        size: Size { width, height: length(32.0_f32) },
        padding: TaffyRect {
            left: length(pad),
            right: length(pad),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(6.0)
    .border(1.0, theme.border)
    .hover_fill(theme.bg_button_hover)
    .tooltip(if exec.is_some() {
        "Lanzar"
    } else {
        "Menú de inicio (clic-der: cambiar estilo)"
    })
    .on_click(click)
    .on_right_click(Msg::StartStyleCycle);
    if es_chakana {
        chakana_paint(base, anim_t, chakana_color, chakana_titila, forma, theme.bg_panel)
    } else {
        base.text(label.to_string(), 14.0, chakana_color)
    }
}

/// **Reloj grande** de la barra de mando: HH:MM grande + fecha chica, alineado a
/// la derecha (como el `clock_big` del boceto). Lee la hora local directo.
pub fn clock_big_view(theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_layout::taffy::prelude::{auto, length, percent, FlexDirection, JustifyContent, Size, Style};
    use llimphi_ui::llimphi_text::Alignment;
    let ahora = chrono::Local::now();
    let hora = ahora.format("%H:%M").to_string();
    let fecha = ahora.format("%a %d %b").to_string();
    let h = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(24.0_f32) },
        ..Default::default()
    })
    .text_aligned(hora, 22.0, theme.fg_text, Alignment::End)
    .text_weight(600.0);
    let f = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(13.0_f32) },
        ..Default::default()
    })
    .text_aligned(fecha, 10.0, theme.fg_muted, Alignment::End);
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(86.0_f32), height: auto() },
        flex_shrink: 0.0,
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .children(vec![h, f])
}

/// Estado visual de la **chakana** según la config + las señales (el PS1
/// topológico). Devuelve `(color, titila)`:
/// - **reposo** (sin comando ni notificación): el acento **titilando**;
/// - **resultado del último comando**: verde ok / rojo error;
/// - **notificación importante**: color semántico (urgente rojo que titila,
///   leve ámbar).
///
/// La prioridad da a las notificaciones sobre el resultado (el aviso ambiental
/// gana). Con `reactiva = false` queda siempre en el acento clásico.
pub fn chakana_vista(
    reactiva: bool,
    titila_idle: bool,
    ultimo_ok: Option<bool>,
    notif: Option<shuma_module_shell::Urgencia>,
    theme: &Theme,
) -> (Color, bool) {
    let verde = Color::from_rgb8(0x5A, 0xD0, 0x8A);
    let rojo = Color::from_rgb8(0xE0, 0x5A, 0x5A);
    let ambar = Color::from_rgb8(0xE0, 0xB2, 0x4A);
    if !reactiva {
        return (theme.accent, titila_idle);
    }
    if let Some(u) = notif {
        return match u {
            shuma_module_shell::Urgencia::Urgente => (rojo, true),
            shuma_module_shell::Urgencia::Leve => (ambar, false),
            // Calma (idle humano) no es un aviso: la chakana ni se entera.
            shuma_module_shell::Urgencia::Calma | shuma_module_shell::Urgencia::Silencio => {
                (theme.accent, titila_idle)
            }
        };
    }
    match ultimo_ok {
        Some(false) => (rojo, false),
        Some(true) => (verde, false),
        None => (theme.accent, titila_idle),
    }
}

/// Pinta la **chakana** (cruz andina escalonada con hueco central) dentro del
/// botón, respirando con `anim_t`. Cuadrícula 12×12: cada brazo es una escalera
/// de 3 pasos que se ensancha hacia el centro (ancho 3 → 5 → hub 7), con hueco
/// 3×3 — los escalones dan los "dientes" que la distinguen de una cruz plana.
fn chakana_paint(view: View<Msg>, anim_t: f32, accent: Color, titila: bool, forma: ChakanaForma, hole: Color) -> View<Msg> {
    view.paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, BezPath, Circle, RoundedRect, Stroke};
        use llimphi_ui::llimphi_raster::peniko::Fill;
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        // MODO CONSOLA: la chakana se transforma en un chevron ❯ que LATE
        // (el input le habla a un programa). Forma simple, animada, señala el
        // modo — la base de una futura familia de morfos (estilo Clippy).
        if let ChakanaForma::Consola = forma {
            let cx = (rect.x + rect.w * 0.5) as f64;
            let cy = (rect.y + rect.h * 0.5) as f64;
            let s = (rect.w.min(rect.h) as f64) * 0.30;
            // Latido: el chevron avanza-retrocede un pelín + alpha respira.
            let beat = 0.5 + 0.5 * (anim_t as f64 * 2.2).sin();
            let push = s * 0.16 * beat;
            let a = (0.75 + 0.25 * beat) as f32;
            let col = accent.with_alpha(a);
            // Halo pulsante detrás.
            let halo = ((anim_t as f64 * 0.7).fract()) as f32;
            scene.stroke(
                &Stroke::new(1.2),
                Affine::IDENTITY,
                accent.with_alpha((1.0 - halo) * 0.35),
                None,
                &Circle::new((cx, cy), s * 1.3 + halo as f64 * s * 0.5),
            );
            // El chevron «❯»: dos segmentos desde la izquierda al vértice.
            let mut p = BezPath::new();
            p.move_to((cx - s * 0.5 + push, cy - s));
            p.line_to((cx + s * 0.5 + push, cy));
            p.line_to((cx - s * 0.5 + push, cy + s));
            scene.stroke(&Stroke::new((s * 0.42) as f32 as f64).with_caps(
                llimphi_ui::llimphi_raster::kurbo::Cap::Round,
            ).with_join(llimphi_ui::llimphi_raster::kurbo::Join::Round), Affine::IDENTITY, col, None, &p);
            return;
        }
        let lado = (rect.w.min(rect.h) as f64) * 0.94;
        let c = lado / 12.0;
        let ox = (rect.x + rect.w * 0.5) as f64 - 6.0 * c;
        let oy = (rect.y + rect.h * 0.5) as f64 - 6.0 * c;
        let cell = |col: f64, row: f64, wc: f64, hc: f64| {
            RoundedRect::new(ox + col * c, oy + row * c, ox + (col + wc) * c, oy + (row + hc) * c, c * 0.08)
        };
        // Reposo → **titila**: respiración de alpha + halo que emana (vida). Con
        // un estado (resultado/notif) queda estable en su color, sin respirar.
        let breath = if titila { 0.5 + 0.5 * (anim_t as f64 * 1.7).sin() } else { 1.0 };
        let a = (0.80 + 0.20 * breath) as f32;
        let acc = accent.with_alpha(a);
        let cx = (rect.x + rect.w * 0.5) as f64;
        let cy = (rect.y + rect.h * 0.5) as f64;
        if titila {
            let halo = ((anim_t as f64 * 0.6).fract()) as f32;
            scene.stroke(
                &Stroke::new(1.1),
                Affine::IDENTITY,
                accent.with_alpha((1.0 - halo) * 0.30),
                None,
                &Circle::new((cx, cy), lado * 0.52 + halo as f64 * lado * 0.16),
            );
        }
        // Escalera monótona: hub 7×7 + brazo medio (5 de ancho) + brazo tip (3),
        // vertical y horizontal → perfil 3·5·7·5·3 en cada eje (3 escalones/lado).
        for r in [
            cell(3.0, 3.0, 6.0, 6.0), // hub 6×6 (cols/filas 3..9)
            cell(4.0, 1.0, 4.0, 10.0), // brazo vertical medio (ancho 4, filas 1..11)
            cell(5.0, 0.0, 2.0, 12.0), // brazo vertical tip (ancho 2, full)
            cell(1.0, 4.0, 10.0, 4.0), // brazo horizontal medio
            cell(0.0, 5.0, 12.0, 2.0), // brazo horizontal tip
        ] {
            scene.fill(Fill::NonZero, Affine::IDENTITY, acc, None, &r);
        }
        // Hueco central (perfora al color del botón).
        scene.fill(Fill::NonZero, Affine::IDENTITY, hole, None, &cell(5.0, 5.0, 2.0, 2.0));
    })
}

/// La **marquesina**: un ticker que desplaza notificaciones/avisos por la barra
/// (la barra de mando del navegador de ventanas, §5.1 del diseño). Toma las
/// notificaciones recientes de `notif`; si no hay, muestra un aviso ambiental. El
/// texto se desliza a la izquierda con `anim_t` y se repite para un loop continuo.
/// Aviso silencioso del sistema para la marquesina: CPU alta, batería baja o
/// temperatura — `None` si todo normal. `cpu`/`bat` son fracciones `0..1`.
/// Umbral de «CPU recaliente» (°C) para el aviso de la marquesina. Los CPU de
/// laptop (p.ej. Iris Xe/Tiger Lake) corren a 85-90°C bajo carga NORMAL (build,
/// video) sin problema; el throttle es ~100°C. A 82°C la alarma roja saltaba con
/// cualquier carga y parecía falsa. 90°C = «se está poniendo caliente de verdad».
pub const CPU_TEMP_ALERTA_C: f32 = 90.0;

pub fn sys_alert(cpu: f32, bat: Option<(f32, bool)>, temp: Option<f32>) -> Option<String> {
    if cpu >= 0.92 {
        return Some(format!("CPU al {}%", (cpu * 100.0).round() as u32));
    }
    if let Some((f, charging)) = bat {
        if !charging && f <= 0.15 {
            return Some(format!("batería {}%", (f * 100.0).round() as u32));
        }
    }
    if let Some(t) = temp {
        if t >= CPU_TEMP_ALERTA_C {
            return Some(format!("CPU {t:.0}°C"));
        }
    }
    None
}

pub fn marquesina_view(
    notif: Option<&crate::notifications::NotifState>,
    alert: Option<&str>,
    anim_t: f32,
    theme: &Theme,
) -> View<Msg> {
    let mut partes: Vec<String> = Vec::new();
    if let Some(a) = alert {
        partes.push(format!("⚠ {a}"));
    }
    if let Some(n) = notif {
        for it in &n.recent {
            partes.push(if it.app.is_empty() {
                it.summary.clone()
            } else {
                format!("{}: {}", it.app, it.summary)
            });
        }
    }
    let text = if partes.is_empty() {
        "sin novedades — pregunta, lanza o dicta".to_string()
    } else {
        partes.join("     ·     ")
    };
    let fg = theme.fg_muted;
    // Punto en color de aviso si hay alerta, si no acento.
    let accent = if alert.is_some() {
        llimphi_ui::llimphi_raster::peniko::Color::from_rgb8(0xE0, 0xB2, 0x4A)
    } else {
        theme.accent
    };
    let dot = View::new(Style {
        size: Size { width: length(7.0_f32), height: length(7.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(accent)
    .radius(4.0);
    let ticker = View::new(Style {
        flex_grow: 1.0,
        flex_basis: length(0.0_f32),
        size: Size { width: auto(), height: percent(1.0_f32) },
        ..Default::default()
    })
    .clip(true)
    .paint_with(move |scene, ts, rect| {
        use llimphi_ui::llimphi_text::{draw_layout, measurement, Alignment};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let layout = ts.layout(&text, 12.0, None, Alignment::Start, 1.2, false, None, 420.0, false, false, 0.0, 0.0);
        let m = measurement(&layout);
        let (tw, th) = (m.width as f64, m.height as f64);
        let gap = 70.0;
        let period = (tw + gap).max(1.0);
        // Desplaza a la izquierda; si el texto entra entero, no hace falta loop.
        let off = if tw > rect.w as f64 { (anim_t as f64 * 42.0).rem_euclid(period) } else { 0.0 };
        let y = rect.y as f64 + (rect.h as f64 - th) * 0.5;
        let x0 = rect.x as f64 - off;
        draw_layout(scene, &layout, fg, (x0, y));
        if off > 0.0 {
            draw_layout(scene, &layout, fg, (x0 + period, y));
        }
    });
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: length(240.0_f32), height: length(28.0_f32) },
        flex_shrink: 0.0,
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(8.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(7.0)
    .border(1.0, theme.border)
    .children(vec![dot, ticker])
}

/// El `tray`: un chip clickeable por item de la bandeja del sistema.
pub fn tray_view(items: &[TrayItem], gap: f32, dir: FlexDirection, theme: &Theme) -> View<Msg> {
    let chips: Vec<View<Msg>> = items
        .iter()
        .map(|it| {
            let tip = if it.label.trim().is_empty() {
                it.key.clone()
            } else {
                it.label.clone()
            };
            let base = chip(theme)
                .fill(theme.bg_panel_alt)
                .radius(6.0)
                .hover_fill(theme.bg_button_hover)
                .tooltip(tip)
                .on_click(Msg::TrayActivate(it.key.clone()));
            match &it.icon {
                Some(icon) => base.children(vec![tray_icon_node(icon)]),
                None => {
                    let fg = if it.status == "NeedsAttention" {
                        theme.accent
                    } else {
                        theme.fg_text
                    };
                    base.text(super::recortar(&it.label, TRAY_LABEL_MAX), 12.0, fg)
                }
            }
        })
        .collect();

    View::new(Style {
        flex_direction: dir,
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(gap),
            height: length(gap),
        },
        ..Default::default()
    })
    .children(chips)
}

/// Un nodo cuadrado con el ícono del item del tray.
fn tray_icon_node(icon: &TrayIcon) -> View<Msg> {
    let blob = Blob::from(icon.rgba.clone());
    let img = Image::new(ImageData { data: blob, format: ImageFormat::Rgba8, alpha_type: ImageAlphaType::Alpha, width: icon.width, height: icon.height });
    View::new(Style {
        size: Size {
            width: length(TRAY_ICON_PX),
            height: length(TRAY_ICON_PX),
        },
        ..Default::default()
    })
    .image(img)
}

/// El widget `clipboard`: ícono (vector) + preview del texto copiado.
pub(super) fn clipboard_view(text: Option<&str>, exec: Option<&str>, theme: &Theme) -> View<Msg> {
    let (preview, fg) = match text {
        Some(t) if !t.is_empty() => (Some(super::recortar(t, CLIPBOARD_PREVIEW_MAX)), theme.fg_text),
        _ => (None, theme.fg_muted),
    };
    let mut kids: Vec<View<Msg>> = vec![super::widgets::clipboard_icon(fg)];
    if let Some(p) = &preview {
        kids.push(
            View::new(Style {
                size: Size { width: auto(), height: length(22.0_f32) },
                align_items: Some(AlignItems::Center),
                ..Default::default()
            })
            .text(p.clone(), 12.0, fg),
        );
    }
    let v = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: auto(), height: length(22.0_f32) },
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(8.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        gap: Size { width: length(5.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .children(kids);
    let v = match text {
        Some(t) if !t.is_empty() => v.tooltip(t.to_string()),
        _ => v,
    };
    let v = v.on_click(Msg::ClipboardMenu);
    match exec {
        Some(cmd) => v.on_right_click(Msg::Spawn(cmd.to_string())),
        None => v,
    }
}

/// El **panel** del historial de portapapeles: cabecera + una fila por copia.
/// `anchor_x` = x (px) del widget que lo abrió, para que salga **justo debajo**;
/// `avail_w` = ancho de la barra, para no desbordar el borde.
/// Una fila del historial de portapapeles para el popup: el texto + su id
/// persistente (para fijar/borrar) y si está fijada. `id: None` = sin store
/// persistente (sólo el ring en memoria): se pinta sin pin/borrar.
pub struct ClipRow {
    pub id: Option<u64>,
    pub texto: String,
    pub fijada: bool,
}

/// Construye las filas del popup desde el store persistente (id + fijada) o, si
/// no hay store, desde el ring en memoria (sin id → sin pin/borrar). Sólo texto
/// (las imágenes no se listan aún en el popup).
pub fn clip_rows(
    store: &Option<pata_portapapeles::Historial>,
    history: &[String],
) -> Vec<ClipRow> {
    if let Some(h) = store {
        if let Ok(entradas) = h.listar() {
            return entradas
                .into_iter()
                .filter_map(|e| match e.contenido {
                    pata_portapapeles::Contenido::Texto(t) => Some(ClipRow {
                        id: Some(e.id),
                        texto: t,
                        fijada: e.fijada,
                    }),
                    pata_portapapeles::Contenido::Imagen { .. } => None,
                })
                .collect();
        }
    }
    history
        .iter()
        .map(|t| ClipRow {
            id: None,
            texto: t.clone(),
            fijada: false,
        })
        .collect()
}

/// Un botón chico cuadrado (pin/borrar) con su glyph y Msg.
fn clip_boton(glyph: &str, activo: bool, tip: &str, msg: Msg, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: length(20.0_f32),
            height: length(20.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .radius(5.0)
    .hover_fill(theme.bg_button_hover)
    .tooltip(tip.to_string())
    .on_click(msg)
    .text(glyph, 12.0, if activo { theme.accent } else { theme.fg_muted })
}

pub fn clipboard_panel(rows: &[ClipRow], anchor_x: f32, avail_w: f32, theme: &Theme) -> View<Msg> {
    // El panel se centra bajo el widget, acotado a la pantalla.
    let left = (anchor_x - CLIP_MENU_W * 0.5).clamp(8.0, (avail_w - CLIP_MENU_W - 8.0).max(8.0));
    let mut hijos: Vec<View<Msg>> = Vec::new();
    hijos.push(
        View::new(Style {
            size: Size {
                width: percent(1.0_f32),
                height: length(22.0_f32),
            },
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::FlexStart),
            ..Default::default()
        })
        .text("Portapapeles", 12.0, theme.fg_muted),
    );
    if rows.is_empty() {
        hijos.push(
            View::new(Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: length(26.0_f32),
                },
                align_items: Some(AlignItems::Center),
                ..Default::default()
            })
            .text("(sin copias todavía)", 12.0, theme.fg_muted),
        );
    } else {
        for row in rows {
            let entry = &row.texto;
            let mut fila_hijos: Vec<View<Msg>> = Vec::new();

            // Pin (★/☆) a la izquierda, si hay store persistente.
            if let Some(id) = row.id {
                fila_hijos.push(clip_boton(
                    if row.fijada { "★" } else { "☆" },
                    row.fijada,
                    if row.fijada { "Quitar fijado" } else { "Fijar" },
                    Msg::ClipboardPin(id),
                    theme,
                ));
            }

            // Texto (crece, empuja el resto a la derecha).
            fila_hijos.push(
                View::new(Style {
                    flex_grow: 1.0_f32,
                    size: Size {
                        width: auto(),
                        height: percent(1.0_f32),
                    },
                    align_items: Some(AlignItems::Center),
                    justify_content: Some(JustifyContent::FlexStart),
                    ..Default::default()
                })
                .text(super::recortar(entry, CLIP_ROW_MAX), 12.0, theme.fg_text),
            );

            // Acción (abrir URL/email/ruta) si aplica.
            if let Some(a) = pata_portapapeles::accion_para(entry) {
                fila_hijos.push(
                    View::new(Style {
                        padding: TaffyRect {
                            left: length(8.0_f32),
                            right: length(8.0_f32),
                            top: length(2.0_f32),
                            bottom: length(2.0_f32),
                        },
                        align_items: Some(AlignItems::Center),
                        ..Default::default()
                    })
                    .radius(6.0)
                    .fill(theme.bg_button_hover)
                    .hover_fill(theme.bg_panel)
                    .tooltip(a.objetivo.clone())
                    .on_click(Msg::ClipboardAction(a.objetivo.clone()))
                    .text(format!("↗ {}", a.etiqueta), 11.0, theme.fg_text),
                );
            }

            // Borrar (✕) a la derecha, si hay store persistente.
            if let Some(id) = row.id {
                fila_hijos.push(clip_boton(
                    "✕",
                    false,
                    "Quitar del historial",
                    Msg::ClipboardDelete(id),
                    theme,
                ));
            }

            let fila = View::new(Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: length(26.0_f32),
                },
                padding: TaffyRect {
                    left: length(8.0_f32),
                    right: length(8.0_f32),
                    top: length(0.0_f32),
                    bottom: length(0.0_f32),
                },
                flex_direction: FlexDirection::Row,
                align_items: Some(AlignItems::Center),
                justify_content: Some(JustifyContent::FlexStart),
                gap: Size {
                    width: length(6.0_f32),
                    height: length(0.0_f32),
                },
                ..Default::default()
            })
            .radius(6.0)
            .hover_fill(theme.bg_button_hover)
            .tooltip(entry.clone())
            .on_click(Msg::ClipboardPick(entry.clone()))
            .children(fila_hijos);
            hijos.push(fila);
        }
    }

    View::new(Style {
        position: llimphi_ui::llimphi_layout::taffy::prelude::Position::Absolute,
        inset: TaffyRect {
            left: length(left),
            top: length(0.0_f32),
            right: auto(),
            bottom: auto(),
        },
        size: Size {
            width: length(CLIP_MENU_W),
            height: auto(),
        },
        flex_direction: FlexDirection::Column,
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(8.0_f32),
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
    .radius(10.0)
    .children(vec![View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: auto(),
        },
        gap: Size {
            width: length(0.0_f32),
            height: length(2.0_f32),
        },
        ..Default::default()
    })
    .children(hijos)])
}

/// El historial de portapapeles como **overlay** para winit.
pub fn clipboard_overlay(rows: &[ClipRow], bar_h: f32, anchor_x: f32, avail_w: f32, theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_layout::taffy::prelude::Position;
    let scrim = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            top: length(0.0_f32),
            right: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        ..Default::default()
    })
    .on_click(Msg::ClipboardMenu)
    .children(vec![clipboard_panel(rows, anchor_x, avail_w, theme)]);
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
    .children(vec![scrim])
}

#[cfg(test)]
mod tests {
    use super::ws_rail_model;

    #[test]
    fn ws_rail_solo_ocupados_y_mas_al_primer_libre() {
        // Ocupados 1 y 3 (huecos: 2, 4..9), activo el 1: se ven [1, 3] y el «+»
        // apunta al hueco 2 — rellena huecos antes que estirar al final.
        let (celdas, libre) = ws_rail_model(1, 9, 0b0000_0101, 0);
        assert_eq!(celdas, vec![1, 3]);
        assert_eq!(libre, Some(2));
    }

    #[test]
    fn ws_rail_activo_vacio_se_ve_igual() {
        // Recién saltaste al 2 (vacío, p. ej. por el «+»): se ve aunque no tenga
        // ventanas — estás parado ahí.
        let (celdas, libre) = ws_rail_model(2, 9, 0b0000_0001, 0);
        assert_eq!(celdas, vec![1, 2]);
        assert_eq!(libre, Some(3));
    }

    #[test]
    fn ws_rail_sin_libres_no_hay_mas() {
        // Los 3 escritorios ocupados: el «+» desaparece.
        let (celdas, libre) = ws_rail_model(1, 3, 0b0111, 0);
        assert_eq!(celdas, vec![1, 2, 3]);
        assert_eq!(libre, None);
    }

    #[test]
    fn ws_rail_otro_monitor_cuenta_como_visible() {
        // El 5 se ve en otro monitor: aparece en el rail aunque loads lo dé vacío.
        let (celdas, _) = ws_rail_model(1, 9, 0b0000_0001, 0b0001_0000);
        assert_eq!(celdas, vec![1, 5]);
    }

    #[test]
    fn ws_rail_sin_wm_no_pinta_nada() {
        // count == 0 (mirada-ctl no responde): ni celdas ni «+».
        assert_eq!(ws_rail_model(0, 0, 0, 0), (vec![], None));
    }
}
