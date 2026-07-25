//! `pata-llimphi` — el frontend Linux del marco.
//!
//! Monta el modelo agnóstico de [`pata_core`] sobre Llimphi. El reparto de
//! responsabilidades es la regla dura del repo (UIs intercambiables sobre un
//! `*-core` agnóstico):
//!
//! - **`pata-core`** decide *qué* mostrar: resuelve la geometría
//!   ([`pata_core::layout::resolve`]) y, por cada [`WidgetSpec`], materializa un
//!   [`Widget`] que emite un view-model ([`WidgetView`]) en cada `tick`.
//! - **este crate** decide *cómo*: muestrea el sistema en un
//!   [`WidgetCtx`](pata_core::widget::WidgetCtx) (ver [`sampler`]) y traduce el
//!   view-model a `View<Msg>` de Llimphi (ver [`render`]).
//!
//! El `shuma_input` es la excepción: es **interacción**, no modelo de dominio,
//! así que lo intercepta el frontend (ver [`shuma`]) en lugar de pasar por el
//! `build` agnóstico —igual que `mirada-launcher` trata su shuma_bar—.
//!
//! Hoy todas las superficies se pintan en una sola ventana, en los rects que el
//! layout resolvió. Cuando el compositor `mirada` reconozca superficies `pata`
//! (Fase 8), cada una será su propia ventana acoplada.

// Íconos XDG de apps (resolución freedesktop + cache): vivía aquí y bajó a
// shuma-module-shell para que el panel de completado los pinte en TODOS los
// frontends del shell; el re-export mantiene el path `crate::app_icons`.
pub use shuma_module_shell::app_icons;
pub mod altab;
pub mod cava;
pub mod cielo;
pub mod grabacion;
pub mod agora;
pub mod khipu;
pub mod tampu;
pub mod usb;
pub mod willay;
pub mod keys;
pub mod layer;
pub mod nouser;
pub mod config_watch;
pub mod nahual;
pub mod open;
pub mod rag;
pub mod render;
pub mod sampler;
pub mod shuma;
pub mod shuma_app;
pub mod flota_discover;
pub mod movil_discover;
pub mod matilda_salud;
pub mod toplevel;
pub mod tray;
pub mod unidades;
pub mod bateria;
pub mod energia;
pub mod bluetooth;
pub mod marquesina;
pub mod mpris;
pub mod network;
pub mod paisaje;
pub mod perfil;
pub mod notifications;
pub mod progreso;
pub mod polkit;
pub mod triage;
pub mod weather;

use std::time::Duration;

use llimphi_motion::{animate, motion, Tween};
use llimphi_theme::Theme;
use llimphi_ui::{App, Handle, Key, KeyEvent, KeyState, Modifiers, NamedKey, View, WheelDelta};

use llimphi_widget_navigator::{NavId, NavMode};

use pata_core::config::{FloatingCard, SurfaceKind};
use pata_core::widget::{build, Widget, WidgetCtx};
use pata_core::{Config, Frame, Rect};

use nahual::NahualState;
use nouser::{MembersOutcome, NavState, PollOutcome};
use rag::{RagState, RagStatus};
use sampler::Sampler;
use shuma::ShumaState;
use tray::TrayHandle;

/// `true` si el live-wire de la **shuma COMPLETA** está activo. Cuando lo está,
/// el drawer Quake monta la shuma entera (`shuma-shell-llimphi`:
/// dientes/sesiones/menubar/canvas + tabs/tiling/atajos/semántico) en vez del
/// módulo bare de una sola sesión, y el cabezal de la barra se reduce a un chip
/// que despliega el drawer (la shuma trae su propio input adentro).
///
/// **Default ON** (2026-06-24): es el modo querido — todas las features de
/// shuma-en-pata (tabs, atajos tipo Ctrl+Shift+T, colores de actividad,
/// `:buscar`, Explorer) sólo viven aquí. El path bare quedó atrás. Opt-OUT con
/// `PATA_SHUMA_FULL=0` (o `false`/`no`) para volver al módulo de una sesión.
pub fn shuma_full_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("PATA_SHUMA_FULL").as_deref(),
            Ok("0") | Ok("false") | Ok("no")
        )
    })
}

/// Eleva un `Msg` de la shuma completa al `Msg` de pata (con el `Debug` opaco).
/// Es la función de `lift`/`map` que se pasa a `shuma_app::{view,update,…}`.
fn lift_shuma(m: shuma_app::Msg) -> Msg {
    Msg::ShumaFull(shuma_app::FullMsg(m))
}

/// Persiste el override por-sidebar de los ejes de la barrita en el TOML del
/// usuario: `docked` → `Surface::reserve`, `rail_outside` → `Surface::rail_outside`
/// (cada uno `Some(v)` fija, `None` no toca). Carga la config viva, muta la surface
/// `si` y la reescribe. El re-anclaje lo hace el llamador (re-exec en layer,
/// `recargar_config` en winit). No-op si `si` está fuera de rango.
pub(crate) fn persistir_eje_sidebar(si: usize, docked: Option<bool>, rail_outside: Option<bool>) {
    // Guardado SPARSE: sólo el valor cambiado, en su path, sin snapshot full.
    if let Some(d) = docked {
        if let Err(e) = pata_config::set_override(&format!("surfaces.{si}.reserve"), d) {
            eprintln!("pata · no pude guardar el override del sidebar: {e}");
        }
    }
    if let Some(o) = rail_outside {
        if let Err(e) = pata_config::set_override(&format!("surfaces.{si}.rail_outside"), o) {
            eprintln!("pata · no pude guardar el override del sidebar: {e}");
        }
    }
}

/// Persiste el **autohide** de la surface `si` como override sparse
/// (`surfaces.{si}.autohide`). Cambia el anclaje → el caller re-ancla/re-exec.
pub(crate) fn persistir_autohide_sidebar(si: usize, autohide: bool) {
    if let Err(e) = pata_config::set_override(&format!("surfaces.{si}.autohide"), autohide) {
        eprintln!("pata · no pude guardar el autohide del sidebar: {e}");
    }
}

/// Persiste el **ancho del panel** desplegado de la surface `si` (arrastre del
/// divisor) como override sparse. Clampa a los límites del slider (120..600).
pub(crate) fn persistir_panel_width_sidebar(si: usize, panel_width: f32) {
    let w = panel_width.clamp(120.0, 600.0) as f64;
    if let Err(e) = pata_config::set_override(&format!("surfaces.{si}.panel_width"), w) {
        eprintln!("pata · no pude guardar el ancho del panel del sidebar: {e}");
    }
}

/// Persiste el modo **dientes de dos pasos** (global) como override sparse
/// (`general.diente_dos_pasos`).
pub(crate) fn persistir_diente_dos_pasos(b: bool) {
    if let Err(e) = pata_config::set_override("general.diente_dos_pasos", b) {
        eprintln!("pata · no pude guardar el modo de dientes: {e}");
    }
}

/// Una acción de **taskbar** sobre una ventana concreta, disparada desde el menú
/// contextual del taskbar de un diente-escritorio. Cada variante mapea a uno o
/// dos verbos de `mirada-ctl` (ver [`crate::sampler::window_action`]). Las que
/// operan sobre «la enfocada» se aplican tras un `focus-window` de la objetivo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinAct {
    /// Traer al frente / enfocar (`focus-window`).
    Focus,
    /// Cerrar la ventana (`close-window`).
    Close,
    /// Cerrar todas las **demás** ventanas de ese escritorio (deja sólo ésta).
    CloseOthers,
    /// Minimizar al scratchpad (`focus-window` + `send-to-scratchpad`).
    Minimize,
    /// Alternar maximizada (`focus-window` + `toggle-maximize`).
    Maximize,
    /// Alternar pantalla completa (`focus-window` + `toggle-fullscreen`).
    Fullscreen,
    /// Alternar flotante/teselada (`focus-window` + `toggle-float`).
    ToggleFloat,
    /// Alternar «visible en todos los escritorios» / sticky (`focus-window` +
    /// `toggle-sticky`).
    Sticky,
    /// Alternar picture-in-picture: esquina 16:9 + sticky (`focus-window` +
    /// `toggle-pip`).
    Pip,
    /// Mandar la ventana al escritorio `n` sin saltar con ella (`focus-window` +
    /// `send-to-workspace n`).
    MoveTo(u8),
}

/// Pestaña del **mezclador** de audio (el popup de volumen, estilo pavucontrol):
/// reproducción (streams que suenan), grabación (streams que graban), y los
/// dispositivos de salida/entrada.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VolumeTab {
    /// Corrientes de reproducción por app (sink-inputs) + el máster.
    #[default]
    Reproduccion,
    /// Corrientes de grabación por app (source-outputs).
    Grabacion,
    /// Dispositivos de salida (sinks): elegir default + volumen por dispositivo.
    Salida,
    /// Dispositivos de entrada (sources/micrófonos): default + volumen.
    Entrada,
}

/// Modo de captura de pantalla (hapiy). Lo elige el diálogo de captura.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturaModo {
    /// Todo el escritorio (todos los monitores compuestos) → PNG.
    Completa,
    /// Una región elegida a mano (con `slurp`, si está) → PNG.
    Region,
    /// Captura completa y la abre en **tullpu** para anotar/recortar.
    Editar,
}

impl CapturaModo {
    /// El comando de shell que ejecuta esta captura vía [`hapiy`].
    fn comando(self) -> &'static str {
        match self {
            // Un pequeño retardo deja cerrarse al overlay del diálogo antes del
            // disparo, para que no salga en la foto.
            CapturaModo::Completa => "sleep 0.3; hapiy",
            // `slurp` da la región interactiva; su formato casa con --region x,y,w,h.
            CapturaModo::Region => "hapiy --region \"$(slurp -f '%x,%y,%w,%h')\"",
            CapturaModo::Editar => "sleep 0.3; hapiy --edit",
        }
    }
}

/// Los mensajes de la app.
#[derive(Clone, Debug)]
pub enum Msg {
    /// Refresh periódico (1 Hz): re-muestrea el sistema y `tick`ea los widgets.
    Tick,
    /// Refresh rápido del visualizador de audio (~20 Hz): drena el último cuadro
    /// de cava y re-pinta. Sólo se dispara si la config declara un `cava`.
    CavaTick,
    /// Latido de animación del **diente vivo** (~20 Hz): avanza su reloj y
    /// re-resuelve la manifestación. Sólo se dispara si la config declara un
    /// diente de contenido `control`.
    DienteTick,
    /// Desplegar/replegar el drawer de shuma.
    ShumaToggle,
    /// Activar la sesión de terminal `i` de la shuma completa y abrir el drawer
    /// Quake directo en ella. Lo emiten los dientes-sesión del rail (el `</>`
    /// como workspace especial: los tabs del terminal son los dientes del
    /// sidebar). No-op si la shuma completa está apagada.
    TerminalSession(usize),
    /// Repliega el drawer por **deshover**: el puntero entró al scrim (área
    /// fuera del contenido). Con guarda anti-churn — ignora el evento espurio del
    /// instante de apertura.
    ShumaAutoClose,
    /// Un evento del **shell real** hospedado (`shuma-module-shell`): teclas,
    /// latido que drena la salida, clicks en cards/etapas, scroll, selección del
    /// cuerpo IDE-text… Todo el contenido del drawer llega por aquí (el `view`
    /// del módulo lo envuelve con su `lift`). pata sólo lo reenvía a
    /// `shuma_module_shell::update`.
    ShumaShell(shuma_module_shell::Msg),
    /// Un evento de la **captura de voz** (`rimay-voz-host`) del botón de
    /// micrófono del input hospedado: mapea `EventoEscucha` al `EstadoEscucha`
    /// que el input pinta (Oyendo/Despierto/Dictando/Esperando) y, en el dictado,
    /// inserta el texto reconocido en el cursor. La captura se arranca/para
    /// drenando el `mic_intent` del módulo (ver `iniciar_voz`/`parar_voz`).
    VozEvento(rimay_voz_host::EventoEscucha),
    /// El puntero **entró/salió** de la zona de controles fantasma (borde derecho
    /// del input). `true` revela TODOS los iconos fugaces (luna/signo/cava/volumen/
    /// CPU/batería) para que el mouse pueda interactuar con ellos; `false` vuelve
    /// a mostrar sólo los salientes.
    RevealFantasmas(bool),
    /// El puntero entró (`true`) o salió (`false`) de UN icono fugaz concreto:
    /// lo **pinnea** — mientras esté pinneado no se oculta (aunque su condición
    /// de salience caiga) y el turno rotativo de las leves se congela. El leave
    /// sólo despinnea si el pin vigente es el propio (los enter/leave entre
    /// iconos vecinos pueden llegar cruzados).
    FantasmaPin(crate::shuma::Fugaz, bool),
    /// Click sobre un icono fugaz: **aprende el uso** (el asiento del icono se
    /// corre a la derecha con los clicks, persistido) y despacha la acción del
    /// icono ([`crate::shuma::accion_fugaz`] — abrir su diálogo).
    FugazClick(crate::shuma::Fugaz),
    /// Un evento de la **shuma COMPLETA** hospedada (`shuma-shell-llimphi`:
    /// dientes/sesiones/menubar/canvas) cuando el live-wire está activo
    /// (`PATA_SHUMA_FULL=1`). El `view` de la shuma lo envuelve con su `lift`;
    /// pata lo reenvía a `shuma_app::update` con el handle del host lifteado.
    ShumaFull(shuma_app::FullMsg),
    /// Tick de la animación de despliegue (sólo re-render). También sirve de
    /// no-op para absorber clicks sobre el borde del panel del drawer.
    ShumaAnim,
    /// Conmuta el drawer entre alto normal (45%) y maximizado (97%). Botón ▢.
    ShumaMaximize,
    /// Arrastre del borde del drawer: `dy` en px (ya con el signo del anchor:
    /// positivo = agrandar). Fija `ShumaState::height_frac`.
    ShumaResize(f32),
    /// Desdockea: abre la sesión en una instancia **standalone** de shuma (en el
    /// mismo cwd) y repliega el drawer. Botón ⇱.
    ShumaUndock,
    /// Desplegar/replegar el drawer del **front universal de nahual** (Super+E).
    NahualToggle,
    /// Un evento del módulo `nahual-module` hospedado (navegación, abrir, vista,
    /// miniaturas…). El `view` del módulo lo envuelve con su `lift`; pata lo
    /// reenvía a `nahual_module::update` y ejecuta los `Effect`s que devuelve.
    Nahual(nahual_module::Msg),
    /// Tick de la animación del drawer de nahual / no-op para absorber clicks.
    NahualAnim,
    /// El worker terminó de construir el `Navigator` de las Mónadas del daemon
    /// (lo dejó en el slot de `NahualState`). El hilo de UI lo toma y lo monta.
    NahualDaemonReady,
    /// El montaje del daemon de Mónadas falló (sin daemon / broker caído). El
    /// usuario se queda navegando POSIX.
    NahualDaemonFailed(String),
    /// Lanzar un programa (click sobre un widget con prop `exec`).
    Spawn(String),
    /// Saltar al escritorio virtual `n` (**1-based**), por click en una celda del
    /// `workspaces` switcher. Se lo pide al WM (`mirada-ctl workspace N`); el
    /// switcher refleja el cambio en el próximo tick.
    SwitchWorkspace(u8),
    /// Click en un **diente-workspace** del rail (`TabsSource::Workspaces`): salta
    /// al escritorio `ws` (**1-based**) Y despliega su taskbar como panel del
    /// sidebar `si`. Combina el salto (`mirada-ctl workspace N`) con abrir el
    /// drawer del diente — el navegador de ventanas (DISENO-SHELL-NAVEGADOR §4-§5).
    WorkspaceTooth { si: usize, ws: u8 },
    /// Cambiar de **contexto de usuario** (`pacha`) por click en un chip del
    /// control center. Le pide al daemon `pacha switch <id>`; el chip activo
    /// se reconcilia en el próximo refresco del panel.
    SwitchPacha(String),
    /// Seleccionar (para **ver**) una instancia del perfil `pacha` en el panel del
    /// diente — el tab de la instancia. Es puramente de UI (qué instancia se pinta
    /// en el panel), distinto de [`Msg::SwitchPacha`] (que ACTIVA el contexto).
    /// `None` restaura la instancia activa como selección.
    PachaSelect(Option<String>),
    /// Rueda del mouse sobre el medidor de volumen: ajusta el volumen del sink
    /// por defecto. El `f32` es el delta de la rueda (signo = dirección).
    VolumeWheel(f32),
    /// Click/click-derecho sobre el volumen: togglea el mute del sink.
    VolumeMute,
    /// Click en el `clipboard`: despliega/repliega el popup con el historial.
    ClipboardMenu,
    /// Elegir una entrada del historial: la vuelve a copiar (`wl-copy`) y cierra.
    ClipboardPick(String),
    /// Acción sobre una entrada (Klipper): abrir el objetivo detectado
    /// (URL/`mailto:`/ruta) con `xdg-open` y cerrar el popup.
    ClipboardAction(String),
    /// (Des)fija la entrada `id` del historial persistente (Klipper).
    ClipboardPin(u64),
    /// Quita la entrada `id` del historial persistente (Klipper).
    ClipboardDelete(u64),
    /// Click en el reloj: despliega/repliega el panel para fijar fecha/hora.
    ClockPanel,
    /// Click en un fantasma astral (reloj de sol / luna / eclipse): despliega/
    /// repliega el diálogo **Cielo** (efemérides ricas de cosmos).
    CieloPanel,
    /// Elige la localidad `n` (0-based) para clima y cielo, desde el selector del
    /// diálogo Cielo. Persiste `general.ubicacion.activa` y re-siembra la ubicación.
    CieloLocalidad(u32),
    /// Click en el fantasma de clima: despliega/repliega el diálogo del
    /// **clima** (cielo meteorológico grande, animado, con los detalles).
    ClimaPanel,
    /// Click en el fantasma khipu: despliega/repliega el diálogo de **captura
    /// rápida** (anotar una nota que se desvanece) y arma el borrador de teclado.
    KhipuPanel,
    /// Un carácter tecleado en el borrador de la nota khipu.
    KhipuChar(char),
    /// Borra el último carácter del borrador de la nota khipu.
    KhipuBackspace,
    /// Enter en el borrador: **anota** la nota y limpia el borrador (sigue abierto).
    KhipuSubmit,
    /// **Refuerza** (revive) la nota khipu `id`: le sube la masa sobre el horizonte.
    KhipuReinforce(u64),
    /// Click en el fantasma del común (tampu): despliega/repliega el diálogo con
    /// lo que tienes en custodia y lo que aportaste (sólo lectura).
    TampuPanel,
    /// Click en el fantasma de captura: despliega/repliega el menú de captura de
    /// pantalla (completa / región / editar en tullpu).
    CapturaPanel,
    /// Dispara una captura de pantalla en el modo dado (vía hapiy) y cierra el menú.
    Captura(CapturaModo),
    /// **Arranca** una grabación de video (screencast) en el modo dado, con o sin
    /// audio (vía wf-recorder). Cierra el menú. No hace nada si ya se está grabando.
    GrabarIniciar(grabacion::GrabModo, bool),
    /// **Detiene** la grabación en curso (SIGINT → mp4 limpio) y avisa dónde quedó.
    GrabarDetener,
    /// Click en el fantasma de medios extraíbles: despliega/repliega el diálogo.
    UsbPanel,
    /// Click en el fantasma «¿Le creo?» (ágora): despliega/repliega el diálogo.
    AgoraPanel,
    /// Lanza la app ágora (para el veredicto interactivo de confianza).
    AgoraAbrir,
    /// Monta la partición `dev` (vía udisksctl).
    UsbMontar(String),
    /// Desmonta la partición `dev`.
    UsbDesmontar(String),
    /// Expulsa (power-off) el disco extraíble `disco`.
    UsbExpulsar(String),
    /// Abre el punto de montaje en el gestor de archivos.
    UsbAbrir(String),
    /// Click izquierdo sobre el medidor de CPU (o el de cores): despliega/
    /// repliega su ventanita de interacción.
    CpuPanel,
    /// Click izquierdo sobre el medidor de RAM: despliega/repliega su ventanita.
    RamPanel,
    /// Click izquierdo sobre el medidor de volumen: despliega/repliega su
    /// ventanita (el mezclador).
    VolumePanel,
    /// Cambia la pestaña del mezclador (reproducción/grabación/salida/entrada).
    VolumeTabSet(VolumeTab),
    /// Volumen de una corriente de **grabación** por app (source-output) `0..1`.
    SourceOutputVolume(u32, f32),
    /// Togglea el mute de una corriente de grabación.
    SourceOutputMute(u32),
    /// Elige el dispositivo de **entrada** (micrófono) por defecto.
    SourceSelect(String),
    /// Volumen de captura de un dispositivo de entrada por nombre `0..1`.
    SourceVolume(String, f32),
    /// Togglea el mute de un dispositivo de entrada.
    SourceMute(String),
    /// Volumen de un dispositivo de **salida** por nombre `0..1` (su máster).
    SinkVolume(String, f32),
    /// Togglea el mute de un dispositivo de salida.
    SinkMute(String),
    /// Click izquierdo sobre el medidor de brillo: despliega/repliega su
    /// ventanita (slider vertical).
    BrightnessPanel,
    /// Ajustar el volumen a una fracción exacta `0..1` desde la ventanita
    /// (click sobre la franja del slider). El sampler refleja en el próximo tick.
    VolumeSet(f32),
    /// Ajustar el brillo a una fracción exacta `0..1` desde la ventanita.
    BrightnessSet(f32),
    /// Ajustar el volumen del sink-input (corriente de una app) `index` a la
    /// fracción `0..1` desde el mezclador. El medidor refleja en el próximo tick.
    SinkInputVolume(u32, f32),
    /// Togglear el mute del sink-input `index` desde el mezclador.
    SinkInputMute(u32),
    /// Elegir el dispositivo de salida (sink) por su nombre de máquina desde el
    /// selector de salida: lo fija por defecto y mueve las corrientes activas a
    /// él. La lista refleja el cambio en el próximo refresco del panel.
    SinkSelect(String),
    /// Ajusta un campo del borrador de fecha/hora `(campo 0..=4, delta)`:
    /// 0=año 1=mes 2=día 3=hora 4=minuto.
    ClockAdjust(u8, i32),
    /// Aplica el borrador al reloj del sistema (apaga NTP + `timedatectl`).
    ClockApply,
    /// Re-activa la sincronización NTP (vuelve a la hora automática).
    ClockSyncNtp,
    /// Rueda del mouse sobre el medidor de brillo: ajusta la luminosidad de la
    /// pantalla. El `f32` es el delta de la rueda (signo = dirección).
    BrightnessWheel(f32),
    /// Descartar el completado flotante del input de shuma (clic afuera de su
    /// panel). Cierra la surface autónoma **y** el popup del módulo, sin ejecutar
    /// ni insertar nada.
    CompletionDismiss,
    /// Desplegar/replegar el control panel (quick settings: volumen, brillo,
    /// batería, Wi-Fi, Bluetooth). Al abrir, refresca las lecturas del sistema.
    ControlToggle,
    /// Conmutar la radio Wi-Fi (`rfkill`). El `bool` es el estado deseado.
    ControlWifi(bool),
    /// Conmutar la radio Bluetooth (`rfkill`). El `bool` es el estado deseado.
    ControlBt(bool),
    /// Fijar el perfil de energía (`powerprofilesctl set <id>`).
    ControlPowerProfile(String),
    /// Encender/apagar la luz nocturna (`wlsunset`).
    ControlNight(bool),
    /// «Mantener despierto» (café): inhibe el idle de energía (suspensión) y, en
    /// el backend layer-shell, también el apagado de pantalla/bloqueo del
    /// compositor (idle-inhibit). Reemplaza al workaround tipo `caffeine`.
    ControlCafe(bool),
    /// **Teclado en pantalla** (OSK): despliega/oculta el teclado virtual. En el
    /// backend layer-shell lanza/mata el proceso `mirada-teclado` (superficie
    /// wlr-layer-shell que inyecta por `zwp_virtual_keyboard`); en dev sólo
    /// refleja el estado.
    ControlTeclado(bool),
    /// **Paisaje sonoro**: enciende/apaga la música ambiental del escritorio
    /// generada por takiy (módulo [`paisaje`]). Sin abrir ninguna app: suena desde
    /// el shell.
    ControlPaisaje(bool),
    /// **Lupa**: fija el factor de zoom de pantalla completa, en porcentaje
    /// (`100` = 1.0× apagada, `200` = 2.0×) vía `mirada-ctl magnify <pct>`.
    /// Accesibilidad para hipermétropes.
    Magnify(u16),
    /// **Grabar pantalla** (screencast): `true` arranca, `false` detiene, vía
    /// `mirada-ctl record start/stop`.
    Record(bool),
    /// Desplegar/replegar el applet de red (lista de redes Wi-Fi).
    NetworkToggle,
    /// Conectar a la red Wi-Fi `ssid` (`nmcli device wifi connect`).
    NetworkConnect(String),
    /// Desconectar la red Wi-Fi activa `ssid` (`nmcli connection down`).
    NetworkDisconnect(String),
    /// Encender/apagar la radio Wi-Fi. El `bool` es el estado deseado.
    NetworkRadio(bool),
    /// Levanta un perfil guardado por nombre (VPN o red conocida): `connection up`.
    NetConnUp(String),
    /// **Olvida** (borra) un perfil guardado por nombre: `connection delete`.
    NetForget(String),
    /// Abrir el campo de contraseña para conectarse a la red segura `ssid`.
    NetworkPasswordPrompt(String),
    /// Carácter tecleado en el campo de contraseña.
    NetworkPasswordChar(char),
    /// Backspace en el campo de contraseña.
    NetworkPasswordBackspace,
    /// Conectar con la contraseña tecleada (vacía = perfil guardado / agente).
    NetworkPasswordSubmit,
    /// Cancelar la entrada de contraseña (vuelve a la lista de redes).
    NetworkPasswordCancel,
    /// Desplegar/replegar el applet de Bluetooth.
    BluetoothToggle,
    /// Encender/apagar el controlador Bluetooth.
    BluetoothPower(bool),
    /// Conectar el dispositivo `mac`.
    BluetoothConnect(String),
    /// Desconectar el dispositivo `mac`.
    BluetoothDisconnect(String),
    /// Lanzar un scan de dispositivos Bluetooth nuevos (12 s).
    BluetoothScan,
    /// **Emparejar** un dispositivo nuevo `mac` (pair → trust → connect).
    BluetoothPair(String),
    /// Carácter tecleado en el diálogo de contraseña de polkit.
    PolkitChar(char),
    /// Backspace en el diálogo de polkit.
    PolkitBackspace,
    /// Confirmar la autenticación con la contraseña tecleada.
    PolkitSubmit,
    /// Cancelar la autenticación de polkit.
    PolkitCancel,
    /// Desplegar/replegar el popup de notificaciones.
    NotificationsToggle,
    /// Conmutar «no molestar».
    NotificationsDnd(bool),
    /// Vaciar el historial de notificaciones.
    NotificationsClear,
    /// Desplegar/replegar el menú de sesión/energía.
    SessionToggle,
    /// Pedir confirmación de una acción disruptiva (reiniciar/apagar/logout).
    SessionConfirm(SessionAction),
    /// Ejecutar una acción de sesión (tras confirmar, o directa si es benigna).
    SessionRun(SessionAction),
    /// Cancelar la confirmación pendiente (vuelve a la lista de acciones).
    SessionCancel,
    /// Pedir la **pantalla de confirmación fullscreen** para una acción disruptiva
    /// (apagar/reiniciar/cerrar sesión/cambiar contexto). La dispara el diente de
    /// sistema/sesión; abre el overlay traslúcido «sobre todo».
    ConfirmPedir(ConfirmAccion),
    /// Aceptar la acción del overlay de confirmación: la corre y cierra el overlay.
    ConfirmAceptar,
    /// Cancelar/cerrar el overlay de confirmación sin correr nada.
    ConfirmCancelar,
    /// Play/pausa del reproductor activo (MPRIS).
    MediaPlayPause,
    /// Pista siguiente.
    MediaNext,
    /// Pista anterior.
    MediaPrev,
    /// Desplegar/replegar el menú del botón de inicio.
    StartToggle,
    /// Cicla al próximo estilo de menú (Classic → XP → GNOME → Classic).
    /// Right-click sobre el botón de inicio.
    StartStyleCycle,
    /// Carácter al buscador del menú de inicio.
    StartChar(char),
    /// Backspace en el buscador del menú de inicio.
    StartBackspace,
    /// Enter en el menú: lanza el primer resultado del filtro.
    StartLaunchFirst,
    /// Desplazar la lista del menú de inicio `delta` px (rueda).
    StartScroll(f32),
    /// Estampa el offset de scroll (px, ya clampeado por la vista) del flyout
    /// de menú abierto — lo emite el `scroll_y` de `panel_abs_scroll`.
    MenuScrollTo(f32),
    /// El puntero entró en la categoría `i` del menú de inicio: muestra sus apps
    /// en el panel de la derecha (submenú al hover).
    MenuHoverCategory(usize),
    /// Lanzar una app del menú de inicio por su `id` en el [`app_bus::AppRegistry`].
    LaunchApp(String),
    /// Activar una ventana del `window_list` (traerla al frente, o minimizarla si
    /// ya está activa — estilo KDE). El `u32` es el [`toplevel::Toplevel::id`];
    /// sólo el backend layer-shell sabe resolverlo.
    ActivateWindow(u32),
    /// Cerrar una ventana del task manager (clic derecho o clic medio). El `u32`
    /// es el [`toplevel::Toplevel::id`]; sólo el backend layer-shell sabe
    /// resolverlo.
    CloseWindow(u32),
    /// Arrastre en curso de un botón del task manager: `id` de la ventana
    /// arrastrada + `dx` (delta horizontal desde el evento anterior). El backend
    /// layer-shell acumula el delta y reordena la lista en vivo. Sólo lo usa el
    /// backend layer-shell (en winit los botones no son arrastrables).
    TaskDragMove(u32, f32),
    /// Fin del arrastre de un botón del task manager (su `id`). Si apenas se
    /// movió, el backend lo reinterpreta como click y activa la ventana; si no,
    /// conserva el nuevo orden ya aplicado en vivo.
    TaskDragEnd(u32),
    /// Activar un item del `tray` (click). El `String` es la `key` del
    /// [`tray::TrayItem`]; sólo el backend layer-shell sabe resolverlo.
    TrayActivate(String),
    /// Activar una **pestaña vertical** del rail (navegador de ventanas estilo
    /// Zen). El `u32` es el id de ventana **de mirada** (la lista sale de
    /// `mirada-ctl windows`, no de foreign-toplevel), así que en AMBOS backends
    /// se resuelve por la CLI del WM (`mirada-ctl focus-window N`) — a
    /// diferencia de [`Msg::ActivateWindow`], cuyo id es del backend que lo pintó.
    RailTabActivate(u32),
    /// Cerrar una pestaña vertical del rail (clic derecho / clic medio). Mismo
    /// espacio de ids que [`Msg::RailTabActivate`]: va por `mirada-ctl
    /// close-window N` en ambos backends.
    RailTabClose(u32),
    /// **Clic-derecho** sobre una fila del taskbar de un diente-escritorio: abre el
    /// **menú contextual** de esa ventana (popup flotante anclado al cursor). `si`
    /// = sidebar del drawer donde vive el menú; `ws` = escritorio (para «cerrar las
    /// demás»); `id` = ventana de mirada; `(x, y)` = ancla en coords de la surface
    /// del drawer (las entrega `on_right_click_screen`).
    WinMenuOpen { si: usize, ws: u8, id: u32, title: String, x: f32, y: f32 },
    /// Cerrar el menú contextual de ventana (clic en el backdrop / tras una acción).
    WinMenuClose,
    /// Ejecutar una acción de taskbar sobre la ventana `u32` desde su menú
    /// contextual. Todas van por `mirada-ctl` (enfocan la ventana y luego aplican
    /// el verbo sobre «la enfocada», salvo focus/close que son por-id). Cierra el
    /// menú al despacharla.
    WinMenuDo(u32, WinAct),
    // --- Sidebar navegador (Fase 11c) ---
    /// Clic en un diente del rail `(surface_idx, tab_idx)`: despliega/repliega su
    /// panel navegador.
    NavTabActivate(usize, usize),
    /// Barrita del sidebar: fija el eje DOCKED de la surface `si` (reserva franja
    /// del escritorio sí/no). Persiste en el TOML y re-ancla la layer surface.
    SidebarSetDocked(usize, bool),
    /// Barrita del sidebar: fija la POSICIÓN del rail de la surface `si` (afuera =
    /// franja saliente / adentro = overlay). Persiste en el TOML y re-ancla.
    SidebarSetRailOutside(usize, bool),
    /// Multiswitch del sidebar: fija el AUTOHIDE de la surface `si` (se esconde y
    /// reaparece al hover). Persiste en el TOML y re-ancla la layer surface.
    SidebarSetAutohide(usize, bool),
    /// Multiswitch del sidebar: fija el modo **dientes de dos pasos** (global de la
    /// vista/tema). Persiste en `general` y recarga en caliente (no re-ancla).
    SidebarSetDienteDosPasos(bool),
    /// Arrastre del **divisor** del drawer: suma `dx` px al `panel_width` de la
    /// surface `si` (con clamp). Repinta el drawer y actualiza su input-region —
    /// SIN redimensionar la layer surface (que se creó a ancho máximo, justamente
    /// para poder redimensionar el panel sin tocar la surface — Iris Xe).
    SidebarResize(usize, f32),
    /// Despliega/repliega el **popover multiswitch** del control mutable del
    /// sidebar `si` (opciones de disposición agrupadas). Lleva el `si` para poder
    /// mostrar el drawer de ESE sidebar y hostear la card aunque no haya ningún
    /// diente desplegado (la ventanita de opciones autónoma, no clipeada).
    SidebarControlToggle(usize),
    // --- Buscador jerárquico del sidebar ---
    /// El buscador toma/suelta el foco de teclado (`true` al clickear la caja).
    SearchFocus(bool),
    /// Un carácter tecleado en el buscador.
    SearchChar(char),
    /// Borrar el último carácter del buscador.
    SearchBackspace,
    /// Vaciar el buscador y soltar el foco (Esc / botón limpiar).
    SearchClear,
    /// Cerrar el panel navegador desplegado (Esc / clic fuera).
    NavClosePanel,
    /// Cambiar el modo del navegador (árbol/grafo).
    NavSetMode(NavMode),
    /// Seleccionar un nodo del navegador.
    NavSelect(NavId),
    /// Expandir/colapsar un nodo rama; al expandir una Mónada sin miembros
    /// resueltos dispara su `resolve_monad`.
    NavToggle(NavId),
    /// Right-click sobre un nodo: si es un archivo, abre el menú "Abrir con…"
    /// (precomputa sus apps); si no, no-op.
    NavContextMenu(NavId),
    /// Elegir cómo abrir el archivo del menú: `Some(app_id)` con esa app nativa,
    /// `None` con el handler del sistema (`xdg-open`).
    NavOpenWith(NavId, Option<String>),
    /// Cerrar el menú "Abrir con…" sin abrir nada.
    NavMenuCancel,
    /// Clic en un diente **hospedado** (de la app enfocada) en el rail de pata:
    /// `(app_id, tooth_id)`. Se reenvía a la app por el rail hospedado. Sólo el
    /// backend layer-shell (que conoce el foco y corre el `HostServer`) lo resuelve.
    HostToothActivate(String, u32),
    /// Desplazar el panel navegador `delta` px.
    NavScroll(f32),
    /// Disparo periódico del poll de Mónadas (`list_monads`).
    NavTick,
    /// Resultado del poll de Mónadas.
    NavPoll(PollOutcome),
    /// Resultado de resolver los miembros de una Mónada.
    NavMembers(MembersOutcome),
    // --- Sidebar RAG (preguntale a tu correo) ---
    /// El hilo de fondo terminó de armar (o no) el motor RAG: `ok` = quedó
    /// disponible; `corpus` = cuántos mensajes leyó de la caché de paloma.
    RagEngineReady { ok: bool, corpus: usize },
    /// Carácter al buscador del panel RAG.
    RagChar(char),
    /// Backspace en el buscador del panel RAG.
    RagBackspace,
    /// Enter en el buscador: lanza la consulta al motor.
    RagSubmit,
    /// Limpia la consulta y la respuesta (click en el buscador / nueva pregunta).
    RagClear,
    /// El motor devolvió una respuesta redactada + sus fuentes citadas.
    RagResult {
        answer: String,
        sources: Vec<rag_motor::RagSource>,
    },
    /// La consulta falló (sin hits, embeddings o IA caídos).
    RagError(String),
    /// Cerrar la app.
    Quit,
}

/// Un widget dentro de un slot: o un widget de `pata-core` (que emite un
/// view-model), o el `shuma_input` —interacción que pinta el frontend—.
pub enum SlotWidget {
    /// Un widget builtin de `pata-core`. `exec` es el comando que lanza al
    /// clickearlo (de la prop `exec` del spec), o `None` si no es clickeable.
    /// `kind` es el `WidgetSpec::kind` (cpu_meter/volume/brightness/clock…): el
    /// render lo usa para teñir el medidor con su gradiente propio y para
    /// cablear la interacción específica (rueda de volumen/brillo, click en el
    /// reloj). `cells` es el ancho cuantizado pedido (0 = automático).
    Core {
        kind: String,
        widget: Box<dyn Widget>,
        exec: Option<String>,
        cells: u32,
    },
    /// El botón de inicio: muestra su `label` y, al clickearlo, despliega el
    /// menú nativo de apps (o lanza `exec` si la config lo fija, override estilo
    /// waybar). Es interacción, no view-model de core.
    Start {
        /// Texto/ícono del botón (prop `label`, default `⊞`).
        label: String,
        /// Comando a lanzar en vez de abrir el menú, si la config lo fija.
        exec: Option<String>,
    },
    /// El cabezal del shell; su estado vive en [`Model::shuma`].
    Shuma,
    /// **Marquesina**: ticker que desplaza notificaciones/avisos recientes por la
    /// barra (la barra de mando del navegador de ventanas, §5.1 del diseño).
    /// Dato del host (`data.notifications`), animado con `data.anim_t`.
    /// **Obsoleta**: la marquesina se mudó al placeholder del input (`Shuma`); el
    /// widget queda como no-op.
    Marquesina,
    /// **Reloj grande** fijo de la barra de mando (HH:MM grande + fecha chica),
    /// como el boceto. Lee la hora local directo (chrono).
    ClockBig,
    /// La lista de ventanas abiertas. Es interacción + IPC (igual que `Shuma`):
    /// los datos los provee el backend (vía wlr-foreign-toplevel en layer-shell)
    /// y se pasan al render aparte, no por el view-model de core.
    WindowList,
    /// El portapapeles: muestra el texto copiado actual. Dato del host (vía
    /// `wl-paste`), no del view-model de core. `exec` (opcional) es el comando a
    /// lanzar al clickearlo — típicamente un selector de historial (cliphist).
    Clipboard {
        /// Comando del selector de historial, o `None` si no es clickeable.
        exec: Option<String>,
    },
    /// La bandeja del sistema (StatusNotifierItem). Dato del host (vía D-Bus, ver
    /// [`tray`]), no del view-model de core. Cada item se activa al clickearlo.
    Tray,
    /// El clima: un dibujo colorido del cielo + la temperatura. Dato del host
    /// (servicio público por `curl`, ver [`weather`]). `exec` (opcional) abre el
    /// pronóstico al clickearlo.
    Weather {
        /// Comando a lanzar al click (un sitio del tiempo), o `None`.
        exec: Option<String>,
    },
    /// El visualizador de audio estilo CAVA: barras animadas con el espectro.
    /// Dato del host (el binario `cava` en modo raw, ver [`cava`]).
    Cava,
    /// El **Front Panel** estilo CDE/Solaris: la franja chunky inferior con
    /// botones biselados Motif (lanzadores), el **switcher de escritorios** en
    /// una caja recessed al centro, y reloj. Renderiza la barra ENTERA (no usa
    /// el reparto en tercios). Dato del host (`AppRegistry` + escritorios +
    /// reloj), pasado por [`render::BarData`].
    FrontPanel,
    /// El botón del control panel (quick settings): un engranaje que abre el
    /// flyout de volumen/brillo/batería/radios ([`Msg::ControlToggle`]).
    Control,
    /// El applet de red (Wi-Fi/Ethernet): un icono de señal que abre un popup
    /// con la lista de redes ([`Msg::NetworkToggle`]). Dato del host (vía
    /// `nmcli`, ver [`network`]), no del view-model de core.
    Network,
    /// El botón de sesión/energía: un símbolo de power que abre un menú con
    /// bloquear/suspender/reiniciar/apagar/cerrar sesión ([`Msg::SessionToggle`]).
    Session,
    /// Controles de reproducción (MPRIS): prev/play-pause/next + título. Dato del
    /// host (vía `playerctl`, ver [`mpris`]). Se oculta si no hay reproductor.
    Media,
    /// El applet de Bluetooth: un icono que abre un popup con el switch del
    /// controlador + la lista de dispositivos ([`Msg::BluetoothToggle`]). Dato del
    /// host (vía `bluetoothctl`, ver [`bluetooth`]).
    Bluetooth,
    /// La campanita de notificaciones: icono + popup con no-molestar, las últimas
    /// notificaciones y limpiar ([`Msg::NotificationsToggle`]). Habla con el daemon
    /// `pata-notify` por D-Bus (ver [`notifications`]).
    Notifications,
}

/// Las acciones del menú de sesión/energía. El logout pasa por el WM (mirada hace
/// su FUS logout: cierra ventanas + relevo), el resto por systemd/loginctl —
/// pata habla con el sistema por su CLI, como con wpctl/nmcli (Regla 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAction {
    /// Bloquear la sesión (el locker del sistema vía `loginctl lock-session`).
    Lock,
    /// Suspender a RAM (`systemctl suspend`).
    Suspend,
    /// Reiniciar (`systemctl reboot`).
    Reboot,
    /// Apagar (`systemctl poweroff`).
    Shutdown,
    /// Cerrar sesión (`mirada-ctl logout`, fallback `loginctl terminate-user`).
    Logout,
}

impl SessionAction {
    /// Las acciones en orden de presentación en el menú.
    pub const ALL: [SessionAction; 5] = [
        SessionAction::Lock,
        SessionAction::Suspend,
        SessionAction::Reboot,
        SessionAction::Shutdown,
        SessionAction::Logout,
    ];

    /// La etiqueta visible (localizada).
    pub fn label(self) -> String {
        rimay_localize::t(match self {
            SessionAction::Lock => "pata-session-lock",
            SessionAction::Suspend => "pata-session-suspend",
            SessionAction::Reboot => "pata-session-reboot",
            SessionAction::Shutdown => "pata-shutdown",
            SessionAction::Logout => "pata-logout",
        })
    }

    /// El comando de shell que la ejecuta.
    ///
    /// **Apagar/reiniciar/suspender van primero por `mirada-ctl`**, igual que
    /// `Logout`. Motivo: bajo `arje` el apagado lo ejecuta PID 1 y se le pide por
    /// su bus, cuyo socket es `root:root` sin escritura para otros. `pata` corre
    /// como el usuario, así que su `systemctl poweroff` moría con `Permission
    /// denied` — y como se lanza en silencio, el botón simplemente **no hacía
    /// nada, sin ningún mensaje**. El compositor sí corre como root y ya es el
    /// agente privilegiado de la sesión.
    ///
    /// El `||` deja el camino viejo como respaldo para una sesión sin mirada
    /// (un host systemd/elogind, donde `systemctl` es el de verdad y la política
    /// de logind ya autoriza al dueño de la sesión activa).
    pub fn command(self) -> &'static str {
        match self {
            SessionAction::Lock => "loginctl lock-session",
            SessionAction::Suspend => "mirada-ctl suspend || systemctl suspend",
            SessionAction::Reboot => "mirada-ctl reboot || systemctl reboot",
            SessionAction::Shutdown => "mirada-ctl poweroff || systemctl poweroff",
            SessionAction::Logout => "mirada-ctl logout || loginctl terminate-user \"$USER\"",
        }
    }

    /// `true` si la acción es disruptiva (reiniciar/apagar/cerrar sesión) y
    /// merece una confirmación antes de ejecutarse.
    pub fn needs_confirm(self) -> bool {
        matches!(
            self,
            SessionAction::Reboot | SessionAction::Shutdown | SessionAction::Logout
        )
    }
}

/// Ejecuta una acción de sesión (desacoplado, como [`spawn_cmd`]).
pub fn run_session_action(a: SessionAction) {
    spawn_cmd(a.command());
}

/// Una acción **disruptiva** que la pantalla de confirmación fullscreen intercepta
/// antes de correr: apagar/reiniciar/cerrar sesión, o cambiar de contexto (`pacha`).
/// El diente de sistema/sesión emite `Msg::ConfirmPedir(…)` con una de estas; el
/// overlay traslúcido pregunta «¿…?» y sólo al aceptar corre [`Self::ejecutar`].
#[derive(Clone, Debug, PartialEq)]
pub enum ConfirmAccion {
    /// Una acción de energía/sesión (reiniciar/apagar/cerrar sesión).
    Session(SessionAction),
    /// Cambiar al contexto de usuario `id` (`pacha switch <id>`). `label` es el
    /// nombre legible para el texto de la pregunta.
    Pacha { id: String, label: String },
}

impl ConfirmAccion {
    /// El verbo en imperativo para el botón de aceptar (localizado donde aplica).
    pub fn verbo(&self) -> String {
        match self {
            ConfirmAccion::Session(a) => a.label(),
            ConfirmAccion::Pacha { .. } => rimay_localize::t("pata-sistema-cambiar-contexto"),
        }
    }

    /// La **pregunta** que muestra el overlay («¿Apagar el equipo?» …).
    pub fn pregunta(&self) -> String {
        match self {
            ConfirmAccion::Session(SessionAction::Shutdown) => "¿Apagar el equipo?".to_string(),
            ConfirmAccion::Session(SessionAction::Reboot) => "¿Reiniciar el equipo?".to_string(),
            ConfirmAccion::Session(SessionAction::Logout) => "¿Cerrar la sesión?".to_string(),
            ConfirmAccion::Session(a) => format!("¿{}?", a.label()),
            ConfirmAccion::Pacha { label, .. } => format!("¿Cambiar al contexto «{label}»?"),
        }
    }

    /// Una línea de contexto bajo la pregunta (qué implica), o vacío.
    pub fn detalle(&self) -> &'static str {
        match self {
            ConfirmAccion::Session(SessionAction::Shutdown) => "Se cerrarán todas las aplicaciones.",
            ConfirmAccion::Session(SessionAction::Reboot) => "Se cerrarán todas las aplicaciones.",
            ConfirmAccion::Session(SessionAction::Logout) => "Se cerrará tu sesión y volverás al inicio.",
            ConfirmAccion::Pacha { .. } => "Cambiará el contexto de usuario activo.",
            _ => "",
        }
    }

    /// Corre la acción confirmada.
    pub fn ejecutar(&self) {
        match self {
            ConfirmAccion::Session(a) => run_session_action(*a),
            ConfirmAccion::Pacha { id, .. } => spawn_cmd(&format!("pacha switch {id}")),
        }
    }
}

/// Despacha una [`WinAct`] del menú contextual de taskbar sobre la ventana `id`.
/// Todas van por `mirada-ctl` (ver [`sampler`]). `ws` y `windows` sólo se usan para
/// «cerrar las demás» (todas las ventanas de ese escritorio menos la objetivo).
/// Compartido por ambos backends para no duplicar el mapeo verbo→acción.
pub(crate) fn do_win_act(id: u32, act: WinAct, ws: u8, windows: &[crate::toplevel::WindowEntry]) {
    match act {
        WinAct::Focus => sampler::activate_window(id),
        WinAct::Close => sampler::close_window(id),
        WinAct::CloseOthers => {
            for w in windows.iter().filter(|w| w.workspace == ws && w.id != id) {
                sampler::close_window(w.id);
            }
        }
        WinAct::Minimize => sampler::window_action(id, "send-to-scratchpad"),
        WinAct::Maximize => sampler::window_action(id, "toggle-maximize"),
        WinAct::Fullscreen => sampler::window_action(id, "toggle-fullscreen"),
        WinAct::ToggleFloat => sampler::window_action(id, "toggle-float"),
        WinAct::Sticky => sampler::window_action(id, "toggle-sticky"),
        WinAct::Pip => sampler::window_action(id, "toggle-pip"),
        WinAct::MoveTo(n) => sampler::move_window_to_workspace(id, n),
    }
}

/// `true` si la config pide el reloj en **UTC** (`general.timezone = "UTC"`).
/// Cualquier otro valor (incluido `"auto"`) usa la hora local. Paridad con el
/// `TzMode` de mirada-launcher (que sólo distinguía auto/UTC). Compartido por
/// ambos backends para construir el sampler.
pub fn usa_utc(cfg: &Config) -> bool {
    cfg.general.timezone.trim().eq_ignore_ascii_case("utc")
}

/// Cosechador único de los hijos «disparar y olvidar» de la barra.
///
/// Los `Command::spawn()` desacoplados (el `mirada-ctl` del switcher, `nmcli`,
/// `bluetoothctl`, `rfkill`, `udisksctl`, `notify-send`, `xdg-open`…) descartaban
/// el [`Child`](std::process::Child) sin llamar nunca a `wait()`. El kernel
/// conserva el `task_struct` de un hijo muerto hasta que alguien lo cosecha, así
/// que **cada invocación dejaba un zombi** colgando de pata. Medido en metal el
/// 2026-07-24: **51 zombis `mirada-ctl`** (más `nmcli` y `media-app`) tras 12 h de
/// sesión, el más viejo de 11 h — una fuga de PIDs proporcional al uso, no al
/// tiempo. Los hijos de larga vida (cava, mpris, polkit, grabación, sampler) ya
/// cosechaban bien; la fuga era sólo la de los one-shot.
///
/// Un solo hilo de por vida los espera por todos. **No puede esperar en serie**:
/// [`spawn_cmd`] también lanza *apps* (un navegador vive horas) y bloquearía la
/// cola detrás de ella, dejando zombis a los que vienen atrás. Así que mantiene
/// la lista y la sondea con `try_wait`. Con la lista vacía duerme en el `recv`
/// —cero costo en reposo, que es la ley de este repo—; sólo mientras haya hijos
/// vivos despierta cada [`SONDEO`](self) a hacer un puñado de `waitpid`.
fn cosechador() -> &'static std::sync::mpsc::Sender<std::process::Child> {
    /// Cada cuánto se sondea a los hijos vivos. Sólo corre si hay alguno.
    const SONDEO: Duration = Duration::from_secs(2);
    static TX: std::sync::OnceLock<std::sync::mpsc::Sender<std::process::Child>> =
        std::sync::OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<std::process::Child>();
        std::thread::Builder::new()
            .name("pata-cosecha".into())
            .spawn(move || {
                let mut vivos: Vec<std::process::Child> = Vec::new();
                loop {
                    // Sin hijos que vigilar, bloquea (no consume nada). Con
                    // hijos, espera con plazo para volver a sondearlos.
                    let llega = if vivos.is_empty() {
                        rx.recv().map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected)
                    } else {
                        rx.recv_timeout(SONDEO)
                    };
                    match llega {
                        Ok(hijo) => vivos.push(hijo),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        // El `Sender` es `'static` y no se suelta nunca, así que
                        // esto no debería pasar; si pasara, terminá de cosechar
                        // lo que quede y salí en vez de girar en vacío.
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            for mut h in vivos.drain(..) {
                                let _ = h.wait();
                            }
                            return;
                        }
                    }
                    // `Ok(None)` = sigue vivo, conservalo; cualquier otra cosa
                    // (salió y lo cosechamos, o ya no es hijo nuestro) se va.
                    vivos.retain_mut(|h| matches!(h.try_wait(), Ok(None)));
                }
            })
            .expect("no pude levantar el hilo cosechador de pata");
        tx
    })
}

/// Entrega al [`cosechador`] el hijo recién lanzado, para que no quede
/// `<defunct>`. Es el reemplazo directo de `let _ = cmd.spawn();` en todo
/// lanzamiento desacoplado: toma el `Result` tal cual lo devuelve `spawn()`, así
/// que el sitio de llamada sigue siendo una línea y sigue ignorando el fallo de
/// lanzamiento (si el binario no está, no hay nada que cosechar).
pub fn desacoplar(hijo: std::io::Result<std::process::Child>) {
    if let Ok(hijo) = hijo {
        let _ = cosechador().send(hijo);
    }
}

/// Lanza `cmd` por `sh -c` como proceso hijo, sin esperarlo (no bloquea). Lo
/// usan ambos backends al recibir [`Msg::Spawn`]. El hijo queda a cargo del
/// [`cosechador`]: no bloquea a quien lanza, pero tampoco deja zombi.
pub fn spawn_cmd(cmd: &str) {
    desacoplar(std::process::Command::new("sh").arg("-c").arg(cmd).spawn());
}

/// Convierte el registro de apps del host en las [`LaunchableApp`] que el input
/// de shuma consume para su launcher sin prefijo (#3) **y** los candidatos-app
/// con ícono del popup de completado. Sólo las `Exec` (spawneables como comando
/// del shell); `Action`/`Wasm` se omiten. El `icon` viaja como hint (glifo o
/// nombre freedesktop) para que la surface flotante lo pinte.
pub(crate) fn apps_lanzables(reg: &app_bus::AppRegistry) -> Vec<shuma_module_shell::LaunchableApp> {
    reg.all()
        .iter()
        .filter_map(|e| match &e.launch {
            app_bus::Launch::Exec { program, args } => {
                let cmd = if args.is_empty() {
                    program.clone()
                } else {
                    format!("{} {}", program, args.join(" "))
                };
                Some(
                    shuma_module_shell::LaunchableApp::new(e.label.clone(), cmd)
                        .con_icono(e.icon.clone()),
                )
            }
            _ => None,
        })
        .collect()
}

/// Desacopla ("mover de verdad") la sesión embebida del shell a un shuma
/// standalone: serializa su salida visible a un archivo de handoff, lanza shuma
/// apuntándole `SHUMA_HANDOFF` + `SHUMA_CWD`, y deja la sesión embebida en
/// limpio (es un *mover*, no copiar — ya no queda duplicada). El cwd y el
/// historial de comandos viajan solos (la history es persistente y compartida;
/// el `cd` fija el directorio); este handoff suma además el scrollback. No migra
/// el PTY vivo: un proceso corriendo no salta de proceso.
pub(crate) fn undock_shuma_session(inner: &mut shuma_module_shell::State) {
    let cwd = inner.cwd.display().to_string();
    let q = shell_quote(&cwd);
    // Snapshot de la salida vigente a un temporal único; si está vacía, sin
    // handoff (el standalone abre limpio igual).
    let mut handoff_env = String::new();
    let snap = inner.output_snapshot(4000);
    if !snap.lines.is_empty() {
        if let Ok(json) = serde_json::to_string(&snap) {
            let path = std::env::temp_dir().join(format!(
                "shuma-handoff-{}-{}.json",
                std::process::id(),
                inner.output_len()
            ));
            if std::fs::write(&path, json).is_ok() {
                handoff_env =
                    format!("SHUMA_HANDOFF={} ", shell_quote(&path.display().to_string()));
            }
        }
    }
    spawn_cmd(&format!(
        "cd {q} 2>/dev/null; {handoff_env}SHUMA_CWD={q} exec shuma-shell-llimphi"
    ));
    // Mover, no copiar: la sesión embebida arranca limpia (su contenido se fue
    // al standalone).
    *inner = shuma_module_shell::State::new(shuma_module::Source::Local);
}

/// Ejecuta un [`nahual_module::Effect`] del módulo hospedado: el host tiene el
/// `Handle` (para spawnear la generación de miniaturas) y el registro de apps
/// (para lanzar). Las miniaturas reentran como `Msg::Nahual(ThumbReady/Failed)`.
fn ejecutar_efecto_nahual(
    registry: &app_bus::AppRegistry,
    ef: nahual_module::Effect,
    handle: &Handle<Msg>,
) {
    use nahual_module::Effect;
    match ef {
        Effect::GenThumb(path) => {
            handle.spawn(move || Msg::Nahual(nahual_module::run_gen_thumb(path)));
        }
        Effect::OpenDefault(path) => {
            // Sin app declarada: que el escritorio decida (xdg-open).
            crate::desacoplar(std::process::Command::new("xdg-open").arg(&path).spawn());
        }
        Effect::Launch { app_id, path } => {
            if let Some(entry) = registry.get(&app_id) {
                // Vía arje si está levantado (Ente OneShot); si no, open crudo.
                let _ = arje_applaunch::open_entry(entry, &path.to_string_lossy());
            }
        }
    }
}

/// Borrador editable de fecha/hora para el panel del reloj. Se inicializa con la
/// hora actual al abrir el panel; los botones ▲/▼ lo ajustan; "Aplicar" lo
/// escribe al reloj del sistema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockDraft {
    pub year: i32,
    pub month: i32,
    pub day: i32,
    pub hour: i32,
    pub minute: i32,
}

impl Default for ClockDraft {
    fn default() -> Self {
        Self {
            year: 2026,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
        }
    }
}

impl ClockDraft {
    /// El borrador inicializado con la hora actual (UTC si `utc`, si no local).
    pub fn from_now(utc: bool) -> Self {
        use chrono::{Datelike, Timelike};
        let (y, mo, d, h, mi) = if utc {
            let n = chrono::Utc::now();
            (n.year(), n.month(), n.day(), n.hour(), n.minute())
        } else {
            let n = chrono::Local::now();
            (n.year(), n.month(), n.day(), n.hour(), n.minute())
        };
        Self {
            year: y,
            month: mo as i32,
            day: d as i32,
            hour: h as i32,
            minute: mi as i32,
        }
    }

    /// Ajusta el campo `f` (0=año…4=minuto) por `delta`. Mes/hora/minuto dan la
    /// vuelta; año y día se acotan a un rango sano.
    pub fn adjust(&mut self, f: u8, delta: i32) {
        let wrap = |v: i32, lo: i32, hi: i32| {
            let span = hi - lo + 1;
            (((v - lo) % span + span) % span) + lo
        };
        match f {
            0 => self.year = (self.year + delta).clamp(1970, 2100),
            1 => self.month = wrap(self.month + delta, 1, 12),
            2 => self.day = (self.day + delta).clamp(1, 31),
            3 => self.hour = wrap(self.hour + delta, 0, 23),
            4 => self.minute = wrap(self.minute + delta, 0, 59),
            _ => {}
        }
    }

    /// El campo `f` como texto a dos/cuatro dígitos.
    pub fn campo(&self, f: u8) -> String {
        match f {
            0 => format!("{:04}", self.year),
            1 => format!("{:02}", self.month),
            2 => format!("{:02}", self.day),
            3 => format!("{:02}", self.hour),
            4 => format!("{:02}", self.minute),
            _ => String::new(),
        }
    }

    /// El sello `"YYYY-MM-DD HH:MM:00"` que consume `timedatectl set-time`.
    pub fn stamp(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:00",
            self.year, self.month, self.day, self.hour, self.minute
        )
    }
}

/// El grosor (px) de la primera barra que hospeda un widget de `kind`, para
/// posicionar su popup debajo. Default 32 si no se encuentra.
pub fn bar_thickness_for(cfg: &Config, kind: &str) -> f32 {
    cfg.surfaces
        .iter()
        .find(|s| {
            s.start
                .iter()
                .chain(&s.center)
                .chain(&s.end)
                .any(|w| w.kind == kind)
        })
        .map(|s| s.thickness)
        .unwrap_or(32.0)
}

/// Tope del historial de portapapeles.
pub const CLIP_HISTORY_MAX: usize = 16;

/// Agrega `nuevo` al frente del `historial` de portapapeles si no es vacío ni
/// igual al actual tope; deduplica (mueve al frente) y recorta a
/// [`CLIP_HISTORY_MAX`]. Compartido por ambos backends. Devuelve `true` si
/// efectivamente entró un clip **nuevo** — la señal que usan los call-sites para
/// emitir el evento al centro de eventos (willay).
pub fn push_clip_history(historial: &mut Vec<String>, nuevo: &Option<String>) -> bool {
    let Some(s) = nuevo else { return false };
    if s.is_empty() {
        return false;
    }
    if historial.first().map(|f| f == s).unwrap_or(false) {
        return false; // ya es el tope
    }
    historial.retain(|x| x != s);
    historial.insert(0, s.clone());
    historial.truncate(CLIP_HISTORY_MAX);
    true
}

/// Abre el historial de portapapeles persistente ([`pata_portapapeles`]) en
/// `$XDG_DATA_HOME/pata-portapapeles` (o `~/.local/share/…`). `None` si no abre
/// —otro pata lo tiene tomado, disco de sólo-lectura—: pata sigue con el ring en
/// memoria. Se avisa por stderr, como el resto de los fallos best-effort de pata.
/// Reconstruye el espejo en memoria `clip_history` (sólo texto, más nuevo
/// primero) desde el store persistente. Se usa al arrancar y tras pin/borrar.
pub(crate) fn clip_history_desde_store(
    store: &Option<pata_portapapeles::Historial>,
) -> Vec<String> {
    store
        .as_ref()
        .and_then(|h| h.listar().ok())
        .map(|es| {
            es.into_iter()
                .filter_map(|e| match e.contenido {
                    pata_portapapeles::Contenido::Texto(t) => Some(t),
                    pata_portapapeles::Contenido::Imagen { .. } => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn abrir_clip_store() -> Option<pata_portapapeles::Historial> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".local").join("share"))
        })?;
    let dir = base.join("pata-portapapeles");
    match pata_portapapeles::Historial::abrir(&dir) {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("pata · portapapeles: no pude abrir el historial persistente: {e}");
            None
        }
    }
}

/// El [`willay_core::Evento`] de un texto recién copiado, para el centro de
/// eventos. **Puro** (no toca el socket): los call-sites lo emiten con
/// `willay_emit::emitir_silencioso` cuando [`push_clip_history`] confirma un clip
/// nuevo. El texto va inline en el payload (es chico) con un tope para que un
/// clip enorme no infle el índice.
pub fn evento_clip(texto: &str, ts_usec: u64) -> willay_core::Evento {
    const MAX: usize = 16 * 1024;
    let recortado: String = texto.chars().take(MAX).collect();
    // Título = primera línea, acotada — el preview del clip en el timeline.
    let titulo: String = recortado.lines().next().unwrap_or("").chars().take(80).collect();
    willay_core::Evento::nuevo(
        willay_core::Clase::Clip,
        ts_usec,
        "portapapeles",
        titulo,
        recortado.clone(),
        willay_core::Payload::Texto(recortado),
    )
}

/// Envuelve `s` en comillas simples para `sh -c`, escapando comillas internas.
/// Para pasar rutas con espacios al stand-in de apertura (Fase 11d).
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `true` si la config declara al menos un widget de ese `kind` en cualquier slot
/// de cualquier superficie. Lo usan ambos backends para arrancar servicios caros
/// (el tray, que toma el nombre del watcher) sólo si hacen falta.
pub fn config_tiene_widget(cfg: &Config, kind: &str) -> bool {
    cfg.surfaces.iter().any(|s| {
        s.start
            .iter()
            .chain(&s.center)
            .chain(&s.end)
            .any(|w| w.kind == kind)
    })
}

/// `true` si alguna superficie es un **navegador de escritorios**
/// (`TabsSource::Workspaces` o un slot con el widget `workspaces`/`escritorios`):
/// su taskbar por diente-escritorio se alimenta de la lista de ventanas del WM, así
/// que hay que muestrearla igual que con `window_tabs`. Sin esto el preset nativo
/// (que NO monta `window_tabs`) dejaba `windows` vacío y cada escritorio se veía
/// «Sin ventanas» — las pestañas nunca aparecían.
pub fn config_quiere_taskbar_ws(cfg: &Config) -> bool {
    use pata_core::config::TabsSource;
    cfg.surfaces.iter().any(|s| {
        s.tabs_source == TabsSource::Workspaces
            || s.start
                .iter()
                .any(|w| matches!(w.kind.as_str(), "workspaces" | "escritorios"))
    })
}

/// La `place` (consulta) para el clima. Prioriza la **localidad activa** de
/// `general.ubicacion` (nombre, o `"lat,lon"` si no tiene nombre); si la ubicación
/// es automática, cae a la vieja prop `place` del widget `weather`, y si tampoco,
/// a `""` (el servicio detecta por IP). Así clima y cielo comparten la ubicación.
pub fn weather_place(cfg: &Config) -> String {
    if let Some(loc) = cfg.general.ubicacion.activa() {
        if !loc.nombre.trim().is_empty() {
            return loc.nombre.clone();
        }
        return format!("{},{}", loc.lat, loc.lon);
    }
    primer_widget(cfg, "weather")
        .map(|w| w.str_prop("place", "").to_string())
        .unwrap_or_default()
}

/// Las coordenadas `(lat, lon)` de la localidad activa de la config, o `None` si
/// la ubicación es automática (entonces el cielo las recibe del clima al
/// resolverlas por IP).
pub fn cielo_loc_inicial(cfg: &Config) -> Option<(f64, f64)> {
    cfg.general.ubicacion.activa().map(|l| (l.lat, l.lon))
}

/// El número de barras del primer widget `cava` (prop `bars`, default 12,
/// acotado a 4..=64).
pub fn cava_bars(cfg: &Config) -> u32 {
    primer_widget(cfg, "cava")
        .map(|w| (w.num_prop("bars", 12.0) as u32).clamp(4, 64))
        .unwrap_or(12)
}

/// El primer `WidgetSpec` de ese `kind` en cualquier slot de cualquier superficie.
fn primer_widget<'a>(cfg: &'a Config, kind: &str) -> Option<&'a pata_core::WidgetSpec> {
    cfg.surfaces.iter().find_map(|s| {
        s.start
            .iter()
            .chain(&s.center)
            .chain(&s.end)
            .find(|w| w.kind == kind)
    })
}

/// `true` si la config declara al menos un `SurfaceKind::Sidebar` con un diente
/// cuyo contenido es un navegador (`kind = "navigator"`). Sólo entonces arranca
/// el plano de datos de nouser (el poll periódico de Mónadas).
pub fn config_tiene_navigator(cfg: &Config) -> bool {
    cfg.surfaces
        .iter()
        .filter(|s| s.kind == SurfaceKind::Sidebar)
        .flat_map(|s| s.tabs.iter())
        .any(|t| t.content.kind == "navigator")
}

/// `true` si la config declara un diente **vivo** (contenido `control`): un
/// diente multifuncional cuyo icono es el canvas del árbitro de atención. Sólo
/// entonces se enciende el latido de animación [`Msg::DienteTick`].
pub fn config_tiene_diente_vivo(cfg: &Config) -> bool {
    cfg.surfaces
        .iter()
        .filter(|s| s.kind == SurfaceKind::Sidebar)
        .flat_map(|s| s.tabs.iter())
        .any(|t| es_diente_vivo(&t.content.kind))
}

/// Los nombres de contenido que marcan un diente **vivo** (el control center con
/// árbitro de atención).
pub fn es_diente_vivo(kind: &str) -> bool {
    matches!(kind, "control" | "vivo")
}

/// Los nombres de contenido que marcan el diente **monitor de sistema** (CPU/RAM/
/// cores).
pub fn es_monitor(kind: &str) -> bool {
    matches!(kind, "monitor" | "sistema" | "system" | "sysmon")
}

/// Los nombres de contenido que marcan el diente **«Flota»** (inventario matilda).
pub fn es_flota(kind: &str) -> bool {
    matches!(kind, "flota" | "fleet" | "matilda")
}

/// Los nombres de contenido que marcan el diente **«Unidades»** (sandokan).
pub fn es_unidades(kind: &str) -> bool {
    matches!(kind, "unidades" | "units" | "sandokan")
}

/// Los nombres de contenido que marcan el diente **«Actividad»** (timeline willay:
/// notificaciones/capturas/clipboard/checkpoints, cronológico). Distinto de
/// «eventos» (que es el RAG semántico sobre willay).
pub fn es_actividad(kind: &str) -> bool {
    matches!(kind, "actividad" | "activity" | "timeline" | "willay")
}

/// `true` si la config declara algún diente **«Actividad»** (para arrancar el
/// hilo de willay sólo cuando hace falta).
pub fn config_tiene_actividad(cfg: &Config) -> bool {
    cfg.surfaces
        .iter()
        .filter(|s| s.kind == SurfaceKind::Sidebar)
        .flat_map(|s| s.tabs.iter())
        .any(|t| es_actividad(&t.content.kind))
}

/// Los nombres de contenido que marcan un diente **perfil** cuyo panel es un
/// sidebar de instancias. Hoy sólo `pacha` (contextos de usuario); a medida que
/// se sumen perfiles (navegador, config del SO…) se amplía este conjunto.
pub fn es_pacha(kind: &str) -> bool {
    matches!(kind, "pacha" | "perfil" | "contexto" | "contextos")
}

/// Los nombres de contenido que marcan el diente de **sistema/sesión** (el del
/// footer del rail): info de usuario+sistema, cambio de contexto (pacha), wawa-panel
/// y las acciones de energía (cerrar sesión / reiniciar / apagar).
pub fn es_sesion(kind: &str) -> bool {
    matches!(kind, "sesion" | "session" | "power" | "energia" | "apagar")
}

/// `true` si la config declara un diente «Unidades» (sólo entonces se arranca el
/// feed del plano de control).
pub fn config_tiene_unidades(cfg: &Config) -> bool {
    cfg.surfaces
        .iter()
        .filter(|s| s.kind == SurfaceKind::Sidebar)
        .flat_map(|s| s.tabs.iter())
        .any(|t| es_unidades(&t.content.kind))
}

/// El path del inventario de flota: `$XDG_CONFIG_HOME/tawasuyu/flota/inventory.json`
/// (o `~/.config/...`). Read-only; matilda lo escribe del lado de shuma.
pub fn flota_inventory_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("tawasuyu/flota/inventory.json")
}

/// Carga el inventario de flota del path default (JSON via serde). `None` si no
/// existe o no parsea — el panel muestra un aviso.
pub fn load_flota() -> Option<matilda_core::Inventory> {
    let text = std::fs::read_to_string(flota_inventory_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Suma las cuentas SSH marcadas **automáticas** (crate `cuentas`) al inventario
/// de flota como hosts, para que pata las vigile por SSH igual que a las del
/// inventario declarado — «como si fueran locales». Si no hay automáticas devuelve
/// `flota` tal cual; si las hay pero no había inventario, crea uno con ellas.
/// Idempotente: no pisa un host del inventario que ya use ese nombre.
pub fn merge_cuentas_automaticas(flota: Option<matilda_core::Inventory>) -> Option<matilda_core::Inventory> {
    sumar_ssh_automaticas(flota, &cuentas::CuentasSsh::load())
}

/// Parte pura de [`merge_cuentas_automaticas`]: suma las cuentas automáticas de
/// `ssh` al inventario. Separada del loader para poder testearla sin tocar disco.
fn sumar_ssh_automaticas(
    flota: Option<matilda_core::Inventory>,
    ssh: &cuentas::CuentasSsh,
) -> Option<matilda_core::Inventory> {
    if ssh.automaticas().next().is_none() {
        return flota;
    }
    let mut inv = flota.unwrap_or_default();
    for c in ssh.automaticas() {
        if c.host.trim().is_empty() || inv.host(&c.id).is_some() {
            continue;
        }
        let mut h = matilda_core::Host::new(&c.id, &c.host);
        if !c.user.trim().is_empty() {
            h = h.with_user(&c.user);
        }
        h = h.with_port(c.port);
        for t in &c.tags {
            h = h.with_tag(t);
        }
        inv.add_host(h);
    }
    Some(inv)
}

/// `true` si la config declara un diente «Flota».
pub fn config_tiene_flota(cfg: &Config) -> bool {
    cfg.surfaces
        .iter()
        .filter(|s| s.kind == SurfaceKind::Sidebar)
        .flat_map(|s| s.tabs.iter())
        .any(|t| es_flota(&t.content.kind))
}

/// `true` si la config declara algún diente **animado** (vivo o monitor): ambos
/// pintan un canvas vivo en el rail y necesitan el latido [`Msg::DienteTick`].
pub fn config_tiene_diente_animado(cfg: &Config) -> bool {
    cfg.surfaces
        .iter()
        .filter(|s| s.kind == SurfaceKind::Sidebar)
        .flat_map(|s| s.tabs.iter())
        .any(|t| {
            es_diente_vivo(&t.content.kind)
                || es_monitor(&t.content.kind)
                || es_unidades(&t.content.kind)
        })
}

/// Dispara el transitorio de volumen en el diente vivo y re-resuelve su
/// manifestación, para respuesta inmediata al subir/bajar/silenciar (sin esperar
/// al muestreo de 1 Hz). Inocuo si la config no declara un diente vivo.
fn flash_volumen_diente(model: &mut Model, frac: f32, muted: bool) {
    use pata_core::atencion::{Manifestacion, VOLUMEN_TTL};
    model
        .atencion
        .flash(Manifestacion::Volumen { frac, muted }, VOLUMEN_TTL, model.diente_t);
    let s = model.senales_diente();
    model.diente_manifest = model.atencion.resolver(s, model.diente_t);
}

/// Arranca la **captura de voz** del micrófono para el input hospedado. Abre el
/// mic (cpal) vía `rimay-voz-host` con VAD por energía y el STT **mock** por
/// default: cualquier utterance real despierta (`Despierto`) y dicta
/// (`Dictando`), re-durmiéndose en silencio (`Esperando`) — así se ven las
/// animaciones de escucha sin daemon ni nube. La guardia + el runtime viven en
/// el `Model` (dropearlos apaga el mic). Para backends reales, cambiar el
/// `VozConfig` (ver el comentario interno).
fn iniciar_voz(model: &mut Model, handle: &Handle<Msg>) {
    use shuma_voz_ui::EstadoEscucha;
    // Config: mock por default → cualquier utterance despierta/dicta con el mic
    // real, sin daemon ni nube. Para backends configurables:
    //   let voz = wawa_config::WawaConfig::load().ai.voz;  // (cuando exista)
    //   let vcfg = rimay_voz::VozConfig {
    //       stt: rimay_voz::Backend::parse(&voz.stt),
    //       tts: rimay_voz::Backend::parse(&voz.tts),
    //       socket: None,
    //   };
    let vcfg = rimay_voz::VozConfig::default();
    let opciones = rimay_voz_host::OpcionesEscucha::default(); // llamado "shuma", sin wake-gate

    // Runtime propio: la captura spawnea tasks tokio; el bucle Elm no es tokio.
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            model.shuma.inner.fijar_escucha(EstadoEscucha::Apagado);
            eprintln!("voz: no se pudo crear el runtime: {e}");
            return;
        }
    };
    let arranque = {
        let _g = rt.enter();
        rimay_voz_host::escuchar_cfg(vcfg, opciones)
    };
    match arranque {
        Ok((guardia, mut rx)) => {
            // Muestra "armado" ya, antes del primer evento del VAD.
            model.shuma.inner.fijar_escucha(EstadoEscucha::Esperando);
            let h = handle.clone();
            rt.spawn(async move {
                while let Some(ev) = rx.recv().await {
                    h.dispatch(Msg::VozEvento(ev));
                }
            });
            model.voz_guardia = Some(guardia);
            model.voz_rt = Some(rt);
            eprintln!("voz: 🎙 escuchando — di «shuma»");
        }
        Err(e) => {
            model.shuma.inner.fijar_escucha(EstadoEscucha::Apagado);
            eprintln!("voz: no se pudo abrir el micrófono: {e}");
        }
    }
}

/// Para la captura de voz: dropea la guardia (para el mic + aborta las tasks) y
/// su runtime, y apaga el indicador de escucha del input.
fn parar_voz(model: &mut Model) {
    model.voz_guardia = None; // Drop: para el mic + tasks
    model.voz_rt = None; // Drop: cierra el runtime
    model.shuma.inner.fijar_escucha(shuma_voz_ui::EstadoEscucha::Apagado);
}

/// Duración (segundos) del fundido de opacidad de los controles fantasma: cuánto
/// tarda `fantasmas_alpha` en ir de 0 a 1 (aparecer) o de 1 a 0 (esfumarse).
pub const FANT_FADE_SEC: f32 = 0.30;
/// Retardo (µs) tras el hover-out antes de empezar a esfumarse — el «demorá un
/// poco» del pedido: quedan revelados un instante más antes del fundido.
pub const FANT_LINGER_US: u64 = 550_000;

/// Avanza la opacidad animada de los controles fantasma hacia su objetivo (1 si
/// el puntero está en la zona o aún dentro del retardo `hasta`, si no 0), a
/// razón de `1/FANT_FADE_SEC` por segundo. Es puro y compartido por ambos
/// backends. Devuelve `true` si sigue animando (para pedir otro frame).
///
/// El `dt` sale del reloj (no del frame-rate), así el fundido dura lo mismo con
/// repintado a 30 Hz (barra activa) o a saltos (ventana en reposo).
pub fn avanzar_fantasmas(
    alpha: &mut f32,
    hover: bool,
    hasta: u64,
    reloj: &mut u64,
    ahora_us: u64,
) -> bool {
    let objetivo = if hover || ahora_us < hasta { 1.0 } else { 0.0 };
    let dt = ahora_us.saturating_sub(*reloj) as f32 / 1_000_000.0;
    *reloj = ahora_us;
    let paso = (dt / FANT_FADE_SEC).clamp(0.0, 1.0);
    if (*alpha - objetivo).abs() <= paso {
        *alpha = objetivo;
    } else if *alpha < objetivo {
        *alpha += paso;
    } else {
        *alpha -= paso;
    }
    (*alpha - objetivo).abs() > 0.001
}

/// Estampa el **snapshot congelado** de los fugaces si no hay uno vigente (lo
/// llaman los handlers de `RevealFantasmas`/`FantasmaPin` del path winit; el
/// layer-shell tiene su gemelo en `app_impl`). El `BarData` es **parcial**:
/// sólo los campos que miran los candidatos fugaces — el resto en default.
fn estampar_fugaz_fijo(model: &mut Model) {
    if model.fugaz_fijo.is_some() {
        return;
    }
    let now = willay_emit::ahora_usec();
    let freeze = {
        let data = render::BarData {
            weather: model.weather_now.as_ref(),
            network: model.network_now.as_ref(),
            media: model.media_now.as_ref(),
            cava: &model.cava_frame,
            anim_t: model.diente_t as f32,
            matilda: model.matilda_salud.as_ref(),
            cpu: model.last_ctx.cpu,
            cpu_cores: &model.last_ctx.cpu_cores
                [..(model.last_ctx.cpu_cores_n as usize).min(model.last_ctx.cpu_cores.len())],
            cpu_temp: model.cpu_temp,
            bat: model.bat_now,
            bat_evento: now < model.bat_evento_hasta,
            fugaz_uso: Some(&model.fugaz_uso),
            volume: model.last_ctx.volume,
            muted: model.last_ctx.muted,
            moon_phase: model.last_ctx.moon_phase,
            sun_longitude: model.last_ctx.sun_longitude_deg,
            cielo: model.cielo_now.as_ref(),
            khipu: Some(&model.khipu_snapshot),
            tampu: model.tampu_now.as_ref(),
            usb: model.usb_now.as_ref(),
            brightness: model.last_ctx.brightness,
            vol_evento: (now < model.vol_evento_hasta).then_some(model.vol_subiendo),
            net_trafico: model.red_trafico,
            fugaz_idx: model.fugaz_idx,
            fugaz_pin: model.fugaz_pin,
            ..Default::default()
        };
        shuma::congelar_fugaces(&data, &model.theme, now)
    };
    model.fugaz_fijo = Some(freeze);
}

#[cfg(test)]
mod tests_fantasmas {
    use super::{avanzar_fantasmas, FANT_FADE_SEC, FANT_LINGER_US};

    /// Con el puntero encima, la opacidad sube hacia 1 (aparición).
    #[test]
    fn hover_sube_hacia_uno() {
        let (mut a, mut reloj) = (0.0_f32, 0);
        // Medio segundo de dt: paso = 0.5/FADE (>1 → llega a 1) — pero avanzamos
        // en tramos chicos para ver la subida monótona.
        let dt = (FANT_FADE_SEC * 1_000_000.0 / 4.0) as u64; // ~1/4 del fade
        let mut t = 0u64;
        let mut prev = a;
        for _ in 0..3 {
            t += dt;
            avanzar_fantasmas(&mut a, true, 0, &mut reloj, t);
            assert!(a >= prev, "la opacidad no debe bajar con hover");
            prev = a;
        }
        assert!(a > 0.5, "tras 3/4 del fade ya debería ir subida: {a}");
    }

    /// Dentro del retardo tras el hover-out, sigue en 1 (no se esfuma aún).
    #[test]
    fn retardo_sostiene_antes_de_esfumar() {
        let (mut a, mut reloj) = (1.0_f32, 0);
        let hasta = FANT_LINGER_US; // vigente hasta el retardo
        // A mitad del retardo: objetivo sigue 1 → no baja.
        let ok = avanzar_fantasmas(&mut a, false, hasta, &mut reloj, FANT_LINGER_US / 2);
        assert_eq!(a, 1.0, "no debe esfumarse dentro del retardo");
        assert!(!ok, "en objetivo → no sigue animando");
    }

    /// Pasado el retardo, baja hacia 0 (esfumado).
    #[test]
    fn pasado_el_retardo_baja_a_cero() {
        let (mut a, mut reloj) = (1.0_f32, FANT_LINGER_US);
        let t = FANT_LINGER_US + (FANT_FADE_SEC * 1_000_000.0 / 3.0) as u64;
        avanzar_fantasmas(&mut a, false, FANT_LINGER_US, &mut reloj, t);
        assert!(a < 1.0 && a >= 0.0, "debería estar bajando: {a}");
    }
}

/// `true` si la config declara al menos un `SurfaceKind::Sidebar` con un diente
/// cuyo contenido es el panel RAG (`kind = "rag"`/`"search"`). Sólo entonces se
/// arma el motor RAG (lectura de la caché de paloma + daemon + LLM).
pub fn config_tiene_rag(cfg: &Config) -> bool {
    cfg.surfaces
        .iter()
        .filter(|s| s.kind == SurfaceKind::Sidebar)
        .flat_map(|s| s.tabs.iter())
        .any(|t| rag::is_rag_kind(&t.content.kind))
}

/// El corpus que monta el primer diente RAG: el prop `source` de su contenido
/// (`"willay"` = centro de eventos, default `"paloma"` = correo). Vacío si no hay
/// diente RAG.
pub fn rag_source(cfg: &Config) -> String {
    cfg.surfaces
        .iter()
        .filter(|s| s.kind == SurfaceKind::Sidebar)
        .flat_map(|s| s.tabs.iter())
        .find(|t| rag::is_rag_kind(&t.content.kind))
        .map(|t| t.content.str_prop("source", "paloma").to_string())
        .unwrap_or_default()
}

/// `true` si el diente abierto del sidebar es un panel RAG (su contenido es
/// `rag`/`search`). El teclado del panel se rutea sólo entonces.
fn rag_panel_open(model: &Model) -> bool {
    model.nav.open.values().any(|&(si, ti)| {
        model
            .cfg
            .surfaces
            .get(si)
            .and_then(|s| s.tabs.get(ti))
            .map(|t| rag::is_rag_kind(&t.content.kind))
            .unwrap_or(false)
    })
}

/// Los widgets vivos de una superficie, repartidos por slot.
pub struct SurfaceWidgets {
    /// Slot inicial (izquierda / arriba).
    pub start: Vec<SlotWidget>,
    /// Slot central.
    pub center: Vec<SlotWidget>,
    /// Slot final (derecha / abajo).
    pub end: Vec<SlotWidget>,
}

impl SurfaceWidgets {
    /// Itera los widgets de core de la superficie (los que se `tick`ean).
    fn core_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn Widget>> {
        self.start
            .iter_mut()
            .chain(self.center.iter_mut())
            .chain(self.end.iter_mut())
            .filter_map(|sw| match sw {
                SlotWidget::Core { widget, .. } => Some(widget),
                SlotWidget::Start { .. }
                | SlotWidget::Shuma
                | SlotWidget::Marquesina
                | SlotWidget::ClockBig
                | SlotWidget::WindowList
                | SlotWidget::Clipboard { .. }
                | SlotWidget::Tray
                | SlotWidget::Weather { .. }
                | SlotWidget::Cava
                | SlotWidget::FrontPanel
                | SlotWidget::Control
                | SlotWidget::Network
                | SlotWidget::Session
                | SlotWidget::Media
                | SlotWidget::Bluetooth
                | SlotWidget::Notifications => None,
            })
    }
}

/// El estado de la app: config + geometría resuelta + widgets vivos + sampler.
pub struct Model {
    /// Paleta de Llimphi.
    pub theme: Theme,
    /// El marco declarado.
    pub cfg: Config,
    /// La geometría resuelta sobre la pantalla.
    pub frame: Frame,
    /// Widgets vivos, en el mismo orden que `cfg.surfaces`.
    pub surfaces: Vec<SurfaceWidgets>,
    /// Tarjetas flotantes (estilo conky) de las superficies `Panel`, cada una con
    /// sus widgets vivos. En layer-shell cada tarjeta es su propia surface; en el
    /// path winit se pintan en absoluto sobre la ventana única.
    pub cards: Vec<(FloatingCard, Vec<Box<dyn Widget>>)>,
    /// Estado del cabezal del shell y su drawer Quake.
    pub shuma: ShumaState,
    /// La **shuma COMPLETA** hospedada (Model de `shuma-shell-llimphi` con
    /// dientes/sesiones), presente sólo con el live-wire activo
    /// ([`shuma_full_enabled`]) y si hay `shuma_input` declarado. Cuando está,
    /// es la fuente de verdad del drawer; el módulo bare (`shuma.inner`) queda
    /// inerte. `None` = path bare por defecto (cero regresión).
    pub shuma_full: Option<shuma_app::Model>,
    /// Estado del drawer del front universal de nahual (módulo hospedado).
    pub nahual: NahualState,
    /// Registro de apps para el menú del botón de inicio.
    pub registry: app_bus::AppRegistry,
    /// `true` cuando el menú de inicio está desplegado.
    pub menu_open: bool,
    /// Texto del buscador del menú de inicio (filtra apps por label).
    pub menu_query: String,
    /// Desplazamiento de la lista del menú (px).
    pub menu_scroll: f32,
    /// Estilo visual del menú de inicio (alternable con right-click sobre
    /// el botón). Default `Classic`. Ver [`MenuStyle`].
    pub menu_style: MenuStyle,
    /// Muestreador del sistema (con estado para el delta de CPU).
    pub sampler: Sampler,
    /// Texto del portapapeles (una línea), para el widget `clipboard`. Se
    /// re-muestrea cada tick vía `wl-paste`.
    pub clipboard: Option<String>,
    /// Historial de copias (más reciente al frente, sin repetidos). Lo alimenta
    /// cada tick desde `clipboard`; el popup lo lista. Es el espejo en memoria
    /// del `clip_store` persistente (para pintar rápido).
    pub clip_history: Vec<String>,
    /// Historial de portapapeles PERSISTENTE (Klipper: sobrevive al relogin,
    /// [`pata_portapapeles`]). `None` si el store no pudo abrir (otro pata lo
    /// tiene tomado, disco RO…): entonces sólo vive el ring en memoria de arriba.
    pub clip_store: Option<pata_portapapeles::Historial>,
    /// `true` cuando el popup del historial del portapapeles está desplegado.
    pub clip_open: bool,
    /// `true` cuando el control panel (quick settings) está desplegado.
    pub control_open: bool,
    /// Lecturas del sistema para el control panel (batería, radios), refrescadas
    /// al abrirlo. Volumen/brillo salen del `last_ctx` del sampler.
    pub control_extras: render::ControlExtras,
    /// `true` cuando el panel del reloj (fijar fecha/hora) está desplegado.
    pub clock_open: bool,
    /// `true` cuando el diálogo **Cielo** (fantasmas astrales) está desplegado.
    pub cielo_open: bool,
    /// Store soberano de **khipu** (captura rápida de notas que se desvanecen).
    pub khipu: khipu::KhipuStore,
    /// Snapshot de khipu (notas visibles + salience), refrescado cada tick.
    pub khipu_snapshot: khipu::KhipuSnapshot,
    /// `true` cuando el diálogo **Khipu** está desplegado.
    pub khipu_open: bool,
    /// Borrador de la nota que se está tecleando (`Some` mientras el diálogo Khipu
    /// captura el teclado). `None` = no se está anotando.
    pub khipu_input: Option<String>,
    /// Borrador de fecha/hora que el panel del reloj edita.
    pub clock_draft: ClockDraft,
    /// `true` cuando la ventanita de CPU está desplegada.
    pub cpu_open: bool,
    /// `true` cuando la ventanita de RAM está desplegada.
    pub ram_open: bool,
    /// `true` cuando la ventanita de volumen está desplegada.
    pub volume_open: bool,
    /// Corrientes de audio por app (sink-inputs) para el mezclador. Se muestrean
    /// al abrir la ventanita de volumen y cada tick mientras está abierta.
    pub sink_inputs: Vec<sampler::SinkInput>,
    /// Dispositivos de salida (sinks) para el selector de salida. Se muestrean
    /// junto con `sink_inputs` mientras la ventanita de volumen está abierta.
    pub sinks: Vec<sampler::Sink>,
    /// Corrientes de **grabación** por app (source-outputs) para el mezclador.
    pub source_outputs: Vec<sampler::SourceOutput>,
    /// Dispositivos de **entrada** (micrófonos/sources) para el selector de entrada.
    pub sources: Vec<sampler::Source>,
    /// Pestaña activa del mezclador (reproducción/grabación/salida/entrada).
    pub volume_tab: VolumeTab,
    /// `true` cuando la ventanita de brillo está desplegada.
    pub brightness_open: bool,
    /// Último snapshot del sistema — cacheado para alimentar las ventanitas
    /// (porcentajes en vivo, lista de cores) sin volver a llamar al sampler.
    pub last_ctx: pata_core::widget::WidgetCtx,
    /// La bandeja del sistema, corriendo en su propio hilo. `None` si la config no
    /// declara ningún widget `tray`.
    pub tray: Option<TrayHandle>,
    /// Feed de clima en su propio hilo. `None` si la config no declara `weather`.
    pub weather: Option<weather::WeatherHandle>,
    /// Última lectura del clima (se refresca con `latest()` cada tick).
    pub weather_now: Option<weather::Weather>,
    /// Efemérides ricas del cielo (cosmos), en su propio hilo lento. Siempre
    /// presente: la luna y los eclipses son globales, salen sin ubicación.
    pub cielo: Option<cielo::CieloHandle>,
    /// Último estado del cielo (se refresca con `latest()` cada tick).
    pub cielo_now: Option<cielo::CieloState>,
    /// Ubicación activa compartida con el hilo del cielo `(lat, lon)`. La siembra
    /// la localidad activa de la config; si es automática, la puebla el clima al
    /// resolverla por IP. La comparten clima↔cielo (misma ubicación para ambos).
    pub cielo_loc: cielo::LugarCompartido,
    /// El común (tampu) en su hilo lento, si el almacén ya existe. `None` = no se
    /// usa el común (sin fantasma).
    pub tampu: Option<tampu::TampuHandle>,
    /// Último snapshot del común (objetos que te involucran + vencidos/anomalías).
    pub tampu_now: Option<tampu::TampuSnapshot>,
    /// `true` cuando el diálogo del **común** está desplegado.
    pub tampu_open: bool,
    /// Vigía de medios extraíbles (USB) en su hilo lento, si hay `lsblk`.
    pub usb: Option<usb::UsbHandle>,
    /// Último snapshot de extraíbles (particiones + si hay alguna sin montar).
    pub usb_now: Option<usb::UsbSnapshot>,
    /// `true` cuando el diálogo de **medios extraíbles** está desplegado.
    pub usb_open: bool,
    /// Vigía de la **red de confianza** (ágora) en su hilo lento, si ágora está
    /// en uso. Sólo lectura del grafo + libreta en disco.
    pub agora: Option<agora::AgoraHandle>,
    /// Último snapshot de la red de confianza (resumen + revocaciones).
    pub agora_now: Option<agora::AgoraSnapshot>,
    /// `true` cuando el diálogo **«¿Le creo?»** está desplegado.
    pub agora_open: bool,
    /// Centro de actividad (willay) en su hilo lento — para el diente «Actividad».
    pub willay: Option<willay::WillayHandle>,
    /// Último snapshot del timeline de actividad.
    pub willay_now: Option<willay::WillaySnapshot>,
    /// `true` cuando el menú de **captura de pantalla** está desplegado.
    pub captura_open: bool,
    /// Grabación de pantalla **en curso** (screencast), o `None` en reposo. Sostiene
    /// el proceso de wf-recorder; el ícono de captura muestra un punto rojo y el
    /// menú un cronómetro mientras está `Some`.
    pub grabacion: Option<grabacion::Grabacion>,
    /// Triage semántico de notificaciones en su propio hilo — alimenta la
    /// importancia de la marquesina del input. `None` si no hay `shuma_input`.
    pub triage: Option<triage::TriageHandle>,
    /// Último resumen del triage (aviso más importante), refrescado por tick.
    pub triage_now: Option<triage::TriageResumen>,
    /// Config de la **chakana** (PS1): reacciona a comandos/notifs + titila en
    /// reposo. Se lee al construir (wawa-panel; cambios en caliente = relanzar).
    pub chakana_cfg: wawa_config::ChakanaSettings,
    /// Feed de red en su propio hilo. `None` si la config no declara `network`.
    pub network: Option<network::NetworkHandle>,
    /// Última lectura de la red (se refresca con `latest()` cada tick).
    pub network_now: Option<network::NetState>,
    /// Feed MPRIS (reproductor) en su propio hilo. `None` si no hay `mpris`.
    pub mpris: Option<mpris::MprisHandle>,
    /// Último estado del reproductor (se refresca cada tick).
    pub media_now: Option<mpris::MediaState>,
    /// **Paisaje sonoro** del escritorio en su propio hilo (música ambiental de
    /// takiy según hora/contexto/actividad/ventanas). Siempre presente; arranca
    /// apagado y no toca el audio hasta encenderlo.
    pub paisaje: Option<paisaje::PaisajeHandle>,
    /// `true` si el usuario encendió el paisaje (gobierna el muestreo de ventanas
    /// aunque no haya widget `window_list`).
    pub paisaje_on: bool,
    /// Último estado observable del paisaje (encendido/sonando/rótulo del momento),
    /// refrescado cada tick — lo lee el fantasma de ánimo.
    pub paisaje_estado: paisaje::PaisajeEstado,
    /// Feed de Bluetooth en su propio hilo. `None` si no hay `bluetooth`.
    pub bluetooth: Option<bluetooth::BluetoothHandle>,
    /// Última lectura de Bluetooth (se refresca cada tick).
    pub bluetooth_now: Option<bluetooth::BtState>,
    /// `true` cuando el popup de Bluetooth está desplegado (path winit).
    pub bluetooth_open: bool,
    /// Campanita de notificaciones: cliente del daemon `pata-notify` en su hilo.
    /// `None` si la config no declara `notifications`.
    pub notifications: Option<notifications::NotificationsHandle>,
    /// `true` cuando el popup de notificaciones está desplegado (path winit).
    pub notifications_open: bool,
    /// Progreso agregado de acciones largas (copiar/mover) del daemon pata-notify,
    /// en su hilo, para la línea finísima a lo largo del input de la barra shell.
    pub progreso: Option<progreso::ProgresoHandle>,
    /// Agente de autenticación polkit en su propio hilo.
    pub polkit: Option<polkit::PolkitHandle>,
    /// Solicitud de autenticación polkit en curso (con el canal de respuesta).
    pub polkit_prompt: Option<polkit::PolkitRequest>,
    /// Contraseña tecleada en el diálogo de polkit.
    pub polkit_input: String,
    /// Peor nivel de batería ya avisado (0 ninguno, 1 bajo, 2 crítico). Ver
    /// [`bateria::decidir`].
    pub bat_avisado: u8,
    /// `true` cuando el popup del applet de red está desplegado (path winit).
    pub network_open: bool,
    /// Entrada de contraseña Wi-Fi en curso: `(ssid, tecleado)`. `None` = lista.
    pub net_password: Option<(String, String)>,
    /// `true` cuando el menú de sesión/energía está desplegado (path winit).
    pub session_open: bool,
    /// Acción de sesión pendiente de confirmación, o `None`.
    pub session_confirm: Option<SessionAction>,
    /// Acción disruptiva pendiente en la **pantalla de confirmación fullscreen**
    /// (apagar/reiniciar/cerrar sesión/cambiar contexto), o `None`. La pinta
    /// [`render::confirm_overlay_view`] como scrim traslúcido sobre todo.
    pub confirm_overlay: Option<ConfirmAccion>,
    /// Cartel OSD vigente (volumen/brillo), o `None`. Se desvanece solo.
    pub osd: Option<render::Osd>,
    /// Visualizador de audio (cava) en su propio hilo. `None` si la config no
    /// declara `cava`.
    pub cava: Option<cava::CavaHandle>,
    /// Último cuadro del visualizador (una fracción `0..1` por banda).
    pub cava_frame: Vec<f32>,
    /// Árbitro del **diente vivo**: decide qué muestra un diente multifuncional
    /// (música/volumen/CPU/batería/reposo). Inerte si la config no declara un
    /// diente de contenido `control`.
    pub atencion: pata_core::atencion::Atencion,
    /// Reloj monotónico (segundos) que avanza con el latido de animación del
    /// diente vivo — base temporal de los TTL del árbitro y de las animaciones.
    pub diente_t: f64,
    /// Última lectura de batería `(fracción 0..1, cargando)`, o `None` si no hay.
    pub bat_now: Option<(f32, bool)>,
    /// Última temperatura de CPU (°C), o `None` si no hay sensor. Muestreada a
    /// 1 Hz; el árbitro la usa para "CPU caliente" por sensor térmico.
    pub cpu_temp: Option<f32>,
    /// Manifestación actual que el rail pinta en el diente vivo.
    pub diente_manifest: pata_core::atencion::Manifestacion,
    /// Inventario de flota (matilda), read-only, para el diente «Flota». `None`
    /// si no hay inventario o la config no declara el diente.
    pub flota: Option<matilda_core::Inventory>,
    /// Discover remoto de la flota (SSH read-only) en su hilo. `None` si no hay
    /// inventario con hosts.
    pub flota_discover: Option<flota_discover::FlotaDiscoverHandle>,
    /// Último estado real observado por host (drift vs lo declarado).
    pub flota_remoto: Option<Vec<flota_discover::HostObs>>,
    /// Censo de presencia de los **equipos móviles** automáticos (tejido) en su
    /// hilo. `None` si no hay cuentas móviles automáticas.
    pub movil_discover: Option<movil_discover::MovilDiscoverHandle>,
    /// Última tanda de observaciones de presencia móvil (online/offline por equipo).
    pub movil_obs: Option<Vec<movil_discover::MovilObs>>,
    /// Muestreo runtime **local** de matilda (docker/systemd/nginx) en su hilo.
    /// `None` si la máquina no tiene nada monitoreable (sin docker ni nginx).
    pub matilda_local: Option<matilda_salud::MatildaLocalHandle>,
    /// Última foto runtime local (se refresca cada tick desde `matilda_local`).
    pub matilda_now: Option<matilda_discover::RuntimeState>,
    /// Salud combinada de la flota (local + remoto), recomputada cada tick. La
    /// consumen el control fantasma de shuma, la marquesina y el centro de control.
    pub matilda_salud: Option<matilda_salud::SaludFlota>,
    /// Feed de unidades del plano de control (sandokan) en su hilo. `None` si la
    /// config no declara un diente «Unidades».
    pub unidades: Option<unidades::UnidadesHandle>,
    /// Último snapshot de unidades (se refresca cada tick desde `unidades`).
    pub unidades_now: Option<sandokan_monitor_core::MonitorSnapshot>,
    /// Estado del sidebar navegador (Mónadas de nouser). Vacío si la config no
    /// declara ningún `SurfaceKind::Sidebar` con un navegador.
    pub nav: NavState,
    /// Estado del sidebar RAG (preguntale a tu correo). Inerte si la config no
    /// declara ningún diente con contenido `rag`/`search`.
    pub rag: RagState,
    /// Tamaño de la pantalla en píxeles.
    pub screen: (i32, i32),
    /// Ventanas abiertas para el `window_list`, en el backend winit. Se muestrean
    /// cada tick por `mirada-ctl windows --porcelain` (en layer-shell la lista
    /// sale de `wlr-foreign-toplevel` directo, ver [`crate::layer`]). Vacío si no
    /// hay compositor que responda.
    pub windows: Vec<crate::toplevel::WindowEntry>,
    /// Realce **optimista** del workspace switcher: `(target_1based, ticks)`.
    /// Al clickear una celda el realce salta al instante a `target` sin esperar
    /// el muestreo de ~1 s; se sostiene unos ticks por si un sample viejo aún
    /// reporta el escritorio anterior, y se suelta al confirmarse (o agotarse).
    /// Ver [`crate::sampler::reconcile_optimistic`]. `None` = sin salto en vuelo.
    pub pending_ws: Option<(u8, u8)>,
    /// Vigía del `launcher.toml`: cada tick comprueba si cambió en disco para
    /// recargar el marco en caliente (reordenar el dock, cambiar acento, etc.).
    pub cfg_watch: crate::config_watch::ConfigWatch,
    /// Guardia de la **captura de voz** del micrófono (Drop = para el mic +
    /// aborta las tasks). `None` = micrófono apagado. La emite `rimay-voz-host`.
    pub voz_guardia: Option<rimay_voz_host::GuardiaEscucha>,
    /// Runtime tokio dedicado de la captura de voz (el bucle Elm no es tokio;
    /// `escuchar_cfg` spawnea sus tasks aquí). Vive junto a la guardia; se dropea
    /// al apagar el micrófono.
    pub voz_rt: Option<tokio::runtime::Runtime>,
    /// `true` mientras el puntero está sobre la **zona de controles fantasma**
    /// (borde derecho del input). No se pinta directo: alimenta el objetivo de
    /// [`fantasmas_alpha`], que sube/baja animado.
    pub fantasmas_hover: bool,
    /// Reloj (µs) hasta el cual los controles fantasma siguen revelados tras el
    /// hover-out: el **retardo** para que no se esfumen apenas se va el puntero.
    /// Se fija en el `leave` a `ahora + FANT_LINGER_US`.
    pub fantasmas_hasta: u64,
    /// Opacidad **animada** `0..1` de los controles fantasma revelados. Sube a 1
    /// mientras hay hover (o dentro del retardo) y baja a 0 después, con fundido.
    pub fantasmas_alpha: f32,
    /// Reloj (µs) del último avance de [`fantasmas_alpha`] — base del `dt` del
    /// fundido, independiente de la tasa de repintado.
    pub fantasmas_reloj: u64,
    /// Turno rotativo de los fantasmas **leves** (con varios salientes se ve uno
    /// a la vez). Avanza cada [`shuma::FUGAZ_ROT_US`]; congelado en reveal/pin.
    pub fugaz_idx: usize,
    /// Reloj (µs) del último giro del turno de fantasmas leves.
    pub fugaz_reloj: u64,
    /// Fantasma **pinneado** por hover: no se oculta ni rota mientras el mouse
    /// esté encima, aunque su condición de salience caiga.
    pub fugaz_pin: Option<shuma::Fugaz>,
    /// Uso aprendido de los fantasmas (clicks persistidos en disco): fija el
    /// asiento de cada icono — más usado, más a la derecha.
    pub fugaz_uso: shuma::FugazUso,
    /// **Snapshot congelado** de los fugaces mientras la zona fantasma está
    /// activa (hover/reveal/pin): [`shuma::congelar_fugaces`] estampado al
    /// entrar el puntero (orden, split visibles, membresía, reloj de caras).
    /// Mientras viva, nada se recoloca bajo el mouse; se libera al esfumarse.
    pub fugaz_fijo: Option<shuma::FugazFreeze>,
    /// Reloj (µs) hasta el cual corre la **ventana de evento** de batería
    /// (enchufar/desenchufar): mientras corre, el fantasma de batería sale fijo.
    pub bat_evento_hasta: u64,
    /// Último volumen visto `(frac, muted)` — detecta el cambio que abre el
    /// acuse (rampa) del fantasma de sonido.
    pub vol_prev: Option<(f32, bool)>,
    /// Reloj (µs) hasta el cual corre el acuse de cambio de volumen.
    pub vol_evento_hasta: u64,
    /// `true` si el último cambio de volumen fue hacia arriba.
    pub vol_subiendo: bool,
    /// Última lectura acumulada de tráfico `(rx, tx, reloj_us)`.
    pub red_trafico_prev: Option<(u64, u64, u64)>,
    /// Tráfico instantáneo normalizado `(rx, tx)` para el fantasma de red.
    pub red_trafico: (f32, f32),
    /// El diálogo del **clima** (fantasma de clima) está abierto (path winit).
    pub clima_open: bool,
    /// Estado de la **marquesina** (grados de atención: urgente/transitorio/
    /// aviso/idle humano, con detección de cambios y fundidos).
    pub marquesina_est: marquesina::MarqEstado,
}

impl Model {
    /// Construye los widgets de cada superficie y el estado de shuma desde la
    /// config. El primer `shuma_input` que aparece define el cabezal.
    /// Recarga el `launcher.toml` y reconstruye el marco en caliente: geometría
    /// (`frame`), widgets de las superficies, tarjetas flotantes y acento del
    /// tema. **Preserva** el shell hospedado (`shuma`) y los hilos de fondo
    /// (tray/weather/cava) —agregar o quitar uno de esos widgets sigue pidiendo
    /// reinicio—. Cubre el caso típico: reordenar el dock / editar la barra.
    fn recargar_config(&mut self) {
        // TOML roto a mitad de una edición a mano: CONSERVÁ el marco actual en
        // vez de pisarlo con el preset (`try_load` diagnostica por stderr).
        let Some(cfg) = pata_config::try_load() else {
            return;
        };
        // El área de trabajo se reserva según el eje DOCKED (`sidebar_docked`),
        // no según la posición del rail (`dientes_outside`, que es visual). La
        // fuente de verdad es el tema/vista (`general`), no el global cross-app.
        let docked = cfg.general.sidebar_docked;
        self.frame = pata_core::resolve(
            &cfg,
            Rect::new(0, 0, self.screen.0, self.screen.1),
            docked,
        );
        self.surfaces = Self::construir_surfaces(&cfg);
        self.cards = Self::construir_cards(&cfg);
        let mut theme = Theme::dark();
        if let Some(c) = render::parse_hex(&cfg.general.accent) {
            theme.accent = c;
        }
        self.theme = theme;
        // El estilo del menú sigue al config recargado (lo cambió una vista).
        self.menu_style = MenuStyle::from_cfg(&cfg.general.menu_style);
        self.cfg = cfg;
    }

    fn construir(cfg: &Config) -> (Vec<SurfaceWidgets>, ShumaState) {
        // El shell hospedado lo define el primer `shuma_input` declarado (orden:
        // start→center→end por superficie). Se arma aparte de los widgets para
        // que el hot-reload pueda reconstruir el layout **sin** recrearlo.
        let shuma = cfg
            .surfaces
            .iter()
            .flat_map(|s| s.start.iter().chain(&s.center).chain(&s.end))
            .find(|spec| spec.kind == "shuma_input")
            .map(ShumaState::from_spec)
            .unwrap_or_default();
        (Self::construir_surfaces(cfg), shuma)
    }

    /// Construye sólo los widgets de cada superficie, **sin** tocar el shell
    /// hospedado ([`ShumaState`]). Lo usa el build inicial (vía [`construir`]) y
    /// el hot-reload, que reconstruye el dock al reordenar la config pero
    /// preserva el `ShumaState` vivo (su terminal no se reinicia). Pública
    /// también para los shots headless (`examples/*_shot.rs`), que necesitan
    /// una `SurfaceWidgets` real para pintar vistas de barra completas.
    pub fn construir_surfaces(cfg: &Config) -> Vec<SurfaceWidgets> {
        let build_slot = |specs: &[pata_core::WidgetSpec]| -> Vec<SlotWidget> {
            specs
                .iter()
                .map(|spec| {
                    if spec.kind == "start_button" {
                        let exec = spec.str_prop("exec", "");
                        SlotWidget::Start {
                            label: spec.str_prop("label", "⊞").to_string(),
                            exec: (!exec.is_empty()).then(|| exec.to_string()),
                        }
                    } else if spec.kind == "shuma_input" {
                        SlotWidget::Shuma
                    } else if spec.kind == "marquesina" {
                        SlotWidget::Marquesina
                    } else if spec.kind == "clock_big" {
                        SlotWidget::ClockBig
                    } else if spec.kind == "window_list" {
                        SlotWidget::WindowList
                    } else if spec.kind == "clipboard" {
                        let exec = spec.str_prop("exec", "");
                        SlotWidget::Clipboard {
                            exec: (!exec.is_empty()).then(|| exec.to_string()),
                        }
                    } else if spec.kind == "tray" {
                        SlotWidget::Tray
                    } else if spec.kind == "weather" {
                        let exec = spec.str_prop("exec", "");
                        SlotWidget::Weather {
                            exec: (!exec.is_empty()).then(|| exec.to_string()),
                        }
                    } else if spec.kind == "cava" {
                        SlotWidget::Cava
                    } else if spec.kind == "front_panel" {
                        SlotWidget::FrontPanel
                    } else if spec.kind == "control" {
                        SlotWidget::Control
                    } else if spec.kind == "network" || spec.kind == "wifi" {
                        SlotWidget::Network
                    } else if spec.kind == "session" || spec.kind == "power" {
                        SlotWidget::Session
                    } else if spec.kind == "mpris" || spec.kind == "media_player" {
                        SlotWidget::Media
                    } else if spec.kind == "bluetooth" || spec.kind == "bt" {
                        SlotWidget::Bluetooth
                    } else if spec.kind == "notifications" || spec.kind == "notify" {
                        SlotWidget::Notifications
                    } else {
                        let exec = spec.str_prop("exec", "");
                        SlotWidget::Core {
                            kind: spec.kind.clone(),
                            widget: build(spec),
                            exec: (!exec.is_empty()).then(|| exec.to_string()),
                            cells: spec.num_prop("cells", 0.0).max(0.0) as u32,
                        }
                    }
                })
                .collect()
        };
        cfg.surfaces
            .iter()
            .map(|s| SurfaceWidgets {
                start: build_slot(&s.start),
                center: build_slot(&s.center),
                end: build_slot(&s.end),
            })
            .collect()
    }

    /// Construye las tarjetas flotantes de todas las superficies `Panel` con sus
    /// widgets vivos. Compartido por el path winit ([`PataApp::init`]) y el
    /// layer-shell ([`crate::layer`]): el modelo se escribe una vez.
    pub fn construir_cards(cfg: &Config) -> Vec<(FloatingCard, Vec<Box<dyn Widget>>)> {
        cfg.surfaces
            .iter()
            .filter(|s| s.kind == SurfaceKind::Panel)
            .flat_map(|s| s.cards.iter())
            .map(|card| {
                let ws = card.widgets.iter().map(build).collect();
                (card.clone(), ws)
            })
            .collect()
    }

    /// `tick`ea todos los widgets de core (barras y tarjetas) con el contexto dado.
    fn tick_widgets(&mut self, ctx: &WidgetCtx) {
        for sw in &mut self.surfaces {
            for w in sw.core_mut() {
                w.tick(ctx);
            }
        }
        for (_, ws) in &mut self.cards {
            for w in ws {
                w.tick(ctx);
            }
        }
    }

    /// Arma las [`pata_core::atencion::Senales`] del diente vivo desde el estado
    /// actual: volumen/mute/CPU del último `WidgetCtx`, batería de `bat_now` y
    /// música de `media_now`.
    fn senales_diente(&self) -> pata_core::atencion::Senales {
        pata_core::atencion::Senales {
            volume: self.last_ctx.volume,
            muted: self.last_ctx.muted,
            cpu: self.last_ctx.cpu,
            cpu_temp: self.cpu_temp,
            bateria: self.bat_now.map(|(f, _)| f),
            cargando: self.bat_now.map(|(_, c)| c).unwrap_or(false),
            musica: self.media_now.as_ref().map(|m| m.playing).unwrap_or(false),
        }
    }

    /// Refresca la manifestación del diente vivo con las señales actuales.
    fn actualizar_diente(&mut self) {
        let s = self.senales_diente();
        self.diente_manifest = self.atencion.update(s, self.diente_t);
    }

    /// Arranca la animación del drawer hacia `destino` (0 = replegado, 1 =
    /// desplegado) y dispara el bucle de `ShumaAnim`.
    fn animar_shuma(&mut self, destino: f32, handle: &Handle<Msg>) {
        let desde = self.shuma.anim.value();
        self.shuma.anim = Tween::new(desde, destino, motion::FAST, motion::ease_out_cubic);
        animate(handle, motion::FAST, || Msg::ShumaAnim);
    }

    fn animar_nahual(&mut self, destino: f32, handle: &Handle<Msg>) {
        let desde = self.nahual.anim.value();
        self.nahual.anim = Tween::new(desde, destino, motion::FAST, motion::ease_out_cubic);
        animate(handle, motion::FAST, || Msg::NahualAnim);
    }
}

/// Estilos del menú de inicio. El default `Classic` es el panel a la
/// izquierda con buscador + lista filtrable (el que la app trae desde
/// el inicio). `XP` evoca el menú de Windows XP — banda superior con
/// usuario, dos columnas (pinned + programs), footer "Apagar". `Gnome`
/// imita Activities — overlay full-screen con grid de tiles y buscador
/// centrado. El usuario alterna estilos con click-derecho sobre el
/// botón de inicio (`Msg::StartStyleCycle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuStyle {
    /// Panel sobrio a la izquierda — el estilo default de pata.
    Classic,
    /// Windows XP — banda azul superior con usuario, dos columnas
    /// (pinned a la izquierda, "todos los programas" a la derecha),
    /// franja inferior con "Cerrar sesión" / "Apagar".
    Xp,
    /// GNOME Activities — overlay full-screen con grid de tiles
    /// centrado y buscador grande arriba. Sin chrome, full-bleed.
    Gnome,
}

impl Default for MenuStyle {
    fn default() -> Self {
        // Xp es el único skin con panel claro propio (Classic hereda `theme.bg_app`
        // y, con tema oscuro, sale negro lavado). Mejor default visual de fábrica.
        MenuStyle::Xp
    }
}

impl MenuStyle {
    /// El estilo desde el slug de config (`general.menu_style`): `"xp"`,
    /// `"grid"`/`"gnome"`/`"kickoff"`/`"activities"`, o lista (`"list"`/vacío).
    pub fn from_cfg(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "xp" | "windows" | "windows-xp" => MenuStyle::Xp,
            "grid" | "gnome" | "kickoff" | "activities" => MenuStyle::Gnome,
            _ => MenuStyle::Classic,
        }
    }

    /// Próximo estilo en la rotación (right-click ciclo).
    pub fn next(self) -> Self {
        match self {
            MenuStyle::Classic => MenuStyle::Xp,
            MenuStyle::Xp => MenuStyle::Gnome,
            MenuStyle::Gnome => MenuStyle::Classic,
        }
    }
}

/// Tamaño inicial de la ventana. Cuando mirada acople las superficies (Fase 8)
/// esto lo fijará el compositor; por ahora cubrimos un 1080p.
const PANTALLA: (i32, i32) = (1920, 1080);

/// La app Llimphi del marco.
pub struct PataApp;

impl App for PataApp {
    type Model = Model;
    type Msg = Msg;

    fn title() -> &'static str {
        "pata"
    }

    fn app_id() -> Option<&'static str> {
        Some("tawasuyu.pata")
    }

    fn initial_size() -> (u32, u32) {
        (PANTALLA.0 as u32, PANTALLA.1 as u32)
    }

    fn init(handle: &Handle<Msg>) -> Model {
        rimay_localize::init();
        let _ = rimay_localize::set_locale(&wawa_config::WawaConfig::load().lang);
        let cfg = pata_config::load();
        let rag_present = config_tiene_rag(&cfg);
        // Qué corpus monta el diente RAG (`source` del prop): "willay" = el centro
        // de eventos, cualquier otro (default "paloma") = el correo.
        let rag_src = rag_present.then(|| rag_source(&cfg)).unwrap_or_default();
        let screen = PANTALLA;
        let docked = cfg.general.sidebar_docked;
        let frame = pata_core::resolve(&cfg, Rect::new(0, 0, screen.0, screen.1), docked);
        let (surfaces, shuma) = Model::construir(&cfg);
        let cards = Model::construir_cards(&cfg);
        let mut sampler = Sampler::with_utc(usa_utc(&cfg));
        let ctx = sampler.sample();
        let clipboard = crate::sampler::leer_clipboard();
        // Historial de portapapeles persistente (best-effort). Carga el texto ya
        // guardado para que el historial sobreviva al relogin.
        let clip_store = abrir_clip_store();
        let clip_history: Vec<String> = clip_store
            .as_ref()
            .and_then(|h| h.listar().ok())
            .map(|entradas| {
                entradas
                    .into_iter()
                    .filter_map(|e| match e.contenido {
                        pata_portapapeles::Contenido::Texto(t) => Some(t),
                        pata_portapapeles::Contenido::Imagen { .. } => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let tray = config_tiene_widget(&cfg, "tray")
            .then(TrayHandle::spawn)
            .flatten();
        // Con `shuma_input` presente los fugaces necesitan clima/red/bt/
        // reproductor/cava aunque no haya widgets declarados (ver el layer).
        let fugaces = config_tiene_widget(&cfg, "shuma_input");
        let weather = (fugaces || config_tiene_widget(&cfg, "weather"))
            .then(|| weather::WeatherHandle::spawn(weather_place(&cfg)));
        // Cielo: ubicación activa de la config (o `None` = automática, que el
        // clima poblará por IP). El hilo corre siempre —la luna y los eclipses
        // salen sin ubicación— y relee este `Arc` cada ciclo.
        let cielo_loc: cielo::LugarCompartido =
            std::sync::Arc::new(std::sync::Mutex::new(cielo_loc_inicial(&cfg)));
        let cielo = Some(cielo::CieloHandle::spawn(cielo_loc.clone()));
        // El común (tampu): sólo si el almacén ya existe (no se lo crea a nadie).
        let tampu = tampu::TampuHandle::spawn();
        // Vigía de extraíbles (si hay lsblk).
        let usb = usb::UsbHandle::spawn();
        // Red de confianza (ágora): sólo si el directorio ya existe.
        let agora = agora::AgoraHandle::spawn();
        // Centro de actividad (willay): sólo si la config declara el diente.
        let willay = config_tiene_actividad(&cfg).then(willay::WillayHandle::spawn);
        // Triage de notificaciones: sólo si hay input de shell que muestre su
        // marquesina (si no, no hay dónde narrar).
        let triage =
            config_tiene_widget(&cfg, "shuma_input").then(triage::TriageHandle::spawn);
        let network = (fugaces
            || config_tiene_widget(&cfg, "network")
            || config_tiene_widget(&cfg, "wifi"))
        .then(network::NetworkHandle::spawn);
        let mpris = (fugaces
            || config_tiene_widget(&cfg, "mpris")
            || config_tiene_widget(&cfg, "media_player"))
        .then(mpris::MprisHandle::spawn);
        // El paisaje sonoro siempre está disponible (toggle del control center);
        // el hilo arranca ocioso y no abre el audio hasta que se enciende.
        let paisaje = Some(paisaje::PaisajeHandle::spawn());
        let bluetooth = (fugaces
            || config_tiene_widget(&cfg, "bluetooth")
            || config_tiene_widget(&cfg, "bt"))
        .then(bluetooth::BluetoothHandle::spawn);
        let notifications = (config_tiene_widget(&cfg, "notifications")
            || config_tiene_widget(&cfg, "notify"))
        .then(notifications::NotificationsHandle::spawn)
        .flatten();
        // La línea de progreso de la barra shell: hilo liviano que pollea el
        // agregado de pata-notify. Best-effort — sin daemon el hilo termina solo y
        // la línea nunca aparece. Se arranca siempre (la barra shell es el default).
        let progreso = progreso::ProgresoHandle::spawn();
        let cava = (fugaces || config_tiene_widget(&cfg, "cava"))
            .then(|| cava::CavaHandle::spawn(cava_bars(&cfg)));
        // Inventario del archivo (gated por el diente Flota) + las cuentas SSH
        // automáticas (siempre): pata las monitorea por SSH como si fueran locales.
        let flota = config_tiene_flota(&cfg).then(load_flota).flatten();
        let flota = merge_cuentas_automaticas(flota);
        let flota_discover = flota.as_ref().and_then(|inv| {
            let hosts: Vec<flota_discover::HostConn> = inv
                .hosts()
                .map(|h| flota_discover::HostConn {
                    name: h.name.clone(),
                    address: h.address.clone(),
                    user: h.ssh_user().to_string(),
                    port: h.ssh_port(),
                })
                .collect();
            let units: Vec<String> = inv.services().map(|s| s.unit.clone()).collect();
            (!hosts.is_empty())
                .then(|| flota_discover::FlotaDiscoverHandle::spawn(hosts, units))
        });
        // Censo de presencia de los equipos móviles «automáticos» del tejido: pata
        // los monitorea (online/offline) como si fueran locales, igual que los
        // servidores SSH de la flota. Inerte si no hay cuentas móviles automáticas.
        let movil_conns: Vec<movil_discover::MovilConn> = cuentas::CuentasMovil::load()
            .automaticas()
            .map(|c| movil_discover::MovilConn {
                id: c.id.clone(),
                label: c.display(),
                device_hex: c.device_hex.clone(),
            })
            .collect();
        let movil_discover = movil_discover::MovilDiscoverHandle::spawn(movil_conns);
        let unidades = config_tiene_unidades(&cfg).then(unidades::UnidadesHandle::spawn);
        // Monitoreo runtime local (docker/systemd/nginx): siempre que la máquina
        // sea monitoreable, sin depender de que la config declare un diente Flota
        // — el escritorio local se vigila igual (fantasma + marquesina).
        let matilda_local = matilda_salud::MatildaLocalHandle::spawn();

        let mut theme = Theme::dark();
        if let Some(c) = render::parse_hex(&cfg.general.accent) {
            theme.accent = c;
        }
        // El estilo del menú arranca del config (lo fija la vista); el
        // right-click sigue ciclándolo como override de sesión.
        let menu_style = MenuStyle::from_cfg(&cfg.general.menu_style);
        let mut model = Model {
            theme,
            cfg,
            frame,
            surfaces,
            cards,
            shuma,
            shuma_full: None,
            nahual: NahualState::default(),
            registry: app_bus::AppRegistry::with_defaults(),
            menu_open: false,
            menu_query: String::new(),
            menu_scroll: 0.0,
            menu_style,
            sampler,
            clipboard,
            clip_history,
            clip_store,
            clip_open: false,
            control_open: false,
            control_extras: render::ControlExtras::default(),
            clock_open: false,
            cielo_open: false,
            khipu: khipu::KhipuStore::open(),
            khipu_snapshot: khipu::KhipuSnapshot::default(),
            khipu_open: false,
            khipu_input: None,
            clock_draft: ClockDraft::default(),
            cpu_open: false,
            ram_open: false,
            volume_open: false,
            sink_inputs: Vec::new(),
            sinks: Vec::new(),
            source_outputs: Vec::new(),
            sources: Vec::new(),
            volume_tab: VolumeTab::default(),
            brightness_open: false,
            last_ctx: pata_core::widget::WidgetCtx::default(),
            tray,
            weather,
            weather_now: None,
            cielo,
            cielo_now: None,
            cielo_loc,
            tampu,
            tampu_now: None,
            tampu_open: false,
            usb,
            usb_now: None,
            usb_open: false,
            agora,
            agora_now: None,
            agora_open: false,
            willay,
            willay_now: None,
            captura_open: false,
            grabacion: None,
            triage,
            triage_now: None,
            chakana_cfg: wawa_config::WawaConfig::load().chakana,
            network,
            network_now: None,
            mpris,
            media_now: None,
            paisaje,
            paisaje_on: false,
            paisaje_estado: paisaje::PaisajeEstado::default(),
            bluetooth,
            bluetooth_now: None,
            bluetooth_open: false,
            notifications,
            notifications_open: false,
            progreso,
            polkit: polkit::PolkitHandle::spawn(),
            polkit_prompt: None,
            polkit_input: String::new(),
            bat_avisado: 0,
            network_open: false,
            net_password: None,
            session_open: false,
            session_confirm: None,
            confirm_overlay: None,
            osd: None,
            cava,
            cava_frame: Vec::new(),
            atencion: pata_core::atencion::Atencion::new(),
            diente_t: 0.0,
            bat_now: None,
            cpu_temp: None,
            diente_manifest: pata_core::atencion::Manifestacion::Reposo,
            flota,
            flota_discover,
            flota_remoto: None,
            movil_discover,
            movil_obs: None,
            matilda_local,
            matilda_now: None,
            matilda_salud: None,
            unidades,
            unidades_now: None,
            nav: NavState::default(),
            rag: if rag_present {
                rag::RagState::presente()
            } else {
                RagState::default()
            },
            screen,
            windows: Vec::new(),
            pending_ws: None,
            // Vigilamos el primer candidato (el que `save` escribe), exista o no:
            // así la PRIMERA aplicación de una vista —que crea launcher.toml— se
            // recarga en caliente igual (mtime None→Some dispara `changed`), no
            // sólo las siguientes.
            cfg_watch: crate::config_watch::ConfigWatch::new(
                pata_config::candidate_paths().into_iter().next(),
            ),
            voz_guardia: None,
            voz_rt: None,
            fantasmas_hover: false,
            fantasmas_hasta: 0,
            fantasmas_alpha: 0.0,
            fantasmas_reloj: 0,
            fugaz_idx: 0,
            fugaz_reloj: 0,
            fugaz_pin: None,
            fugaz_uso: shuma::FugazUso::open(),
            fugaz_fijo: None,
            bat_evento_hasta: 0,
            vol_prev: None,
            vol_evento_hasta: 0,
            vol_subiendo: true,
            red_trafico_prev: None,
            red_trafico: (0.0, 0.0),
            clima_open: false,
            marquesina_est: marquesina::MarqEstado::default(),
        };
        // Primer tick para que los widgets arranquen con datos.
        model.tick_widgets(&ctx);

        handle.spawn_periodic(Duration::from_secs(1), || Msg::Tick);
        // Live-wire de la shuma COMPLETA (opt-in): si está activo y la config
        // declara un `shuma_input`, construimos el Model entero y le enganchamos
        // sus efectos (ticks, watcher de config, rail, contenedores) al loop de
        // pata vía un handle lifteado. La shuma gestiona su propio latido —no
        // necesita el tick bare de abajo.
        if model.shuma.present && shuma_full_enabled() {
            let mut full = shuma_app::new();
            full.chromeless = true; // hospedada en el drawer: sin menubar/rails, sólo canvas
            shuma_app::wire_effects(&mut full, handle, lift_shuma);
            model.shuma_full = Some(full);
        } else if model.shuma.present {
            // Latido del shell hospedado (path bare): drena su salida (`Tick`
            // del módulo) a ~100 ms —igual que `shuma-shell-llimphi`—. El
            // `update` puro avanza runs y PTY/TUI sin bloquear.
            handle.spawn_periodic(Duration::from_millis(100), || {
                Msg::ShumaShell(shuma_module_shell::Msg::Tick)
            });
        }
        // Visualizador de audio: re-pinta a ~10 Hz, sólo si la config declara un
        // `cava`. Era 20 Hz, pero cada latido re-renderiza el panel entero (vello)
        // y obliga al compositor a recomponer: con la CPU cargada el compositor
        // se atrasa con los key-release y el teclado repite letras. 10 Hz alcanza
        // de sobra para barras de audio ambientales.
        if model.cava.is_some() {
            handle.spawn_periodic(Duration::from_millis(100), || Msg::CavaTick);
        }
        // Latido de los dientes animados (control vivo + monitor): re-resuelve y
        // re-pinta a ~10 Hz (misma razón que cava). Sólo si la config declara alguno.
        if config_tiene_diente_animado(&model.cfg) {
            handle.spawn_periodic(Duration::from_millis(100), || Msg::DienteTick);
        }
        // Plano de datos del sidebar: poll de Mónadas a nouser, sólo si la config
        // declara un navegador (no molestar al broker si no hace falta).
        if config_tiene_navigator(&model.cfg) {
            handle.dispatch(Msg::NavTick);
            handle.spawn_periodic(nouser::REFRESH_INTERVAL, || Msg::NavTick);
        }
        // Motor RAG: pesado (conecta al daemon de embeddings, lee la caché de
        // paloma, levanta un cliente LLM), así que se arma en un hilo aparte para
        // no demorar el arranque. El resultado se deja en el slot compartido y se
        // avisa al bucle con `RagEngineReady`. Sólo si la config declara un diente RAG.
        if rag_present {
            let slot = model.rag.engine.clone();
            let h = handle.clone();
            let source = rag_src;
            std::thread::spawn(move || {
                // La fuente se elige por el prop `source`: willay (eventos) o
                // paloma (correo, default). Ambos motores son `dyn RagMotor`.
                let engine: Option<Box<dyn rag_motor::RagMotor>> = match source.as_str() {
                    "willay" | "eventos" => willay_rag::Engine::try_build()
                        .map(|e| Box::new(e) as Box<dyn rag_motor::RagMotor>),
                    _ => paloma_rag::RagEngine::try_build()
                        .map(|e| Box::new(e) as Box<dyn rag_motor::RagMotor>),
                };
                let (ok, corpus) = match &engine {
                    Some(e) => (true, e.corpus_len()),
                    None => (false, 0),
                };
                if let Ok(mut g) = slot.lock() {
                    *g = engine;
                }
                h.dispatch(Msg::RagEngineReady { ok, corpus });
            });
        }
        model
    }

    fn update(mut model: Model, msg: Msg, handle: &Handle<Msg>) -> Model {
        match msg {
            Msg::Tick => {
                // Cosecha una grabación que murió sola (p. ej. wf-recorder no pudo
                // iniciar el encode): así el punto rojo no queda pegado.
                if model.grabacion.as_mut().is_some_and(|g| !g.vivo()) {
                    model.grabacion = None;
                }
                let mut ctx = model.sampler.sample();
                // Reconcilia el realce optimista del switcher: si hay un salto en
                // vuelo, sostiene el destino hasta que el muestreo lo confirme
                // (evita el parpadeo de un sample tomado antes de aplicarse).
                let (pending, active) =
                    sampler::reconcile_optimistic(model.pending_ws, ctx.active_workspace);
                model.pending_ws = pending;
                ctx.active_workspace = active;
                model.tick_widgets(&ctx);
                model.last_ctx = ctx;
                model.clipboard = crate::sampler::leer_clipboard();
                if push_clip_history(&mut model.clip_history, &model.clipboard) {
                    if let Some(t) = &model.clipboard {
                        // Persiste el clip nuevo (Klipper). Best-effort.
                        if let Some(store) = &model.clip_store {
                            let _ = store.empujar(
                                pata_portapapeles::Contenido::Texto(t.clone()),
                                willay_emit::ahora_usec(),
                            );
                        }
                        willay_emit::emitir_silencioso(&evento_clip(t, willay_emit::ahora_usec()));
                    }
                }
                if let Some(h) = &model.weather {
                    if let Some(w) = h.latest() {
                        // Ubicación automática: si el clima resolvió coords por IP
                        // y la config no fija una localidad, sembralas para el
                        // cielo (misma ubicación para clima y fantasmas astrales).
                        if cielo_loc_inicial(&model.cfg).is_none() {
                            if let Some((lat, lon)) = w.coords {
                                if let Ok(mut g) = model.cielo_loc.lock() {
                                    *g = Some((lat as f64, lon as f64));
                                }
                            }
                        }
                        model.weather_now = Some(w);
                    }
                }
                if let Some(h) = &mut model.cielo {
                    if let Some(st) = h.latest() {
                        model.cielo_now = Some(st.clone());
                    }
                }
                model.khipu_snapshot = model.khipu.snapshot(khipu::ahora_unix());
                if let Some(h) = &mut model.tampu {
                    if let Some(s) = h.latest() {
                        model.tampu_now = Some(s.clone());
                    }
                }
                if let Some(h) = &mut model.usb {
                    if let Some(s) = h.latest() {
                        model.usb_now = Some(s.clone());
                    }
                }
                if let Some(h) = &mut model.agora {
                    if let Some(s) = h.latest() {
                        model.agora_now = Some(s.clone());
                    }
                }
                if let Some(h) = &mut model.willay {
                    if let Some(s) = h.latest() {
                        model.willay_now = Some(s.clone());
                    }
                }
                if let Some(h) = &model.network {
                    if let Some(n) = h.latest() {
                        model.network_now = Some(n);
                    }
                }
                if let Some(h) = &model.mpris {
                    if let Some(m) = h.latest() {
                        model.media_now = Some(m);
                    }
                }
                if let Some(h) = &model.bluetooth {
                    if let Some(b) = h.latest() {
                        model.bluetooth_now = Some(b);
                    }
                }
                if let Some(h) = &model.unidades {
                    if let Some(s) = h.latest() {
                        model.unidades_now = Some(s);
                    }
                }
                if let Some(h) = &model.flota_discover {
                    if let Some(v) = h.latest() {
                        model.flota_remoto = Some(v);
                    }
                }
                if let Some(h) = &model.movil_discover {
                    if let Some(v) = h.latest() {
                        model.movil_obs = Some(v);
                    }
                }
                if let Some(h) = &model.matilda_local {
                    if let Some(rt) = h.latest() {
                        model.matilda_now = Some(rt);
                    }
                }
                // Salud combinada (local + remoto): la recomputamos cada tick para
                // que refleje tanto una foto local nueva como un discover remoto.
                model.matilda_salud = crate::matilda_salud::SaludFlota::compute(
                    model.matilda_now.as_ref(),
                    model.flota_remoto.as_deref(),
                );
                // Aviso de batería baja: lee /sys cada tick y avisa al cruzar
                // un umbral descargando (una sola vez por escalón).
                if let Some((pct, charging)) = bateria::read() {
                    let (nuevo, aviso) = bateria::decidir(pct, charging, model.bat_avisado);
                    model.bat_avisado = nuevo;
                    // Enchufar/desenchufar es un EVENTO: abre la ventana en que
                    // el fantasma de batería sale fijo como acuse del cable.
                    if bateria::transicion(model.bat_now.map(|(_, c)| c), charging) {
                        model.bat_evento_hasta =
                            willay_emit::ahora_usec() + bateria::EVENTO_US;
                    }
                    model.bat_now = Some((pct as f32 / 100.0, charging));
                    if let Some(a) = aviso {
                        bateria::avisar(a, pct);
                    }
                }
                model.cpu_temp = sampler::cpu_temp_celsius();
                // Acuse de cambio de volumen (rampa del fantasma de sonido).
                {
                    let ahora = (model.last_ctx.volume, model.last_ctx.muted);
                    if let Some((v0, m0)) = model.vol_prev {
                        if (ahora.0 - v0).abs() > 0.005 || ahora.1 != m0 {
                            model.vol_subiendo = ahora.0 >= v0;
                            model.vol_evento_hasta = willay_emit::ahora_usec() + 1_800_000;
                        }
                    }
                    model.vol_prev = Some(ahora);
                }
                // Tráfico de red para las microbarras del fantasma de red.
                if let Some((rx, tx)) = network::trafico_totales() {
                    let ahora_us = willay_emit::ahora_usec();
                    if let Some((rx0, tx0, t0)) = model.red_trafico_prev {
                        let dt = (ahora_us.saturating_sub(t0) as f64 / 1_000_000.0).max(0.1);
                        model.red_trafico = (
                            network::trafico_frac(rx.saturating_sub(rx0) as f64 / dt),
                            network::trafico_frac(tx.saturating_sub(tx0) as f64 / dt),
                        );
                    }
                    model.red_trafico_prev = Some((rx, tx, ahora_us));
                }
                // Triage semántico de notificaciones: importancia por significado
                // (no por palabras), sin discriminar "es correo". Se drena como
                // weather/network.
                if let Some(h) = &model.triage {
                    if let Some(r) = h.latest() {
                        model.triage_now = r;
                    }
                }
                // Marquesina en el input del shell hospedado (placeholder cuando el
                // input está vacío, en vez de «tipea un comando»): **cicla** entre
                // las fuentes de estado del escritorio —notificaciones (triage),
                // sistema, foco de ventana, audio sonando, portapapeles—, cambiando
                // de tarjeta cada ~4 s; una notificación urgente manda y titila. La
                // fase de titileo sale del reloj (~0.5 s). Ver `crate::marquesina`.
                let fase = (willay_emit::ahora_usec() / 500_000 % 2) as u8;
                let ahora_us = willay_emit::ahora_usec();
                let sistema =
                    crate::render::sys_alert(model.last_ctx.cpu, model.bat_now, model.cpu_temp);
                // Gravedad del aviso de sistema: CPU recaliente o batería crítica
                // descargando ⇒ cambio BRUSCO (tarjeta fija, sin fundido).
                let sistema_urgente = model
                    .cpu_temp
                    .map(|t| t >= crate::render::CPU_TEMP_ALERTA_C)
                    .unwrap_or(false)
                    || matches!(model.bat_now, Some((frac, false)) if frac <= 0.05);
                let flota_aviso = model.matilda_salud.as_ref().and_then(|s| s.resumen());
                let fuentes = crate::marquesina::Fuentes {
                    triage: model.triage_now.as_ref().map(|r| (r.texto.as_str(), r.urgencia)),
                    sistema: sistema.as_deref().map(|s| (s, sistema_urgente)),
                    flota: flota_aviso.as_deref(),
                    audio: model
                        .media_now
                        .as_ref()
                        .filter(|m| m.playing)
                        .map(|m| m.title.as_str()),
                    foco: Some(model.last_ctx.focused_title.as_str()),
                    clip: model.clipboard.as_deref(),
                    hora: model.last_ctx.clock.hour,
                };
                // «pensando…» del agente manda sobre la rotativa (paridad con
                // el layer): el spinner de claude no se ve en el panel.
                let ocupado = match model.shuma_full.as_ref() {
                    Some(full) => shuma_app::active_shell_state(full)
                        .is_some_and(|st| st.claude_ocupado),
                    None => model.shuma.inner.claude_ocupado,
                };
                let marq = if ocupado {
                    let mut m = shuma_module_shell::Marquesina::leve("pensando…");
                    m.icono = Some('✻');
                    m.icono_rgb = Some([0xB0, 0x7A, 0xE8]);
                    Some(m)
                } else {
                    crate::marquesina::rotativa(&fuentes, &mut model.marquesina_est, ahora_us)
                };
                // Como en el layer: en live-wire el input pintado es la sesión
                // activa del modelo full — la marquesina va ahí; en bare, al inner.
                if let Some(full) = model.shuma_full.as_mut() {
                    shuma_app::set_active_marquesina(full, marq, fase);
                } else {
                    model.shuma.inner.set_marquesina(marq, fase);
                }
                // El control center persistente necesita perfil de energía + luz
                // nocturna frescos (el flyout los leía sólo al abrirse).
                if config_tiene_diente_vivo(&model.cfg) {
                    let (pp, night) = render::read_power_night();
                    model.control_extras.power_profile = pp;
                    model.control_extras.night = night;
                }
                // Diente vivo: refresca su manifestación con las señales nuevas.
                model.actualizar_diente();
                // Fundido de los controles fantasma (esfumado tras el retardo).
                avanzar_fantasmas(
                    &mut model.fantasmas_alpha,
                    model.fantasmas_hover,
                    model.fantasmas_hasta,
                    &mut model.fantasmas_reloj,
                    willay_emit::ahora_usec(),
                );
                // Turno rotativo de los fantasmas leves (uno a la vez); se
                // congela mientras el reveal está activo o hay un pin.
                shuma::avanzar_fugaz_idx(
                    &mut model.fugaz_idx,
                    &mut model.fugaz_reloj,
                    willay_emit::ahora_usec(),
                    model.fantasmas_hover
                        || model.fantasmas_alpha > 0.01
                        || model.fugaz_pin.is_some(),
                );
                // Zona fantasma apagada del todo → libera el orden congelado:
                // el asiento aprendido por los clicks recién rige ahora, con
                // los iconos ya invisibles (nadie los ve saltar).
                if !model.fantasmas_hover
                    && model.fantasmas_alpha <= 0.01
                    && model.fugaz_pin.is_none()
                {
                    model.fugaz_fijo = None;
                }
                // Agente polkit: si llega una autenticación y no hay otra en
                // curso, abrimos el diálogo; si ya hay una, la nueva se rechaza.
                if let Some(h) = &model.polkit {
                    while let Some(req) = h.try_recv() {
                        if model.polkit_prompt.is_none() {
                            model.polkit_input.clear();
                            model.polkit_prompt = Some(req);
                        } else {
                            let _ = req.reply.send(None);
                        }
                    }
                }
                // Mezclador por app: refresca mientras el popup de volumen está
                // abierto (los sliders siguen al sistema en vivo).
                if model.volume_open {
                    model.sink_inputs = sampler::sample_sink_inputs();
                    model.sinks = sampler::sample_sinks();
                    model.source_outputs = sampler::sample_source_outputs();
                    model.sources = sampler::sample_sources();
                }
                // El OSD se desvanece al cumplir su tiempo.
                if model.osd.map(|o| o.expired()).unwrap_or(false) {
                    model.osd = None;
                }
                // Lista de ventanas para el task manager, las pestañas verticales del
                // rail (widget `window_tabs`) o el taskbar del navegador de escritorios
                // (`TabsSource::Workspaces`, el preset nativo): sólo si la config las
                // declara (no molestar al WM con un subproceso por tick de balde).
                if config_tiene_widget(&model.cfg, "window_list")
                    || config_tiene_widget(&model.cfg, "window_tabs")
                    || config_quiere_taskbar_ws(&model.cfg)
                    || model.paisaje_on
                {
                    model.windows = sampler::sample_windows();
                }
                // Alimenta el paisaje sonoro con el estado del escritorio (apps +
                // foco + media) que pata ya conoce por ser el shell — sin espiar
                // Wayland. El hilo decide con histéresis si regenerar/silenciar.
                if let Some(h) = &model.paisaje {
                    if model.paisaje_on {
                        let focus = model.windows.iter().find(|w| w.active).map(|w| w.app_id.clone());
                        let apps = model.windows.iter().map(|w| w.app_id.clone()).collect();
                        let media = model.media_now.as_ref().map(|m| m.playing).unwrap_or(false);
                        h.push_desktop(paisaje::DesktopSnapshot { apps, focus, media });
                    }
                    model.paisaje_estado = h.estado();
                    model.control_extras.paisaje = model.paisaje_estado.enabled;
                }
                // Hot-reload: si el launcher.toml cambió en disco, reconstruye el
                // dock/superficies (preservando el shell hospedado).
                if model.cfg_watch.changed() {
                    model.recargar_config();
                }
            }
            Msg::CavaTick => {
                if let Some(h) = &model.cava {
                    if let Some(frame) = h.latest() {
                        model.cava_frame = frame;
                    }
                }
            }
            Msg::DienteTick => {
                // Latido de animación del diente vivo (~20 Hz): avanza el reloj
                // monotónico, drena el visualizador (si hay) y re-resuelve la
                // manifestación para que los transitorios caduquen a tiempo.
                model.diente_t += 0.05;
                if let Some(h) = &model.cava {
                    if let Some(frame) = h.latest() {
                        model.cava_frame = frame;
                    }
                }
                let s = model.senales_diente();
                model.diente_manifest = model.atencion.resolver(s, model.diente_t);
            }
            Msg::Quit => handle.quit(),
            Msg::ShumaAutoClose => {
                // Deshover (gesto liviano, path ventana): repliega SÓLO el vistazo;
                // el modo firme se queda (estás leyendo la salida).
                if model.shuma.present
                    && model.shuma.open
                    && model.shuma.open_mode.cierra_por_gesto_liviano()
                {
                    model.shuma.open = false;
                    model.animar_shuma(0.0, handle);
                }
            }
            Msg::ShumaToggle => {
                if model.shuma.present {
                    model.shuma.open = !model.shuma.open;
                    let destino = if model.shuma.open { 1.0 } else { 0.0 };
                    // Toggle a mano: al abrir es un vistazo (Fugaz).
                    if model.shuma.open {
                        model.shuma.open_mode = shuma::OpenMode::Fugaz;
                    }
                    // A6 — al abrir el drawer estás mirando la salida: acusa el
                    // aviso de comando largo (apaga el punto ámbar del cabezal).
                    // En el path bare; con la shuma completa el aviso lo gestiona
                    // ella adentro (cada diente tiene su badge).
                    if model.shuma.open && model.shuma_full.is_none() {
                        model.shuma.inner.ack_long_alerts();
                    }
                    model.animar_shuma(destino, handle);
                }
            }
            Msg::TerminalSession(i) => {
                // Clic en un diente-sesión del rail: activa esa sesión en la shuma
                // completa y desplega el drawer directo en ella («abrir ese tab
                // desde el tab»). El SelectSession acusa la alerta larga adentro.
                if let Some(full) = model.shuma_full.take() {
                    model.shuma_full = Some(shuma_app::update(
                        full,
                        shuma_app::Msg::SelectSession(i),
                        handle,
                        lift_shuma,
                    ));
                }
                if model.shuma.present && !model.shuma.open {
                    model.shuma.open = true;
                    model.shuma.open_mode = shuma::OpenMode::Firme;
                    model.animar_shuma(1.0, handle);
                }
            }
            Msg::ShumaFull(m) => {
                // Las apps deben estar en la sesión activa ANTES de procesar la
                // tecla, para que ya la primera ofrezca candidatos-app (tier 0)
                // en el completado — espejo del asegurar_apps del path bare.
                let sin_apps = model
                    .shuma_full
                    .as_ref()
                    .and_then(shuma_app::active_shell_state)
                    .is_some_and(|s| s.apps.is_empty());
                if sin_apps {
                    let apps = apps_lanzables(&model.registry);
                    if let Some(full) = model.shuma_full.as_mut() {
                        shuma_app::asegurar_shell_apps(full, move || apps);
                    }
                }
                // Sólo el **enviar** de la sesión activa despliega el drawer para
                // ver la salida (espeja el open-al-Enter del bare); `FocusInput`
                // (hover/click en el input) enfoca sin expandir.
                let abrir = model.shuma.present
                    && !model.shuma.open
                    && shuma_app::msg_is_submit(&m);
                // Live-wire: reenviar a la shuma completa hospedada con el handle
                // del host lifteado (sus efectos async vuelven como `ShumaFull`).
                if let Some(full) = model.shuma_full.take() {
                    model.shuma_full = Some(shuma_app::update(full, m.0, handle, lift_shuma));
                }
                if abrir {
                    model.shuma.open = true;
                    model.shuma.open_mode = shuma::OpenMode::Firme; // enviar = firme
                    model.animar_shuma(1.0, handle);
                }
            }
            Msg::ShumaShell(m) => {
                // Sólo el **enviar** (o Enter) despliega el drawer: `FocusInput`
                // (hover/click en el input) enfoca sin expandir. La expansión va
                // atada a pedir salida. Idempotente si ya está abierto.
                let submitting = matches!(m, shuma_module_shell::Msg::Submit);
                // A6 — mientras el drawer está abierto, el usuario está mirando:
                // un comando largo que termina ahí no debe dejar badge stale al
                // plegar después. Lo acusamos en cada Tick del shell con drawer
                // abierto (equivalente al ShellTick del chasis sobre la activa).
                let es_tick = matches!(m, shuma_module_shell::Msg::Tick);
                model.shuma.inner = shuma_module_shell::update(model.shuma.inner.clone(), m);
                // El usuario tocó el botón de micrófono del input (que alterna
                // `mic_intent`): arranca o pará la captura de voz. Con el STT mock
                // por default, cualquier utterance real despierta y dicta —así se
                // ven las animaciones de escucha sin daemon ni nube.
                match model.shuma.inner.tomar_mic_intent() {
                    Some(true) => iniciar_voz(&mut model, handle),
                    Some(false) => parar_voz(&mut model),
                    None => {}
                }
                // Mientras escucha, bumpea el reloj de voz en cada tick (~10 Hz)
                // para que el halo del micrófono animе su respiración/anillos.
                if es_tick && model.shuma.inner.escucha().activo() {
                    model.shuma.inner.set_voz_reloj((willay_emit::ahora_usec() / 1000) as u64);
                }
                // #2 — drenar aichat/semántica que el módulo dejó pendiente (`:?`,
                // `:buscar`): correrlos en un thread y devolver el resultado AL INPUT.
                // Antes esto sólo pasaba en modo full; ahora también desde la barra bare.
                if let Some(req) = model.shuma.inner.take_llm_request() {
                    let kind = req.kind;
                    handle.spawn(move || {
                        let (ok, text) = match shuma_shell_llimphi::update::run_llm_blocking(&req) {
                            Ok(t) => (true, t),
                            Err(e) => (false, e),
                        };
                        Msg::ShumaShell(shuma_module_shell::Msg::LlmResult { kind, ok, text })
                    });
                }
                if let Some(req) = model.shuma.inner.take_semantic_request() {
                    handle.spawn(move || {
                        let (ok, hits) = match shuma_shell_llimphi::update::run_semantic_blocking(&req) {
                            Ok(h) => (true, h),
                            Err(e) => (false, vec![(e, 0.0)]),
                        };
                        Msg::ShumaShell(shuma_module_shell::Msg::SemanticResult { ok, hits })
                    });
                }
                // #3 — launcher: proveer las apps al módulo (una vez) y lanzar la
                // que el input haya pedido (spawn detached).
                if model.shuma.inner.apps.is_empty() {
                    model.shuma.inner.apps = apps_lanzables(&model.registry);
                }
                if let Some(cmd) = model.shuma.inner.take_app_launch() {
                    spawn_cmd(&cmd);
                }
                if es_tick && model.shuma.open {
                    model.shuma.inner.ack_long_alerts();
                }
                if submitting && model.shuma.present {
                    // Enviar = firme (abre si estaba plegado, o escala un vistazo).
                    model.shuma.open_mode = shuma::OpenMode::Firme;
                    if !model.shuma.open {
                        model.shuma.open = true;
                        model.shuma.inner.ack_long_alerts();
                        model.animar_shuma(1.0, handle);
                    }
                }
            }
            Msg::VozEvento(ev) => {
                use rimay_voz_host::EventoEscucha as E;
                use shuma_voz_ui::EstadoEscucha as Es;
                let ahora_ms = (willay_emit::ahora_usec() / 1000) as u64;
                let estado = match &ev {
                    E::Escuchando => Es::Oyendo,   // VAD detectó voz (grabando)
                    E::Desperto => Es::Despierto,  // reconoció el llamado
                    E::Dictar(_) => Es::Dictando,  // texto fluyendo al input
                    E::SeDurmio => Es::Esperando,  // sigue armado, no apagado
                };
                model.shuma.inner.fijar_escucha(estado);
                model.shuma.inner.set_voz_reloj(ahora_ms);
                // El dictado entra por el mismo camino que el texto tipeado.
                if let E::Dictar(t) = ev {
                    model.shuma.inner = shuma_module_shell::update(
                        model.shuma.inner.clone(),
                        shuma_module_shell::Msg::InsertAtCursor(t),
                    );
                }
            }
            Msg::RevealFantasmas(si) => {
                let now = willay_emit::ahora_usec();
                model.fantasmas_hover = si;
                if si {
                    // El path winit no tiene un pump fino de ~30 Hz: revela directo
                    // (la barra layer-shell sí lo funde en `draw`).
                    model.fantasmas_alpha = 1.0;
                    // Congela el snapshot de fugaces mientras el puntero ande
                    // cerca: los clicks bumpean el uso pero nada se recoloca.
                    estampar_fugaz_fijo(&mut model);
                } else {
                    model.fantasmas_hasta = now + FANT_LINGER_US;
                }
                model.fantasmas_reloj = now;
            }
            Msg::FantasmaPin(id, entra) => {
                if entra {
                    model.fugaz_pin = Some(id);
                    estampar_fugaz_fijo(&mut model);
                } else if model.fugaz_pin == Some(id) {
                    // Sólo despinnea el propio: los enter/leave entre iconos
                    // vecinos pueden llegar cruzados. Al soltar, el retardo del
                    // reveal evita que se esfume de golpe bajo el mouse.
                    model.fugaz_pin = None;
                    model.fantasmas_hasta = willay_emit::ahora_usec() + FANT_LINGER_US;
                }
            }
            Msg::FugazClick(id) => {
                // Aprende el uso (asiento a la derecha, persistido) y despacha
                // la acción del icono (abrir su diálogo).
                model.fugaz_uso.bump(id);
                // El icono de sonido con un reproductor activo es un
                // «empausador»: el click izquierdo alterna play/pausa en vez de
                // abrir el panel (que sigue en el click derecho).
                if id == shuma::Fugaz::Sonido
                    && model.media_now.as_ref().map(|m| m.has_player).unwrap_or(false)
                {
                    return Self::update(model, Msg::MediaPlayPause, handle);
                }
                if let Some(m) = shuma::accion_fugaz(id) {
                    return Self::update(model, m, handle);
                }
            }
            Msg::ShumaAnim => {}
            Msg::ShumaMaximize => {
                model.shuma.maximized = !model.shuma.maximized;
                model.shuma.height_frac = None;
            }
            Msg::ShumaResize(frac_delta) => {
                let cur = model.shuma.height_frac.unwrap_or(if model.shuma.maximized {
                    0.95
                } else {
                    model.cfg.general.shuma_height.clamp(0.1, 0.95)
                });
                model.shuma.height_frac = Some((cur + frac_delta).clamp(0.15, 0.98));
                model.shuma.maximized = false;
            }
            Msg::ShumaUndock => {
                // Desacople real ("mover de verdad"): la sesión embebida se va a
                // un shuma standalone —con su scrollback vía handoff, cwd e
                // historial incluidos— y el drawer queda en limpio. Ya no
                // duplica ni deja la sesión colgada en la barra.
                undock_shuma_session(&mut model.shuma.inner);
                model.shuma.maximized = false;
                if model.shuma.open {
                    model.shuma.open = false;
                    model.animar_shuma(0.0, handle);
                }
            }
            Msg::NahualToggle => {
                model.nahual.ensure();
                model.nahual.open = !model.nahual.open;
                let destino = if model.nahual.open { 1.0 } else { 0.0 };
                model.animar_nahual(destino, handle);
                // Al abrir por primera vez, monta las Mónadas del daemon en un
                // worker (es caro: descubrimiento + consulta inicial). Una sola
                // vez (gateado por `DaemonLoad::Idle`); no bloquea el arranque
                // ni el toggle.
                if model.nahual.open && model.nahual.daemon == nahual::DaemonLoad::Idle {
                    model.nahual.daemon = nahual::DaemonLoad::Loading;
                    let slot = model.nahual.slot.clone();
                    handle.spawn(move || match nahual_module::connect_daemon_navigator() {
                        Ok(nav) => {
                            if let Ok(mut g) = slot.lock() {
                                *g = Some(nav);
                            }
                            Msg::NahualDaemonReady
                        }
                        Err(e) => Msg::NahualDaemonFailed(e.to_string()),
                    });
                }
            }
            Msg::Nahual(m) => {
                // El módulo es puro: lo actualizamos y ejecutamos sus Effects
                // (el host tiene el Handle + el registro de apps).
                if let Some(inner) = model.nahual.inner.take() {
                    let (inner, efectos) = nahual_module::update(inner, m);
                    model.nahual.inner = Some(inner);
                    for ef in efectos {
                        ejecutar_efecto_nahual(&model.registry, ef, handle);
                    }
                }
            }
            Msg::NahualAnim => {}
            Msg::NahualDaemonReady => {
                // El worker dejó el Navigator listo: tomalo y montalo sobre la
                // pila del módulo (sin I/O — la consulta cara ya corrió).
                let nav = model.nahual.slot.lock().ok().and_then(|mut g| g.take());
                if let (Some(nav), Some(inner)) = (nav, model.nahual.inner.as_mut()) {
                    inner.mount_navigator(nav);
                    model.nahual.daemon = nahual::DaemonLoad::Mounted;
                }
            }
            Msg::NahualDaemonFailed(e) => {
                model.nahual.daemon = nahual::DaemonLoad::Failed(e);
            }
            Msg::Spawn(cmd) => spawn_cmd(&cmd),
            Msg::SwitchPacha(id) => {
                spawn_cmd(&format!("pacha switch {id}"));
                // Refrescamos el control center para que el chip activo salte.
                model.control_extras = render::ControlExtras::read();
            }
            Msg::PachaSelect(id) => {
                // Sólo cambia qué instancia se VE en el panel del diente perfil.
                model.nav.pacha_sel = id;
            }
            Msg::SwitchWorkspace(n) => {
                sampler::switch_workspace(n);
                // Realce optimista: la celda clickeada se marca activa al
                // instante (sin esperar el muestreo de ~1 s). Se sostiene unos
                // ticks y se reconcilia en `Msg::Tick`. Repintamos ya con un ctx
                // que refleja el salto.
                model.pending_ws = Some((n, sampler::OPTIMISTIC_TICKS));
                let mut ctx = model.last_ctx.clone();
                ctx.active_workspace = n;
                model.tick_widgets(&ctx);
                model.last_ctx = ctx;
            }
            Msg::WorkspaceTooth { si, ws } => {
                // Salto de escritorio (optimista, igual que SwitchWorkspace)...
                sampler::switch_workspace(ws);
                model.pending_ws = Some((ws, sampler::OPTIMISTIC_TICKS));
                let mut ctx = model.last_ctx.clone();
                ctx.active_workspace = ws;
                model.tick_widgets(&ctx);
                model.last_ctx = ctx;
                // ...y desplegar/replegar su taskbar según el modo de expansión del
                // rag (como cualquier diente): en modo "un clic (dos pasos)" el
                // primer toque solo cambia de escritorio SIN abrir el panel, y un
                // re-clic expande su taskbar; en modo normal cambia y despliega. El id
                // va CODIFICADO (`WS_BASE + ws`): el rail mezcla escritorios con tabs.
                let ws_id = render::sidebar::WS_BASE as usize + ws as usize;
                model.nav.activate_tab("", si, ws_id, model.cfg.general.diente_dos_pasos);
            }
            Msg::VolumeWheel(dy) => {
                // Rueda arriba (dy<0) = subir; el stack da dy>0 al rodar abajo.
                if dy != 0.0 {
                    sampler::nudge_volume(dy < 0.0);
                    let nuevo = (model.last_ctx.volume + if dy < 0.0 { 0.05 } else { -0.05 })
                        .clamp(0.0, 1.0);
                    model.osd = Some(render::Osd::flash(render::OsdKind::Volume, nuevo, false));
                    let muted = model.last_ctx.muted;
                    flash_volumen_diente(&mut model, nuevo, muted);
                }
            }
            Msg::VolumeMute => {
                sampler::toggle_mute();
                let muted = !model.last_ctx.muted;
                let vol = model.last_ctx.volume;
                model.osd = Some(render::Osd::flash(render::OsdKind::Volume, vol, muted));
                flash_volumen_diente(&mut model, vol, muted);
            }
            Msg::ClipboardMenu => {
                model.clip_open = !model.clip_open;
                if model.clip_open {
                    model.menu_open = false;
                }
            }
            Msg::CompletionDismiss => {
                // Path winit: no hay surface flotante (el popup vive in-drawer);
                // el descarte sólo cierra el popup del módulo.
                model.shuma.inner.close_completion();
            }
            Msg::ControlToggle => {
                model.control_open = !model.control_open;
                if model.control_open {
                    // Refresca batería/radios al abrir (volumen/brillo van por
                    // el último ctx del sampler, ya en vivo).
                    model.control_extras = render::ControlExtras::read();
                    model.menu_open = false;
                    model.clip_open = false;
                }
            }
            Msg::ControlWifi(on) => {
                render::set_radio("wlan", on);
                model.control_extras.wifi = on;
            }
            Msg::ControlBt(on) => {
                render::set_radio("bluetooth", on);
                model.control_extras.bt = on;
            }
            Msg::ControlPowerProfile(id) => {
                render::set_power_profile(&id);
                model.control_extras.power_profile = Some(id);
            }
            Msg::ControlNight(on) => {
                render::set_night(on);
                model.control_extras.night = on;
            }
            Msg::ControlCafe(on) => {
                // Backend winit (dev): sólo refleja el estado; la inhibición real
                // (suspensión + idle del compositor) vive en el path layer-shell.
                model.control_extras.cafe = on;
            }
            Msg::ControlTeclado(on) => {
                // Backend winit (dev): sólo refleja el estado; el OSK real
                // (`mirada-teclado`, superficie layer-shell) se lanza/mata en el
                // path layer-shell.
                model.control_extras.teclado = on;
            }
            Msg::ControlPaisaje(on) => {
                model.paisaje_on = on;
                if let Some(h) = &model.paisaje {
                    h.set_enabled(on);
                }
                model.control_extras.paisaje = on;
            }
            Msg::Magnify(pct) => {
                // Lupa de pantalla: el compositor la aplica (sigue el puntero).
                spawn_cmd(&format!("mirada-ctl magnify {pct}"));
                model.control_extras.magnify_pct = pct;
            }
            Msg::Record(on) => {
                // Grabar pantalla: el compositor toma sus cuadros y los encodea.
                spawn_cmd(if on { "mirada-ctl record start" } else { "mirada-ctl record stop" });
                model.control_extras.recording = on;
            }
            Msg::NetworkToggle => {
                model.network_open = !model.network_open;
                model.net_password = None;
                if model.network_open {
                    model.menu_open = false;
                    model.clip_open = false;
                    model.control_open = false;
                }
            }
            Msg::NetworkPasswordPrompt(ssid) => {
                model.net_password = Some((ssid, String::new()));
            }
            Msg::NetworkPasswordChar(c) => {
                if let Some((_, pw)) = &mut model.net_password {
                    pw.push(c);
                }
            }
            Msg::NetworkPasswordBackspace => {
                if let Some((_, pw)) = &mut model.net_password {
                    pw.pop();
                }
            }
            Msg::NetworkPasswordSubmit => {
                if let Some((ssid, pw)) = model.net_password.take() {
                    network::connect_with(&ssid, &pw);
                    model.network_open = false;
                }
            }
            Msg::NetworkPasswordCancel => model.net_password = None,
            Msg::NetworkConnect(ssid) => {
                network::connect(&ssid);
                model.network_open = false;
            }
            Msg::NetworkDisconnect(ssid) => {
                network::disconnect(&ssid);
                model.network_open = false;
            }
            Msg::NetConnUp(name) => {
                network::conn_up(&name);
                model.network_open = false;
            }
            Msg::NetForget(name) => network::forget(&name),
            Msg::NetworkRadio(on) => {
                network::set_wifi_radio(on);
                // Reflejo optimista: el próximo muestreo confirma.
                if let Some(n) = &mut model.network_now {
                    n.wifi_enabled = on;
                }
            }
            Msg::SessionToggle => {
                model.session_open = !model.session_open;
                model.session_confirm = None;
                if model.session_open {
                    model.menu_open = false;
                    model.clip_open = false;
                    model.control_open = false;
                    model.network_open = false;
                }
            }
            Msg::SessionConfirm(a) => model.session_confirm = Some(a),
            Msg::SessionCancel => model.session_confirm = None,
            Msg::SessionRun(a) => {
                run_session_action(a);
                model.session_open = false;
                model.session_confirm = None;
            }
            Msg::ConfirmPedir(accion) => {
                // Abre la pantalla de confirmación fullscreen; cierra menús/paneles que
                // compitan por el foco visual.
                model.confirm_overlay = Some(accion);
                model.session_open = false;
                model.session_confirm = None;
                model.menu_open = false;
            }
            Msg::ConfirmAceptar => {
                if let Some(accion) = model.confirm_overlay.take() {
                    accion.ejecutar();
                }
            }
            Msg::ConfirmCancelar => model.confirm_overlay = None,
            Msg::MediaPlayPause => mpris::play_pause(),
            Msg::MediaNext => mpris::next(),
            Msg::MediaPrev => mpris::previous(),
            Msg::BluetoothToggle => {
                model.bluetooth_open = !model.bluetooth_open;
                if model.bluetooth_open {
                    model.menu_open = false;
                    model.clip_open = false;
                    model.control_open = false;
                    model.network_open = false;
                }
            }
            Msg::BluetoothPower(on) => {
                bluetooth::set_power(on);
                if let Some(b) = &mut model.bluetooth_now {
                    b.powered = on;
                }
            }
            Msg::BluetoothConnect(mac) => bluetooth::connect(&mac),
            Msg::BluetoothDisconnect(mac) => bluetooth::disconnect(&mac),
            Msg::BluetoothScan => bluetooth::scan(),
            Msg::BluetoothPair(mac) => bluetooth::pair(&mac),
            Msg::NotificationsToggle => {
                model.notifications_open = !model.notifications_open;
                if model.notifications_open {
                    model.menu_open = false;
                    model.clip_open = false;
                    model.control_open = false;
                    model.network_open = false;
                    model.bluetooth_open = false;
                }
            }
            Msg::NotificationsDnd(on) => {
                if let Some(h) = &model.notifications {
                    h.set_dnd(on);
                }
            }
            Msg::NotificationsClear => {
                if let Some(h) = &model.notifications {
                    h.clear();
                }
            }
            Msg::PolkitChar(c) => model.polkit_input.push(c),
            Msg::PolkitBackspace => {
                model.polkit_input.pop();
            }
            Msg::PolkitSubmit => {
                if let Some(req) = model.polkit_prompt.take() {
                    let _ = req.reply.send(Some(std::mem::take(&mut model.polkit_input)));
                }
            }
            Msg::PolkitCancel => {
                if let Some(req) = model.polkit_prompt.take() {
                    let _ = req.reply.send(None);
                }
                model.polkit_input.clear();
            }
            Msg::ClipboardPick(text) => {
                sampler::copiar_clipboard(&text);
                model.clip_open = false;
            }
            Msg::ClipboardAction(objetivo) => {
                // Abre el objetivo detectado (URL/mailto/ruta) con el opener del
                // sistema. Best-effort; no bloquea si no hay `xdg-open`.
                crate::desacoplar(std::process::Command::new("xdg-open").arg(&objetivo).spawn());
                model.clip_open = false;
            }
            Msg::ClipboardPin(id) => {
                if let Some(store) = &model.clip_store {
                    let _ = store.alternar_fijado(id);
                }
                model.clip_history = clip_history_desde_store(&model.clip_store);
            }
            Msg::ClipboardDelete(id) => {
                if let Some(store) = &model.clip_store {
                    let _ = store.borrar(id);
                }
                model.clip_history = clip_history_desde_store(&model.clip_store);
            }
            Msg::ClockPanel => {
                model.clock_open = !model.clock_open;
                if model.clock_open {
                    model.clock_draft = ClockDraft::from_now(usa_utc(&model.cfg));
                    model.menu_open = false;
                    model.clip_open = false;
                }
            }
            Msg::CieloPanel => {
                model.cielo_open = !model.cielo_open;
                if model.cielo_open {
                    model.menu_open = false;
                    model.clip_open = false;
                    model.clock_open = false;
                    model.clima_open = false;
                }
            }
            Msg::ClimaPanel => {
                model.clima_open = !model.clima_open;
                if model.clima_open {
                    model.menu_open = false;
                    model.clip_open = false;
                    model.clock_open = false;
                    model.cielo_open = false;
                }
            }
            Msg::KhipuPanel => {
                model.khipu_open = !model.khipu_open;
                if model.khipu_open {
                    model.khipu_snapshot = model.khipu.snapshot(khipu::ahora_unix());
                    model.khipu_input = Some(String::new()); // listo para teclear
                    model.menu_open = false;
                    model.clip_open = false;
                    model.clock_open = false;
                    model.cielo_open = false;
                } else {
                    model.khipu_input = None;
                }
            }
            Msg::KhipuChar(c) => {
                if let Some(d) = &mut model.khipu_input {
                    if !c.is_control() {
                        d.push(c);
                    }
                }
            }
            Msg::KhipuBackspace => {
                if let Some(d) = &mut model.khipu_input {
                    d.pop();
                }
            }
            Msg::KhipuSubmit => {
                let texto = model.khipu_input.clone().unwrap_or_default();
                model.khipu.jot(&texto, khipu::ahora_unix());
                model.khipu_input = Some(String::new()); // limpia, sigue anotando
                model.khipu_snapshot = model.khipu.snapshot(khipu::ahora_unix());
            }
            Msg::KhipuReinforce(id) => {
                model.khipu.reinforce(id, khipu::ahora_unix());
                model.khipu_snapshot = model.khipu.snapshot(khipu::ahora_unix());
            }
            Msg::TampuPanel => {
                model.tampu_open = !model.tampu_open;
                if model.tampu_open {
                    model.menu_open = false;
                    model.clip_open = false;
                    model.clock_open = false;
                    model.cielo_open = false;
                    model.khipu_open = false;
                    model.khipu_input = None;
                }
            }
            Msg::CapturaPanel => {
                model.captura_open = !model.captura_open;
                if model.captura_open {
                    model.menu_open = false;
                    model.clip_open = false;
                    model.tampu_open = false;
                    model.cielo_open = false;
                    model.khipu_open = false;
                    model.khipu_input = None;
                }
            }
            Msg::Captura(m) => {
                model.captura_open = false;
                spawn_cmd(m.comando());
            }
            Msg::GrabarIniciar(modo, audio) => {
                model.captura_open = false;
                if model.grabacion.is_none() {
                    match grabacion::Grabacion::iniciar(modo, audio) {
                        Ok(g) => model.grabacion = Some(g),
                        Err(e) => eprintln!("pata: no se pudo grabar: {e}"),
                    }
                }
            }
            Msg::GrabarDetener => {
                model.captura_open = false;
                if let Some(g) = model.grabacion.take() {
                    let _ = g.detener();
                }
            }
            Msg::UsbPanel => {
                model.usb_open = !model.usb_open;
                if model.usb_open {
                    model.menu_open = false;
                    model.clip_open = false;
                    model.tampu_open = false;
                    model.captura_open = false;
                    model.cielo_open = false;
                    model.khipu_open = false;
                    model.khipu_input = None;
                }
            }
            Msg::UsbMontar(dev) => usb::montar(&dev),
            Msg::UsbDesmontar(dev) => usb::desmontar(&dev),
            Msg::UsbExpulsar(disco) => {
                usb::expulsar(&disco);
                model.usb_open = false;
            }
            Msg::UsbAbrir(punto) => spawn_cmd(&usb::abrir(&punto)),
            Msg::AgoraPanel => {
                model.agora_open = !model.agora_open;
                if model.agora_open {
                    model.menu_open = false;
                    model.clip_open = false;
                    model.tampu_open = false;
                    model.captura_open = false;
                    model.cielo_open = false;
                    model.khipu_open = false;
                    model.khipu_input = None;
                    model.usb_open = false;
                }
            }
            Msg::AgoraAbrir => {
                spawn_cmd("agora-app");
                model.agora_open = false;
            }
            Msg::CieloLocalidad(n) => {
                // `u32::MAX` = automática (por IP): vaciamos la selección activa
                // dejando el índice fuera de rango si no hay lista, o volviendo a
                // auto poniendo `activa` a un índice inexistente.
                let locs = &model.cfg.general.ubicacion.localidades;
                if n == u32::MAX {
                    // Auto: siembra `None` y deja que el clima resuelva por IP.
                    if let Ok(mut g) = model.cielo_loc.lock() {
                        *g = None;
                    }
                    model.cfg.general.ubicacion.activa = locs.len() as u32; // fuera de rango = auto
                } else if let Some(loc) = locs.get(n as usize) {
                    let coords = (loc.lat, loc.lon);
                    if let Ok(mut g) = model.cielo_loc.lock() {
                        *g = Some(coords);
                    }
                    model.cfg.general.ubicacion.activa = n;
                }
            }
            Msg::ClockAdjust(f, delta) => model.clock_draft.adjust(f, delta),
            Msg::ClockApply => {
                sampler::set_system_time(&model.clock_draft.stamp());
                model.clock_open = false;
            }
            Msg::ClockSyncNtp => {
                sampler::sync_ntp();
                model.clock_open = false;
            }
            Msg::BrightnessWheel(dy) => {
                if dy != 0.0 {
                    sampler::nudge_brightness(dy < 0.0);
                    let nuevo = (model.last_ctx.brightness + if dy < 0.0 { 0.05 } else { -0.05 })
                        .clamp(0.0, 1.0);
                    model.osd =
                        Some(render::Osd::flash(render::OsdKind::Brightness, nuevo, false));
                }
            }
            Msg::CpuPanel => {
                model.cpu_open = !model.cpu_open;
                if model.cpu_open {
                    model.ram_open = false;
                    model.volume_open = false;
                    model.brightness_open = false;
                    model.clip_open = false;
                    model.clock_open = false;
                }
            }
            Msg::RamPanel => {
                model.ram_open = !model.ram_open;
                if model.ram_open {
                    model.cpu_open = false;
                    model.volume_open = false;
                    model.brightness_open = false;
                    model.clip_open = false;
                    model.clock_open = false;
                }
            }
            Msg::VolumePanel => {
                model.volume_open = !model.volume_open;
                if model.volume_open {
                    model.sink_inputs = sampler::sample_sink_inputs();
                    model.sinks = sampler::sample_sinks();
                    model.source_outputs = sampler::sample_source_outputs();
                    model.sources = sampler::sample_sources();
                    model.cpu_open = false;
                    model.ram_open = false;
                    model.brightness_open = false;
                    model.clip_open = false;
                    model.clock_open = false;
                }
            }
            Msg::VolumeTabSet(t) => model.volume_tab = t,
            Msg::SourceOutputVolume(index, frac) => sampler::set_source_output_volume(index, frac),
            Msg::SourceOutputMute(index) => sampler::toggle_source_output_mute(index),
            Msg::SourceVolume(name, frac) => sampler::set_source_volume(&name, frac),
            Msg::SourceMute(name) => sampler::toggle_source_mute(&name),
            Msg::SourceSelect(name) => {
                sampler::set_default_source(&name);
                for s in &mut model.sources {
                    s.is_default = s.name == name;
                }
            }
            Msg::SinkVolume(name, frac) => sampler::set_sink_volume(&name, frac),
            Msg::SinkMute(name) => sampler::toggle_sink_mute(&name),
            Msg::SinkInputVolume(index, frac) => sampler::set_sink_input_volume(index, frac),
            Msg::SinkInputMute(index) => sampler::toggle_sink_input_mute(index),
            Msg::SinkSelect(name) => {
                sampler::set_default_sink(&name);
                // Refleja al toque el nuevo default en el selector (la marca ●),
                // sin esperar al próximo tick.
                for s in &mut model.sinks {
                    s.is_default = s.name == name;
                }
            }
            Msg::BrightnessPanel => {
                model.brightness_open = !model.brightness_open;
                if model.brightness_open {
                    model.cpu_open = false;
                    model.ram_open = false;
                    model.volume_open = false;
                    model.clip_open = false;
                    model.clock_open = false;
                }
            }
            Msg::VolumeSet(frac) => {
                sampler::set_volume(frac);
                model.osd = Some(render::Osd::flash(render::OsdKind::Volume, frac, false));
            }
            Msg::BrightnessSet(frac) => {
                sampler::set_brightness(frac);
                model.osd = Some(render::Osd::flash(render::OsdKind::Brightness, frac, false));
            }
            Msg::StartToggle => {
                model.menu_open = !model.menu_open;
                if !model.menu_open {
                    model.menu_query.clear();
                    model.menu_scroll = 0.0;
                }
            }
            Msg::StartStyleCycle => {
                model.menu_style = model.menu_style.next();
            }
            Msg::StartChar(c) => {
                if !c.is_control() {
                    model.menu_query.push(c);
                    model.menu_scroll = 0.0;
                }
            }
            Msg::StartBackspace => {
                model.menu_query.pop();
                model.menu_scroll = 0.0;
            }
            Msg::StartScroll(delta) => model.menu_scroll += delta,
            Msg::MenuScrollTo(v) => model.menu_scroll = v,
            // El path winit (dev) muestra la primera categoría estática; el
            // hover-submenú vivo es del backend layer-shell (la barra real).
            Msg::MenuHoverCategory(_) => {}
            Msg::StartLaunchFirst => {
                let id = render::menu_filtered(model.registry.all(), &model.menu_query)
                    .first()
                    .map(|a| a.id.clone());
                if let Some(id) = id {
                    if let Some(app) = model.registry.get(&id) {
                        // Vía arje si está levantado (Ente OneShot); si no, crudo.
                        arje_applaunch::launch_entry(app);
                    }
                    model.menu_open = false;
                    model.menu_query.clear();
                    model.menu_scroll = 0.0;
                }
            }
            Msg::LaunchApp(id) => {
                if let Some(app) = model.registry.get(&id) {
                    // Vía arje si está levantado (Ente OneShot); si no, crudo.
                    arje_applaunch::launch_entry(app);
                }
                model.menu_open = false;
                model.menu_query.clear();
                model.menu_scroll = 0.0;
            }
            Msg::TrayActivate(key) => {
                if let Some(t) = &model.tray {
                    t.activate(key);
                }
            }
            // En layer-shell el window_list resuelve el id por su cliente
            // foreign-toplevel; en winit lo muestreamos del WM y activamos por su
            // CLI (`mirada-ctl focus-window N`).
            Msg::ActivateWindow(id) => sampler::activate_window(id),
            // Cierre por id del task manager (clic derecho / clic medio), por la
            // CLI del WM.
            Msg::CloseWindow(id) => sampler::close_window(id),
            // Pestañas verticales del rail: ids de mirada en ambos backends →
            // siempre por la CLI del WM.
            Msg::RailTabActivate(id) => sampler::activate_window(id),
            Msg::RailTabClose(id) => sampler::close_window(id),
            // Menú contextual del taskbar de un diente-escritorio (path winit): sólo
            // estado + acción (no hay input-region que reajustar en la ventana única).
            Msg::WinMenuOpen { si, ws, id, title, x, y } => {
                model.nav.win_menu = Some(nouser::WinMenu { si, ws, win_id: id, title, x, y });
            }
            Msg::WinMenuClose => model.nav.win_menu = None,
            Msg::WinMenuDo(id, act) => {
                let ws = model.nav.win_menu.as_ref().map(|m| m.ws).unwrap_or(0);
                do_win_act(id, act, ws, &model.windows);
                model.nav.win_menu = None;
            }
            // El reordenamiento por arrastre sólo vive en el backend layer-shell;
            // en winit (dev) los botones no son arrastrables, estos no se emiten.
            Msg::TaskDragMove(_, _) => {}
            Msg::TaskDragEnd(_) => {}
            // --- Sidebar navegador (Fase 11c) ---
            Msg::NavTabActivate(si, ti) => {
                model.nav.activate_tab("", si, ti, model.cfg.general.diente_dos_pasos)
            }
            // Barrita del sidebar (backend winit, dev): persiste el eje y re-resuelve
            // el marco. No hay layer surfaces que reanclar (ventana única anidada).
            Msg::SidebarSetDocked(si, docked) => {
                persistir_eje_sidebar(si, Some(docked), None);
                model.recargar_config();
            }
            Msg::SidebarSetRailOutside(si, outside) => {
                persistir_eje_sidebar(si, None, Some(outside));
                model.recargar_config();
            }
            Msg::SidebarSetAutohide(si, autohide) => {
                persistir_autohide_sidebar(si, autohide);
                model.recargar_config();
            }
            Msg::SidebarSetDienteDosPasos(b) => {
                persistir_diente_dos_pasos(b);
                model.cfg.general.diente_dos_pasos = b;
            }
            Msg::SidebarResize(si, dx) => {
                if let Some(s) = model.cfg.surfaces.get_mut(si) {
                    s.panel_width = (s.panel_width + dx).clamp(120.0, 600.0);
                    persistir_panel_width_sidebar(si, s.panel_width);
                }
            }
            Msg::SidebarControlToggle(si) => {
                model.nav.control_open = !model.nav.control_open;
                model.nav.control_si = model.nav.control_open.then_some(si);
            }
            Msg::SearchFocus(f) => model.nav.search_focused = f,
            Msg::SearchChar(c) => {
                model.nav.search.push(c);
                model.nav.apply_search();
            }
            Msg::SearchBackspace => {
                model.nav.search.pop();
                model.nav.apply_search();
            }
            Msg::SearchClear => {
                model.nav.search.clear();
                model.nav.search_focused = false;
            }
            Msg::NavClosePanel => model.nav.open.clear(),
            Msg::NavSetMode(m) => model.nav.mode = m,
            Msg::NavSelect(id) => model.nav.selected = Some(id),
            Msg::NavToggle(id) => {
                if model.nav.expanded.contains(&id) {
                    model.nav.expanded.remove(&id);
                } else {
                    model.nav.expanded.insert(id);
                    // Carga perezosa: al abrir una Mónada sin miembros, pídelos.
                    if let (Some(mid), Some(sock)) =
                        (model.nav.needs_resolve(id), model.nav.socket.clone())
                    {
                        handle.spawn(move || Msg::NavMembers(nouser::resolve(sock, mid)));
                    }
                }
            }
            Msg::NavContextMenu(id) => {
                // Fase 11d-extra: right-click sobre un archivo abre el menú "Abrir
                // con…". Precomputamos sus apps aquí (con el registro) para que el
                // render no lo toque.
                if let Some(path) = model.nav.file_path(id).map(str::to_owned) {
                    let opts = open::handlers_for_path(&model.registry, &path);
                    model.nav.open_menu(id, opts);
                }
            }
            Msg::NavOpenWith(id, app_id) => {
                if let Some(path) = model.nav.file_path(id).map(str::to_owned) {
                    match app_id {
                        Some(aid) => {
                            let _ = open::open_with_id(&model.registry, &aid, &path);
                        }
                        None => {
                            let _ = open::open_system(&path);
                        }
                    }
                }
                model.nav.close_menu();
            }
            Msg::NavMenuCancel => model.nav.close_menu(),
            // El rail hospedado vive en el backend layer-shell (conoce el foco y
            // corre el HostServer). En winit no hay toplevels: no-op.
            Msg::HostToothActivate(_, _) => {}
            Msg::NavScroll(delta) => {
                model.nav.scroll = (model.nav.scroll + delta).max(0.0);
            }
            Msg::NavTick => {
                let sock = model.nav.socket.clone();
                handle.spawn(move || Msg::NavPoll(nouser::poll(sock)));
            }
            Msg::NavPoll(outcome) => match outcome {
                PollOutcome::Ok { socket, resp } => {
                    model.nav.socket = Some(socket);
                    model.nav.apply_monads(*resp);
                }
                PollOutcome::Failed(e) => {
                    // Invalida el socket cacheado para re-descubrir en el próximo poll.
                    model.nav.socket = None;
                    model.nav.error = Some(e);
                }
            },
            Msg::NavMembers(outcome) => match outcome {
                MembersOutcome::Ok { monad, members } => model.nav.apply_members(monad, members),
                MembersOutcome::Failed(e) => model.nav.error = Some(e),
            },
            // --- Sidebar RAG (preguntale a tu correo) ---
            Msg::RagEngineReady { ok, corpus } => {
                model.rag.corpus_len = corpus;
                model.rag.status = if ok { RagStatus::Idle } else { RagStatus::Unavailable };
            }
            Msg::RagChar(c) => {
                // Ignoramos controles; el resto va al buscador (motor listo o con
                // una respuesta ya servida, para encadenar otra pregunta).
                if !c.is_control()
                    && matches!(model.rag.status, RagStatus::Idle | RagStatus::Ready)
                {
                    model.rag.query.push(c);
                }
            }
            Msg::RagBackspace => {
                model.rag.query.pop();
            }
            Msg::RagClear => {
                model.rag.query.clear();
                model.rag.answer.clear();
                model.rag.sources.clear();
                model.rag.error = None;
                if matches!(model.rag.status, RagStatus::Ready) {
                    model.rag.status = RagStatus::Idle;
                }
            }
            Msg::RagSubmit => {
                let q = model.rag.query.trim().to_string();
                if !q.is_empty()
                    && matches!(model.rag.status, RagStatus::Idle | RagStatus::Ready)
                {
                    model.rag.status = RagStatus::Asking;
                    model.rag.answer.clear();
                    model.rag.sources.clear();
                    model.rag.error = None;
                    // El `ask` del motor sólo encola en su runtime y vuelve
                    // enseguida; el lock es breve y no contiende con el hilo de UI.
                    if let Ok(guard) = model.rag.engine.lock() {
                        if let Some(engine) = guard.as_ref() {
                            let h = handle.clone();
                            engine.ask(q, Box::new(move |res| match res {
                                Ok(a) => h.dispatch(Msg::RagResult {
                                    answer: a.answer,
                                    sources: a.sources,
                                }),
                                Err(e) => h.dispatch(Msg::RagError(e.to_string())),
                            }));
                        } else {
                            model.rag.status = RagStatus::Unavailable;
                        }
                    }
                }
            }
            Msg::RagResult { answer, sources } => {
                model.rag.answer = answer;
                model.rag.sources = sources;
                model.rag.error = None;
                model.rag.status = RagStatus::Ready;
            }
            Msg::RagError(e) => {
                model.rag.error = Some(e);
                model.rag.status = RagStatus::Ready;
            }
        }
        model
    }

    fn view(model: &Model) -> View<Msg> {
        render::root(model)
    }

    fn view_overlay(model: &Model) -> Option<View<Msg>> {
        // Cada popup anclado a la barra (quick settings, menú de inicio, panel
        // del reloj…) entra una sola vez con un fade + leve descenso desde la
        // barra, en vez de aparecer de golpe. La `key` por overlay dispara la
        // animación al aparecer y queda estable mientras sigue abierto (los
        // re-render del tick a 1 Hz no la rearman). Los drawers Quake
        // (shuma/nahual) traen su propio `Tween`, el polkit es modal y el OSD se
        // desvanece solo: esos no se envuelven.
        use llimphi_ui::llimphi_raster::kurbo::Affine;
        fn entra(v: View<Msg>, key: u64) -> View<Msg> {
            v.animated_enter_from(key, motion::FAST, Affine::translate((0.0, -10.0_f64)))
        }
        // La pantalla de confirmación fullscreen es lo más modal de todo: tapa
        // cualquier otro overlay (incluido el menú de inicio) mientras hay una acción
        // disruptiva pendiente.
        if let Some(accion) = &model.confirm_overlay {
            let screen = (model.screen.0 as f32, model.screen.1 as f32);
            return Some(entra(
                render::confirm_overlay_view(accion, screen.0, screen.1, &model.theme),
                42,
            ));
        }
        // El diálogo de polkit es modal: tapa todo lo demás mientras está activo.
        if let Some(req) = &model.polkit_prompt {
            let screen = (model.screen.0 as f32, model.screen.1 as f32);
            return Some(render::polkit_overlay(
                &req.message,
                &model.polkit_input,
                screen,
                &model.theme,
            ));
        }
        // El drawer Quake tiene prioridad; luego el menú de inicio; luego los
        // popups de widgets (historial de portapapeles, panel del reloj).
        if let Some(d) = nahual::drawer_overlay(&model.nahual, model.screen, &model.theme) {
            return Some(d);
        }
        // Live-wire: con la shuma completa montada, el drawer la pinta entera
        // (dientes/sesiones/menubar/canvas) elevada al `Msg` de pata.
        if let Some(full) = &model.shuma_full {
            if let Some(d) =
                shuma::drawer_overlay_full(&model.shuma, full, model.screen, &model.theme)
            {
                return Some(d);
            }
        } else if let Some(d) = shuma::drawer_overlay(&model.shuma, model.screen, &model.theme) {
            return Some(d);
        }
        if model.menu_open {
            let bar_h = bar_thickness_for(&model.cfg, "start_button");
            let screen_size = (model.screen.0 as f32, model.screen.1 as f32);
            return Some(entra(match model.menu_style {
                MenuStyle::Classic => render::start_menu_overlay(
                    model.registry.all(),
                    &model.menu_query,
                    model.menu_scroll,
                    bar_h,
                    screen_size.1,
                    &model.theme,
                ),
                MenuStyle::Xp => render::start_menu_xp_overlay(
                    model.registry.all(),
                    &model.menu_query,
                    model.menu_scroll,
                    bar_h,
                    screen_size,
                    &model.theme,
                ),
                MenuStyle::Gnome => render::start_menu_gnome_overlay(
                    model.registry.all(),
                    &model.menu_query,
                    bar_h,
                    screen_size,
                    &model.theme,
                ),
            }, 1));
        }
        if model.clip_open {
            let bar_h = bar_thickness_for(&model.cfg, "clipboard");
            let rows = render::clip_rows(&model.clip_store, &model.clip_history);
            return Some(entra(render::clipboard_overlay(
                &rows,
                bar_h,
                // Path winit (app suelta de prueba): sin ancho/cursor rastreado,
                // cae al borde izquierdo como antes. El anclado «justo debajo»
                // del widget vive en el layer-shell (el DM).
                0.0,
                f32::MAX,
                &model.theme,
            ), 2));
        }
        if model.control_open {
            let bar_h = bar_thickness_for(&model.cfg, "control");
            let screen = (model.screen.0 as f32, model.screen.1 as f32);
            return Some(entra(render::control_overlay(
                model.last_ctx.volume,
                model.last_ctx.muted,
                model.last_ctx.brightness,
                &model.control_extras,
                bar_h,
                screen,
                &model.theme,
            ), 3));
        }
        if model.network_open {
            let bar_h = bar_thickness_for(&model.cfg, "network");
            let pw = model
                .net_password
                .as_ref()
                .map(|(s, p)| (s.as_str(), p.as_str()));
            return Some(entra(render::network_overlay(
                model.network_now.as_ref(),
                pw,
                bar_h,
                &model.theme,
            ), 4));
        }
        if model.session_open {
            let bar_h = bar_thickness_for(&model.cfg, "session");
            return Some(entra(
                render::session_overlay(model.session_confirm, bar_h, &model.theme),
                5,
            ));
        }
        if model.bluetooth_open {
            let bar_h = bar_thickness_for(&model.cfg, "bluetooth");
            return Some(entra(render::bluetooth_overlay(
                model.bluetooth_now.as_ref(),
                bar_h,
                &model.theme,
            ), 6));
        }
        if model.notifications_open {
            let bar_h = bar_thickness_for(&model.cfg, "notifications");
            let snap = model.notifications.as_ref().map(|n| n.snapshot());
            return Some(entra(
                render::notifications_overlay(snap.as_ref(), bar_h, &model.theme),
                7,
            ));
        }
        if model.clock_open {
            let bar_h = bar_thickness_for(&model.cfg, "clock");
            return Some(entra(
                render::clock_overlay(&model.clock_draft, bar_h, &model.theme),
                8,
            ));
        }
        if model.cielo_open {
            let bar_h = bar_thickness_for(&model.cfg, "shuma_input");
            let u = &model.cfg.general.ubicacion;
            return Some(entra(
                render::cielo_overlay(
                    model.cielo_now.as_ref(),
                    &u.localidades,
                    u.activa,
                    model.last_ctx.sun_longitude_deg,
                    bar_h,
                    &model.theme,
                ),
                20,
            ));
        }
        if model.clima_open {
            let bar_h = bar_thickness_for(&model.cfg, "shuma_input");
            return Some(entra(
                render::clima_overlay(
                    model.weather_now.as_ref(),
                    model.diente_t as f32,
                    bar_h,
                    &model.theme,
                ),
                20,
            ));
        }
        if model.khipu_open {
            let bar_h = bar_thickness_for(&model.cfg, "shuma_input");
            return Some(entra(
                render::khipu_overlay(
                    Some(&model.khipu_snapshot),
                    model.khipu_input.as_deref(),
                    bar_h,
                    &model.theme,
                ),
                21,
            ));
        }
        if model.tampu_open {
            let bar_h = bar_thickness_for(&model.cfg, "shuma_input");
            return Some(entra(
                render::tampu_overlay(model.tampu_now.as_ref(), bar_h, &model.theme),
                22,
            ));
        }
        if model.captura_open {
            let bar_h = bar_thickness_for(&model.cfg, "shuma_input");
            let grab = model.grabacion.as_ref().map(|g| g.segundos());
            return Some(entra(render::captura_overlay(grab, bar_h, &model.theme), 23));
        }
        if model.usb_open {
            let bar_h = bar_thickness_for(&model.cfg, "shuma_input");
            return Some(entra(
                render::usb_overlay(model.usb_now.as_ref(), bar_h, &model.theme),
                24,
            ));
        }
        if model.agora_open {
            let bar_h = bar_thickness_for(&model.cfg, "shuma_input");
            return Some(entra(
                render::agora_overlay(model.agora_now.as_ref(), bar_h, &model.theme),
                25,
            ));
        }
        if model.cpu_open {
            let bar_h = bar_thickness_for(&model.cfg, "cpu_meter");
            return Some(entra(
                render::cpu_overlay(&model.last_ctx, bar_h, &model.theme),
                9,
            ));
        }
        if model.ram_open {
            let bar_h = bar_thickness_for(&model.cfg, "ram_meter");
            return Some(entra(
                render::ram_overlay(&model.last_ctx, bar_h, &model.theme),
                10,
            ));
        }
        if model.volume_open {
            let bar_h = bar_thickness_for(&model.cfg, "volume");
            return Some(entra(render::volume_overlay(
                &model.last_ctx,
                &model.sinks,
                &model.sink_inputs,
                &model.sources,
                &model.source_outputs,
                model.volume_tab,
                bar_h,
                &model.theme,
            ), 11));
        }
        if model.brightness_open {
            let bar_h = bar_thickness_for(&model.cfg, "brightness");
            return Some(entra(
                render::brightness_overlay(&model.last_ctx, bar_h, &model.theme),
                12,
            ));
        }
        // El OSD es la prioridad más baja: feedback transitorio cuando no hay
        // ningún menú/drawer abierto.
        if let Some(osd) = model.osd.filter(|o| !o.expired()) {
            let screen = (model.screen.0 as f32, model.screen.1 as f32);
            return Some(render::osd_overlay(&osd, screen, &model.theme));
        }
        None
    }

    fn on_key(model: &Model, event: &KeyEvent) -> Option<Msg> {
        if event.state != KeyState::Pressed {
            return None;
        }
        // 0) El diálogo de polkit es modal: captura el teclado por encima de todo.
        if model.polkit_prompt.is_some() {
            return match &event.key {
                Key::Named(NamedKey::Escape) => Some(Msg::PolkitCancel),
                Key::Named(NamedKey::Backspace) => Some(Msg::PolkitBackspace),
                Key::Named(NamedKey::Enter) => Some(Msg::PolkitSubmit),
                Key::Character(s) => s.chars().next().map(Msg::PolkitChar),
                _ => None,
            };
        }
        // 0) Super+E abre/cierra el front universal de nahual (file manager).
        //    Con su drawer abierto, el teclado va al módulo (Esc / Super+E cierran).
        if event.modifiers.meta {
            if let Key::Character(s) = &event.key {
                if s.eq_ignore_ascii_case("e") {
                    return Some(Msg::NahualToggle);
                }
            }
        }
        if model.nahual.open {
            if let Key::Named(NamedKey::Escape) = &event.key {
                return Some(Msg::NahualToggle);
            }
            if let Some(inner) = &model.nahual.inner {
                if let Some(m) = nahual_module::on_key(inner, event) {
                    return Some(Msg::Nahual(m));
                }
            }
            return None;
        }
        // 1) El hotkey del shuma_input abre/cierra el drawer (prioridad).
        if model.shuma.present {
            if let Some(hk) = &model.shuma.hotkey {
                if keys::matches(hk, &event.key) {
                    return Some(Msg::ShumaToggle);
                }
            }
        }
        // 2) Con el drawer abierto, el teclado va al **shell real**. Ctrl+Shift+Q
        // repliega (el shell sigue vivo); todo lo demás —Esc/Ctrl+C/flechas/Tab/
        // texto— va al módulo, que decide entre su input de línea y el PTY/TUI.
        // La `W` es «cerrar pestaña» de shuma (reparto de terminal, ver
        // `layer/event_handlers`), y sólo repliega en el path bare, que no tiene
        // pestañas que cerrar.
        if model.shuma.open {
            let m = &event.modifiers;
            if m.ctrl && m.shift {
                if let Key::Character(s) = &event.key {
                    if s.eq_ignore_ascii_case("q")
                        || (model.shuma_full.is_none() && s.eq_ignore_ascii_case("w"))
                    {
                        return Some(Msg::ShumaToggle);
                    }
                }
            }
            // Live-wire: con la shuma completa montada, la tecla la traduce ella
            // según su foco interno (input de la sesión activa, PTY/TUI, rails).
            if let Some(full) = &model.shuma_full {
                // Esc repliega el drawer SALVO que shuma tenga algo propio que
                // descartar (modal/dropdown/campo) o el shell corra una TUI de
                // pantalla completa que necesite el Esc — ahí se lo dejamos a ella.
                if matches!(&event.key, Key::Named(NamedKey::Escape))
                    && shuma_app::escape_closes_drawer(full)
                {
                    return Some(Msg::ShumaToggle);
                }
                return shuma_app::on_key(full, event).map(lift_shuma);
            }
            // Módulo bare (sin shuma completa): Esc repliega el drawer.
            if matches!(&event.key, Key::Named(NamedKey::Escape)) {
                return Some(Msg::ShumaToggle);
            }
            return Some(Msg::ShumaShell(shuma_module_shell::Msg::Key(event.clone())));
        }
        // 2.45) Con el campo de contraseña Wi-Fi abierto, el teclado va al campo.
        if model.net_password.is_some() {
            return match &event.key {
                Key::Named(NamedKey::Escape) => Some(Msg::NetworkPasswordCancel),
                Key::Named(NamedKey::Backspace) => Some(Msg::NetworkPasswordBackspace),
                Key::Named(NamedKey::Enter) => Some(Msg::NetworkPasswordSubmit),
                Key::Character(s) => s.chars().next().map(Msg::NetworkPasswordChar),
                _ => None,
            };
        }
        // 2.46) Con el diálogo Khipu abierto, el teclado va al borrador de la nota.
        if model.khipu_open && model.khipu_input.is_some() {
            return match &event.key {
                Key::Named(NamedKey::Escape) => Some(Msg::KhipuPanel), // cierra
                Key::Named(NamedKey::Backspace) => Some(Msg::KhipuBackspace),
                Key::Named(NamedKey::Enter) => Some(Msg::KhipuSubmit),
                Key::Character(s) => s.chars().next().map(Msg::KhipuChar),
                _ => None,
            };
        }
        // 2.5) Con el menú de inicio abierto, el teclado va al buscador.
        if model.menu_open {
            return match &event.key {
                Key::Named(NamedKey::Escape) => Some(Msg::StartToggle),
                Key::Named(NamedKey::Backspace) => Some(Msg::StartBackspace),
                Key::Named(NamedKey::Enter) => Some(Msg::StartLaunchFirst),
                Key::Character(s) => s.chars().next().map(Msg::StartChar),
                _ => None,
            };
        }
        // 2.6) Con el popup del portapapeles o el panel del reloj abierto, Esc
        // los cierra.
        if model.clip_open {
            if let Key::Named(NamedKey::Escape) = &event.key {
                return Some(Msg::ClipboardMenu);
            }
        }
        if model.clock_open {
            if let Key::Named(NamedKey::Escape) = &event.key {
                return Some(Msg::ClockPanel);
            }
        }
        if model.cpu_open {
            if let Key::Named(NamedKey::Escape) = &event.key {
                return Some(Msg::CpuPanel);
            }
        }
        if model.ram_open {
            if let Key::Named(NamedKey::Escape) = &event.key {
                return Some(Msg::RamPanel);
            }
        }
        if model.volume_open {
            if let Key::Named(NamedKey::Escape) = &event.key {
                return Some(Msg::VolumePanel);
            }
        }
        if model.brightness_open {
            if let Key::Named(NamedKey::Escape) = &event.key {
                return Some(Msg::BrightnessPanel);
            }
        }
        // 2.7) Con el panel RAG desplegado, el teclado va a su buscador: texto a
        // la consulta, Enter pregunta, Esc cierra el panel.
        if rag_panel_open(model) {
            return match &event.key {
                Key::Named(NamedKey::Escape) => Some(Msg::NavClosePanel),
                Key::Named(NamedKey::Backspace) => Some(Msg::RagBackspace),
                Key::Named(NamedKey::Enter) => Some(Msg::RagSubmit),
                Key::Character(s) => s.chars().next().map(Msg::RagChar),
                _ => None,
            };
        }
        // 3) Con el menú "Abrir con…" abierto, Esc lo cierra primero.
        if model.nav.menu.is_some() {
            if let Key::Named(NamedKey::Escape) = &event.key {
                return Some(Msg::NavMenuCancel);
            }
        }
        // 4) Con el panel navegador desplegado, Esc lo cierra (no la app).
        if !model.nav.open.is_empty() {
            if let Key::Named(NamedKey::Escape) = &event.key {
                return Some(Msg::NavClosePanel);
            }
        }
        // 5) Sin nada abierto, Esc cierra la app.
        match &event.key {
            Key::Named(NamedKey::Escape) => Some(Msg::Quit),
            _ => None,
        }
    }

    fn on_wheel(
        model: &Model,
        delta: WheelDelta,
        cursor: (f32, f32),
        modifiers: Modifiers,
    ) -> Option<Msg> {
        // Live-wire: con el drawer de la shuma completa abierto, la rueda
        // desplaza su contenido (salida de la sesión, listas, paneles).
        if model.shuma.open {
            if let Some(full) = &model.shuma_full {
                return shuma_app::on_wheel(full, delta, cursor, modifiers).map(lift_shuma);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `true` si `pid` existe y está en estado `Z` (zombi) según `/proc`. Si el
    /// proceso ya fue cosechado del todo, `/proc/<pid>` no existe → no es zombi.
    /// El `comm` del campo 2 puede traer espacios y paréntesis, así que se lee
    /// desde el ÚLTIMO `)` en adelante (la receta canónica de `proc(5)`).
    fn es_zombi(pid: u32) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        let Some(resto) = stat.rsplit_once(')').map(|(_, r)| r) else {
            return false;
        };
        resto.split_whitespace().next() == Some("Z")
    }

    /// El [`cosechador`] no deja `<defunct>`. La regresión que arregla se midió en
    /// metal (2026-07-24): 51 zombis `mirada-ctl` colgando de pata tras 12 h de
    /// sesión, porque los lanzamientos desacoplados descartaban el `Child`.
    #[test]
    fn el_cosechador_no_deja_zombis() {
        let pids: Vec<u32> = (0..8)
            .map(|_| {
                let hijo = std::process::Command::new("true").spawn().expect("lanzar /bin/true");
                let pid = hijo.id();
                desacoplar(Ok(hijo));
                pid
            })
            .collect();
        // El hilo sondea cada 2 s; damos margen de sobra antes de fallar.
        for _ in 0..100 {
            if !pids.iter().copied().any(es_zombi) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let quedan: Vec<u32> = pids.into_iter().filter(|&p| es_zombi(p)).collect();
        panic!("quedaron zombis sin cosechar: {quedan:?}");
    }

    #[test]
    fn cuentas_ssh_automaticas_entran_a_la_flota() {
        let mut ssh = cuentas::CuentasSsh::default();
        // Automática con host → entra como host de flota.
        let a = ssh.add("vps", "VPS");
        {
            let c = ssh.get_mut(&a).unwrap();
            c.host = "10.0.0.9".into();
            c.user = "root".into();
            c.port = 2222;
            c.automatica = true;
            c.tags = vec!["prod".into()];
        }
        // NO automática → no entra.
        ssh.add("nas", "NAS");
        // Automática pero sin host → se ignora (nada que alcanzar).
        let sinhost = ssh.add("fantasma", "Fantasma");
        ssh.get_mut(&sinhost).unwrap().automatica = true;

        let inv = sumar_ssh_automaticas(None, &ssh).expect("hay automáticas → inventario");
        let nombres: Vec<&str> = inv.hosts().map(|h| h.name.as_str()).collect();
        assert_eq!(nombres, vec!["vps"], "sólo la automática con host");
        let h = inv.host("vps").unwrap();
        assert_eq!(h.address, "10.0.0.9");
        assert_eq!(h.ssh_user(), "root");
        assert_eq!(h.ssh_port(), 2222);
        assert!(h.has_tag("prod"));
    }

    #[test]
    fn sin_automaticas_no_toca_la_flota() {
        let mut ssh = cuentas::CuentasSsh::default();
        ssh.add("nas", "NAS"); // no automática
        assert!(sumar_ssh_automaticas(None, &ssh).is_none(), "sin automáticas → None");
        // Y un inventario existente pasa intacto.
        let inv = matilda_core::Inventory::new();
        assert!(sumar_ssh_automaticas(Some(inv), &ssh).is_some());
    }

    #[test]
    fn no_pisa_un_host_del_inventario_con_igual_nombre() {
        let mut ssh = cuentas::CuentasSsh::default();
        let id = ssh.add("edge", "Edge");
        {
            let c = ssh.get_mut(&id).unwrap();
            c.host = "1.1.1.1".into();
            c.automatica = true;
        }
        let mut inv = matilda_core::Inventory::new();
        inv.add_host(matilda_core::Host::new("edge", "9.9.9.9"));
        let merged = sumar_ssh_automaticas(Some(inv), &ssh).unwrap();
        // El del inventario gana (no lo pisa la cuenta).
        assert_eq!(merged.host("edge").unwrap().address, "9.9.9.9");
    }

    #[test]
    fn historial_dedup_y_tope() {
        let mut h = Vec::new();
        assert!(push_clip_history(&mut h, &Some("a".into())), "clip nuevo");
        assert!(push_clip_history(&mut h, &Some("b".into())));
        assert!(push_clip_history(&mut h, &Some("a".into())), "re-copia: a vuelve al frente"); // nuevo evento
        assert_eq!(h, vec!["a".to_string(), "b".to_string()]);
        // vacío y repetido del tope se ignoran → no es clip nuevo (false)
        assert!(!push_clip_history(&mut h, &Some(String::new())));
        assert!(!push_clip_history(&mut h, &Some("a".into())), "ya es el tope");
        assert!(!push_clip_history(&mut h, &None));
        assert_eq!(h.len(), 2);
        // tope
        for i in 0..30 {
            push_clip_history(&mut h, &Some(format!("x{i}")));
        }
        assert_eq!(h.len(), CLIP_HISTORY_MAX);
    }

    #[test]
    fn evento_clip_preview_y_payload() {
        use willay_core::{Clase, Payload};
        let e = evento_clip("primera línea\nsegunda", 42);
        assert_eq!(e.clase, Clase::Clip);
        assert_eq!(e.ts_usec, 42);
        assert_eq!(e.origen, "portapapeles");
        assert_eq!(e.titulo, "primera línea", "título = 1ra línea");
        assert_eq!(e.cuerpo, "primera línea\nsegunda", "cuerpo = texto completo (búsqueda)");
        assert!(matches!(e.payload, Payload::Texto(t) if t == "primera línea\nsegunda"));
    }

    #[test]
    fn clock_draft_ajusta_con_wrap_y_clamp() {
        let mut d = ClockDraft {
            year: 2026,
            month: 12,
            day: 1,
            hour: 23,
            minute: 59,
        };
        d.adjust(1, 1); // mes 12 +1 → 1 (wrap)
        assert_eq!(d.month, 1);
        d.adjust(3, 1); // hora 23 +1 → 0 (wrap)
        assert_eq!(d.hour, 0);
        d.adjust(4, 1); // min 59 +1 → 0 (wrap)
        assert_eq!(d.minute, 0);
        d.adjust(0, -1000); // año clamp inferior
        assert_eq!(d.year, 1970);
        d.adjust(2, 100); // día clamp superior
        assert_eq!(d.day, 31);
    }

    #[test]
    fn clock_draft_stamp() {
        let d = ClockDraft {
            year: 2026,
            month: 6,
            day: 5,
            hour: 9,
            minute: 7,
        };
        assert_eq!(d.stamp(), "2026-06-05 09:07:00");
    }
}
