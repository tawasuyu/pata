//! Widget `network`: el applet de Wi-Fi/Ethernet. Un **dibujo del nivel de
//! señal** en la barra (barras ascendentes estilo celular) que al clickearse
//! despliega un popup con la lista de redes para conectarse.
//!
//! El icono se pinta a mano (kurbo/vello), como el clima; el popup reusa el
//! patrón de filas clickeables del historial de portapapeles.

use llimphi_theme::{elevation, radius, Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::llimphi_raster::vello::Scene;
use llimphi_ui::{PaintRect, Shadow, View};
use llimphi_widget_switch::{switch_view, SwitchPalette};

use crate::network::{NetState, NetStatus, WifiAp};
use crate::Msg;

/// Ancho del icono de señal (px).
const ICON_W: f32 = 22.0;
/// Ancho del popup (px).
pub(super) const PANEL_W: f32 = 280.0;
/// Alto de una fila de red.
const ROW_H: f32 = 30.0;

// ============================================================
// Icono de la barra
// ============================================================

/// El widget `network`: el icono de señal + (si hay) el SSID. Click → popup.
pub fn network_view(state: Option<&NetState>, theme: &Theme) -> View<Msg> {
    let status = state.map(|s| s.status.clone()).unwrap_or(NetStatus::Sin);
    let (tooltip, etiqueta) = descripcion(&status);
    let st = status.clone();
    let acento = theme.accent;
    let tenue = theme.fg_muted;
    let icono = View::new(Style {
        size: Size {
            width: length(ICON_W),
            height: length(22.0_f32),
        },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| dibujar_red(scene, rect, &st, acento, tenue));

    let mut hijos = vec![icono];
    if let Some(txt) = etiqueta {
        hijos.push(
            View::new(Style {
                size: Size {
                    width: auto(),
                    height: length(22.0_f32),
                },
                align_items: Some(AlignItems::Center),
                ..Default::default()
            })
            .text(txt, 12.0, theme.fg_text),
        );
    }

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: auto(),
            height: length(22.0_f32),
        },
        padding: TaffyRect {
            left: length(6.0_f32),
            right: length(6.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(5.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    })
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .tooltip(tooltip)
    .on_click(Msg::NetworkToggle)
    .children(hijos)
}

/// Tooltip + etiqueta corta opcional (SSID) según el estado.
fn descripcion(status: &NetStatus) -> (String, Option<String>) {
    match status {
        NetStatus::Wifi { ssid, signal } => {
            (format!("{ssid} · {signal}%"), Some(recortar(ssid, 12)))
        }
        NetStatus::Ethernet => ("Cable conectado".to_string(), None),
        NetStatus::WifiOff => ("Wi-Fi apagado".to_string(), None),
        NetStatus::Desconectado => ("Sin conexión".to_string(), None),
        NetStatus::Sin => ("Red no disponible".to_string(), None),
    }
}

/// Recorta `s` a `max` caracteres con elipsis.
fn recortar(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// Pinta el indicador de red dentro de `rect` según el estado.
fn dibujar_red(scene: &mut Scene, rect: PaintRect, status: &NetStatus, acento: Color, tenue: Color) {
    match status {
        NetStatus::Wifi { signal, .. } => barras_senal(scene, rect, nivel(*signal), 4, acento, tenue),
        NetStatus::Ethernet => dibujar_cable(scene, rect, acento),
        // Apagado/desconectado/ausente: barras todas tenues + barra cruzada.
        NetStatus::WifiOff | NetStatus::Desconectado | NetStatus::Sin => {
            barras_senal(scene, rect, 0, 4, acento, tenue);
            if matches!(status, NetStatus::WifiOff | NetStatus::Sin) {
                tachar(scene, rect, tenue);
            }
        }
    }
}

/// Cuántas de las 4 barras se encienden para una señal 0..=100.
fn nivel(signal: u8) -> u8 {
    match signal {
        0..=20 => 1,
        21..=45 => 2,
        46..=70 => 3,
        _ => 4,
    }
}

/// Cuatro barras ascendentes; las primeras `on` en `acento`, el resto `tenue`.
fn barras_senal(scene: &mut Scene, rect: PaintRect, on: u8, total: u8, acento: Color, tenue: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, RoundedRect};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    if rect.w <= 0.0 || rect.h <= 0.0 || total == 0 {
        return;
    }
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    let gap = 2.0_f64;
    let n = total as f64;
    let bw = ((w - gap * (n - 1.0)) / n).max(1.5);
    for i in 0..total {
        let frac = (i as f64 + 1.0) / n; // altura creciente
        let bh = (h * (0.35 + 0.65 * frac)).max(2.0);
        let bx = x + i as f64 * (bw + gap);
        let by = y + h - bh;
        let color = if i < on { acento } else { tenue };
        let rr = RoundedRect::new(bx, by, bx + bw, y + h, 1.0);
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rr);
    }
}

/// Un icono de cable: dos nodos unidos por una línea (ethernet conectado).
fn dibujar_cable(scene: &mut Scene, rect: PaintRect, color: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle, Line, Point, Stroke};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    let r = (h * 0.16).max(2.0);
    let cy = y + h * 0.5;
    let (x0, x1) = (x + w * 0.25, x + w * 0.75);
    scene.stroke(
        &Stroke::new(1.6),
        Affine::IDENTITY,
        color,
        None,
        &Line::new(Point::new(x0, cy), Point::new(x1, cy)),
    );
    scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &Circle::new(Point::new(x0, cy), r));
    scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &Circle::new(Point::new(x1, cy), r));
}

/// Un candado pequeño centrado en `rect`: cuerpo redondeado + arco del grillete.
fn dibujar_candado(scene: &mut Scene, rect: PaintRect, color: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Arc, Point, Stroke, Vec2, RoundedRect};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    let cx = rect.x as f64 + rect.w as f64 * 0.5;
    let cy = rect.y as f64 + rect.h as f64 * 0.5;
    let bw = 8.0_f64;
    let bh = 6.0_f64;
    // Cuerpo.
    let body = RoundedRect::new(cx - bw * 0.5, cy, cx + bw * 0.5, cy + bh, 1.2);
    scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &body);
    // Grillete (semicírculo sobre el cuerpo).
    let arc = Arc::new(
        Point::new(cx, cy),
        Vec2::new(2.6, 2.6),
        std::f64::consts::PI,
        std::f64::consts::PI,
        0.0,
    );
    scene.stroke(&Stroke::new(1.4), Affine::IDENTITY, color, None, &arc);
}

/// Una barra diagonal tachando el icono (radio apagada / sin NM).
fn tachar(scene: &mut Scene, rect: PaintRect, color: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Line, Point, Stroke};
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    scene.stroke(
        &Stroke::new(1.8),
        Affine::IDENTITY,
        color,
        None,
        &Line::new(Point::new(x, y + h), Point::new(x + w, y)),
    );
}

// ============================================================
// Popup (lista de redes)
// ============================================================

/// El cuerpo del popup de red: switch de la radio + lista de redes, o —si hay una
/// entrada de contraseña en curso (`password = Some((ssid, tecleado))`)— el campo
/// de contraseña. Lo enmarcan `network_overlay` (winit) y `network_menu_view`
/// (layer-shell).
pub(super) fn network_panel(
    state: Option<&NetState>,
    password: Option<(&str, &str)>,
    bt: Option<&crate::bluetooth::BtState>,
    theme: &Theme,
) -> View<Msg> {
    let hijos: Vec<View<Msg>> = match password {
        Some((ssid, typed)) => password_rows(ssid, typed, theme),
        None => {
            let mut filas = vec![header_row(state, theme)];
            llenar_lista(&mut filas, state, theme);
            agregar_auto_levanta(&mut filas, state, theme);
            agregar_guardadas(&mut filas, state, theme);
            agregar_detalles(&mut filas, state, theme);
            // Sección Bluetooth embebida (el fantasma de red es el applet de
            // conectividad completo): switch + buscar + dispositivos, las
            // mismas filas del panel BT dedicado.
            if let Some(bt) = bt {
                filas.push(seccion("Bluetooth", theme));
                filas.push(super::bluetooth::header_row(Some(bt), theme));
                if bt.available && bt.powered {
                    for d in bt.devices.iter().take(5) {
                        filas.push(super::bluetooth::device_row(d, theme));
                    }
                }
            }
            filas
        }
    };
    enmarcar(hijos, theme)
}

/// Las filas de la lista de redes (o las notas de estado vacío).
fn llenar_lista(hijos: &mut Vec<View<Msg>>, state: Option<&NetState>, theme: &Theme) {
    match state {
        Some(s) if matches!(s.status, NetStatus::Sin) => {
            hijos.push(nota("NetworkManager no disponible", theme));
        }
        Some(s) if !s.wifi_enabled => {
            hijos.push(nota("Wi-Fi apagado", theme));
        }
        Some(s) if s.networks.is_empty() => {
            hijos.push(nota("Buscando redes…", theme));
        }
        Some(s) => {
            for ap in s.networks.iter().take(8) {
                hijos.push(ap_row(ap, theme));
            }
        }
        None => hijos.push(nota("Buscando redes…", theme)),
    }
}

/// Anuncia lo que la red activa **levanta automáticamente** (NM secondaries):
/// el disparador *detectado* de la saga túnel («esta red levanta la VPN de la
/// oficina»). Informativo, en acento; nada si ninguna red activa declara VPN.
fn agregar_auto_levanta(hijos: &mut Vec<View<Msg>>, state: Option<&NetState>, theme: &Theme) {
    let Some(s) = state else { return };
    let mut nombres: Vec<String> = Vec::new();
    for c in s.saved.iter().filter(|c| c.active) {
        for r in &c.raises {
            if !nombres.contains(r) {
                nombres.push(r.clone());
            }
        }
    }
    if nombres.is_empty() {
        return;
    }
    let fila = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect { left: length(2.0_f32), right: length(0.0_f32), top: length(0.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .text(format!("Esta red levanta: {}", recortar(&nombres.join(", "), 26)), 11.5, theme.accent);
    hijos.push(fila);
}

/// Agrega la sección **Guardadas / VPN**: los perfiles conocidos de NetworkManager
/// con toggle conectar/desconectar y olvidar. Nada si no hay perfiles.
fn agregar_guardadas(hijos: &mut Vec<View<Msg>>, state: Option<&NetState>, theme: &Theme) {
    let Some(s) = state else { return };
    if s.saved.is_empty() {
        return;
    }
    hijos.push(seccion("Guardadas y VPN", theme));
    for c in s.saved.iter().take(8) {
        hijos.push(saved_row(c, theme));
    }
}

/// Una fila de perfil guardado: nombre (+ tag VPN) · toggle · olvidar.
fn saved_row(c: &crate::network::SavedConn, theme: &Theme) -> View<Msg> {
    use crate::network::ConnKind;
    let es_vpn = matches!(c.kind, ConnKind::Vpn);
    let etiqueta_txt = if es_vpn { format!("VPN · {}", c.name) } else { c.name.clone() };
    let color = if c.active { theme.accent } else { theme.fg_text };
    // Si es un túnel activo con alcance derivado, mostramos una segunda línea
    // tenue («Todo el tráfico» / «Sólo 10.0.0.0/24») — la respuesta a «¿me agarra
    // toda la máquina?». Sin alcance, una sola línea como antes.
    let nombre = match c.scope.as_ref().filter(|_| c.active) {
        Some(scope) => View::new(Style {
            flex_grow: 1.0,
            flex_direction: FlexDirection::Column,
            size: Size { width: auto(), height: length(ROW_H) },
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        })
        .children(vec![
            View::new(Style {
                size: Size { width: auto(), height: length(16.0_f32) },
                align_items: Some(AlignItems::Center),
                ..Default::default()
            })
            .text(recortar(&etiqueta_txt, 18), 12.5, color),
            View::new(Style {
                size: Size { width: auto(), height: length(12.0_f32) },
                align_items: Some(AlignItems::Center),
                ..Default::default()
            })
            .text(scope.label(), 10.0, theme.fg_muted),
        ]),
        None => View::new(Style {
            flex_grow: 1.0,
            size: Size { width: auto(), height: length(ROW_H) },
            align_items: Some(AlignItems::Center),
            ..Default::default()
        })
        .text(recortar(&etiqueta_txt, 18), 12.5, color),
    };

    let toggle_txt = if c.active { "Desconectar" } else { "Conectar" };
    let name_t = c.name.clone();
    let toggle_msg = if c.active {
        Msg::NetworkDisconnect(name_t)
    } else {
        Msg::NetConnUp(name_t)
    };
    let toggle = mini_boton(toggle_txt, toggle_msg, if c.active { theme.fg_muted } else { theme.accent }, theme);
    // Olvidar sólo tiene sentido para perfiles que no son la conexión de cable
    // interna; lo ofrecemos para todos salvo mientras están activos (evita
    // borrar la conexión en uso de un clic).
    let forget = mini_boton("Olvidar", Msg::NetForget(c.name.clone()), theme.fg_muted, theme);

    fila_base(vec![nombre, toggle, forget])
}

/// Un botón compacto de texto para las filas de perfiles.
fn mini_boton(label: &str, msg: Msg, color: Color, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: auto(), height: length(22.0_f32) },
        padding: TaffyRect { left: length(8.0_f32), right: length(8.0_f32), top: length(0.0_f32), bottom: length(0.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .radius(5.0)
    .fill(theme.bg_button)
    .hover_fill(theme.bg_button_hover)
    .on_click(msg)
    .text(label.to_string(), 11.0, color)
}

/// Agrega la línea de **detalles** de la conexión activa (IP · DNS). Nada si no hay.
fn agregar_detalles(hijos: &mut Vec<View<Msg>>, state: Option<&NetState>, theme: &Theme) {
    let Some(s) = state else { return };
    let d = &s.details;
    if d.ip.is_none() && d.dns.is_none() {
        return;
    }
    let mut partes = Vec::new();
    if let Some(ip) = &d.ip {
        partes.push(format!("IP {ip}"));
    }
    if let Some(dns) = &d.dns {
        partes.push(format!("DNS {dns}"));
    }
    hijos.push(seccion("Detalles", theme));
    hijos.push(nota(&partes.join("  ·  "), theme));
}

/// Un rótulo de sección tenue.
fn seccion(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(20.0_f32) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect { left: length(2.0_f32), right: length(0.0_f32), top: length(4.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .text(t.to_string(), 11.0, theme.accent)
}

/// Las filas del campo de contraseña para conectarse a `ssid`.
fn password_rows(ssid: &str, typed: &str, theme: &Theme) -> Vec<View<Msg>> {
    let titulo = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(format!("Conectar a {}", recortar(ssid, 22)), 13.0, theme.fg_text);

    // El campo: puntos por carácter (no mostramos la contraseña en claro).
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

    let conectar = fila_accion("Conectar", theme.accent, theme).on_click(Msg::NetworkPasswordSubmit);
    let cancelar = fila_accion("Cancelar", theme.fg_muted, theme).on_click(Msg::NetworkPasswordCancel);
    vec![titulo, campo, conectar, cancelar]
}

/// Una fila de acción centrada (Conectar/Cancelar del campo de contraseña).
fn fila_accion(label: &str, color: Color, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .text(label.to_string(), 13.0, color)
}

/// Enmarca las filas en la caja del popup (fondo + sombra).
fn enmarcar(hijos: Vec<View<Msg>>, theme: &Theme) -> View<Msg> {
    let (a, blur, dy) = elevation::E4;
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: length(PANEL_W),
            height: auto(),
        },
        padding: TaffyRect {
            left: length(12.0_f32),
            right: length(12.0_f32),
            top: length(10.0_f32),
            bottom: length(10.0_f32),
        },
        gap: Size {
            width: length(0.0_f32),
            height: length(4.0_f32),
        },
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
    .children(hijos)
}

/// Cabecera: título «Red» + switch de la radio Wi-Fi.
fn header_row(state: Option<&NetState>, theme: &Theme) -> View<Msg> {
    let on = state.map(|s| s.wifi_enabled).unwrap_or(false);
    let etiqueta = View::new(Style {
        flex_grow: 1.0,
        size: Size {
            width: auto(),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text("Wi-Fi".to_string(), 13.0, theme.fg_text);
    let sw = View::new(Style {
        size: Size {
            width: length(44.0_f32),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexEnd),
        ..Default::default()
    })
    .children(vec![switch_view(
        if on { 1.0 } else { 0.0 },
        Msg::NetworkRadio(!on),
        &SwitchPalette::from_theme(theme),
    )]);
    fila_base(vec![etiqueta, sw])
}

/// Una fila de red: nivel de señal + SSID (+ candado si es segura). Click →
/// conectar, o desconectar si ya es la activa.
fn ap_row(ap: &WifiAp, theme: &Theme) -> View<Msg> {
    let acento = theme.accent;
    let tenue = theme.fg_muted;
    let on = nivel(ap.signal);
    let mini = View::new(Style {
        size: Size {
            width: length(18.0_f32),
            height: length(ROW_H),
        },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| {
        // Centrar verticalmente un mini-indicador de 14px de alto.
        let r = PaintRect {
            x: rect.x,
            y: rect.y + (rect.h - 14.0) * 0.5,
            w: rect.w,
            h: 14.0,
        };
        barras_senal(scene, r, on, 4, acento, tenue);
    });

    let nombre = if ap.active {
        format!("{}  ✓", ap.ssid)
    } else {
        ap.ssid.clone()
    };
    let color = if ap.active { theme.accent } else { theme.fg_text };
    let etiqueta = View::new(Style {
        flex_grow: 1.0,
        size: Size {
            width: auto(),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(recortar(&nombre, 24), 12.5, color);

    // Candado pintado a mano (evitamos el emoji 🔒, que cae a tofu sin fuente de
    // color — la misma razón por la que el Control panel usa ♪/☀/✕).
    let secure = ap.secure;
    let tenue_lock = theme.fg_muted;
    let candado = View::new(Style {
        size: Size {
            width: length(16.0_f32),
            height: length(ROW_H),
        },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| {
        if secure {
            dibujar_candado(scene, rect, tenue_lock);
        }
    });

    let ssid = ap.ssid.clone();
    let msg = if ap.active {
        Msg::NetworkDisconnect(ssid)
    } else if ap.secure {
        // Red segura: pedir contraseña (vacía = perfil guardado / agente).
        Msg::NetworkPasswordPrompt(ssid)
    } else {
        Msg::NetworkConnect(ssid)
    };
    fila_base(vec![mini, etiqueta, candado])
        .radius(6.0)
        .hover_fill(theme.bg_button_hover)
        .on_click(msg)
}

/// Una nota tenue (estado vacío).
fn nota(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 12.0, theme.fg_muted)
}

fn fila_base(hijos: Vec<View<Msg>>) -> View<Msg> {
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: percent(1.0_f32),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(8.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(hijos)
}

// ============================================================
// Marcos: overlay (winit) y bar-grows (layer-shell)
// ============================================================

/// El overlay completo para **winit**: scrim (cierra al click) + panel anclado
/// arriba a la derecha, bajo la barra. `password = Some((ssid, tecleado))` muestra
/// el campo de contraseña en vez de la lista.
pub fn network_overlay(
    state: Option<&NetState>,
    password: Option<(&str, &str)>,
    bar_h: f32,
    theme: &Theme,
) -> View<Msg> {
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
    .children(vec![network_panel(state, password, None, theme)]);

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
    .on_click(Msg::NetworkToggle)
    .children(vec![fila])
}
