//! Control panel (quick settings): un flyout con volumen, brillo, batería y
//! switches de Wi-Fi/Bluetooth. Unifica en un solo overlay lo que antes estaba
//! disperso en widgets sueltos de la barra (volumen/brillo) y lo que faltaba
//! del todo (batería, radios). Se abre desde un botón de la barra
//! ([`Msg::ControlToggle`]); el scrim cierra al click afuera.
//!
//! Volumen/brillo reusan los mismos mensajes que las ventanitas existentes
//! ([`Msg::VolumeSet`]/[`Msg::BrightnessSet`], fracción absoluta `0..1`); las
//! radios emiten [`Msg::ControlWifi`]/[`Msg::ControlBt`]. Las lecturas del
//! sistema (batería, estado de las radios) viven en [`ControlExtras`].

use llimphi_theme::{elevation, radius, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, FlexDirection, Position, Size, Style},
    AlignItems, JustifyContent, Rect as TaffyRect,
};
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_text::Alignment;
use llimphi_ui::{Shadow, View};
use llimphi_widget_switch::{switch_view, SwitchPalette};

use crate::Msg;

/// Ancho del panel (px).
pub(super) const PANEL_W: f32 = 300.0;
/// Alto de una fila de slider.
const ROW_H: f32 = 30.0;
/// Largo de la pista del slider horizontal.
const TRACK_W: f32 = 150.0;
const TRACK_H: f32 = 8.0;

/// Lecturas del sistema que no provee el `WidgetCtx` del sampler: estado de la
/// batería y de las radios. Se refrescan al abrir el panel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ControlExtras {
    /// `(porcentaje 0..=100, cargando)`, o `None` si no hay batería (desktop).
    pub battery: Option<(u8, bool)>,
    pub wifi: bool,
    pub bt: bool,
    /// Perfil de energía activo (`power-saver`/`balanced`/`performance`), o `None`
    /// si no hay `powerprofilesctl` (power-profiles-daemon).
    pub power_profile: Option<String>,
    /// `true` si la luz nocturna (`wlsunset`) está corriendo.
    pub night: bool,
    /// **Paisaje sonoro** encendido: música ambiental del escritorio (takiy) sonando
    /// desde el shell. Estado interno de pata (lo sobrescribe el host con el suyo).
    pub paisaje: bool,
    /// «Mantener despierto» (el café integrado): mientras esté en `true`, el
    /// idle de energía no suspende y el compositor no apaga pantalla ni bloquea.
    /// Estado interno de pata (no una lectura del sistema).
    pub cafe: bool,
    /// Instancias del perfil `pacha` con su contenido resumido, para el panel del
    /// **diente perfil** (sidebar de instancias). Se lee junto con `pachas` (misma
    /// cadencia cacheada), del catálogo `pachas.ron` + `pacha list`.
    pub pacha_cat: Vec<crate::perfil::PachaInfo>,
    /// **Lupa**: factor de zoom de pantalla completa vigente, en porcentaje
    /// (`100` = 1.0× apagada). Lo fija pata al clickear el segmento (best-effort:
    /// no hay readback del compositor, así que los atajos lo mueven sin avisar).
    pub magnify_pct: u16,
    /// **Grabación de pantalla** en curso (screencast). Best-effort, como
    /// `magnify_pct`: pata lo refleja al togglear; el atajo de teclado lo cambia
    /// sin avisar.
    pub recording: bool,
    /// **Teclado en pantalla** (OSK) desplegado: pata lanza/mata el proceso
    /// `mirada-teclado` (superficie layer-shell) al togglear. Estado interno.
    pub teclado: bool,
}

impl ControlExtras {
    /// Lee batería de `/sys/class/power_supply`, las radios de `rfkill`, el perfil
    /// de energía y la luz nocturna. Tolerante: lo que no se puede leer queda en
    /// su default.
    pub fn read() -> Self {
        Self {
            battery: read_battery(),
            wifi: rfkill_on("wlan"),
            bt: rfkill_on("bluetooth"),
            power_profile: read_power_profile(),
            night: night_on(),
            cafe: false, // estado interno; el host lo sobrescribe con el suyo.
            paisaje: false, // ídem: lo fija el host desde el hilo del paisaje.
            pacha_cat: crate::perfil::read_pacha_infos(),
            // Sin readback del compositor; arranca «apagada» y se actualiza al
            // clickear un segmento de la lupa.
            magnify_pct: 100,
            recording: false,
            teclado: false, // estado interno; el host lo sobrescribe con el suyo.
        }
    }
}

/// Lee **sólo** perfil de energía + luz nocturna `(power_profile, night)`. Para
/// refrescar el control center persistente sin re-leer batería/radios (que el
/// host ya tiene en vivo): esos dos campos del flyout, en cambio, sólo se leían
/// al abrirlo y quedaban viejos en el panel del diente.
pub fn read_power_night() -> (Option<String>, bool) {
    (read_power_profile(), night_on())
}

/// El perfil de energía activo, vía `powerprofilesctl get`. `None` si el binario
/// no está (no hay power-profiles-daemon).
fn read_power_profile() -> Option<String> {
    let out = std::process::Command::new("powerprofilesctl")
        .arg("get")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!p.is_empty()).then_some(p)
}

/// Los perfiles que ofrecemos y su rótulo, en orden ahorro→rendimiento.
pub(super) const PERFILES: [(&str, &str); 3] = [
    ("power-saver", "Ahorro"),
    ("balanced", "Equilibrado"),
    ("performance", "Rendimiento"),
];

/// Fija el perfil de energía (`powerprofilesctl set`). No bloquea.
pub fn set_power_profile(name: &str) {
    crate::desacoplar(std::process::Command::new("powerprofilesctl")
        .args(["set", name])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn());
}

/// `true` si la luz nocturna está encendida.
///
/// Lee el knob `night_light` del `config.ron` de mirada — **no** un proceso.
/// Antes se preguntaba `pgrep -x wlsunset`, y en una máquina sin ese paquete
/// (como ésta) el switch era un **no-op silencioso**: se prendía, el `pgrep`
/// daba falso y el botón volvía solo (auditado en metal el 2026-07-24). Ahora la
/// luz nocturna la hace el compositor y el estado vive en el archivo que escribe
/// el toggle (`mirada-ctl luz-nocturna`), que es la misma fuente que lee el
/// compositor.
///
/// Escaneo tolerante en vez de parsear el RON: **a propósito** pata no depende de
/// `mirada-brain` (sería invertir la dirección del grafo — la barra no tiene por
/// qué linkear el modelo del escritorio) y tampoco queremos un proceso por
/// muestreo, que ya nos costó CPU antes. Si la clave no está, el default de
/// fábrica es apagada.
fn night_on() -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let p = std::path::Path::new(&home).join(".config/mirada/config.ron");
    let Ok(txt) = std::fs::read_to_string(p) else {
        return false;
    };
    txt.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//"))
        .find_map(|l| l.strip_prefix("night_light:"))
        .map(|v| v.trim_start().starts_with("true"))
        .unwrap_or(false)
}

/// Enciende/apaga la luz nocturna. Va por `mirada-ctl luz-nocturna`, que escribe
/// el knob en el `config.ron`; el compositor lo recarga en caliente y aplica la
/// rampa de gamma por salida él mismo (horario y temperaturas configurables, ver
/// `mirada_brain::night`). Cero dependencias externas: `wlsunset` ya no participa.
/// Desacoplado (no espera) como el resto de los toggles de la barra.
pub fn set_night(on: bool) {
    crate::spawn_cmd(if on {
        "mirada-ctl luz-nocturna on"
    } else {
        "mirada-ctl luz-nocturna off"
    });
}

/// Primer `BAT*` con `capacity` + `status`. `None` si no hay (máquina de escritorio).
fn read_battery() -> Option<(u8, bool)> {
    let base = std::path::Path::new("/sys/class/power_supply");
    let rd = std::fs::read_dir(base).ok()?;
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name();
        if !name.to_string_lossy().starts_with("BAT") {
            continue;
        }
        let cap = std::fs::read_to_string(p.join("capacity")).ok()?;
        let pct: u8 = cap.trim().parse().ok()?;
        let status = std::fs::read_to_string(p.join("status")).unwrap_or_default();
        let charging = status.trim().eq_ignore_ascii_case("Charging");
        return Some((pct.min(100), charging));
    }
    None
}

/// `true` si la radio `kind` (`wlan`/`bluetooth`) está habilitada (no bloqueada).
/// Lee `rfkill -rn` y mira la columna `soft`. Sin `rfkill` → asume encendida.
fn rfkill_on(kind: &str) -> bool {
    let out = std::process::Command::new("rfkill")
        .args(["-rno", "TYPE,SOFT"])
        .output();
    let Ok(out) = out else {
        return true;
    };
    String::from_utf8_lossy(&out.stdout).lines().any(|l| {
        let l = l.trim();
        l.starts_with(kind) && !l.contains("blocked")
    })
}

/// Conmuta una radio vía `rfkill` (no espera). `wlan`/`bluetooth`.
pub fn set_radio(kind: &str, on: bool) {
    let action = if on { "unblock" } else { "block" };
    crate::desacoplar(std::process::Command::new("rfkill").args([action, kind]).spawn());
}

/// El botón de la barra que abre el control panel (un engranaje clickeable).
pub fn control_button_view(theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: length(28.0_f32), height: percent(1.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .tooltip("Configuración rápida".to_string())
    .on_click(Msg::ControlToggle)
    .text("⚙".to_string(), 16.0, theme.fg_text)
}

/// El overlay completo: scrim (cierra al click) + el panel anclado arriba a la
/// derecha, bajo la barra.
pub fn control_overlay(
    volume: f32,
    muted: bool,
    brightness: f32,
    extras: &ControlExtras,
    bar_h: f32,
    screen: (f32, f32),
    theme: &Theme,
) -> View<Msg> {
    let _ = screen;
    let panel = control_panel(volume, muted, brightness, extras, theme);
    // Fila que empuja el panel a la derecha.
    let fila = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: auto() },
        justify_content: Some(JustifyContent::FlexEnd),
        padding: TaffyRect {
            left: length(0.0_f32),
            right: length(8.0_f32),
            top: length(8.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(vec![panel]);

    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            top: length(bar_h),
            right: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        size: Size { width: percent(1.0_f32), height: auto() },
        ..Default::default()
    })
    .on_click(Msg::ControlToggle)
    .children(vec![fila])
}

/// Las filas del control (volumen, brillo, batería, Wi-Fi, Bluetooth, perfil de
/// energía, luz nocturna), **sin** título ni chrome de tarjeta. Las comparten el
/// flyout flotante ([`control_panel`]) y el control center del sidebar
/// ([`control_center_view`]).
pub(super) fn control_sections(
    volume: f32,
    muted: bool,
    brightness: f32,
    extras: &ControlExtras,
    theme: &Theme,
) -> Vec<View<Msg>> {
    let mut hijos: Vec<View<Msg>> = Vec::new();
    // Glifos DejaVu-safe (el sistema no trae emoji a color → tofu): ♪ volumen,
    // ☀ brillo. El mute se marca tachando con ✕.
    let vol_glifo = if muted { "✕" } else { "♪" };
    hijos.push(slider_row(vol_glifo, volume, theme, Msg::VolumeSet));
    hijos.push(slider_row("☀", brightness, theme, Msg::BrightnessSet));

    if let Some((pct, charging)) = extras.battery {
        let valor = if charging {
            format!("{pct}% ⚡")
        } else {
            format!("{pct}%")
        };
        hijos.push(kv_row("Batería", &valor, theme));
    }

    hijos.push(switch_row("Wi-Fi", extras.wifi, theme, Msg::ControlWifi));
    hijos.push(switch_row("Bluetooth", extras.bt, theme, Msg::ControlBt));

    // Perfil de energía (sólo si hay power-profiles-daemon).
    if let Some(actual) = &extras.power_profile {
        hijos.push(perfil_row(actual, theme));
    }
    hijos.push(switch_row("Luz nocturna", extras.night, theme, Msg::ControlNight));
    hijos.push(switch_row("Mantener despierto", extras.cafe, theme, Msg::ControlCafe));
    hijos.push(switch_row("Teclado en pantalla", extras.teclado, theme, Msg::ControlTeclado));
    hijos.push(switch_row("Paisaje sonoro", extras.paisaje, theme, Msg::ControlPaisaje));
    hijos.push(lupa_row(extras.magnify_pct, theme));
    hijos.push(switch_row("Grabar pantalla", extras.recording, theme, Msg::Record));
    hijos
}

/// Niveles de **lupa** que ofrece el control, como `(porcentaje, rótulo)`. `100`
/// = apagada (1.0×). El paso casa con `MAGNIFY_STEP_PCT` del Cerebro.
pub(super) const NIVELES_LUPA: [(u16, &str); 4] =
    [(100, "1×"), (150, "1.5×"), (200, "2×"), (300, "3×")];

/// Fila «Lupa» + selector segmentado de factores de zoom de pantalla completa
/// (accesibilidad). El activo va en acento; al click manda `Msg::Magnify(pct)`
/// (→ `mirada-ctl magnify <pct>`). Calca el patrón de [`perfil_row`].
fn lupa_row(actual_pct: u16, theme: &Theme) -> View<Msg> {
    let etiqueta = View::new(Style {
        size: Size { width: length(60.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text("Lupa".to_string(), 12.5, theme.fg_text);

    let botones: Vec<View<Msg>> = NIVELES_LUPA
        .iter()
        .map(|(pct, rotulo)| {
            // El «1×» (apagada) cubre cualquier factor <=100; el resto, igualdad.
            let activo = if *pct == 100 { actual_pct <= 100 } else { *pct == actual_pct };
            let v = View::new(Style {
                flex_grow: 1.0,
                size: Size { width: auto(), height: length(24.0_f32) },
                align_items: Some(AlignItems::Center),
                justify_content: Some(JustifyContent::Center),
                ..Default::default()
            })
            .radius(5.0)
            .hover_fill(theme.bg_button_hover)
            .on_click(Msg::Magnify(*pct))
            .text(
                rotulo.to_string(),
                11.0,
                if activo { theme.bg_panel } else { theme.fg_muted },
            );
            if activo {
                v.fill(theme.accent)
            } else {
                v.fill(theme.bg_button)
            }
        })
        .collect();

    let seg = View::new(Style {
        flex_direction: FlexDirection::Row,
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(4.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(botones);

    fila_base(vec![etiqueta, seg])
}

/// Construye un [`ControlExtras`] con los datos **vivos** del modelo (batería,
/// radios) en vez de la lectura cacheada al abrir el flyout — para el control
/// center del sidebar, que es persistente. `power_profile`/`night` salen de la
/// base cacheada (se leen sólo al togglear; estar levemente atrás es tolerable).
pub fn extras_vivos(
    bat_now: Option<(f32, bool)>,
    wifi: bool,
    bt: bool,
    base: &ControlExtras,
) -> ControlExtras {
    ControlExtras {
        battery: bat_now.map(|(f, c)| ((f * 100.0).round() as u8, c)),
        wifi,
        bt,
        power_profile: base.power_profile.clone(),
        night: base.night,
        cafe: base.cafe,
        paisaje: base.paisaje,
        pacha_cat: base.pacha_cat.clone(),
        magnify_pct: base.magnify_pct,
        recording: base.recording,
        teclado: base.teclado,
    }
}

/// El **control center** del sidebar: reloj grande + las filas de control, en un
/// panel de alto completo (sin la tarjeta flotante del flyout). Reusa las mismas
/// filas y los mismos `Msg` que el quick-settings de la barra.
/// Los datos que alimentan los paneles del sidebar (control center y monitor de
/// sistema). Se arma en el host (mismo paquete en winit y layer-shell) y se pasa
/// por referencia, así el despacho del panel no acarrea media docena de
/// parámetros sueltos. Lleva el `WidgetCtx` entero: el control center usa
/// clock/volume/muted/brightness, el monitor usa cpu/cpu_cores/ram.
pub struct CentroDatos<'a> {
    pub ctx: &'a pata_core::widget::WidgetCtx,
    pub extras: &'a ControlExtras,
    pub media: Option<&'a crate::mpris::MediaState>,
    pub net: Option<&'a crate::network::NetState>,
    /// `(ssid, tecleado)` si hay una entrada de contraseña Wi-Fi en curso.
    pub net_password: Option<(&'a str, &'a str)>,
    pub bt: Option<&'a crate::bluetooth::BtState>,
    /// Inventario de flota (matilda) para el diente «Flota».
    pub flota: Option<&'a matilda_core::Inventory>,
    /// Estado real observado de la flota por host (discover SSH read-only).
    pub flota_remoto: Option<&'a [crate::flota_discover::HostObs]>,
    /// Censo de presencia de los equipos móviles automáticos (tejido).
    pub movil: Option<&'a [crate::movil_discover::MovilObs]>,
    /// Salud combinada de la flota (local + remoto) — la fila resumen del centro
    /// de control (destino del control fantasma de la flota).
    pub matilda: Option<&'a crate::matilda_salud::SaludFlota>,
    /// Snapshot de unidades (sandokan) para el diente «Unidades».
    pub unidades: Option<&'a sandokan_monitor_core::MonitorSnapshot>,
    /// Ventanas del WM etiquetadas por escritorio (`mirada-ctl windows`) — el
    /// taskbar de un diente-workspace (`TabsSource::Workspaces`) las filtra por
    /// su nº de escritorio. Vacío si no aplica.
    pub windows: &'a [crate::toplevel::WindowEntry],
    /// Timeline de actividad (willay) para el diente «Actividad». Vacío si no hay
    /// daemon o el diente no está montado.
    pub willay: &'a [crate::willay::EventoVista],
}

pub fn control_center_view(panel_h: f32, d: &CentroDatos, theme: &Theme) -> View<Msg> {
    let mut hijos = vec![reloj_grande(&d.ctx.clock, theme)];
    // "Sonando ahora" + transporte, sólo si hay un reproductor MPRIS.
    if let Some(m) = d.media.filter(|m| m.has_player) {
        hijos.push(media_row(m, theme));
    }
    // Volumen y brillo (sliders), batería (lectura).
    let vol_glifo = if d.ctx.muted { "✕" } else { "♪" };
    hijos.push(slider_row(vol_glifo, d.ctx.volume, theme, Msg::VolumeSet));
    hijos.push(slider_row("☀", d.ctx.brightness, theme, Msg::BrightnessSet));
    if let Some((pct, charging)) = d.extras.battery {
        let valor = if charging {
            format!("{pct}% ⚡")
        } else {
            format!("{pct}%")
        };
        hijos.push(kv_row("Batería", &valor, theme));
    }
    // Salud de la flota (matilda): local + remoto, en una fila con semáforo. Es
    // el destino del control fantasma de la flota (click → este panel).
    if let Some(salud) = d.matilda {
        hijos.push(flota_salud_row(salud, theme));
    }
    // Censo de equipos móviles (tejido): una fila por equipo automático con ●/○.
    // Que un móvil esté offline es normal (teléfono apagado), no una alarma.
    if let Some(obs) = d.movil.filter(|o| !o.is_empty()) {
        for row in movil_censo_rows(obs, theme) {
            hijos.push(row);
        }
    }
    // Wi-Fi y Bluetooth CON su lista (no sólo toggle): reusa los panels de la
    // barra, que ya traen switch + lista de redes/dispositivos clickeable.
    hijos.push(super::network::network_panel(d.net, d.net_password, None, theme));
    hijos.push(super::bluetooth::bluetooth_panel(d.bt, theme));
    // Perfil de energía + luz nocturna.
    if let Some(actual) = &d.extras.power_profile {
        hijos.push(perfil_row(actual, theme));
    }
    hijos.push(switch_row("Luz nocturna", d.extras.night, theme, Msg::ControlNight));
    hijos.push(switch_row("Mantener despierto", d.extras.cafe, theme, Msg::ControlCafe));
    hijos.push(switch_row("Paisaje sonoro", d.extras.paisaje, theme, Msg::ControlPaisaje));
    // Lupa (zoom de pantalla completa, accesibilidad).
    hijos.push(lupa_row(d.extras.magnify_pct, theme));
    // Grabar pantalla (screencast → ~/Videos, con audio del sistema).
    hijos.push(switch_row("Grabar pantalla", d.extras.recording, theme, Msg::Record));
    // Los contextos de usuario (pacha) NO van aquí: cada perfil tiene SU diente
    // dedicado (el diente `pacha`, primero del rail — ver `sidebar::pacha_panel`),
    // no chips mezclados con el reloj/control. Antes había un `pacha_row` aquí.

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: length(panel_h) },
        // Padding chico: los panels embebidos miden 280/260 px fijos y deben
        // entrar en el ancho del sidebar (≈300) sin desbordar.
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(8.0_f32),
            top: length(10.0_f32),
            bottom: length(10.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(10.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .children(hijos)
}

/// "Sonando ahora": título de la pista + transporte (anterior / play-pausa /
/// siguiente). Glifos DejaVu-safe (◀◀ ▶ ▮▮ ▶▶). Reusa los `Msg::Media*` de la
/// barra.
fn media_row(media: &crate::mpris::MediaState, theme: &Theme) -> View<Msg> {
    let titulo = if media.title.trim().is_empty() {
        "Reproduciendo…".to_string()
    } else {
        media.title.clone()
    };
    let titulo_v = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(20.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(titulo, 12.0, theme.fg_text);
    let pp = if media.playing { "▮▮" } else { "▶" };
    let botones = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(30.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        gap: Size { width: length(6.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![
        super::panels::boton_panel("◀◀", Msg::MediaPrev, theme, None),
        super::panels::boton_panel(pp, Msg::MediaPlayPause, theme, Some(theme.accent)),
        super::panels::boton_panel("▶▶", Msg::MediaNext, theme, None),
    ]);
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: auto() },
        gap: Size { width: length(0.0_f32), height: length(4.0_f32) },
        ..Default::default()
    })
    .children(vec![titulo_v, botones])
}

/// Reloj grande (HH:MM) + fecha, como cabezal del control center.
fn reloj_grande(clock: &pata_core::widget::ClockReading, theme: &Theme) -> View<Msg> {
    let hora = format!("{:02}:{:02}", clock.hour, clock.minute);
    const DIAS: [&str; 7] = [
        "domingo", "lunes", "martes", "miércoles", "jueves", "viernes", "sábado",
    ];
    let dia = DIAS.get(clock.weekday as usize).copied().unwrap_or("");
    let fecha = format!("{} {}/{:02}/{}", dia, clock.day, clock.month, clock.year);
    let hora_v = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(34.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(hora, 26.0, theme.fg_text);
    let fecha_v = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(18.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(fecha, 12.0, theme.fg_muted);
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: auto() },
        ..Default::default()
    })
    .children(vec![hora_v, fecha_v])
}

pub(super) fn control_panel(
    volume: f32,
    muted: bool,
    brightness: f32,
    extras: &ControlExtras,
    theme: &Theme,
) -> View<Msg> {
    let mut hijos: Vec<View<Msg>> = vec![titulo("Control", theme)];
    hijos.extend(control_sections(volume, muted, brightness, extras, theme));

    let (a, blur, dy) = elevation::E4;
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(PANEL_W), height: auto() },
        padding: TaffyRect {
            left: length(14.0_f32),
            right: length(14.0_f32),
            top: length(12.0_f32),
            bottom: length(12.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(8.0_f32) },
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

fn titulo(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(22.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 13.0, theme.fg_muted)
}

/// Fila glifo + slider horizontal clickeable (mapea x → fracción → `on_set`).
fn slider_row(glifo: &str, frac: f32, theme: &Theme, on_set: fn(f32) -> Msg) -> View<Msg> {
    let icono = View::new(Style {
        size: Size { width: length(26.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text(glifo.to_string(), 15.0, theme.fg_text);

    // Pista: fondo + relleno proporcional.
    let frac = frac.clamp(0.0, 1.0);
    let relleno = View::new(Style {
        size: Size { width: percent(frac), height: length(TRACK_H) },
        ..Default::default()
    })
    .fill(theme.accent)
    .radius((TRACK_H / 2.0) as f64);
    let pista = View::new(Style {
        size: Size { width: length(TRACK_W), height: length(TRACK_H) },
        ..Default::default()
    })
    .fill(theme.bg_button)
    .radius((TRACK_H / 2.0) as f64)
    .on_click_at(move |x, _y, w, _h| {
        if w <= 0.0 {
            return None;
        }
        Some(on_set((x / w).clamp(0.0, 1.0)))
    })
    .children(vec![relleno]);
    let pista_wrap = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .children(vec![pista]);

    let valor = View::new(Style {
        size: Size { width: length(38.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexEnd),
        ..Default::default()
    })
    .text_aligned(
        format!("{:.0}%", frac * 100.0),
        12.0,
        theme.fg_muted,
        Alignment::End,
    );

    // Rueda sobre TODA la fila (no sólo la pista): 5% por muesca.
    fila_base(vec![icono, pista_wrap, valor])
        .on_scroll(move |_dx, dy| super::panels::wheel_frac(frac, dy).map(on_set))
}

/// Fila etiqueta (izquierda) + valor (derecha): batería.
fn kv_row(label: &str, valor: &str, theme: &Theme) -> View<Msg> {
    let etiqueta = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(label.to_string(), 12.5, theme.fg_text);
    let v = View::new(Style {
        size: Size { width: length(90.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexEnd),
        ..Default::default()
    })
    .text_aligned(valor.to_string(), 12.5, theme.fg_muted, Alignment::End);
    fila_base(vec![etiqueta, v])
}

/// Fila «Flota»: etiqueta + semáforo con resumen compacto de salud (local +
/// remoto). Verde con todo arriba; ámbar si un servicio se cayó; rojo si un
/// contenedor cayó o un host quedó inalcanzable. Espeja el color del control
/// fantasma que abre este panel.
fn flota_salud_row(salud: &crate::matilda_salud::SaludFlota, theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_raster::peniko::Color;
    let (color, texto) = if salud.hay_problema() {
        let caidos = salud.total_down() + salud.svc_caidos() + salud.inalcanzables();
        let col = if salud.severidad() >= 2 {
            Color::from_rgb8(0xE0, 0x5A, 0x5A)
        } else {
            Color::from_rgb8(0xFB, 0xBF, 0x24)
        };
        (col, format!("● {caidos} caído{}", if caidos == 1 { "" } else { "s" }))
    } else {
        (Color::from_rgb8(0x5A, 0xD0, 0x8A), format!("● {} up", salud.total_up()))
    };
    let etiqueta = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text("Flota".to_string(), 12.5, theme.fg_text);
    let v = View::new(Style {
        size: Size { width: length(110.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexEnd),
        ..Default::default()
    })
    .text_aligned(texto, 12.5, color, Alignment::End);
    fila_base(vec![etiqueta, v])
}

/// Fila etiqueta + switch (radios).
/// Una fila por equipo móvil del censo del tejido: el nombre a la izquierda y su
/// estado (● en línea / ○ offline / sin parear) a la derecha, con semáforo. La
/// primera lleva el rótulo de sección «Equipos» para separarla de la flota.
fn movil_censo_rows(obs: &[crate::movil_discover::MovilObs], theme: &Theme) -> Vec<View<Msg>> {
    use llimphi_ui::llimphi_raster::peniko::Color;
    let verde = Color::from_rgb8(0x5A, 0xD0, 0x8A);
    let gris = Color::from_rgb8(0x8A, 0x8A, 0x8A);
    let ambar = Color::from_rgb8(0xFB, 0xBF, 0x24);
    obs.iter()
        .enumerate()
        .map(|(i, o)| {
            let etiqueta_txt = if i == 0 { format!("Equipos · {}", o.label) } else { o.label.clone() };
            let (color, texto) = if o.sin_parear {
                (ambar, "○ sin parear".to_string())
            } else if o.online {
                let n = o.nombre.clone().unwrap_or_else(|| "en línea".to_string());
                (verde, format!("● {n}"))
            } else {
                (gris, "○ offline".to_string())
            };
            let etiqueta = View::new(Style {
                flex_grow: 1.0,
                size: Size { width: auto(), height: length(ROW_H) },
                align_items: Some(AlignItems::Center),
                ..Default::default()
            })
            .text(etiqueta_txt, 12.5, theme.fg_text);
            let v = View::new(Style {
                size: Size { width: length(130.0_f32), height: length(ROW_H) },
                align_items: Some(AlignItems::Center),
                justify_content: Some(JustifyContent::FlexEnd),
                ..Default::default()
            })
            .text_aligned(texto, 12.5, color, Alignment::End);
            fila_base(vec![etiqueta, v])
        })
        .collect()
}

fn switch_row(label: &str, on: bool, theme: &Theme, make: fn(bool) -> Msg) -> View<Msg> {
    let etiqueta = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(label.to_string(), 12.5, theme.fg_text);
    let sw = View::new(Style {
        size: Size { width: length(44.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexEnd),
        ..Default::default()
    })
    .children(vec![switch_view(
        if on { 1.0 } else { 0.0 },
        make(!on),
        &SwitchPalette::from_theme(theme),
    )]);
    fila_base(vec![etiqueta, sw])
}

// Los chips de contextos (`pacha`) se removieron del control center: ahora pacha
// es un diente dedicado (`sidebar::pacha_panel`), no una fila más del quick-settings.

fn fila_base(hijos: Vec<View<Msg>>) -> View<Msg> {
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(8.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// Fila «Energía» + selector segmentado de perfiles (ahorro/equilibrado/
/// rendimiento). El activo va en acento.
fn perfil_row(actual: &str, theme: &Theme) -> View<Msg> {
    let etiqueta = View::new(Style {
        size: Size { width: length(60.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text("Energía".to_string(), 12.5, theme.fg_text);

    let botones: Vec<View<Msg>> = PERFILES
        .iter()
        .map(|(id, rotulo)| {
            let activo = *id == actual;
            let v = View::new(Style {
                flex_grow: 1.0,
                size: Size { width: auto(), height: length(24.0_f32) },
                align_items: Some(AlignItems::Center),
                justify_content: Some(JustifyContent::Center),
                ..Default::default()
            })
            .radius(5.0)
            .hover_fill(theme.bg_button_hover)
            .on_click(Msg::ControlPowerProfile(id.to_string()))
            .text(
                rotulo.to_string(),
                11.0,
                if activo { theme.bg_panel } else { theme.fg_muted },
            );
            if activo {
                v.fill(theme.accent)
            } else {
                v.fill(theme.bg_button)
            }
        })
        .collect();

    let seg = View::new(Style {
        flex_direction: FlexDirection::Row,
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(4.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(botones);

    fila_base(vec![etiqueta, seg])
}
