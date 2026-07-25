//! El cabezal `shuma_input` y su drawer **Quake** — hospeda el **shell real** de
//! shuma.
//!
//! La frontera del SDD §5: el marco (`pata`) provee el borde; `shuma` provee el
//! contenido. `shuma_input` es el cabezal que vive en una barra; al activarlo
//! (click o hotkey) el frontend **despliega un drawer** estilo Quake sobre el
//! escritorio que **monta el módulo [`shuma_module_shell`]** —exactamente el
//! mismo shell de `shuma-shell-llimphi`: cards por comando, etapas de pipe
//! clickeables, cuerpo IDE-text read-only, barra de scroll arrastrable y
//! detección PTY/TUI (vim/htop a pantalla completa)—.
//!
//! pata **no reimplementa** nada del shell (Regla 2: la lógica de dominio no sabe
//! quién la pinta): instancia el [`shuma_module_shell::State`], le rutea las
//! teclas (`Msg::Key`), el latido que drena la salida (`Msg::Tick`) y los clicks
//! —que el `view` ya emite envueltos por el `lift` [`Msg::ShumaShell`]— y pinta
//! su `view`. Esto reemplaza de un saque las dos viejas reimplementaciones: las
//! cards propias del path winit y el terminal PTY aparte del path layer-shell.

use llimphi_motion::Tween;
use llimphi_theme::Theme;
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::View;

use pata_core::WidgetSpec;
use shuma_module::Source;

use crate::{shuma_app, Msg};

/// Alto máximo del drawer (path winit), como fracción de la pantalla.
const DRAWER_FRAC: f32 = 0.45;

/// **Cómo se abrió el drawer** — gobierna qué gestos lo cierran (SDD §5 UX). El
/// principio: cierras con la misma clase de gesto con que abriste.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenMode {
    /// **Vistazo** transitorio: se abrió con un gesto liviano (click en el input
    /// sin ejecutar, o la esquina caliente). Se cierra fácil — el mismo gesto de
    /// nuevo, un click en el input aún sin tipear, `Escape`, o al perder el foco
    /// de teclado / que el puntero se vaya (deshover). Es el default: abrir sin
    /// comprometerse no debe secuestrar el escritorio.
    #[default]
    Fugaz,
    /// **Firme**: se abrió *pidiendo salida* (Enter / botón enviar) — estás
    /// trabajando en la salida. Acaparador: sobrevive a los gestos livianos
    /// (deshover, perder foco) para no evaporarse mientras lees; sólo lo cierran
    /// acciones deliberadas — `Escape`, el botón ✕/─ o `Ctrl+Shift+Q`.
    Firme,
}

impl OpenMode {
    /// `true` si este modo se cierra con gestos **livianos** (deshover, perder el
    /// foco de teclado, re-click en el input vacío). Sólo el vistazo [`Fugaz`].
    pub fn cierra_por_gesto_liviano(self) -> bool {
        matches!(self, OpenMode::Fugaz)
    }
}

/// Qué hacer ante un **click sobre el input** (no hover) según el estado del
/// drawer — la lógica pura del «toggle del vistazo». Ver [`ShumaState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickInputAccion {
    /// Abrir el drawer como vistazo [`OpenMode::Fugaz`].
    AbrirFugaz,
    /// Cerrarlo (era un vistazo y el input sigue vacío: el mismo click lo repliega).
    Cerrar,
    /// No tocar la apertura — sólo enfocar (ya está abierto con texto, o es firme).
    SoloEnfocar,
}

/// Decide la acción de un click en el input a partir del estado del drawer.
/// Pura y testeable: `open`=desplegado, `mode`=cómo se abrió, `input_vacio`=si
/// el input no tiene texto (el «sin haber tipeado» del pedido).
pub fn accion_click_input(open: bool, mode: OpenMode, input_vacio: bool) -> ClickInputAccion {
    match (open, mode) {
        // Cerrado → un click SÓLO enfoca la barra fina (el compositor ya le da el
        // teclado); NO abre el drawer. Así, al tipear con el drawer plegado, aparece
        // el completado flotante bonito (gate `!shuma.open`). El drawer se despliega
        // con Enter (para ver la salida), no con el click.
        (false, _) => ClickInputAccion::SoloEnfocar,
        // Vistazo abierto y sin tipear → el mismo click lo cierra (toggle).
        (true, OpenMode::Fugaz) if input_vacio => ClickInputAccion::Cerrar,
        // Vistazo con texto, o modo firme → sólo enfocar (no lo replegamos).
        (true, _) => ClickInputAccion::SoloEnfocar,
    }
}

/// El estado del cabezal del shell y su drawer. Vive en el `Model` del frontend
/// —es interacción, no modelo de dominio—, no en `pata-core`. El **contenido**
/// del drawer es el shell real, hospedado en [`ShumaState::inner`].
pub struct ShumaState {
    /// `true` cuando el drawer está desplegado.
    pub open: bool,
    /// Cómo se abrió el drawer (gobierna el cierre). Sólo significativo con
    /// `open == true`.
    pub open_mode: OpenMode,
    /// El **shell real**, hospedado como módulo. Fuente de verdad del contenido
    /// (input, runs, historial, cwd, PTY/TUI). pata sólo le rutea eventos y lo
    /// pinta; nunca toca sus campos directamente.
    pub inner: shuma_module_shell::State,
    /// Hotkey que abre/cierra el drawer (de la prop `hotkey`), o `None`.
    pub hotkey: Option<String>,
    /// Prompt al frente del cabezal (`›`, `$`, …).
    pub prompt: String,
    /// Texto del cabezal cuando el drawer está plegado.
    pub placeholder: String,
    /// Animación de despliegue `0..1` (0 = replegado, 1 = desplegado).
    pub anim: Tween<f32>,
    /// `true` si el config declaró algún `shuma_input` (si no, no hay cabezal
    /// ni drawer).
    pub present: bool,
    /// `true` si el drawer está maximizado (ocupa casi toda la pantalla en vez
    /// del 45% por defecto). Lo conmuta el botón ▢ de la barra de título.
    pub maximized: bool,
    /// Alto del drawer como fracción de la pantalla, si el usuario lo arrastró
    /// (handle de resize). `None` = usa `maximized`/`shuma_height`. Arrastrar el
    /// borde lo fija y sale de `maximized`.
    pub height_frac: Option<f32>,
}

impl Default for ShumaState {
    fn default() -> Self {
        Self {
            open: false,
            open_mode: OpenMode::Fugaz,
            inner: shuma_module_shell::State::new(Source::Local),
            hotkey: None,
            prompt: "›".into(),
            placeholder: "shuma".into(),
            anim: Tween::idle(0.0),
            present: false,
            maximized: true, // el drawer arranca full-height; ▢ lo achica al shuma_height
            height_frac: None,
        }
    }
}

impl ShumaState {
    /// Construye el estado desde la spec del `shuma_input` (prompt/placeholder/
    /// hotkey). Marca `present = true`. Se RE-ADJUNTA a la sesión persistente
    /// del daemon que quedó montada (si sigue viva): reiniciar el compositor
    /// respawnea pata, y claude —o lo que corriera— reaparece donde estaba.
    pub fn from_spec(spec: &WidgetSpec) -> Self {
        let hotkey = spec.str_prop("hotkey", "");
        Self {
            prompt: spec.str_prop("prompt", "›").to_string(),
            placeholder: spec.str_prop("placeholder", "shuma").to_string(),
            hotkey: if hotkey.is_empty() {
                None
            } else {
                Some(hotkey.to_string())
            },
            present: true,
            // Con la shuma COMPLETA activa el drawer pinta el chasis: el
            // reattach va a su sesión activa (`auto_reattach_activa`, lo hace
            // el host al construirla). Hacerlo también aquí dejaba un adjunto
            // INVISIBLE en `inner` que re-dimensionaba el PTY y robaba el
            // stream sin que el drawer lo mostrara jamás.
            inner: if crate::shuma_full_enabled() {
                shuma_module_shell::State::new(Source::Local)
            } else {
                shuma_module_shell::auto_reattach(shuma_module_shell::State::new(
                    Source::Local,
                ))
            },
            ..Self::default()
        }
    }

    /// `true` si el drawer debe pintarse (abierto o aún animando el cierre).
    pub fn visible(&self) -> bool {
        self.open || self.anim.value() > 0.01
    }
}

/// El cabezal de la barra: **el input vivo del shell**. No es un placeholder ni
/// un cabezal-rótulo — es el mismísimo `shell_input_view` del shell hospedado,
/// llevado a la barra. Tecleas aquí, las teclas las recibe el shell, Enter ejecuta.
/// Click en el chip → despliega el drawer (para ver la salida); el shell además
/// recibe `FocusInput` por su propio `on_click` interno.
pub fn headline_view(
    state: &ShumaState,
    data: &crate::render::BarData,
    theme: &Theme,
) -> View<Msg> {
    let full = data.shuma_full;
    // Iconos fugaces sobre la derecha del input (aparecen sólo cuando importan,
    // se desvanecen al acercarse el texto). El largo del input es el proxy del
    // fade — pero en modo live-wire el input que tecleas es el de la SESIÓN
    // ACTIVA (`full`), no el `inner` bare (que queda vacío): usar `inner` dejaba
    // `input_len=0` y los iconos NUNCA se escondían aunque el texto llegara
    // hasta ellos. Se toma el largo de la sesión activa cuando está montada.
    let shell = full.and_then(shuma_app::active_shell_state).unwrap_or(&state.inner);
    let avance = shuma_module_shell::input_avance_en_fila(shell);
    let char_w = shuma_module_shell::input_char_w_px(shell);
    let overlay = iconos_fugaces(data, avance, char_w, data.revelar_alpha, theme);
    // Label flotante pwd/git sobre el borde superior del input (estilo Flutter
    // del boceto): el cwd sale de la sesión activa en live-wire, o del inner bare.
    let cwd = full
        .and_then(shuma_app::active_cwd)
        .unwrap_or_else(|| state.inner.cwd.clone());
    let etiqueta = etiqueta_flotante(&cwd, theme);
    // Live-wire: con la shuma completa montada, el cabezal ES el input vivo de
    // la **sesión activa** de la shuma (mismo `shell_input_view`, ruteado a esa
    // sesión vía `lift_shuma`), no un chip. Tipear aquí ejecuta en esa sesión y
    // FocusInput despliega el drawer. Si la activa no es un shell (form de nueva
    // sesión), caemos al chip como fallback.
    if let Some(full) = full {
        if let Some(input) = shuma_app::active_input_view(full, theme, crate::lift_shuma) {
            let mut children = vec![input, etiqueta, overlay];
            if let Some(h) = hairline_progreso(data.progreso, theme) {
                children.push(h);
            }
            return wrap_headline(children, state.open);
        }
        return headline_chip(state, theme);
    }
    let input = shuma_module_shell::input_view(&state.inner, theme, Msg::ShumaShell);
    let mut children = vec![input, etiqueta];
    // A6 — aviso de comando largo: cuando el drawer está plegado (no estás
    // mirando la salida) y terminó algún comando largo, el cabezal gana un punto
    // ámbar. Es el equivalente en pata de la badge del diente del chasis; al
    // abrir el drawer se acusa y desaparece. Sin notificaciones del sistema.
    if !state.open && state.inner.long_alerts() > 0 {
        children.push(long_alert_badge());
    }
    children.push(overlay);
    if let Some(h) = hairline_progreso(data.progreso, theme) {
        children.push(h);
    }
    wrap_headline(children, state.open)
}

/// Label flotante **pwd/git** sobre el borde superior del input (estilo Flutter
/// outlined-field del boceto `barra_mando_demo`): va a caballo del borde, con el
/// color de la barra detrás para "notchar" el borde. pwd abreviado con `~`, rama
/// git en acento.
fn etiqueta_flotante(cwd: &std::path::Path, theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_text::Alignment;
    let pretty = match std::env::var("HOME").ok().filter(|h| !h.is_empty()) {
        Some(home) if cwd.starts_with(&home) => {
            format!("~{}", cwd.to_string_lossy().trim_start_matches(&home))
        }
        _ => cwd.to_string_lossy().into_owned(),
    };
    let rama = rama_git(cwd);
    // Ancho POR CONTENIDO (`auto`), no fijo: el hueco/notch del borde se ajusta al
    // largo real del pwd (y de la rama). Antes eran cajas fijas (240/96/344 px) y
    // el hueco quedaba siempre del mismo tamaño sin importar el texto.
    let pwd = View::new(Style {
        size: Size { width: auto(), height: length(15.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .text_aligned(pretty, 11.0, theme.fg_text, Alignment::Start)
    .text_weight(600.0);
    let mut children = vec![pwd];
    // El chip de rama sólo se agrega si hay repo: sin él, el hueco no reserva su
    // ancho (ni el gap) — se ciñe al pwd.
    if let Some(b) = rama {
        let git = View::new(Style {
            size: Size { width: auto(), height: length(15.0_f32) },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .text_aligned(format!("· {b}"), 11.0, theme.accent, Alignment::Start);
        children.push(git);
    }
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(14.0_f32),
            top: length(-8.0_f32),
            right: auto(),
            bottom: auto(),
        },
        flex_direction: FlexDirection::Row,
        align_items: Some(AlignItems::Center),
        size: Size { width: auto(), height: length(16.0_f32) },
        gap: Size { width: length(6.0_f32), height: length(0.0_f32) },
        padding: TaffyRect {
            left: length(6.0_f32),
            right: length(6.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(3.0)
    .children(children)
}

/// Rama git de `cwd` (o de un ancestro): lee `.git/HEAD`. Sin dependencias.
fn rama_git(cwd: &std::path::Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        if let Ok(s) = std::fs::read_to_string(d.join(".git/HEAD")) {
            let s = s.trim();
            return Some(
                s.strip_prefix("ref: refs/heads/")
                    .map(str::to_string)
                    .unwrap_or_else(|| s.chars().take(7).collect()),
            );
        }
        dir = d.parent();
    }
    None
}

/// Ancho de la **zona de controles fantasma** (la franja caliente del borde
/// derecho del input). Siempre está montada aunque no haya iconos: es lo que el
/// puntero "toca" para revelarlos todos.
const ZONA_FANTASMA_W: f32 = 168.0;

/// Identidad estable de cada control fantasma. Sirve para tres cosas: el turno
/// rotativo de los salientes leves, el orden pegajoso durante el reveal (los ya
/// visibles no se corren de lugar) y el pin por hover (el icono bajo el mouse
/// no se oculta aunque su condición caiga).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fugaz {
    /// **Sonido** unificado (antes música + volumen): cava cuando suena algo,
    /// rampa creciente/decreciente al mover el volumen, altavoz con intensidad
    /// de color por % (o tachado en mute) cuando el audio está idle.
    Sonido,
    /// **Cielo** unificado (antes sol + eclipse + luna + signo): un solo icono
    /// que alterna las caras astrales; la cara saliente manda cuando la hay.
    Cielo,
    Cpu,
    Flota,
    /// **Control** multifacético (antes batería + ánimo): brillo por defecto,
    /// batería cuando hay que acusarla (cable/baja) o en su turno de cara.
    Control,
    /// **Red**: la interfaz conectada (Wi-Fi con arcos por señal / cable) +
    /// dos microbarras verticales con el tráfico ↓rx ↑tx.
    Red,
    /// **Clima**: el cielo meteorológico, colorido y animado.
    Clima,
    Khipu,
    Tampu,
    Captura,
    Usb,
}

impl Fugaz {
    /// Todos los fantasmas, en orden canónico. Base del snapshot de asientos
    /// [`orden_asientos`] (el orden congelado mientras el puntero anda cerca).
    pub const TODOS: [Fugaz; 11] = [
        Fugaz::Sonido,
        Fugaz::Cielo,
        Fugaz::Cpu,
        Fugaz::Flota,
        Fugaz::Control,
        Fugaz::Red,
        Fugaz::Clima,
        Fugaz::Khipu,
        Fugaz::Tampu,
        Fugaz::Captura,
        Fugaz::Usb,
    ];

    /// Nombre estable para persistir el uso (no cambia aunque el enum se
    /// reordene). Es la clave del archivo `fugaces-uso.json`. Los nombres de
    /// fantasmas retirados (musica/volumen/sol/luna/…) quedan huérfanos en el
    /// JSON viejo y simplemente se ignoran.
    pub fn nombre(self) -> &'static str {
        match self {
            Fugaz::Sonido => "sonido",
            Fugaz::Cielo => "cielo",
            Fugaz::Cpu => "cpu",
            Fugaz::Flota => "flota",
            Fugaz::Control => "control",
            Fugaz::Red => "red",
            Fugaz::Clima => "clima",
            Fugaz::Khipu => "khipu",
            Fugaz::Tampu => "tampu",
            Fugaz::Captura => "captura",
            Fugaz::Usb => "usb",
        }
    }
}

/// **Prior de utilidad** de cada fantasma: la estimación inicial de cuánto le
/// sirve al usuario (más alto = más útil = se sienta más a la **derecha**).
/// Es el arranque del orden aprendido: sin uso registrado manda el prior; cada
/// click real lo va corrigiendo vía [`FugazUso`].
pub fn prior(f: Fugaz) -> f32 {
    match f {
        Fugaz::Sonido => 9.0,
        Fugaz::Red => 8.0,
        Fugaz::Control => 8.0,
        Fugaz::Clima => 7.0,
        Fugaz::Usb => 7.0,
        Fugaz::Cpu => 6.0,
        Fugaz::Captura => 5.0,
        Fugaz::Khipu => 5.0,
        Fugaz::Flota => 4.0,
        Fugaz::Tampu => 3.0,
        Fugaz::Cielo => 2.0,
    }
}

/// **Uso aprendido** de los fantasmas: cuenta los clicks de cada uno y persiste
/// en `~/.local/share/pata/fugaces-uso.json` (escritura atómica, silencioso).
/// El puntaje de asiento es `prior + 2·√usos`: los primeros clicks mueven el
/// asiento rápido y después satura — el orden queda «más o menos fijo».
#[derive(Default)]
pub struct FugazUso {
    usos: std::collections::HashMap<String, u32>,
    path: Option<std::path::PathBuf>,
}

impl FugazUso {
    /// Abre (o crea vacío) el registro de uso. Nunca falla: sin archivo o con
    /// JSON roto arranca de cero (el prior manda).
    pub fn open() -> Self {
        let path = ruta_uso();
        let usos = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { usos, path }
    }

    /// Cuántos clicks lleva `f`.
    pub fn usos(&self, f: Fugaz) -> u32 {
        self.usos.get(f.nombre()).copied().unwrap_or(0)
    }

    /// Registra un uso (click) de `f` y persiste.
    pub fn bump(&mut self, f: Fugaz) {
        *self.usos.entry(f.nombre().to_string()).or_insert(0) += 1;
        self.persist();
    }

    /// Puntaje de asiento de `f`: prior + uso aprendido (√ para saturar).
    pub fn score(&self, f: Fugaz) -> f32 {
        prior(f) + 2.0 * (self.usos(f) as f32).sqrt()
    }

    fn persist(&self) {
        let Some(path) = &self.path else { return };
        let Ok(bytes) = serde_json::to_vec_pretty(&self.usos) else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// `~/.local/share/pata/fugaces-uso.json` (respeta `XDG_DATA_HOME`), o `None`
/// si no se puede resolver `HOME` (el uso queda en memoria).
fn ruta_uso() -> Option<std::path::PathBuf> {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("HOME").ok().map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })?;
    Some(base.join("pata").join("fugaces-uso.json"))
}

/// El **orden de asientos** vigente: todos los fantasmas ordenados por puntaje
/// ascendente (el más útil/usado queda último = más a la derecha). Es el
/// snapshot que el modelo **congela** cuando el puntero se acerca a la zona:
/// mientras esté congelado, un click (que bumpea el uso) NO recoloca los
/// iconos bajo el mouse — el asiento nuevo recién rige al esfumarse el reveal.
pub fn orden_asientos(uso: Option<&FugazUso>) -> Vec<Fugaz> {
    let puntaje = |f: Fugaz| uso.map(|u| u.score(f)).unwrap_or_else(|| prior(f));
    let mut orden = Fugaz::TODOS.to_vec();
    orden.sort_by(|a, b| {
        puntaje(*a).partial_cmp(&puntaje(*b)).unwrap_or(core::cmp::Ordering::Equal)
    });
    orden
}

/// La **acción de click** de cada fantasma (qué diálogo abre), en un solo lugar:
/// la vista arma el click como [`Msg::FugazClick`] y el update la resuelve aquí
/// después de registrar el uso. `None` = el icono no abre nada (sólo narra).
pub fn accion_fugaz(f: Fugaz) -> Option<Msg> {
    match f {
        Fugaz::Sonido => Some(Msg::VolumePanel),
        Fugaz::Cielo => Some(Msg::CieloPanel),
        Fugaz::Cpu => None,
        Fugaz::Flota | Fugaz::Control => Some(Msg::ControlToggle),
        Fugaz::Red => Some(Msg::NetworkToggle),
        Fugaz::Clima => Some(Msg::ClimaPanel),
        Fugaz::Khipu => Some(Msg::KhipuPanel),
        Fugaz::Tampu => Some(Msg::TampuPanel),
        Fugaz::Captura => Some(Msg::CapturaPanel),
        Fugaz::Usb => Some(Msg::UsbPanel),
    }
}

/// Cadencia del turno de los fantasmas **leves** (µs): con varios salientes a
/// la vez no se apilan fijos — se turnan de a uno, como la marquesina.
pub const FUGAZ_ROT_US: u64 = 4_000_000;

/// Avanza el turno de los fantasmas leves cada [`FUGAZ_ROT_US`]. `congelar`
/// (reveal activo o icono pinneado por hover) detiene el turno **y** re-estampa
/// el reloj — así el orden no cambia bajo el mouse y al soltarlo el próximo
/// giro tarda el intervalo completo. Devuelve `true` si rotó (repintar).
pub fn avanzar_fugaz_idx(idx: &mut usize, reloj: &mut u64, ahora_us: u64, congelar: bool) -> bool {
    if congelar {
        *reloj = ahora_us;
        return false;
    }
    if ahora_us.saturating_sub(*reloj) >= FUGAZ_ROT_US {
        *idx = idx.wrapping_add(1);
        *reloj = ahora_us;
        true
    } else {
        false
    }
}

/// **Snapshot congelado** de los fugaces mientras el puntero anda cerca de la
/// zona (hover o pin): estampa TODO lo que decide posiciones —orden de
/// asientos, split visibles/ocultos, membresía y el reloj de caras— para que
/// nada se recoloque bajo el mouse. Se estampa al entrar (RevealFantasmas /
/// FantasmaPin) y se libera cuando la zona se esfuma del todo.
#[derive(Clone, Debug)]
pub struct FugazFreeze {
    /// Orden de asientos vigente al congelar ([`orden_asientos`]).
    pub orden: Vec<Fugaz>,
    /// Los que estaban visibles al congelar (van al frente, a la derecha):
    /// el split frente/fondo NO se recomputa con el pin/salience vivos.
    pub visibles: Vec<Fugaz>,
    /// Los candidatos presentes al congelar: un fugaz cuyo dato llega a mitad
    /// del hover NO se inserta (aparecería corriendo a los demás); uno cuyo
    /// dato se va deja un hueco del mismo ancho (nadie se mueve).
    pub presentes: Vec<Fugaz>,
    /// Reloj (µs) estampado al congelar: fija `cara_reloj` (los iconos
    /// multifacéticos no alternan glifo bajo el mouse).
    pub reloj_us: u64,
}

/// Computa el [`FugazFreeze`] con los datos vigentes. `data` puede ser un
/// `BarData` parcial (sólo los campos de fugaces): lo construyen los handlers
/// de `RevealFantasmas`/`FantasmaPin` de ambos paths.
pub fn congelar_fugaces(data: &crate::render::BarData, theme: &Theme, ahora_us: u64) -> FugazFreeze {
    let cara_reloj = (ahora_us / 5_000_000) as usize;
    let cands = candidatos_fugaces(data, cara_reloj, false, theme);
    let resumen: Vec<(Fugaz, bool, bool)> =
        cands.iter().map(|(f, s, c, _)| (*f, *s, *c)).collect();
    FugazFreeze {
        orden: orden_asientos(data.fugaz_uso),
        visibles: elegir_visibles(&resumen, data.fugaz_idx, data.fugaz_pin),
        presentes: resumen.iter().map(|r| r.0).collect(),
        reloj_us: ahora_us,
    }
}

/// Regla de visibilidad FUERA del reveal, pura y testeable. `cands` viene en
/// orden canónico como `(id, saliente, crítica)`:
/// - las **críticas** salientes van todas, fijas — lo urgente manda, no rota;
/// - de las **leves** salientes se ve UNA, turnándose por `idx`;
/// - el **pinneado** por hover se queda aunque su condición haya caído.
pub fn elegir_visibles(cands: &[(Fugaz, bool, bool)], idx: usize, pin: Option<Fugaz>) -> Vec<Fugaz> {
    let mut vis: Vec<Fugaz> =
        cands.iter().filter(|(_, s, c)| *s && *c).map(|(f, _, _)| *f).collect();
    let leves: Vec<Fugaz> =
        cands.iter().filter(|(_, s, c)| *s && !*c).map(|(f, _, _)| *f).collect();
    if !leves.is_empty() {
        let f = leves[idx % leves.len()];
        if !vis.contains(&f) {
            vis.push(f);
        }
    }
    if let Some(p) = pin {
        if !vis.contains(&p) && cands.iter().any(|(f, _, _)| *f == p) {
            vis.push(p);
        }
    }
    vis
}

/// Overlay de **controles fantasma** ("iconos fugaces") anclado a la derecha del
/// cabezal del input. Cada control tiene una condición de **salience** (algo
/// suena, la CPU se recalienta, la batería está baja, la luna llena/nueva, el
/// Sol ingresa a un signo…). Cada uno tiene además un **asiento aprendido**
/// ([`prior`] + uso persistido en [`FugazUso`]): el más útil/usado se sienta más
/// a la derecha, y como el subconjunto visible se alinea a la derecha (FlexEnd),
/// un icono que aparece solo siempre asoma pegado al borde. Fuera del reveal se
/// ve poco: las **críticas** fijas + UNA leve **turnándose** cada
/// [`FUGAZ_ROT_US`] (nada de tres iconos clavados). Al **acercar el puntero** a
/// la zona se muestran **todos** para interactuar; los que ya estaban visibles
/// **no se corren de lugar** (quedan anclados a la derecha, los demás aparecen a
/// su izquierda) y el turno se congela. El icono **bajo el mouse queda
/// pinneado**: no se oculta aunque su condición caiga. Fuera del reveal, además,
/// los salientes se desvanecen a medida que el texto tipeado se acerca
/// ([`fade_por_texto`]).
///
/// El contenedor está **siempre** presente (con un `hover_fill` transparente que
/// lo hace hit-testeable): es la franja que dispara `RevealFantasmas` al
/// entrar/salir el puntero — y durante el reveal se **ensancha** para cubrir
/// todos los iconos (si no, el mouse "salía" de la franja al recorrerlos y se
/// esfumaban). Overlay absoluto → no empuja el layout del input, y se posa a la
/// izquierda del micrófono (no sobre él).
fn iconos_fugaces(
    data: &crate::render::BarData,
    // (avance del texto en su última fila visual, caracteres que entran en una
    // fila) — ver [`fade_por_texto`].
    avance: (usize, usize),
    // Ancho de un carácter del input en px, medido por su pintor. Traduce el
    // ancho de la franja de iconos a caracteres.
    char_w: f32,
    revelar_alpha: f32,
    theme: &Theme,
) -> View<Msg> {
    // Fase de parpadeo compartida por los iconos que titilan (batería a cero,
    // CPU recaliente): del reloj, ~0.5 s. La barra real repinta continuo.
    let titila = (willay_emit::ahora_usec() / 500_000) % 2 == 0;
    let revelar_alpha = revelar_alpha.clamp(0.0, 1.0);
    let revelando = revelar_alpha > 0.01;
    // Reloj de caras: los iconos multifacéticos (cielo/control) alternan su
    // glifo cada ~5 s. Con el snapshot congelado (puntero cerca) el reloj queda
    // ESTAMPADO: ningún icono cambia de cara bajo el mouse.
    let reloj_us = data.fugaz_fijo.map_or_else(willay_emit::ahora_usec, |f| f.reloj_us);
    let cara_reloj = (reloj_us / 5_000_000) as usize;
    let cands = candidatos_fugaces(data, cara_reloj, titila, theme);
    // Ancho REAL que ocupa hoy la franja: la suma de los slots de los iconos
    // salientes (los que se ven sin reveal) más su separación. No es una
    // constante — con música sonando y batería baja la franja es más ancha que
    // en reposo, y entonces el texto la alcanza antes.
    let n_cores = data.cpu_cores.len();
    let salientes = cands.iter().filter(|(_, sal, _, _)| *sal);
    let ancho_zona: f32 = salientes.map(|(id, ..)| ancho_slot(*id, n_cores) + 6.0).sum();
    let cols_zona = if char_w > 0.1 {
        (ancho_zona / char_w).ceil() as usize
    } else {
        0
    };
    // Opacidad base de los SALIENTES (se desvanecen al acercarse el texto). Los
    // revelados usan `revelar_alpha`. Un icono saliente toma el mayor de ambos
    // (así el reveal lo enciende del todo sin apagarlo cuando el reveal se va).
    let base = fade_por_texto(avance.0, avance.1, cols_zona);
    diag_fugaces(avance, char_w, cols_zona, base, revelar_alpha);
    render_fugaces(data, cands, base, revelar_alpha, revelando, theme)
}

/// Los **candidatos** fugaces en orden canónico: `(identidad, saliente,
/// crítica, vista)`. Las vistas se construyen siempre (baratas); la selección
/// decide cuáles se ven. Es la única fuente de la salience — la usa la vista
/// ([`iconos_fugaces`]) y el snapshot congelado ([`congelar_fugaces`]).
pub(crate) fn candidatos_fugaces(
    data: &crate::render::BarData,
    cara_reloj: usize,
    titila: bool,
    theme: &Theme,
) -> Vec<(Fugaz, bool, bool, View<Msg>)> {
    let mut cands: Vec<(Fugaz, bool, bool, View<Msg>)> = Vec::new();
    let mut agregar =
        |cands: &mut Vec<(Fugaz, bool, bool, View<Msg>)>, id: Fugaz, saliente: bool, critica: bool, v: View<Msg>| {
            cands.push((id, saliente, critica, v));
        };

    // Sonido (música + volumen unificados): cava cuando suena, rampa mientras
    // el volumen se mueve, altavoz con intensidad por % (o mute) en idle.
    // Saliente cuando suena algo, está silenciado o el volumen acaba de cambiar.
    // «Suena algo» = el player MPRIS reporta playing **o el cava trae energía
    // real**: audio sin player (juego, video en un navegador sin MPRIS, beep)
    // también es sonido — antes el icono quedaba en altavoz idle.
    let cava_activo = data.cava.iter().any(|v| *v > 0.02);
    let hay_player = data.media.map(|m| m.has_player).unwrap_or(false);
    let sonando = data.media.map(|m| m.playing).unwrap_or(false) || cava_activo;
    // Con el icono pinneado por hover y un reproductor vivo, el icono se vuelve
    // play/pausa; `Some(playing)` = pintar el transporte (el click lo alterna).
    let transporte = (data.fugaz_pin == Some(Fugaz::Sonido) && hay_player)
        .then(|| data.media.map(|m| m.playing).unwrap_or(false));
    agregar(
        &mut cands,
        Fugaz::Sonido,
        sonando || data.muted || data.vol_evento.is_some(),
        false,
        fugaz_item(
            icono_sonido(data.cava, sonando, data.volume, data.muted, data.vol_evento, transporte, theme),
            Fugaz::Sonido,
        ),
    );
    // CPU: mini-cava por núcleo — saliente con carga alta (leve) o recalentada
    // (crítica: eso sí es urgente).
    if !data.cpu_cores.is_empty() {
        let caliente = data.cpu_temp.map(|t| t >= TEMP_CALIENTE).unwrap_or(false);
        agregar(
            &mut cands,
            Fugaz::Cpu,
            data.cpu > 0.55 || caliente,
            caliente,
            fugaz_item(icono_cpu(data.cpu_cores, caliente, titila, theme), Fugaz::Cpu),
        );
    }
    // Flota (matilda): pila de servidores con un semáforo — saliente sólo cuando
    // hay un problema (contenedor/servicio caído u host inalcanzable), local o
    // remoto. Crítica. Click → centro de control (resumen de la flota).
    if let Some(salud) = data.matilda {
        agregar(
            &mut cands,
            Fugaz::Flota,
            salud.hay_problema(),
            true,
            fugaz_item(icono_flota(salud, titila, theme), Fugaz::Flota),
        );
    }
    // Control (batería + brillo unificados, el centro de control): batería
    // cuando hay que acusarla —ventana del cable o baja descargando, crítica—;
    // si no, alterna las caras brillo/batería con el reloj. Abre el centro de
    // control (batería/brillo/radios/energía).
    {
        let baja = data.bat.map(|(f, c)| !c && f <= 0.30).unwrap_or(false);
        let acusar = baja || data.bat_evento;
        agregar(
            &mut cands,
            Fugaz::Control,
            acusar,
            true,
            fugaz_item(
                icono_control(data.bat, acusar, data.brightness, cara_reloj, titila, theme),
                Fugaz::Control,
            ),
        );
    }
    // Cielo (sol + eclipse + luna + signo unificados): UNA sola cara a la vez.
    // La cara saliente manda (eclipse en víspera > luna llena/nueva > mediodía
    // solar > ingreso a signo); sin salientes, alternan con el reloj. Leve.
    {
        let c = data.cielo;
        let (ilum, creciente, dias_llena) = match c {
            Some(c) => {
                (Some(c.luna_iluminacion), Some(c.luna_creciente), Some(c.luna_dias_a_llena))
            }
            None => (None, None, None),
        };
        let cerca_mediodia = c
            .map(|c| c.tiene_lugar && c.sol_sobre_horizonte && c.hora_angulo_deg.abs() < 7.5)
            .unwrap_or(false);
        let eclipse_cerca = c
            .and_then(|c| c.eclipse_dias)
            .map(|d| (0.0..3.0).contains(&d))
            .unwrap_or(false);
        let cerca_llena = match ilum {
            Some(k) => k > 0.97,
            None => (data.moon_phase - 0.5).abs() < 0.03,
        };
        let cerca_nueva = match ilum {
            Some(k) => k < 0.03,
            None => data.moon_phase.min(1.0 - data.moon_phase) < 0.03,
        };
        let cara = if eclipse_cerca && c.is_some() {
            CaraCielo::Eclipse
        } else if cerca_llena || cerca_nueva {
            CaraCielo::Luna
        } else if cerca_mediodia {
            CaraCielo::Sol
        } else if data.sun_longitude.rem_euclid(30.0) < 1.5 {
            CaraCielo::Signo
        } else {
            // Sin saliente: turno de caras (el sol sólo con cielo computado).
            match cara_reloj % if c.is_some() { 3 } else { 2 } {
                0 => CaraCielo::Luna,
                1 => CaraCielo::Signo,
                _ => CaraCielo::Sol,
            }
        };
        let vista = match cara {
            CaraCielo::Luna => {
                icono_luna(data.moon_phase, ilum, creciente, dias_llena, theme)
            }
            CaraCielo::Signo => icono_signo(data.sun_longitude, theme),
            // Las caras Sol/Eclipse sólo se eligen con `c` presente.
            CaraCielo::Sol => match c {
                Some(c) => icono_sol(c, theme),
                None => icono_luna(data.moon_phase, ilum, creciente, dias_llena, theme),
            },
            CaraCielo::Eclipse => match c {
                Some(c) => icono_eclipse(c, theme),
                None => icono_luna(data.moon_phase, ilum, creciente, dias_llena, theme),
            },
        };
        agregar(
            &mut cands,
            Fugaz::Cielo,
            eclipse_cerca || cerca_llena || cerca_nueva || cerca_mediodia
                || data.sun_longitude.rem_euclid(30.0) < 1.5,
            false,
            fugaz_item(vista, Fugaz::Cielo),
        );
    }
    // Red: la interfaz conectada + tráfico en microbarras. Saliente cuando NO
    // hay conexión (radio apagada o sin asociar) — el aviso útil. Leve.
    if let Some(net) = data.network {
        use crate::network::NetStatus;
        let sin_red = matches!(net.status, NetStatus::WifiOff | NetStatus::Desconectado);
        agregar(
            &mut cands,
            Fugaz::Red,
            sin_red,
            false,
            fugaz_item(icono_red(net, data.net_trafico, theme), Fugaz::Red),
        );
    }
    // Clima: colorido y animado. Saliente cuando el cielo pide paraguas
    // (lluvia/nieve/tormenta). Leve.
    if let Some(w) = data.weather {
        use crate::weather::Sky;
        let feo = matches!(w.sky, Sky::Rain | Sky::Snow | Sky::Storm);
        agregar(
            &mut cands,
            Fugaz::Clima,
            feo,
            false,
            fugaz_item(icono_clima(w, data.anim_t, titila, theme), Fugaz::Clima),
        );
    }
    // Khipu: nota que se desvanece. Saliente cuando alguna está **por caer** del
    // horizonte (última chance de reforzarla ⇒ crítica). Abre la captura rápida.
    if let Some(k) = data.khipu {
        agregar(
            &mut cands,
            Fugaz::Khipu,
            k.hay_por_caer,
            true,
            fugaz_item(icono_khipu(k, titila, theme), Fugaz::Khipu),
        );
    }
    // Común (tampu): saliente cuando hay una **devolución vencida** o una
    // manipulación sobre algo tuyo — ambas piden acción ⇒ crítica.
    if let Some(t) = data.tampu {
        agregar(
            &mut cands,
            Fugaz::Tampu,
            t.hay_vencido || t.hay_anomalia,
            true,
            fugaz_item(icono_tampu(t, titila, theme), Fugaz::Tampu),
        );
    }
    // Captura de pantalla: nunca es «saliente» (es una acción, no un estado) —
    // sólo aparece al revelar la zona. Abre el menú de captura.
    agregar(
        &mut cands,
        Fugaz::Captura,
        false,
        false,
        fugaz_item(icono_captura(data.grabando.is_some(), theme), Fugaz::Captura),
    );
    // Medios extraíbles (USB): saliente cuando hay uno **sin montar** (recién
    // insertado — pide acción ⇒ crítica). Abre el diálogo montar/abrir/expulsar.
    if let Some(u) = data.usb {
        if !u.particiones.is_empty() {
            agregar(
                &mut cands,
                Fugaz::Usb,
                u.hay_sin_montar,
                true,
                fugaz_item(icono_usb(u, titila, theme), Fugaz::Usb),
            );
        }
    }
    cands
}

/// El **ancho de asiento** fijo de cada fugaz: el máximo de todas sus caras.
/// Cada icono vive centrado en un slot de este ancho, así un cambio de cara
/// (cava↔altavoz↔rampa, brillo↔batería, luna↔sol↔signo) **jamás** corre a los
/// vecinos. `n_cores` dimensiona el mini-cava de CPU (una barra por núcleo).
fn ancho_slot(f: Fugaz, n_cores: usize) -> f32 {
    match f {
        Fugaz::Sonido => 44.0,                                        // cava 44 / rampa 30 / altavoz 26
        Fugaz::Cpu => ((n_cores.max(1) as f32) * 3.5 + 4.0).min(44.0),
        Fugaz::Cielo => 20.0,                                         // luna/sol/eclipse/signo: 20
        Fugaz::Control => 28.0,                                       // batería 28 / brillo 20
        Fugaz::Red => 30.0,
        Fugaz::Clima | Fugaz::Flota | Fugaz::Captura => 22.0,
        Fugaz::Tampu => 20.0,
        Fugaz::Khipu | Fugaz::Usb => 14.0,
    }
}

/// Ordena, selecciona y monta los candidatos en la franja fantasma. Con
/// `data.fugaz_fijo` presente (puntero cerca) TODO queda estampado del
/// snapshot: orden, split frente/fondo, membresía y huecos — nada se mueve
/// bajo el mouse.
fn render_fugaces(
    data: &crate::render::BarData,
    mut cands: Vec<(Fugaz, bool, bool, View<Msg>)>,
    base: f32,
    revelar_alpha: f32,
    revelando: bool,
    theme: &Theme,
) -> View<Msg> {
    let n_cores = data.cpu_cores.len();
    // ASIENTO APRENDIDO: orden por puntaje ascendente — el más útil (prior) y
    // más usado (clicks persistidos) se sienta más a la **derecha**. Sort
    // estable: a igual puntaje sobrevive el orden canónico de arriba.
    // Con el puntero cerca el modelo pasa el **snapshot congelado**
    // ([`congelar_fugaces`] estampado al arrancar el reveal): un click bumpea
    // el uso pero NO recoloca los iconos bajo el mouse — quedan fijos hasta
    // que la zona se esfuma.
    match data.fugaz_fijo {
        Some(fijo) => cands.sort_by_key(|c| {
            fijo.orden.iter().position(|f| *f == c.0).unwrap_or(usize::MAX)
        }),
        None => {
            let puntaje =
                |f: Fugaz| data.fugaz_uso.map(|u| u.score(f)).unwrap_or_else(|| prior(f));
            cands.sort_by(|a, b| {
                puntaje(a.0).partial_cmp(&puntaje(b.0)).unwrap_or(core::cmp::Ordering::Equal)
            });
        }
    }

    // Selección: congelada del snapshot (el pin/salience vivos NO recomputan
    // el split — eso teleportaba iconos entre grupos bajo el mouse), o viva
    // fuera del reveal (críticas fijas + una leve turnándose + pin).
    let visibles: Vec<Fugaz> = match data.fugaz_fijo {
        Some(fijo) => fijo.visibles.clone(),
        None => {
            let resumen: Vec<(Fugaz, bool, bool)> =
                cands.iter().map(|(f, s, c, _)| (*f, *s, *c)).collect();
            elegir_visibles(&resumen, data.fugaz_idx, data.fugaz_pin)
        }
    };
    // MEMBRESÍA congelada: un candidato que aparece a mitad del hover no se
    // inserta (correría a los demás); uno cuyo dato se fue deja un **hueco**
    // del ancho de su slot (los vecinos no se corren).
    if let Some(fijo) = data.fugaz_fijo {
        cands.retain(|c| fijo.presentes.contains(&c.0));
        for id in &fijo.presentes {
            if !cands.iter().any(|c| c.0 == *id) {
                let pos = fijo.orden.iter().position(|f| f == id).unwrap_or(usize::MAX);
                let hueco = View::new(Style {
                    size: Size {
                        width: length(ancho_slot(*id, n_cores)),
                        height: length(20.0_f32),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                });
                let at = cands
                    .iter()
                    .position(|c| {
                        fijo.orden.iter().position(|f| *f == c.0).unwrap_or(usize::MAX) > pos
                    })
                    .unwrap_or(cands.len());
                cands.insert(at, (*id, false, false, hueco));
            }
        }
    }
    // Cada icono en su **slot de ancho fijo** (ver [`ancho_slot`]): el ancho de
    // la fila y la posición de cada vecino no dependen de la cara vigente.
    let anchos: f32 = cands.iter().map(|c| ancho_slot(c.0, n_cores)).sum();
    let n_total = cands.len();
    // Orden pegajoso: los visibles van al FRENTE (la derecha del row FlexEnd),
    // en orden de asiento entre sí; en reveal los demás aparecen a su izquierda
    // (también por asiento). Así el que ya estaba a la derecha se queda donde
    // apareció cuando llegan los otros; al esfumarse el reveal, todos vuelven a
    // su asiento.
    let mut fondo: Vec<View<Msg>> = Vec::new();
    let mut frente: Vec<View<Msg>> = Vec::new();
    for (id, _saliente, _, v) in cands {
        let slot = View::new(Style {
            size: Size { width: length(ancho_slot(id, n_cores)), height: auto() },
            flex_shrink: 0.0,
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        })
        .children(vec![v]);
        let pinneado = data.fugaz_pin == Some(id);
        if visibles.contains(&id) {
            // Visible ∧ ¬saliente sólo pasa pinneado. El pin fuerza opacidad
            // plena (el mouse está o estuvo encima); si no, el fade normal.
            let a = if pinneado { 1.0 } else { base.max(revelar_alpha) };
            frente.push(slot.alpha(a));
        } else if revelando {
            fondo.push(slot.alpha(revelar_alpha));
        }
    }
    let iconos: Vec<View<Msg>> = fondo.into_iter().chain(frente).collect();

    // RESPALDO de los iconos: el parche opaco bajo la banda, para que los glifos
    // del input no se lean a través de ellos.
    //
    // **Sólo se pinta cuando hacen falta las dos cosas a la vez**: que el usuario
    // esté mirando la franja (hover/pin) Y que el texto ya haya llegado hasta
    // acá (`base < 1`, los iconos estarían escondidos si no fuera por el hover).
    // Ésa es la única situación en que hay algo que tapar — el reveal trae los
    // iconos de vuelta ENCIMA del texto.
    //
    // Con hover pero sin texto debajo no hay nada que tapar (fondo transparente,
    // que es más liviano); y con texto pero sin hover los iconos ya no están, así
    // que un parche sería un rectángulo flotando solo sobre la barra. Antes era
    // `base.max(revelar_alpha)` — un O en vez de un Y — y por eso el parche
    // aparecía en las dos situaciones donde sobra.
    //
    // Va en un nodo INTERIOR, ceñido a los iconos, no en el contenedor de
    // hover: ése abarca la zona ancha donde se los despierta (`ZONA_FANTASMA_W`)
    // y pintarlo entero dejaría una plancha sobre medio input.
    const TEXTO_ENCIMA: f32 = 0.999;
    let mirando = revelar_alpha.max(if data.fugaz_pin.is_some() { 1.0 } else { 0.0 });
    let hace_falta = base < TEXTO_ENCIMA;
    let visibilidad = if hace_falta { mirando.clamp(0.0, 1.0) } else { 0.0 };
    let color_respaldo = theme.bg_panel_alt.with_alpha(0.92 * visibilidad);
    // A lo ANCHO de la franja, no ceñido a los iconos: un parche del tamaño
    // justo de los glifos se lee como «una cosa botada encima» del input. Una
    // banda que acompaña el ancho de la zona se lee como parte de la barra.
    let banda = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: auto() },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexEnd),
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(8.0_f32),
            top: length(2.0_f32),
            bottom: length(2.0_f32),
        },
        gap: Size { width: length(6.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .fill(color_respaldo)
    .radius(9.0)
    .children(iconos);

    // El contenedor SIEMPRE se monta —aun vacío— para que su rect exista y el
    // `hover_fill` (transparente) lo haga hit-testeable: entrar/salir de la
    // franja dispara `RevealFantasmas` (con esfumado animado y retardo). La
    // franja abarca SIEMPRE el ancho donde los iconos viven revelados (antes se
    // ensanchaba recién durante el reveal: acercarse por la izquierda no los
    // despertaba y el hover se sentía angosto). El `right: 38` salva el
    // micrófono. Sin alpha propio: cada icono ya trae el suyo.
    let _ = revelando;
    let ancho = ZONA_FANTASMA_W.max(anchos + (n_total.saturating_sub(1) as f32) * 6.0 + 12.0);
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: auto(),
            right: length(38.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        size: Size { width: length(ancho), height: auto() },
        flex_direction: FlexDirection::Row,
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexEnd),
        gap: Size { width: length(6.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .hover_fill(theme.bg_panel.with_alpha(0.0))
    .on_pointer_enter(Msg::RevealFantasmas(true))
    .on_pointer_leave(Msg::RevealFantasmas(false))
    .children(vec![banda])
}

/// Envuelve un icono en un nodo hit-testeable: click (si [`accion_fugaz`] le da
/// un destino) ruteado por [`Msg::FugazClick`] —que además **aprende el uso**—,
/// enter/leave que **pinnean** el icono bajo el mouse (no se oculta ni rota
/// mientras el puntero esté encima), y **rueda** donde el icono tiene un eje
/// natural: volumen sobre música/volumen/ánimo, brillo sobre la batería. El
/// wrapper se ciñe al contenido (`auto`) y no empuja el layout.
fn fugaz_item(icono: View<Msg>, id: Fugaz) -> View<Msg> {
    let v = View::new(Style {
        flex_shrink: 0.0,
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .on_pointer_enter(Msg::FantasmaPin(id, true))
    .on_pointer_leave(Msg::FantasmaPin(id, false))
    .children(vec![icono]);
    let v = match id {
        // Con reproductor activo el click izquierdo es play/pausa (lo decide
        // `FugazClick`); el derecho SIEMPRE abre el panel de volumen —así el
        // panel no se pierde cuando el icono actúa de «empausador».
        Fugaz::Sonido => v
            .on_scroll(|_dx, dy| (dy != 0.0).then_some(Msg::VolumeWheel(dy)))
            .on_right_click(Msg::VolumePanel),
        Fugaz::Control => v.on_scroll(|_dx, dy| (dy != 0.0).then_some(Msg::BrightnessWheel(dy))),
        _ => v,
    };
    if accion_fugaz(id).is_some() {
        v.on_click(Msg::FugazClick(id))
    } else {
        v
    }
}

#[cfg(test)]
mod tests_fugaces {
    use super::{
        avanzar_fugaz_idx, candidatos_fugaces, congelar_fugaces, elegir_visibles,
        orden_asientos, Fugaz, FugazUso, FUGAZ_ROT_US,
    };

    /// Audio con energía en el cava pero SIN player MPRIS ⇒ el fugaz de sonido
    /// es saliente igual (el bug era «tengo audio y no me sale cava»).
    #[test]
    fn cava_con_energia_hace_saliente_al_sonido() {
        let theme = llimphi_theme::Theme::dark();
        let quieto = crate::render::BarData::default();
        let cands = candidatos_fugaces(&quieto, 0, false, &theme);
        let sonido = cands.iter().find(|c| c.0 == Fugaz::Sonido).unwrap();
        assert!(!sonido.1, "sin audio ni mute, el sonido no es saliente");

        let con_audio = crate::render::BarData { cava: &[0.0, 0.35, 0.6], ..Default::default() };
        let cands = candidatos_fugaces(&con_audio, 0, false, &theme);
        let sonido = cands.iter().find(|c| c.0 == Fugaz::Sonido).unwrap();
        assert!(sonido.1, "energía en el cava = algo suena, aunque MPRIS calle");
    }

    /// El snapshot congelado estampa orden completo, membresía y reloj: es lo
    /// que mantiene los iconos inmóviles bajo el mouse (split y presentes NO
    /// se recomputan con el pin/salience vivos).
    #[test]
    fn congelar_estampa_orden_membresia_y_reloj() {
        let theme = llimphi_theme::Theme::dark();
        let data = crate::render::BarData::default();
        let f = congelar_fugaces(&data, &theme, 7_000_000);
        assert_eq!(f.orden.len(), Fugaz::TODOS.len());
        assert_eq!(f.reloj_us, 7_000_000);
        // Con BarData default sólo hay datos para estos cuatro candidatos.
        assert_eq!(f.presentes, vec![Fugaz::Sonido, Fugaz::Control, Fugaz::Cielo, Fugaz::Captura]);
        // Con el default sólo el cielo es saliente (moon_phase 0.0 = luna
        // nueva): la leve en turno es la única visible fuera del reveal.
        assert_eq!(f.visibles, vec![Fugaz::Cielo]);
    }

    /// El snapshot de asientos NO cambia con bumps posteriores: es lo que
    /// mantiene los iconos fijos bajo el mouse aunque el click aprenda uso.
    #[test]
    fn orden_congelado_sobrevive_al_bump() {
        let mut uso = FugazUso::default();
        let congelado = orden_asientos(Some(&uso));
        for _ in 0..30 {
            uso.bump(Fugaz::Cielo); // el cielo escala asientos…
        }
        let vivo = orden_asientos(Some(&uso));
        assert_ne!(congelado, vivo, "el bump debe mover el orden vivo");
        // …pero el snapshot estampado sigue siendo el mismo Vec: quien pinta
        // con el snapshot no ve el corrimiento.
        assert_eq!(congelado.len(), Fugaz::TODOS.len());
        assert!(congelado.iter().position(|f| *f == Fugaz::Cielo).unwrap()
            < congelado.iter().position(|f| *f == Fugaz::Sonido).unwrap());
    }

    /// Sin uso manda el prior; el uso real corrige el asiento (y satura por √).
    #[test]
    fn uso_corrige_el_asiento() {
        let mut uso = FugazUso::default(); // sin path → en memoria, no persiste
        assert!(uso.score(Fugaz::Cielo) < uso.score(Fugaz::Sonido));
        for _ in 0..20 {
            uso.bump(Fugaz::Cielo);
        }
        assert!(uso.score(Fugaz::Cielo) > uso.score(Fugaz::Sonido));
    }

    /// Con varias leves salientes se ve UNA sola, y el turno avanza con el reloj.
    #[test]
    fn leves_se_turnan() {
        let cands = [
            (Fugaz::Sonido, true, false),
            (Fugaz::Cpu, true, false),
            (Fugaz::Cielo, true, false),
        ];
        let (mut idx, mut reloj) = (0usize, 0u64);
        assert_eq!(elegir_visibles(&cands, idx, None), vec![Fugaz::Sonido]);
        assert!(avanzar_fugaz_idx(&mut idx, &mut reloj, FUGAZ_ROT_US, false));
        assert_eq!(elegir_visibles(&cands, idx, None), vec![Fugaz::Cpu]);
        assert!(avanzar_fugaz_idx(&mut idx, &mut reloj, 2 * FUGAZ_ROT_US, false));
        assert_eq!(elegir_visibles(&cands, idx, None), vec![Fugaz::Cielo]);
    }

    /// Las críticas van todas fijas; la leve en turno se suma al lado.
    #[test]
    fn criticas_mandan_fijas() {
        let cands = [
            (Fugaz::Sonido, true, false),
            (Fugaz::Control, true, true),
            (Fugaz::Flota, true, true),
        ];
        let vis = elegir_visibles(&cands, 0, None);
        assert_eq!(vis, vec![Fugaz::Control, Fugaz::Flota, Fugaz::Sonido]);
    }

    /// Congelado (reveal o pin): el turno no avanza y el reloj se re-estampa,
    /// así al soltar no salta de inmediato.
    #[test]
    fn congelar_detiene_turno() {
        let (mut idx, mut reloj) = (0usize, 0u64);
        assert!(!avanzar_fugaz_idx(&mut idx, &mut reloj, 10 * FUGAZ_ROT_US, true));
        assert_eq!(idx, 0);
        // Recién descongelado: falta el intervalo completo desde el release.
        assert!(!avanzar_fugaz_idx(&mut idx, &mut reloj, 10 * FUGAZ_ROT_US + 1, false));
        assert!(avanzar_fugaz_idx(&mut idx, &mut reloj, 11 * FUGAZ_ROT_US, false));
    }

    /// El pinneado queda visible aunque su condición haya caído; uno ajeno a la
    /// lista de candidatos se ignora.
    #[test]
    fn pin_retiene_al_apuntado() {
        let cands = [(Fugaz::Sonido, true, false), (Fugaz::Captura, false, false)];
        let vis = elegir_visibles(&cands, 0, Some(Fugaz::Captura));
        assert_eq!(vis, vec![Fugaz::Sonido, Fugaz::Captura]);
        let vis = elegir_visibles(&cands, 0, Some(Fugaz::Usb));
        assert_eq!(vis, vec![Fugaz::Sonido]);
    }

    /// Sin salientes ni pin no se ve nada (la franja queda vacía hasta el reveal).
    #[test]
    fn sin_salientes_nada() {
        let cands = [(Fugaz::Captura, false, false)];
        assert!(elegir_visibles(&cands, 3, None).is_empty());
    }
}

/// Icono de **música**: el visualizador `cava` (barras de audio) reusado de la
/// barra, en tamaño compacto. Es fugaz — lo monta [`iconos_fugaces`] sólo cuando
/// suena algo.
fn icono_musica(cava: &[f32], theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: length(44.0_f32), height: length(20.0_f32) },
        flex_shrink: 0.0,
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .tooltip("Reproduciendo")
    .children(vec![crate::render::cava_view(cava, theme)])
}

/// Umbral de temperatura (°C) a partir del cual el abanico de CPU titila — a la
/// par del `sys_alert` de la barra (mismo const compartido).
const TEMP_CALIENTE: f32 = crate::render::CPU_TEMP_ALERTA_C;

/// Icono de **CPU**: un mini-visualizador **estilo cava** — una barra vertical
/// por núcleo, con la altura = carga de ese core y el color verde→rojo por carga,
/// con gradiente vertical como el cava de audio. Cada core se lee por separado
/// (barras distintas), no un promedio. Si la CPU está recaliente, titila (se
/// atenúa en fase impar). Reusa el dato `cpu_cores` que ya muestrea pata.
fn icono_cpu(cores: &[f32], caliente: bool, titila: bool, _theme: &Theme) -> View<Msg> {
    let cores: Vec<f32> = cores.iter().map(|c| c.clamp(0.0, 1.0)).collect();
    let dim = caliente && !titila;
    // Ancho proporcional a la cantidad de cores para que cada barra respire
    // (mismo criterio que el cava de audio: barras finas con gap).
    let w = ((cores.len().max(1) as f32) * 3.5 + 4.0).min(44.0);
    View::new(Style {
        size: Size { width: length(w), height: length(20.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip("CPU")
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Point, RoundedRect};
        use llimphi_ui::llimphi_raster::peniko::{Fill, Gradient};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let n = cores.len().max(1);
        let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
        let gap = 1.5_f64;
        let bw = ((w - gap * (n as f64 - 1.0)) / n as f64).max(1.0);
        let a_mul = if dim { 0.35_f32 } else { 1.0 };
        for (i, &load) in cores.iter().enumerate() {
            let load = load.clamp(0.0, 1.0);
            // Piso mínimo: un core en idle igual muestra una barrita — así se
            // lee la VARIACIÓN entre cores en vez de verse todas iguales.
            let bh = (load as f64 * h).max(1.5);
            let bx = x + i as f64 * (bw + gap);
            let by = y + h - bh;
            let rr = RoundedRect::new(bx, by, bx + bw, y + h, 1.0);
            // Gradiente vertical como el cava de audio: base tenue → color carga.
            let top = color_carga(load).with_alpha(a_mul);
            let base = color_carga(load).with_alpha(a_mul * 0.45);
            let g = Gradient::new_linear(Point::new(bx, y + h), Point::new(bx, by))
                .with_stops([base, top].as_slice());
            scene.fill(Fill::NonZero, Affine::IDENTITY, &g, None, &rr);
        }
    })
}

/// Icono de **flota** (matilda): una pila de tres "servidores" (barras
/// redondeadas) con un semáforo en la esquina — verde si todo corre, ámbar si un
/// servicio se cayó, rojo si un contenedor cayó o un host quedó inalcanzable
/// (titila cuando es grave). Lo monta [`iconos_fugaces`] sólo cuando hay problema
/// (o con la zona revelada). Reúne el estado local y el de la flota remota; el
/// tooltip narra el detalle.
fn icono_flota(salud: &crate::matilda_salud::SaludFlota, titila: bool, theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_raster::peniko::Color;
    let sev = salud.severidad();
    let grave = sev >= 2;
    let dim = grave && !titila;
    let dot = match sev {
        0 => Color::from_rgb8(0x5A, 0xD0, 0x8A), // verde: todo corre
        1 => Color::from_rgb8(0xFB, 0xBF, 0x24), // ámbar: servicio caído
        _ => Color::from_rgb8(0xE0, 0x5A, 0x5A), // rojo: contenedor caído / host caído
    };
    let barra = theme.fg_muted;
    let tip = tooltip_flota(salud);
    View::new(Style {
        size: Size { width: length(22.0_f32), height: length(20.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(tip)
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle, RoundedRect};
        use llimphi_ui::llimphi_raster::peniko::Fill;
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let a_mul = if dim { 0.35_f32 } else { 1.0 };
        let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
        // Tres "servidores" apilados (barras horizontales redondeadas).
        let n = 3usize;
        let gap = h * 0.14;
        let bh = (h - gap * (n as f64 - 1.0)) / n as f64;
        let bw = w * 0.82;
        for i in 0..n {
            let by = y + i as f64 * (bh + gap);
            let rr = RoundedRect::new(x, by, x + bw, by + bh, 1.5);
            scene.fill(Fill::NonZero, Affine::IDENTITY, barra.with_alpha(a_mul * 0.85), None, &rr);
        }
        // Semáforo: un disco en la esquina inferior derecha.
        let r = h * 0.15;
        let cx = x + w - r;
        let cy = y + h - r;
        scene.fill(Fill::NonZero, Affine::IDENTITY, dot.with_alpha(a_mul), None, &Circle::new((cx, cy), r));
    })
}

/// Texto del tooltip del icono de flota: el resumen del problema si lo hay, o el
/// conteo sano (`Flota — N up`).
fn tooltip_flota(salud: &crate::matilda_salud::SaludFlota) -> String {
    salud
        .resumen()
        .unwrap_or_else(|| format!("Flota — {} up", salud.total_up()))
}

/// Icono de **batería**: cuerpo + terminal + relleno según la carga (verde→rojo
/// a medida que baja), con un **rayo** encima cuando está enchufada — así el
/// acuse de enchufar/desenchufar se lee de un vistazo. Titila (se atenúa)
/// cuando está llegando a cero descargando.
fn icono_bateria(frac: f32, cargando: bool, titila: bool, theme: &Theme) -> View<Msg> {
    let frac = frac.clamp(0.0, 1.0);
    let baja = !cargando && frac <= 0.12;
    let dim = baja && !titila;
    let borde = theme.fg_muted;
    let rayo = theme.accent;
    let tip = format!(
        "Batería · {:.0}%{}",
        frac * 100.0,
        if cargando { " · cargando" } else { "" }
    );
    View::new(Style {
        size: Size { width: length(28.0_f32), height: length(16.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(tip)
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, BezPath, Point, Rect as KRect, RoundedRect, Stroke};
        use llimphi_ui::llimphi_raster::peniko::Fill;
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let a_mul = if dim { 0.30_f32 } else { 1.0 };
        let bw = rect.w as f64 * 0.78;
        let bh = rect.h as f64 * 0.62;
        let bx = rect.x as f64 + 2.0;
        let by = rect.y as f64 + (rect.h as f64 - bh) * 0.5;
        // Cuerpo (borde) + terminal + relleno.
        let cuerpo = RoundedRect::new(bx, by, bx + bw, by + bh, 2.5);
        scene.stroke(&Stroke::new(1.3), Affine::IDENTITY, borde.with_alpha(a_mul), None, &cuerpo);
        let tx = bx + bw;
        let term = KRect::new(tx, by + bh * 0.3, tx + 2.4, by + bh * 0.7);
        scene.fill(Fill::NonZero, Affine::IDENTITY, borde.with_alpha(a_mul), None, &term);
        let fill_w = ((bw - 3.0) * frac as f64).max(0.0);
        let col = color_carga(1.0 - frac).with_alpha(a_mul); // poca carga → rojo
        let relleno = RoundedRect::new(bx + 1.5, by + 1.5, bx + 1.5 + fill_w, by + bh - 1.5, 1.5);
        scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &relleno);
        // Rayo de carga: un zigzag en acento sobre el cuerpo, sólo enchufada.
        if cargando {
            let cx = bx + bw * 0.5;
            let cy = by + bh * 0.5;
            let (rw, rh) = (bw * 0.28, bh * 0.95);
            let mut r = BezPath::new();
            r.move_to(Point::new(cx + rw * 0.35, cy - rh * 0.5));
            r.line_to(Point::new(cx - rw * 0.5, cy + rh * 0.12));
            r.line_to(Point::new(cx - rw * 0.02, cy + rh * 0.12));
            r.line_to(Point::new(cx - rw * 0.35, cy + rh * 0.5));
            r.line_to(Point::new(cx + rw * 0.5, cy - rh * 0.12));
            r.line_to(Point::new(cx + rw * 0.02, cy - rh * 0.12));
            r.close_path();
            scene.fill(Fill::NonZero, Affine::IDENTITY, rayo, None, &r);
        }
    })
}

/// Icono de **volumen**: un altavoz vectorial (cuerpo + cono) con ondas cuya
/// cantidad crece con el nivel; silenciado, pinta una tachadura roja. Lo monta
/// [`iconos_fugaces`] cuando está muteado o con la zona revelada.
fn icono_volumen(frac: f32, muted: bool, theme: &Theme) -> View<Msg> {
    let frac = frac.clamp(0.0, 1.0);
    // Intensidad de color por nivel: bajo = apagado (muted-gris), alto = pleno.
    // En mute el altavoz entero se apaga y manda la tachadura roja.
    let fg = if muted {
        theme.fg_muted.with_alpha(0.55)
    } else {
        lerp_color(theme.fg_muted.with_alpha(0.6), theme.fg_text, 0.25 + 0.75 * frac)
    };
    View::new(Style {
        size: Size { width: length(26.0_f32), height: length(18.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(if muted { "Silenciado" } else { "Volumen" })
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Arc, BezPath, Line, Point, Stroke};
        use llimphi_ui::llimphi_raster::peniko::{Color, Fill};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
        let cy = y + h * 0.5;
        // Cuerpo + cono del altavoz (un polígono a la izquierda).
        let bx = x + 2.0;
        let bh = h * 0.34;
        let cono_x = bx + w * 0.30;
        let mut cono = BezPath::new();
        cono.move_to(Point::new(bx, cy - bh * 0.5));
        cono.line_to(Point::new(cono_x, cy - bh * 0.5));
        cono.line_to(Point::new(cono_x + w * 0.16, cy - h * 0.42));
        cono.line_to(Point::new(cono_x + w * 0.16, cy + h * 0.42));
        cono.line_to(Point::new(cono_x, cy + bh * 0.5));
        cono.line_to(Point::new(bx, cy + bh * 0.5));
        cono.close_path();
        scene.fill(Fill::NonZero, Affine::IDENTITY, fg, None, &cono);
        if muted {
            // Tachadura roja en diagonal.
            let rojo = Color::from_rgb8(0xe0, 0x6c, 0x6c);
            let ax = cono_x + w * 0.24;
            scene.stroke(
                &Stroke::new(1.8),
                Affine::IDENTITY,
                rojo,
                None,
                &Line::new((ax, y + 2.0), (x + w - 2.0, y + h - 2.0)),
            );
        } else {
            // Ondas: 1..3 arcos según el nivel.
            let ondas = 1 + (frac * 2.0).round() as i32;
            let ax = cono_x + w * 0.20;
            for i in 0..ondas {
                let r = h * (0.28 + i as f64 * 0.20);
                let arco = Arc::new(
                    (ax, cy),
                    (r, r),
                    -core::f64::consts::FRAC_PI_4,
                    core::f64::consts::FRAC_PI_2,
                    0.0,
                );
                scene.stroke(&Stroke::new(1.4), Affine::IDENTITY, fg, None, &arco);
            }
        }
    })
}

/// Cara vigente del icono **cielo** (los cuatro glifos astrales unificados).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaraCielo {
    Luna,
    Sol,
    Signo,
    Eclipse,
}

/// Icono de **sonido** (música + volumen unificados), por estado:
/// - el mouse está **encima** con un reproductor activo (`transporte`) → el
///   botón de play/pausa (el «empausador»): un click lo alterna;
/// - hay algo **sonando** → el cava vivo (barras de audio);
/// - el volumen **acaba de cambiar** → una rampa creciente/decreciente llena
///   hasta el nivel (el acuse del gesto);
/// - **idle** → el altavoz con intensidad de color según el % (o tachado en mute).
fn icono_sonido(
    cava: &[f32],
    sonando: bool,
    frac: f32,
    muted: bool,
    evento: Option<bool>,
    transporte: Option<bool>,
    theme: &Theme,
) -> View<Msg> {
    // Con el puntero encima y un reproductor vivo, el cava/altavoz se vuelve el
    // botón de play/pausa: es la afordancia del gesto (click = alternar).
    if let Some(playing) = transporte {
        return icono_transporte(playing, theme);
    }
    if let Some(subiendo) = evento {
        return icono_rampa_volumen(frac, subiendo, muted, theme);
    }
    if sonando && !muted {
        return icono_musica(cava, theme);
    }
    icono_volumen(frac, muted, theme)
}

/// Icono de **transporte**: al pasar el mouse sobre el sonido con un reproductor
/// activo, el icono muta a un botón de **pausa** (si suena) o **play** (si está
/// en pausa) sobre una pastilla tenue que lo delata como pulsable. El click lo
/// alterna vía [`Msg::MediaPlayPause`] (lo rutea el click del fugaz según haya
/// reproductor). Glifos pintados a mano, al estilo del widget `mpris`.
fn icono_transporte(playing: bool, theme: &Theme) -> View<Msg> {
    let fg = theme.fg_text;
    let acento = theme.accent;
    View::new(Style {
        size: Size { width: length(30.0_f32), height: length(20.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(if playing { "Pausar" } else { "Reproducir" })
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, BezPath, Point, Rect, RoundedRect};
        use llimphi_ui::llimphi_raster::peniko::Fill;
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
        // Pastilla de fondo tenue en acento: lee como botón (afordancia de click).
        let pill = RoundedRect::new(x, y, x + w, y + h, (h * 0.5).min(9.0));
        scene.fill(Fill::NonZero, Affine::IDENTITY, acento.with_alpha(0.16), None, &pill);
        // Glifo centrado en un cuadro de lado `g`.
        let g = h * 0.55;
        let gx = x + (w - g) * 0.5;
        let gy = y + (h - g) * 0.5;
        if playing {
            // Pausa: dos barras.
            let bw = g * 0.30;
            scene.fill(Fill::NonZero, Affine::IDENTITY, fg, None,
                &Rect::new(gx + g * 0.12, gy, gx + g * 0.12 + bw, gy + g));
            scene.fill(Fill::NonZero, Affine::IDENTITY, fg, None,
                &Rect::new(gx + g * 0.58, gy, gx + g * 0.58 + bw, gy + g));
        } else {
            // Play: triángulo apuntando a la derecha.
            let mut p = BezPath::new();
            p.move_to(Point::new(gx + g * 0.18, gy));
            p.line_to(Point::new(gx + g * 0.88, gy + g * 0.5));
            p.line_to(Point::new(gx + g * 0.18, gy + g));
            p.close_path();
            scene.fill(Fill::NonZero, Affine::IDENTITY, fg, None, &p);
        }
    })
}

/// La **rampa de volumen**: barras verticales de altura escalonada (ascendente
/// si está subiendo, descendente si baja), encendidas hasta el nivel actual —
/// el acuse fugaz del wheel/OSD, en acento.
fn icono_rampa_volumen(frac: f32, subiendo: bool, muted: bool, theme: &Theme) -> View<Msg> {
    let frac = frac.clamp(0.0, 1.0);
    let acento = theme.accent;
    let apagado = theme.fg_muted;
    let tip = format!("Volumen · {:.0}%{}", frac * 100.0, if muted { " · silenciado" } else { "" });
    View::new(Style {
        size: Size { width: length(30.0_f32), height: length(18.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(tip)
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, RoundedRect};
        use llimphi_ui::llimphi_raster::peniko::Fill;
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
        let n = 6usize;
        let gap = 2.0_f64;
        let bw = ((w - gap * (n as f64 - 1.0)) / n as f64).max(1.5);
        for i in 0..n {
            // Altura escalonada: la rampa apunta hacia donde va el volumen.
            let t = (i as f64 + 1.0) / n as f64;
            let t = if subiendo { t } else { 1.0 - t + 1.0 / n as f64 };
            let bh = (h * (0.25 + 0.75 * t)).min(h);
            let bx = x + i as f64 * (bw + gap);
            let by = y + h - bh;
            // Encendida si su escalón cae bajo el nivel actual.
            let nivel = (i as f64 + 0.5) / n as f64;
            let on = nivel <= frac as f64;
            let col = if on { acento } else { apagado.with_alpha(0.35) };
            let rr = RoundedRect::new(bx, by, bx + bw, y + h, 1.0);
            scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &rr);
        }
    })
}

/// Icono de **control** (batería + brillo unificados; abre el centro de
/// control): con `acusar` (cable recién tocado / batería baja) muestra la
/// batería fija; si no, alterna brillo ↔ batería con el reloj de caras (brillo
/// solo cuando no hay batería que mostrar).
fn icono_control(
    bat: Option<(f32, bool)>,
    acusar: bool,
    brillo: f32,
    cara_reloj: usize,
    titila: bool,
    theme: &Theme,
) -> View<Msg> {
    match bat {
        Some((frac, cargando)) if acusar || cara_reloj % 2 == 1 => {
            icono_bateria(frac, cargando, titila, theme)
        }
        _ => icono_brillo(brillo, theme),
    }
}

/// Icono de **brillo**: un disco con rayos cuya intensidad (alpha y largo de
/// rayos) sigue el % de brillo de pantalla. Rueda = ajustar brillo.
fn icono_brillo(frac: f32, theme: &Theme) -> View<Msg> {
    let frac = frac.clamp(0.0, 1.0);
    let col = lerp_color(theme.fg_muted, theme.accent, 0.2 + 0.8 * frac);
    let tip = format!("Brillo · {:.0}%", frac * 100.0);
    View::new(Style {
        size: Size { width: length(20.0_f32), height: length(20.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(tip)
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle, Line, Stroke};
        use llimphi_ui::llimphi_raster::peniko::Fill;
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let cx = (rect.x + rect.w * 0.5) as f64;
        let cy = (rect.y + rect.h * 0.5) as f64;
        let r = (rect.w.min(rect.h) as f64) * 0.20;
        scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &Circle::new((cx, cy), r));
        // Ocho rayos; el largo respira con el nivel.
        let largo = r * (0.6 + 0.9 * frac as f64);
        for i in 0..8 {
            let a = i as f64 * core::f64::consts::FRAC_PI_4;
            let (s, c) = a.sin_cos();
            let l = Line::new(
                (cx + c * (r + 1.5), cy + s * (r + 1.5)),
                (cx + c * (r + 1.5 + largo), cy + s * (r + 1.5 + largo)),
            );
            scene.stroke(&Stroke::new(1.4), Affine::IDENTITY, col, None, &l);
        }
    })
}

/// Icono de **red**: el glifo de la interfaz conectada (arcos Wi-Fi escalados
/// por señal, o el conector de cable) + dos **microbarras verticales** con el
/// tráfico instantáneo (↓rx en acento, ↑tx en texto). Sin conexión, el glifo
/// se apaga con una tachadura.
fn icono_red(
    net: &crate::network::NetState,
    trafico: (f32, f32),
    theme: &Theme,
) -> View<Msg> {
    use crate::network::NetStatus;
    let fg = theme.fg_text;
    let dim = theme.fg_muted;
    let acento = theme.accent;
    let (rx, tx) = (trafico.0.clamp(0.0, 1.0), trafico.1.clamp(0.0, 1.0));
    let (wifi_sig, cable, viva) = match &net.status {
        NetStatus::Ethernet => (None, true, true),
        NetStatus::Wifi { signal, .. } => (Some(*signal as f32 / 100.0), false, true),
        _ => (None, false, false),
    };
    let tip = match &net.status {
        NetStatus::Ethernet => "Red · cable".to_string(),
        NetStatus::Wifi { ssid, signal } => format!("Red · {ssid} · {signal}%"),
        NetStatus::WifiOff => "Red · Wi-Fi apagada".to_string(),
        _ => "Red · sin conexión".to_string(),
    };
    View::new(Style {
        size: Size { width: length(30.0_f32), height: length(20.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(tip)
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Arc, Circle, Line, Rect as KRect, Stroke};
        use llimphi_ui::llimphi_raster::peniko::{Color, Fill};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
        // Glifo a la izquierda (~22px), microbarras a la derecha.
        let gx = x + w * 0.36; // centro del glifo
        let base = if viva { fg } else { dim.with_alpha(0.5) };
        if cable {
            // Conector RJ45 estilizado: cuerpo + tres pines + cola.
            let bw = w * 0.34;
            let bh = h * 0.42;
            let bx = gx - bw * 0.5;
            let by = y + h * 0.18;
            let cuerpo = KRect::new(bx, by, bx + bw, by + bh);
            scene.fill(Fill::NonZero, Affine::IDENTITY, base, None, &cuerpo);
            for i in 0..3 {
                let px = bx + bw * (0.22 + 0.28 * i as f64);
                let pin = KRect::new(px, by + bh, px + bw * 0.12, by + bh + h * 0.14);
                scene.fill(Fill::NonZero, Affine::IDENTITY, base, None, &pin);
            }
            let cola = Line::new((gx, by + bh + h * 0.14), (gx, y + h * 0.92));
            scene.stroke(&Stroke::new(1.4), Affine::IDENTITY, base, None, &cola);
        } else {
            // Arcos Wi-Fi: el punto siempre; los arcos se encienden por señal.
            let cy = y + h * 0.82;
            scene.fill(Fill::NonZero, Affine::IDENTITY, base, None, &Circle::new((gx, cy), 1.6));
            let sig = wifi_sig.unwrap_or(0.0) as f64;
            for i in 0..3 {
                let umbral = (i as f64 + 0.5) / 3.0;
                let on = viva && sig >= umbral * 0.9;
                let col = if on { base } else { dim.with_alpha(0.30) };
                let r = h * (0.24 + 0.20 * i as f64);
                let arco = Arc::new(
                    (gx, cy),
                    (r, r),
                    -core::f64::consts::FRAC_PI_2 - 0.62,
                    1.24,
                    0.0,
                );
                scene.stroke(&Stroke::new(1.5), Affine::IDENTITY, col, None, &arco);
            }
        }
        if !viva {
            let rojo = Color::from_rgb8(0xe0, 0x6c, 0x6c);
            let tach = Line::new((x + w * 0.10, y + 2.0), (x + w * 0.58, y + h - 2.0));
            scene.stroke(&Stroke::new(1.6), Affine::IDENTITY, rojo, None, &tach);
        }
        // Microbarras de tráfico ↓rx / ↑tx, pegadas al borde derecho.
        let bw = 3.0_f64;
        let gap = 2.5_f64;
        let x_tx = x + w - bw;
        let x_rx = x_tx - gap - bw;
        for (bx, frac, col) in [(x_rx, rx as f64, acento), (x_tx, tx as f64, fg)] {
            // Riel tenue + barra por tráfico (piso mínimo para que se vea vivo).
            let riel = KRect::new(bx, y + 1.0, bx + bw, y + h - 1.0);
            scene.fill(Fill::NonZero, Affine::IDENTITY, dim.with_alpha(0.18), None, &riel);
            let bh = ((h - 2.0) * frac).max(if viva { 1.5 } else { 0.0 });
            let barra = KRect::new(bx, y + h - 1.0 - bh, bx + bw, y + h - 1.0);
            scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &barra);
        }
    })
}

/// Icono de **clima**: colorido y animado según el cielo — sol con rayos que
/// giran, nube, gotas que caen, copos, rayo que titila. `anim_t` (segundos
/// corridos) anima; la barra ya repinta continuo cuando está activa.
fn icono_clima(w: &crate::weather::Weather, anim_t: f32, titila: bool, _theme: &Theme) -> View<Msg> {
    use crate::weather::Sky;
    let sky = w.sky;
    let tip = format!("{:.0}°C · {}", w.temp_c, w.desc);
    let t = anim_t as f64;
    View::new(Style {
        size: Size { width: length(22.0_f32), height: length(20.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(tip)
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, BezPath, Circle, Line, Point, Stroke};
        use llimphi_ui::llimphi_raster::peniko::{Color, Fill};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
        let sol_col = Color::from_rgb8(0xF5, 0xC5, 0x4A);
        let nube_col = Color::from_rgb8(0xC9, 0xD4, 0xE3);
        let nube_gris = Color::from_rgb8(0x8E, 0x9A, 0xAC);
        let gota_col = Color::from_rgb8(0x6F, 0xA8, 0xE8);
        let copo_col = Color::from_rgb8(0xEA, 0xF2, 0xFB);
        let rayo_col = Color::from_rgb8(0xF7, 0xD9, 0x4C);
        let niebla_col = Color::from_rgb8(0xAF, 0xB8, 0xC4);

        // Sol (pleno o asomando tras la nube) con rayos que giran despacio.
        let sol = |scene: &mut llimphi_ui::llimphi_raster::vello::Scene, cx: f64, cy: f64, r: f64| {
            scene.fill(Fill::NonZero, Affine::IDENTITY, sol_col, None, &Circle::new((cx, cy), r));
            for i in 0..8 {
                let a = i as f64 * core::f64::consts::FRAC_PI_4 + t * 0.5;
                let (s, c) = a.sin_cos();
                let l = Line::new(
                    (cx + c * (r + 1.2), cy + s * (r + 1.2)),
                    (cx + c * (r + 3.4), cy + s * (r + 3.4)),
                );
                scene.stroke(&Stroke::new(1.2), Affine::IDENTITY, sol_col, None, &l);
            }
        };
        // Nube: tres lóbulos + base.
        let nube = |scene: &mut llimphi_ui::llimphi_raster::vello::Scene, cx: f64, cy: f64, esc: f64, col: Color| {
            scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &Circle::new((cx - 3.5 * esc, cy), 2.6 * esc));
            scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &Circle::new((cx, cy - 1.8 * esc), 3.2 * esc));
            scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &Circle::new((cx + 3.6 * esc, cy), 2.7 * esc));
            let base = llimphi_ui::llimphi_raster::kurbo::Rect::new(
                cx - 3.5 * esc,
                cy - 0.5 * esc,
                cx + 3.6 * esc,
                cy + 2.4 * esc,
            );
            scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &base.to_rounded_rect(2.0 * esc));
        };
        // Gotas/copos cayendo: fase por columna, loop vertical con `t`.
        let caida = |scene: &mut llimphi_ui::llimphi_raster::vello::Scene, copo: bool| {
            for i in 0..3 {
                let fx = x + w * (0.28 + 0.22 * i as f64);
                let ciclo = h * 0.34;
                let fy = y + h * 0.62 + ((t * 6.0 + i as f64 * ciclo * 0.45) % ciclo);
                if copo {
                    for k in 0..3 {
                        let a = k as f64 * core::f64::consts::FRAC_PI_3;
                        let (s, c) = a.sin_cos();
                        let l = Line::new((fx - c * 1.6, fy - s * 1.6), (fx + c * 1.6, fy + s * 1.6));
                        scene.stroke(&Stroke::new(0.9), Affine::IDENTITY, copo_col, None, &l);
                    }
                } else {
                    let l = Line::new((fx, fy), (fx - 0.8, fy + 2.6));
                    scene.stroke(&Stroke::new(1.3), Affine::IDENTITY, gota_col, None, &l);
                }
            }
        };

        let cx = x + w * 0.5;
        match sky {
            Sky::Clear => sol(scene, cx, y + h * 0.5, h * 0.22),
            Sky::PartlyCloudy => {
                sol(scene, x + w * 0.36, y + h * 0.38, h * 0.18);
                nube(scene, x + w * 0.60, y + h * 0.62, 1.0, nube_col);
            }
            Sky::Cloudy => {
                nube(scene, x + w * 0.38, y + h * 0.42, 0.9, nube_gris);
                nube(scene, x + w * 0.62, y + h * 0.58, 1.05, nube_col);
            }
            Sky::Fog => {
                nube(scene, cx, y + h * 0.34, 0.9, niebla_col);
                for i in 0..3 {
                    let fy = y + h * (0.58 + 0.16 * i as f64);
                    let ondu = (t * 1.3 + i as f64).sin() * 1.5;
                    let l = Line::new((x + 2.0 + ondu, fy), (x + w - 2.0 + ondu, fy));
                    scene.stroke(&Stroke::new(1.2), Affine::IDENTITY, niebla_col.with_alpha(0.8), None, &l);
                }
            }
            Sky::Rain => {
                nube(scene, cx, y + h * 0.34, 1.0, nube_gris);
                caida(scene, false);
            }
            Sky::Snow => {
                nube(scene, cx, y + h * 0.34, 1.0, nube_col);
                caida(scene, true);
            }
            Sky::Storm => {
                nube(scene, cx, y + h * 0.32, 1.0, nube_gris);
                // El rayo destella (fase del titila compartido).
                if titila {
                    let mut r = BezPath::new();
                    r.move_to(Point::new(cx + 1.5, y + h * 0.42));
                    r.line_to(Point::new(cx - 2.2, y + h * 0.68));
                    r.line_to(Point::new(cx + 0.2, y + h * 0.68));
                    r.line_to(Point::new(cx - 1.5, y + h * 0.95));
                    r.line_to(Point::new(cx + 2.8, y + h * 0.62));
                    r.line_to(Point::new(cx + 0.6, y + h * 0.62));
                    r.close_path();
                    scene.fill(Fill::NonZero, Affine::IDENTITY, rayo_col, None, &r);
                }
                caida(scene, false);
            }
            _ => {
                // Sin clasificar: nube neutra.
                nube(scene, cx, y + h * 0.5, 1.0, nube_col);
            }
        }
    })
}

/// Icono de **luna**: la fase actual como shapes (disco oscuro + disco iluminado
/// desplazado según `phase`), espejo compacto del widget `moon` de la barra.
/// Cuando hay cielo (cosmos) el tooltip trae la **iluminación precisa**, si está
/// creciente o menguante, y los días a la próxima **llena**. Saliente cerca de la
/// llena/nueva.
fn icono_luna(
    phase: f32,
    ilum: Option<f32>,
    creciente: Option<bool>,
    dias_llena: Option<f32>,
    _theme: &Theme,
) -> View<Msg> {
    let phase = phase.clamp(0.0, 1.0) as f64;
    let tip = match (ilum, creciente, dias_llena) {
        (Some(k), Some(cr), Some(d)) => {
            let sentido = if cr { "creciente" } else { "menguante" };
            let cuando = if d < 0.5 {
                "· llena hoy".to_string()
            } else {
                format!("· llena en {} días", d.round() as i32)
            };
            format!("Luna · {:.0}% iluminada · {sentido} {cuando}", k * 100.0)
        }
        _ => "Fase lunar".to_string(),
    };
    View::new(Style {
        size: Size { width: length(20.0_f32), height: length(20.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(tip)
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle};
        use llimphi_ui::llimphi_raster::peniko::{BlendMode, Color, Fill};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let cx = (rect.x + rect.w * 0.5) as f64;
        let cy = (rect.y + rect.h * 0.5) as f64;
        let r = ((rect.w.min(rect.h) as f64) * 0.5 - 1.5).max(2.0);
        let dark = Color::from_rgba8(46, 51, 76, 255);
        let light = Color::from_rgba8(245, 235, 199, 255);
        let disco = Circle::new((cx, cy), r);
        scene.fill(Fill::NonZero, Affine::IDENTITY, dark, None, &disco);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, Affine::IDENTITY, &disco);
        let dx = -2.0 * r * (core::f64::consts::PI * phase).cos();
        scene.fill(Fill::NonZero, Affine::IDENTITY, light, None, &Circle::new((cx + dx, cy), r));
        scene.pop_layer();
    })
}

/// Icono de **reloj de sol** (`cosmos-sundial`): un gnomon con su **sombra**
/// proyectada en el suelo. La sombra apunta hacia abajo-izquierda por la mañana
/// (ángulo horario negativo), recta al mediodía solar, abajo-derecha por la tarde;
/// su largo crece a medida que el Sol baja (razón de sombra). Saliente al
/// mediodía. El tooltip narra cuánto falta (o hace) para el mediodía solar.
fn icono_sol(c: &crate::cielo::CieloState, theme: &Theme) -> View<Msg> {
    let ha = c.hora_angulo_deg.clamp(-90.0, 90.0) as f64;
    // Largo de sombra normalizado a [0.25, 1] del alto (razón acotada; al mediodía
    // es corta, al atardecer larga). Sin sombra (noche) → pintamos sólo el gnomon.
    let ratio = c.sombra_largo_ratio.unwrap_or(0.0).clamp(0.0, 6.0) as f64;
    let de_noche = !c.sol_sobre_horizonte;
    let min = c.minutos_a_mediodia;
    let tip = if de_noche {
        "Reloj de sol · el Sol está bajo el horizonte".to_string()
    } else if min.abs() < 1.0 {
        "Reloj de sol · mediodía solar".to_string()
    } else if min > 0.0 {
        format!("Reloj de sol · mediodía en {} min", min.round() as i32)
    } else {
        format!("Reloj de sol · mediodía hace {} min", (-min).round() as i32)
    };
    let sol = theme.accent;
    let piso = theme.fg_muted;
    View::new(Style {
        size: Size { width: length(20.0_f32), height: length(20.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(tip)
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle, Line, Point, Stroke};
        use llimphi_ui::llimphi_raster::peniko::Fill;
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
        // Suelo: una línea horizontal cerca de la base.
        let gy = y + h * 0.82;
        let gx0 = x + w * 0.12;
        let gx1 = x + w * 0.88;
        scene.stroke(&Stroke::new(1.2), Affine::IDENTITY, piso.with_alpha(0.7), None, &Line::new(Point::new(gx0, gy), Point::new(gx1, gy)));
        // Gnomon: un poste vertical en el centro.
        let bx = x + w * 0.5;
        let top = y + h * 0.30;
        scene.stroke(&Stroke::new(1.8), Affine::IDENTITY, piso, None, &Line::new(Point::new(bx, gy), Point::new(bx, top)));
        // Sol: un disquito arriba, del lado opuesto a la sombra.
        if !de_noche {
            let signo = if ha >= 0.0 { -1.0 } else { 1.0 };
            let scx = bx + signo * w * 0.26;
            let scy = y + h * 0.18;
            scene.fill(Fill::NonZero, Affine::IDENTITY, sol, None, &Circle::new(Point::new(scx, scy), h * 0.10));
            // Sombra: desde la base del gnomon, en dirección del azimut (proyectado
            // a un abanico mañana→tarde por el ángulo horario) y largo por la razón.
            let largo = (h * 0.18 + ratio.min(3.0) * h * 0.16).min(w * 0.42);
            let ang = ha / 90.0 * core::f64::consts::FRAC_PI_2 * 0.9; // −80°..80°
            let ex = bx + ang.sin() * largo;
            let ey = gy; // la sombra corre por el suelo
            scene.stroke(&Stroke::new(2.4), Affine::IDENTITY, piso.with_alpha(0.55), None, &Line::new(Point::new(bx, gy), Point::new(ex, ey)));
        }
    })
}

/// Icono de **eclipse** (`cosmos-eclipses`): un disco brillante parcialmente tapado
/// por uno oscuro (la Luna sobre el Sol si es solar; la sombra de la Tierra sobre
/// la Luna si es lunar), con la fracción tapada según la magnitud. Fantasma raro:
/// sólo sale en la víspera. El tooltip dice qué eclipse y cuándo.
fn icono_eclipse(c: &crate::cielo::CieloState, theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_raster::peniko::Color;
    let solar = c.eclipse_solar;
    let mag = c.eclipse_magnitud.clamp(0.0, 1.2) as f64;
    let dias = c.eclipse_dias.unwrap_or(0.0);
    let cuando = if dias < 1.0 {
        "hoy".to_string()
    } else {
        format!("en {} días", dias.round() as i32)
    };
    let tip = format!(
        "Eclipse {} {cuando} · magnitud {:.2}",
        if solar { "solar" } else { "lunar" },
        mag
    );
    // Solar: disco brillante (Sol) tapado por uno oscuro. Lunar: disco lunar
    // entrando en la sombra rojiza (Luna de sangre).
    let disco = if solar { theme.accent } else { Color::from_rgb8(0xE8, 0xE0, 0xC8) };
    View::new(Style {
        size: Size { width: length(20.0_f32), height: length(20.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(tip)
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle, Point};
        use llimphi_ui::llimphi_raster::peniko::{BlendMode, Color, Fill};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let cx = (rect.x + rect.w * 0.5) as f64;
        let cy = (rect.y + rect.h * 0.5) as f64;
        let r = ((rect.w.min(rect.h) as f64) * 0.5 - 1.5).max(2.0);
        let cuerpo = Circle::new(Point::new(cx, cy), r);
        // Disco base (Sol brillante o Luna clara).
        scene.fill(Fill::NonZero, Affine::IDENTITY, disco, None, &cuerpo);
        // Tapado: un disco oscuro desplazado; cuanto mayor la magnitud, más cubre.
        let sombra = if solar {
            Color::from_rgba8(30, 33, 48, 255)
        } else {
            Color::from_rgba8(120, 40, 40, 235) // umbra rojiza (Luna de sangre)
        };
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, Affine::IDENTITY, &cuerpo);
        let dx = (1.0 - mag).clamp(0.0, 1.0) * 2.0 * r; // mag 1 = centrado (total)
        scene.fill(Fill::NonZero, Affine::IDENTITY, sombra, None, &Circle::new(Point::new(cx - r + dx, cy - r * 0.15), r));
        scene.pop_layer();
    })
}

/// Icono de **khipu**: una cuerda vertical con **nudos** —uno por nota visible
/// (hasta 4)—. El nudo más bajo (la nota más moribunda) titila cuando alguna está
/// por caer del horizonte: la última chance de reforzarla. Un khipu de verdad
/// registra con nudos; aquí cada nudo es un pensamiento que aún pende del hilo.
fn icono_khipu(k: &crate::khipu::KhipuSnapshot, titila: bool, theme: &Theme) -> View<Msg> {
    let n = k.notas.len().min(4);
    let por_caer = k.hay_por_caer;
    let cuerda = theme.fg_muted;
    let nudo = theme.fg_text;
    let alerta = theme.accent;
    let tip = if k.notas.is_empty() {
        "Khipu — anotar una nota".to_string()
    } else if por_caer {
        format!("Khipu — {} notas · una está por desvanecerse", k.notas.len())
    } else {
        format!("Khipu — {} notas", k.notas.len())
    };
    View::new(Style {
        size: Size { width: length(14.0_f32), height: length(20.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(tip)
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle, Line, Point, Stroke};
        use llimphi_ui::llimphi_raster::peniko::Fill;
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
        let cx = x + w * 0.5;
        // La cuerda (línea vertical).
        scene.stroke(&Stroke::new(1.6), Affine::IDENTITY, cuerda, None, &Line::new(Point::new(cx, y + 1.0), Point::new(cx, y + h - 1.0)));
        // Nudos repartidos a lo largo; el último (más bajo) titila si hay alerta.
        let cnt = n.max(1);
        for i in 0..cnt {
            let ny = y + h * (0.22 + 0.56 * (i as f64 / (cnt.max(1) as f64 - 1.0).max(1.0)));
            let ultimo = i + 1 == cnt;
            let (col, r) = if ultimo && por_caer {
                let dim = !titila;
                (alerta.with_alpha(if dim { 0.4 } else { 1.0 }), 3.0)
            } else {
                (nudo, 2.4)
            };
            scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &Circle::new(Point::new(cx, ny), r));
        }
        // Sin notas: un solo nudo tenue arriba (invita a anotar).
        if n == 0 {
            scene.fill(Fill::NonZero, Affine::IDENTITY, cuerda.with_alpha(0.6), None, &Circle::new(Point::new(cx, y + h * 0.3), 2.2));
        }
    })
}

/// Icono del **común** (tampu): una casita —la «casita de libros» del común— con
/// un punto de estado: verde si todo en orden, ámbar si hay una devolución
/// vencida, rojo (titila) si hay una manipulación sobre algo tuyo. Un objeto que
/// pende del común, con su cadena de custodia detrás.
fn icono_tampu(t: &crate::tampu::TampuSnapshot, titila: bool, theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_raster::peniko::Color;
    let casa = theme.fg_muted;
    let n = t.objetos.len();
    let (dot, grave) = if t.hay_anomalia {
        (Color::from_rgb8(0xE0, 0x5A, 0x5A), true) // rojo: manipulación
    } else if t.hay_vencido {
        (Color::from_rgb8(0xFB, 0xBF, 0x24), false) // ámbar: vencido
    } else {
        (Color::from_rgb8(0x5A, 0xD0, 0x8A), false) // verde: en orden
    };
    let dim = grave && !titila;
    let tip = if t.hay_anomalia {
        "Común — manipulación en la cadena de algo tuyo".to_string()
    } else if t.hay_vencido {
        "Común — una devolución venció".to_string()
    } else if n == 0 {
        "Común — sin objetos tuyos en juego".to_string()
    } else {
        format!("Común — {n} objeto{}", if n == 1 { "" } else { "s" })
    };
    View::new(Style {
        size: Size { width: length(20.0_f32), height: length(20.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(tip)
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, BezPath, Circle, Point, Stroke};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
        // Casita: un pentágono (paredes + techo a dos aguas).
        let mx = x + w * 0.5;
        let left = x + w * 0.18;
        let right = x + w * 0.82;
        let eave = y + h * 0.42; // altura del alero
        let base = y + h * 0.82;
        let mut casa_p = BezPath::new();
        casa_p.move_to(Point::new(left, base));
        casa_p.line_to(Point::new(left, eave));
        casa_p.line_to(Point::new(mx, y + h * 0.18)); // cumbrera
        casa_p.line_to(Point::new(right, eave));
        casa_p.line_to(Point::new(right, base));
        casa_p.close_path();
        scene.stroke(&Stroke::new(1.5), Affine::IDENTITY, casa, None, &casa_p);
        // Puerta.
        let mut puerta = BezPath::new();
        puerta.move_to(Point::new(mx - w * 0.10, base));
        puerta.line_to(Point::new(mx - w * 0.10, base - h * 0.20));
        puerta.line_to(Point::new(mx + w * 0.10, base - h * 0.20));
        puerta.line_to(Point::new(mx + w * 0.10, base));
        scene.stroke(&Stroke::new(1.2), Affine::IDENTITY, casa.with_alpha(0.8), None, &puerta);
        // Semáforo en la esquina superior derecha.
        let a = if dim { 0.35 } else { 1.0 };
        scene.fill(
            llimphi_ui::llimphi_raster::peniko::Fill::NonZero,
            Affine::IDENTITY,
            dot.with_alpha(a),
            None,
            &Circle::new(Point::new(x + w * 0.86, y + h * 0.16), h * 0.11),
        );
    })
}

/// Icono de **captura de pantalla / grabación**: una cámara compacta (cuerpo +
/// visor + lente). No titila ni sale saliente: es una acción, disponible al
/// revelar la zona. Cuando `grabando`, la lente se pinta rellena de rojo y aparece
/// un punto rojo arriba a la derecha — el acuse de que hay un screencast en curso
/// aunque el menú esté cerrado.
fn icono_captura(grabando: bool, theme: &Theme) -> View<Msg> {
    let fg = if grabando { llimphi_ui::llimphi_raster::peniko::Color::from_rgba8(0xE0, 0x3A, 0x3A, 0xFF) } else { theme.fg_muted };
    View::new(Style {
        size: Size { width: length(22.0_f32), height: length(18.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(if grabando { "Grabando… (clic para detener)" } else { "Captura de pantalla" })
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle, Point, RoundedRect, Stroke};
        use llimphi_ui::llimphi_raster::peniko::Color;
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
        // Cuerpo de la cámara.
        let bx = x + w * 0.10;
        let by = y + h * 0.28;
        let cuerpo = RoundedRect::new(bx, by, bx + w * 0.80, by + h * 0.60, 2.0);
        scene.stroke(&Stroke::new(1.4), Affine::IDENTITY, fg, None, &cuerpo);
        // Joroba del visor.
        let jx = x + w * 0.34;
        let joroba = RoundedRect::new(jx, y + h * 0.14, jx + w * 0.22, by + 0.5, 1.2);
        scene.stroke(&Stroke::new(1.3), Affine::IDENTITY, fg, None, &joroba);
        // Lente. Rellena de rojo mientras se graba.
        let cx = x + w * 0.5;
        let cy = by + h * 0.30;
        let lente = Circle::new(Point::new(cx, cy), h * 0.17);
        if grabando {
            scene.fill(llimphi_ui::llimphi_raster::peniko::Fill::NonZero, Affine::IDENTITY, fg, None, &lente);
        } else {
            scene.stroke(&Stroke::new(1.4), Affine::IDENTITY, fg, None, &lente);
        }
        // Punto rojo de «grabando» arriba a la derecha.
        if grabando {
            let rojo = Color::from_rgba8(0xE0, 0x3A, 0x3A, 0xFF);
            scene.fill(
                llimphi_ui::llimphi_raster::peniko::Fill::NonZero,
                Affine::IDENTITY,
                rojo,
                None,
                &Circle::new(Point::new(x + w * 0.90, y + h * 0.14), h * 0.12),
            );
        }
    })
}

/// Icono de **medios extraíbles** (USB): el símbolo del conector USB (un tridente
/// con un círculo y un cuadrado en las puntas). Un punto ámbar (titila) cuando hay
/// un extraíble **sin montar** — recién insertado, esperando.
fn icono_usb(u: &crate::usb::UsbSnapshot, titila: bool, theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_raster::peniko::Color;
    let sin_montar = u.hay_sin_montar;
    let fg = theme.fg_muted;
    let n = u.particiones.len();
    let tip = if sin_montar {
        "USB — un medio sin montar (clic para montar)".to_string()
    } else {
        format!("Medios extraíbles — {n} montado{}", if n == 1 { "" } else { "s" })
    };
    View::new(Style {
        size: Size { width: length(14.0_f32), height: length(20.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .tooltip(tip)
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle, Line, Point, Rect as KRect, Stroke};
        use llimphi_ui::llimphi_raster::peniko::Fill;
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
        let cx = x + w * 0.5;
        let st = Stroke::new(1.5);
        // Eje vertical.
        scene.stroke(&st, Affine::IDENTITY, fg, None, &Line::new(Point::new(cx, y + h * 0.12), Point::new(cx, y + h * 0.88)));
        // Cabeza (flecha) arriba.
        let mut cabeza = llimphi_ui::llimphi_raster::kurbo::BezPath::new();
        cabeza.move_to(Point::new(cx - w * 0.16, y + h * 0.24));
        cabeza.line_to(Point::new(cx, y + h * 0.08));
        cabeza.line_to(Point::new(cx + w * 0.16, y + h * 0.24));
        scene.stroke(&st, Affine::IDENTITY, fg, None, &cabeza);
        // Rama izquierda con círculo.
        let ly = y + h * 0.44;
        scene.stroke(&st, Affine::IDENTITY, fg, None, &Line::new(Point::new(cx, ly), Point::new(cx - w * 0.28, ly)));
        scene.stroke(&st, Affine::IDENTITY, fg, None, &Line::new(Point::new(cx - w * 0.28, ly), Point::new(cx - w * 0.28, y + h * 0.60)));
        scene.fill(Fill::NonZero, Affine::IDENTITY, fg, None, &Circle::new(Point::new(cx - w * 0.28, y + h * 0.60), 2.0));
        // Rama derecha con cuadrado.
        let ry = y + h * 0.58;
        scene.stroke(&st, Affine::IDENTITY, fg, None, &Line::new(Point::new(cx, ry), Point::new(cx + w * 0.28, ry)));
        scene.stroke(&st, Affine::IDENTITY, fg, None, &Line::new(Point::new(cx + w * 0.28, ry), Point::new(cx + w * 0.28, y + h * 0.72)));
        let sq = 3.0;
        scene.fill(Fill::NonZero, Affine::IDENTITY, fg, None, &KRect::new(cx + w * 0.28 - sq * 0.5, y + h * 0.72 - sq * 0.5, cx + w * 0.28 + sq * 0.5, y + h * 0.72 + sq * 0.5));
        // Aviso de sin-montar: un punto ámbar que titila junto a la cabeza.
        if sin_montar {
            let a = if titila { 1.0 } else { 0.35 };
            let ambar = Color::from_rgb8(0xFB, 0xBF, 0x24).with_alpha(a);
            scene.fill(Fill::NonZero, Affine::IDENTITY, ambar, None, &Circle::new(Point::new(x + w * 0.9, y + h * 0.14), 2.6));
        }
    })
}

/// Los doce glifos zodiacales desde Aries (0°). Espeja `pata_core::widget`
/// (constante privada allí); aquí se usa para el icono fugaz compacto.
const SIGNOS_GLIFO: [&str; 12] =
    ["♈", "♉", "♊", "♋", "♌", "♍", "♎", "♏", "♐", "♑", "♒", "♓"];

/// Color del glifo zodiacal según su elemento (fuego/tierra/aire/agua). Espeja
/// `render::widgets::astro_color`.
fn color_signo(idx: usize) -> llimphi_ui::llimphi_raster::peniko::Color {
    use llimphi_ui::llimphi_raster::peniko::Color;
    match idx % 12 {
        0 | 4 | 8 => Color::from_rgba8(232, 96, 64, 255),   // fuego (Aries/Leo/Sagitario)
        1 | 5 | 9 => Color::from_rgba8(120, 168, 96, 255),  // tierra (Tauro/Virgo/Capricornio)
        2 | 6 | 10 => Color::from_rgba8(232, 192, 96, 255), // aire (Géminis/Libra/Acuario)
        _ => Color::from_rgba8(96, 168, 232, 255),          // agua (Cáncer/Escorpio/Piscis)
    }
}

/// Icono de **signo**: el glifo zodiacal del Sol coloreado por su elemento
/// (fuego/tierra/aire/agua) — el mismo "dibujito por chip" del widget astral de
/// la barra. Saliente al ingresar el Sol a un signo nuevo.
fn icono_signo(sun_longitude: f32, _theme: &Theme) -> View<Msg> {
    let lon = sun_longitude.rem_euclid(360.0);
    let idx = (lon / 30.0) as usize % 12;
    let glifo = SIGNOS_GLIFO[idx];
    let color = color_signo(idx);
    View::new(Style {
        size: Size { width: length(20.0_f32), height: length(18.0_f32) },
        flex_shrink: 0.0,
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .tooltip("Signo solar")
    .text(glifo.to_string(), 14.0, color)
}


/// Convierte HSV (`h,s,v` en `0..1`) a un [`Color`] RGB opaco. Quedó del aura
/// de ánimo (retirada con la fusión de fantasmas); se conserva con sus tests
/// como utilidad de paleta.
#[allow(dead_code)]
fn hsv_color(h: f32, s: f32, v: f32) -> llimphi_ui::llimphi_raster::peniko::Color {
    use llimphi_ui::llimphi_raster::peniko::Color;
    let h = (h.rem_euclid(1.0)) * 6.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Color::from_rgb8(
        (((r + m) * 255.0).round()) as u8,
        (((g + m) * 255.0).round()) as u8,
        (((b + m) * 255.0).round()) as u8,
    )
}

/// Color de un nivel `0..1`: verde (bajo) → rojo (alto). Lenguaje compartido por
/// el abanico de CPU y el relleno de batería.
fn color_carga(nivel: f32) -> llimphi_ui::llimphi_raster::peniko::Color {
    use llimphi_ui::llimphi_raster::peniko::Color;
    lerp_color(
        Color::from_rgb8(0x5A, 0xD0, 0x8A),
        Color::from_rgb8(0xE0, 0x5A, 0x5A),
        nivel,
    )
}

/// Interpolación lineal entre dos colores (alpha fijo en 1.0; el llamador lo
/// ajusta con `with_alpha`).
fn lerp_color(
    a: llimphi_ui::llimphi_raster::peniko::Color,
    b: llimphi_ui::llimphi_raster::peniko::Color,
    t: f32,
) -> llimphi_ui::llimphi_raster::peniko::Color {
    use llimphi_ui::llimphi_raster::peniko::Color;
    let t = t.clamp(0.0, 1.0);
    let ca = a.components;
    let cb = b.components;
    Color::new([
        ca[0] + (cb[0] - ca[0]) * t,
        ca[1] + (cb[1] - ca[1]) * t,
        ca[2] + (cb[2] - ca[2]) * t,
        1.0,
    ])
}

/// Archivo del diagnóstico de los fugaces. Se escribe a **archivo** y no a
/// stderr porque a pata la respawnea el compositor: nadie ve su salida estándar.
pub const DIAG_FUGACES: &str = "/tmp/pata-diag-fugaces.txt";

/// Traza los números crudos del fade de los fugaces, una línea por cambio de
/// `avance`. Gateada por [`crate::layer::diag_on`] — `touch /tmp/pata-diag` y
/// respawn, sin necesidad de inyectarle env a un proceso que no arrancás vos.
///
/// La regla del fade está certificada por sonda (a 1100 px de caja el alpha
/// llega a 0 cerca del carácter 120), así que cuando los iconos «no se
/// esconden» lo que falla es una ENTRADA — casi siempre `avance` en 0 porque el
/// estado que se lee no es el que se está tipeando.
fn diag_fugaces(avance: (usize, usize), char_w: f32, cols_zona: usize, base: f32, revelar: f32) {
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    if !crate::layer::diag_on() {
        return;
    }
    // Una línea por cambio de `avance`: la trayectoria completa del tipeo sin
    // inundar el archivo con el mismo cuadro repetido a 30 Hz.
    static ULTIMO: AtomicUsize = AtomicUsize::new(usize::MAX);
    if ULTIMO.swap(avance.0, Ordering::Relaxed) == avance.0 {
        return;
    }
    let linea = format!(
        "avance={:<4} cols={:<4} char_w={char_w:.2} cols_zona={cols_zona:<3} \
         base={base:.2} revelar={revelar:.2}\n",
        avance.0, avance.1
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(DIAG_FUGACES) {
        let _ = f.write_all(linea.as_bytes());
    }
}

/// Fade del overlay según cuánto **le falta al texto para alcanzarlo**.
///
/// La cuenta es geométrica y ya no tiene números mágicos: `avance` es lo que
/// ocupa el texto en su fila y `cols` lo que entra en una fila, así que
/// `cols - avance` es cuántos caracteres faltan para el borde derecho. Los
/// iconos no viven EN el borde sino en una franja de `cols_zona` caracteres
/// antes — el ancho real que suman hoy los que se están viendo. Entonces
/// desaparecen cuando el texto llega a esa franja (más un respiro), y empiezan
/// a ceder `AVISO` caracteres antes de eso.
///
/// Que `cols_zona` se mida en vivo importa: con música sonando la franja es
/// más ancha que en reposo, y los iconos se esconden antes — solos.
fn fade_por_texto(avance: usize, cols: usize, cols_zona: usize) -> f32 {
    /// Respiro entre el texto y el primer icono: no esperamos a que se toquen.
    const RESPIRO: usize = 6;
    /// Cuánto antes de eso arrancan a desvanecerse.
    const AVISO: usize = 18;
    /// Fracción de fila que debe quedar libre para que los iconos sigan estando.
    /// Medido en vivo (`/tmp/pata-diag-fugaces.txt`): con el umbral puramente
    /// geométrico —«escondete cuando el texto esté a 15 caracteres»— los iconos
    /// aguantaban a opacidad PLENA hasta el 78% de la fila y recién se iban al
    /// 90%. Contra una fila de 164 columnas eso es una eternidad tipeando con
    /// los iconos encima. Que se vayan cuando la fila se está llenando, no
    /// cuando el texto los va a chocar.
    const FRAC_OCULTOS: f32 = 0.25;
    /// Y que empiecen a ceder bastante antes de eso.
    const FRAC_AVISO: f32 = 0.20;
    let falta = cols.saturating_sub(avance);
    // El ancho real de la franja es el PISO: por angosta que sea, nunca se
    // quedan hasta que el texto las toca.
    let ocultos = (cols_zona + RESPIRO).max((cols as f32 * FRAC_OCULTOS) as usize);
    let cediendo = ocultos + AVISO.max((cols as f32 * FRAC_AVISO) as usize);
    if falta >= cediendo {
        1.0
    } else if falta <= ocultos {
        0.0
    } else {
        (falta - ocultos) as f32 / (cediendo - ocultos).max(1) as f32
    }
}

/// Envuelve los hijos del cabezal (input vivo + badge) en el contenedor que
/// llena el espacio de la barra. Click sobre el borde (no sobre el input)
/// Envuelve el input del cabezal. YA NO togglea el drawer al click (eso tapaba el
/// completado flotante): el click sólo enfoca; el drawer se abre con Enter/enviar.
/// Línea **finísima** de progreso a lo largo del borde inferior del input de la
/// barra shell: refleja las acciones largas en curso (copiar/mover archivos) que
/// pata-notify agrega. Pista tenue + relleno de acento hasta la fracción. `None`
/// (sin actividad) = nada; fracción `< 0` (indeterminada) = línea de acento tenue
/// a todo lo ancho. Va **absoluta**: no altera el layout del input, sólo lo subraya.
fn hairline_progreso(progreso: Option<f32>, theme: &Theme) -> Option<View<Msg>> {
    let f = progreso?;
    let indet = f < 0.0;
    let frac = if indet { 1.0 } else { f.clamp(0.0, 1.0) };
    let fill = View::new(Style {
        size: Size { width: percent(frac), height: percent(1.0_f32) },
        ..Default::default()
    })
    .fill(if indet { theme.accent.with_alpha(0.45) } else { theme.accent })
    .radius(1.0);
    Some(
        View::new(Style {
            position: Position::Absolute,
            inset: TaffyRect {
                left: length(8.0_f32),
                right: length(8.0_f32),
                top: auto(),
                bottom: length(0.0_f32),
            },
            size: Size { width: auto(), height: length(2.0_f32) },
            ..Default::default()
        })
        .fill(theme.border.with_alpha(0.35))
        .radius(1.0)
        .children(vec![fill]),
    )
}

fn wrap_headline(children: Vec<View<Msg>>, open: bool) -> View<Msg> {
    let v = View::new(Style {
        flex_direction: FlexDirection::Row,
        // Llenar el espacio disponible de la barra en vez de un bloque fijo de
        // 380 px "botado en el medio": flex_basis 0 + grow toma el remanente; un
        // min razonable evita que se aplaste con muchos widgets. El alto lo fija
        // el propio input (auto).
        size: Size {
            width: auto(),
            height: auto(),
        },
        min_size: Size {
            width: length(220.0_f32),
            height: auto(),
        },
        flex_basis: length(0.0_f32),
        flex_grow: 1.0,
        flex_shrink: 1.0,
        padding: TaffyRect {
            left: length(0.0_f32),
            right: length(0.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexStart),
        gap: Size {
            width: length(0.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(children);
    // SIN toggle-al-click y SIN hover-drawer: el cabezal ya NO despliega el drawer
    // al clickearlo ni al pasar el puntero ni al tipear. El click SÓLO enfoca la
    // barra fina (el compositor le da el teclado), así el completado flotante bonito
    // aparece al tipear (gate `!shuma.open`). El drawer expande sólo cuando se **pide
    // salida**: Enter (lo arma `press_key`) o el botón de enviar del input. (Antes el
    // wrapper tenía `on_click(ShumaToggle)` y el click en el input caía en él → abría
    // el vistazo Fugaz y tapaba el completado.)
    let _ = open;
    v
}

/// A6 — el punto ámbar con halo del cabezal: «terminó un comando largo». Mismo
/// lenguaje visual que la badge del diente del chasis (`session_tooth_icon`).
fn long_alert_badge() -> View<Msg> {
    View::new(Style {
        size: Size { width: length(16.0_f32), height: length(16.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .paint_with(|scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle};
        use llimphi_ui::llimphi_raster::peniko::{Color, Fill};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let cx = (rect.x + rect.w * 0.5) as f64;
        let cy = (rect.y + rect.h * 0.5) as f64;
        let ambar = Color::from_rgb8(0xf7, 0xc8, 0x7a);
        let rad = (rect.w.min(rect.h) as f64 * 0.22).max(2.5);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            ambar.with_alpha(0.30),
            None,
            &Circle::new((cx, cy), rad * 1.9),
        );
        scene.fill(Fill::NonZero, Affine::IDENTITY, ambar, None, &Circle::new((cx, cy), rad));
    })
}

/// El cabezal en modo live-wire (shuma completa): un chip `prompt placeholder`
/// que despliega el drawer al click. Sin input vivo —ese vive adentro del drawer.
fn headline_chip(state: &ShumaState, theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_text::Alignment;
    let etiqueta = format!("{} {}", state.prompt, state.placeholder);
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: auto(),
            height: auto(),
        },
        min_size: Size {
            width: length(220.0_f32),
            height: auto(),
        },
        flex_basis: length(0.0_f32),
        flex_grow: 1.0,
        flex_shrink: 1.0,
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexStart),
        ..Default::default()
    })
    .on_click(Msg::ShumaToggle)
    .text_aligned(etiqueta, 13.0, theme.fg_muted, Alignment::Start)
}

/// La fracción de pantalla que ocupa el drawer según esté o no maximizado.
/// Un **TUI de pantalla completa** (claude/vim/htop) pide la terminal entera:
/// piso 0.95 aunque el drawer estuviera restaurado a media altura — sin esto
/// la TUI quedaba "cortada a mitad de pantalla".
fn drawer_frac(maximized: bool, tui_full: bool) -> f32 {
    let base = if maximized { 0.97 } else { DRAWER_FRAC };
    if tui_full {
        base.max(0.95)
    } else {
        base
    }
}

/// Tipo de botón de la barra de título del drawer — cada uno se pinta a mano
/// (vectores) para no depender de glifos que no estén en la fuente fallback.
#[derive(Clone, Copy)]
enum TbKind {
    /// Desdockea: abre la sesión en una instancia standalone de shuma.
    Undock,
    /// Minimiza: repliega el drawer (el input sigue en la barra).
    Minimize,
    /// Maximiza / restaura el alto del drawer.
    Maximize,
    /// Cierra el drawer.
    Close,
}

/// Un botón cuadrado de la barra de título, con su ícono pintado y su `on_click`.
fn tb_button(kind: TbKind, msg: Msg, theme: &Theme) -> View<Msg> {
    let fg = theme.fg_muted;
    let danger = kind_is_close(kind);
    View::new(Style {
        size: Size { width: length(28.0_f32), height: length(24.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Line, Rect as KRect, Stroke};
        use llimphi_ui::llimphi_raster::peniko::Color;
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        // Caja de 12×12 centrada en la que se dibuja el glifo.
        let s = 11.0_f64;
        let cx = (rect.x + rect.w * 0.5) as f64;
        let cy = (rect.y + rect.h * 0.5) as f64;
        let (x0, y0, x1, y1) = (cx - s * 0.5, cy - s * 0.5, cx + s * 0.5, cy + s * 0.5);
        let col = if danger {
            Color::from_rgb8(0xe0, 0x6c, 0x6c)
        } else {
            Color::new([fg.components[0], fg.components[1], fg.components[2], 0.85])
        };
        let st = Stroke::new(1.4);
        match kind {
            TbKind::Minimize => {
                scene.stroke(&st, Affine::IDENTITY, col, None, &Line::new((x0, y1), (x1, y1)));
            }
            TbKind::Maximize => {
                scene.stroke(
                    &st,
                    Affine::IDENTITY,
                    col,
                    None,
                    &KRect::new(x0, y0, x1, y1),
                );
            }
            TbKind::Close => {
                scene.stroke(&st, Affine::IDENTITY, col, None, &Line::new((x0, y0), (x1, y1)));
                scene.stroke(&st, Affine::IDENTITY, col, None, &Line::new((x0, y1), (x1, y0)));
            }
            TbKind::Undock => {
                // Cajita con una flecha saliendo hacia arriba-derecha.
                scene.stroke(
                    &st,
                    Affine::IDENTITY,
                    col,
                    None,
                    &KRect::new(x0, y0 + 2.5, x1 - 2.5, y1),
                );
                let a = (x1 - 4.0, y0 + 4.0);
                let b = (x1 + 1.0, y0 - 1.0);
                scene.stroke(&st, Affine::IDENTITY, col, None, &Line::new(a, b));
                scene.stroke(&st, Affine::IDENTITY, col, None, &Line::new(b, (b.0 - 4.5, b.1)));
                scene.stroke(&st, Affine::IDENTITY, col, None, &Line::new(b, (b.0, b.1 + 4.5)));
            }
        }
    })
    .on_click(msg)
}

fn kind_is_close(kind: TbKind) -> bool {
    matches!(kind, TbKind::Close)
}

/// La barra de título del drawer: el título a la izquierda y, a la derecha, los
/// controles desdockear · minimizar · maximizar · cerrar. Click en su fondo es
/// un no-op (`ShumaAnim`) para no cerrar el drawer al arrastrar/pulsar el borde.
pub fn drawer_titlebar(state: &ShumaState, theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_text::Alignment;
    let titulo = View::new(Style {
        flex_grow: 1.0,
        flex_basis: length(0.0_f32),
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text_aligned(state.placeholder.clone(), 12.0, theme.fg_muted, Alignment::Start);

    let controles = vec![
        tb_button(TbKind::Undock, Msg::ShumaUndock, theme),
        tb_button(TbKind::Minimize, Msg::ShumaToggle, theme),
        tb_button(TbKind::Maximize, Msg::ShumaMaximize, theme),
        tb_button(TbKind::Close, Msg::ShumaToggle, theme),
    ];

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(28.0_f32) },
        flex_shrink: 0.0,
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(6.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        gap: Size { width: length(2.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .on_click(Msg::ShumaAnim)
    .children(vec![titulo].into_iter().chain(controles).collect())
}

/// El drawer desplegado **con la shuma COMPLETA** (live-wire): mismo scrim +
/// panel inferior que [`drawer_overlay`], pero el cuerpo es la shuma entera
/// (dientes/sesiones/menubar/canvas) elevada al `Msg` de pata vía `Msg::ShumaFull`.
/// El overlay de la propia shuma (menús/modales) se pinta encima del cuerpo.
pub fn drawer_overlay_full(
    state: &ShumaState,
    full: &shuma_app::Model,
    screen: (i32, i32),
    theme: &Theme,
) -> Option<View<Msg>> {
    if !state.visible() {
        return None;
    }
    let t = state.anim.value().clamp(0.0, 1.0);
    let (_sw, sh) = screen;
    let tui_full = shuma_app::active_shell_state(full).is_some_and(|s| s.is_fullscreen_tui());
    let alto = (sh as f32 * drawer_frac(state.maximized, tui_full) * t).max(1.0);

    // Cuerpo: la shuma completa. Su `view` trae su propio fondo, rails y chrome.
    // Va dentro de un contenedor que crece bajo la barra de título.
    let cuerpo = View::new(Style {
        flex_grow: 1.0,
        flex_basis: length(0.0_f32),
        min_size: Size { width: auto(), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![shuma_app::view(full, crate::lift_shuma)]);
    let hijos = vec![drawer_titlebar(state, theme), cuerpo];
    // El overlay interno de shuma NO va acá dentro (ver `drawer_body_view_full`):
    // se apila abajo, absoluto sobre TODA la pantalla, que es el sistema de
    // coordenadas en el que sus menús se anclan al puntero.

    let panel = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            right: length(0.0_f32),
            top: auto(),
            bottom: length(0.0_f32),
        },
        size: Size {
            width: percent(1.0_f32),
            height: length(alto),
        },
        flex_direction: FlexDirection::Column,
        ..Default::default()
    })
    .on_click(Msg::ShumaAnim)
    .children(hijos);

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
    .fill(theme.bg_app)
    .alpha(0.55 * t)
    .on_click(Msg::ShumaToggle)
    .children(match overlay_absoluto(full) {
        // El overlay de shuma va sobre el SCRIM (que cubre la pantalla), no
        // dentro del panel: sus menús se anclan en coords de pantalla.
        Some(ov) => vec![panel, ov],
        None => vec![panel],
    });

    Some(scrim)
}

/// El drawer desplegado (path **winit**): scrim que cierra al click + panel
/// inferior con el shell real hospedado. `None` si no hay nada que mostrar.
pub fn drawer_overlay(state: &ShumaState, screen: (i32, i32), theme: &Theme) -> Option<View<Msg>> {
    if !state.visible() {
        return None;
    }
    let t = state.anim.value().clamp(0.0, 1.0);
    let (_sw, sh) = screen;
    let alto =
        (sh as f32 * drawer_frac(state.maximized, state.inner.is_fullscreen_tui()) * t).max(1.0);

    // El cuerpo es el shell real: su `view` ya trae cards/input/scroll/PTY y
    // pinta su propio fondo (`bg_app`). Los clicks de sus widgets vuelven como
    // `Msg::ShumaShell(..)` gracias al `lift`. Va dentro de un contenedor que
    // crece bajo la barra de título.
    let cuerpo = View::new(Style {
        flex_grow: 1.0,
        flex_basis: length(0.0_f32),
        min_size: Size { width: auto(), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![shuma_module_shell::view(&state.inner, theme, Msg::ShumaShell)]);

    let panel = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            right: length(0.0_f32),
            top: auto(),
            bottom: length(0.0_f32),
        },
        size: Size {
            width: percent(1.0_f32),
            height: length(alto),
        },
        flex_direction: FlexDirection::Column,
        ..Default::default()
    })
    // Absorbe los clicks sobre el borde del panel (padding) para que no se
    // filtren al scrim y cierren el drawer; `ShumaAnim` es un no-op de re-render.
    .on_click(Msg::ShumaAnim)
    .children(vec![drawer_titlebar(state, theme), cuerpo]);

    // Scrim a pantalla completa: oscurece el fondo y cierra al click.
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
    .fill(theme.bg_app)
    .alpha(0.55 * t)
    .on_click(Msg::ShumaToggle)
    .children(vec![panel]);

    Some(scrim)
}

/// El **cuerpo** del drawer (sin scrim ni posición absoluta), para el backend
/// `wlr-layer-shell`: ahí la propia layer surface ya *es* el panel del Quake (la
/// barra crece hacia arriba), así que no hace falta scrim ni animación. Es el
/// shell real hospedado, **sin el input** — el input ya vive en la barra (ver
/// [`headline_view`]). Llena el contenedor que le da el caller.
pub fn drawer_body_view(state: &ShumaState, theme: &Theme, popup_al_tope: bool) -> View<Msg> {
    shuma_module_shell::body_view(&state.inner, theme, Msg::ShumaShell, popup_al_tope)
}

/// El **cuerpo** del drawer en modo live-wire (path layer-shell): la shuma
/// COMPLETA (dientes/sesiones/menubar/canvas) elevada al `Msg` de pata. La
/// sesión activa pinta su cuerpo SIN input (vive en la barra, `hosted_bar`), así
/// que no se duplica. Apila el overlay interno (dropdowns/menús/modales) encima.
pub fn drawer_body_view_full(full: &shuma_app::Model, _theme: &Theme) -> View<Msg> {
    // OJO: el overlay de shuma NO va acá. Va como capa absoluta sobre la surface
    // entera (ver [`overlay_absoluto`] y `render::shuma_open_view`), por dos
    // razones: (1) metido como hermano en este contenedor compartía el flujo
    // —que es `Row` por defecto— y le comía la mitad del ancho al canvas (bug
    // del 25-jul: «click derecho en un tab y el drawer se reduce a la mitad, con
    // el menú a la derecha»); (2) los menús se anclan en coordenadas de surface,
    // así que su capa tiene que ser la surface, no este cuerpo.
    View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        ..Default::default()
    })
    .children(vec![shuma_app::view(full, crate::lift_shuma)])
}

/// El overlay interno de shuma (menús contextuales, dropdowns, modales) como
/// **capa absoluta que cubre toda la caja del caller** — que debe ser la surface
/// entera, porque ahí es donde caen las coordenadas del puntero con las que se
/// ancla el menú. `None` si shuma no tiene nada que superponer.
pub fn overlay_absoluto(full: &shuma_app::Model) -> Option<View<Msg>> {
    Some(capa_absoluta(shuma_app::view_overlay(full, crate::lift_shuma)?))
}

/// Envuelve `inner` en una **capa que flota sobre la caja del caller** sin
/// participar de su flujo. Es lo que separa un overlay de un hermano más: un
/// hijo normal en un contenedor `Row` (el default de taffy) se reparte el ancho
/// con los demás — así fue como el menú contextual de una pestaña le comió la
/// mitad del ancho al canvas del drawer (25-jul).
pub fn capa_absoluta(inner: View<Msg>) -> View<Msg> {
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            right: length(0.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(vec![inner])
}


#[cfg(test)]
mod tests_modo_apertura {
    use super::{accion_click_input, ClickInputAccion, OpenMode};

    #[test]
    fn cerrado_solo_enfoca_no_abre() {
        // Un click en el input con el drawer plegado SÓLO enfoca la barra fina (no
        // abre el drawer) → tipear muestra el completado flotante; el drawer va con Enter.
        assert_eq!(accion_click_input(false, OpenMode::Fugaz, true), ClickInputAccion::SoloEnfocar);
        assert_eq!(accion_click_input(false, OpenMode::Firme, false), ClickInputAccion::SoloEnfocar);
    }

    #[test]
    fn vistazo_vacio_cierra_por_re_click() {
        // El «volver a hacer click en el input sin haber tipeado» del pedido.
        assert_eq!(accion_click_input(true, OpenMode::Fugaz, true), ClickInputAccion::Cerrar);
    }

    #[test]
    fn vistazo_con_texto_no_cierra_solo_enfoca() {
        // Si ya escribió algo, re-clickear el input NO lo repliega (perdería lo
        // tipeado): sólo enfoca.
        assert_eq!(accion_click_input(true, OpenMode::Fugaz, false), ClickInputAccion::SoloEnfocar);
    }

    #[test]
    fn firme_nunca_cierra_por_click_en_input() {
        // El modo firme es acaparador: un click en el input jamás lo repliega.
        assert_eq!(accion_click_input(true, OpenMode::Firme, true), ClickInputAccion::SoloEnfocar);
        assert_eq!(accion_click_input(true, OpenMode::Firme, false), ClickInputAccion::SoloEnfocar);
    }

    #[test]
    fn solo_el_vistazo_cierra_por_gesto_liviano() {
        assert!(OpenMode::Fugaz.cierra_por_gesto_liviano());
        assert!(!OpenMode::Firme.cierra_por_gesto_liviano());
    }
}

#[cfg(test)]
mod tests_iconos {
    use super::fade_por_texto;

    /// El fade se mide por DISTANCIA a la franja de iconos, no por largo
    /// tipeado: con una barra ancha, cuarenta letras no tapan nada.
    #[test]
    fn fade_pleno_de_lejos_oculto_al_alcanzarlos() {
        const COLS: usize = 120;
        const ZONA: usize = 20; // la franja ocupa 20 caracteres
        assert_eq!(fade_por_texto(0, COLS, ZONA), 1.0);
        assert_eq!(fade_por_texto(44, COLS, ZONA), 1.0, "lejos: enteros");
        // Se apagan al llegar a la franja + respiro, no al borde de la barra.
        assert_eq!(fade_por_texto(COLS - ZONA - 6, COLS, ZONA), 0.0, "en la franja");
        assert_eq!(fade_por_texto(COLS, COLS, ZONA), 0.0);
        // En el medio, monótono decreciente a medida que se acerca.
        let a = fade_por_texto(COLS - ZONA - 20, COLS, ZONA);
        let b = fade_por_texto(COLS - ZONA - 12, COLS, ZONA);
        assert!(a > b && a < 1.0 && b > 0.0, "a={a} b={b}");
    }

    /// Los números REALES medidos en la barra del usuario
    /// (`/tmp/pata-diag-fugaces.txt`, 2026-07-22): fila de 164 columnas, franja
    /// de 9. Con el umbral viejo los iconos aguantaban a opacidad plena hasta el
    /// carácter 128 y recién se iban cerca del 147 — «todavía envalentonados».
    #[test]
    fn los_fugaces_ceden_mientras_se_llena_la_fila() {
        const COLS: usize = 164;
        const ZONA: usize = 9;
        let a = |avance| fade_por_texto(avance, COLS, ZONA);

        assert_eq!(a(0), 1.0, "input vacío: los iconos son el contenido");
        assert_eq!(a(60), 1.0, "a un tercio de fila todavía no molestan");

        // Empiezan a ceder cerca de la mitad de la fila…
        let mitad = a(95);
        assert!(mitad < 1.0 && mitad > 0.0, "cediendo a los 95: {mitad}");

        // …y ya no están cuando queda un cuarto de fila. El pedido del usuario
        // era exactamente éste: invisibles «según la longitud del texto», no
        // cuando el texto los va a chocar.
        assert_eq!(a(125), 0.0, "a 3/4 de fila ya no están");
        assert_eq!(a(164), 0.0);

        // Monótona: nunca se re-encienden al seguir escribiendo.
        let mut previo = 1.0;
        for avance in 0..=COLS {
            let v = a(avance);
            assert!(v <= previo + 1e-6, "sube en avance={avance}: {previo} → {v}");
            previo = v;
        }
    }

    /// El ancho real de la franja sigue siendo el PISO: con una franja ancha
    /// (música + batería + red) se esconden ANTES, no después.
    #[test]
    fn una_franja_ancha_los_esconde_antes() {
        let angosta = fade_por_texto(120, 164, 4);
        let ancha = fade_por_texto(120, 164, 60);
        assert!(ancha <= angosta, "franja ancha cede antes: {ancha} vs {angosta}");
        assert_eq!(ancha, 0.0);
    }

    /// Una caja angosta (barra chica) no debe dejarlos para siempre.
    #[test]
    fn en_una_fila_corta_tambien_se_van() {
        assert_eq!(fade_por_texto(30, 40, 4), 0.0, "40 columnas, 30 escritas");
    }

    /// Los iconos se esconden ANTES cuando hay más iconos a la vista: la franja
    /// mide más, y el texto la alcanza antes. Nada que configurar.
    #[test]
    fn mas_iconos_se_esconden_antes() {
        // Desde que el umbral es proporcional a la fila (ver `fade_por_texto`),
        // el ancho de la franja es el PISO y sólo manda cuando de verdad es
        // ancha — con pocos iconos gana el «se está llenando la fila». La
        // propiedad que este test cuida sigue intacta: más franja, se van antes.
        const COLS: usize = 120;
        let pocos = fade_por_texto(75, COLS, 8);
        let muchos = fade_por_texto(75, COLS, 40);
        assert!(muchos < pocos, "franja ancha se apaga antes: {muchos} < {pocos}");
        assert_eq!(muchos, 0.0);
        assert!(pocos > 0.0, "con franja angosta todavía se ven algo: {pocos}");
    }

    /// Una barra angosta esconde los iconos con mucho menos texto que una
    /// ancha — lo que antes era imposible con umbrales fijos.
    #[test]
    fn el_umbral_depende_del_ancho() {
        // Mismas treinta letras, dos anchos: en la angosta ya se fueron, en la
        // ancha ni se enteran.
        assert_eq!(fade_por_texto(30, 40, 10), 0.0, "barra angosta: ocultos");
        assert_eq!(fade_por_texto(30, 200, 10), 1.0, "barra ancha: ni cerca");
    }

    use super::hsv_color;

    #[test]
    fn hsv_esquinas_de_la_rueda() {
        // Vértices canónicos de HSV con s=v=1: rojo/verde/azul puros y el blanco.
        assert_eq!(hsv_color(0.0, 1.0, 1.0), Color::from_rgb8(255, 0, 0));
        assert_eq!(hsv_color(1.0 / 3.0, 1.0, 1.0), Color::from_rgb8(0, 255, 0));
        assert_eq!(hsv_color(2.0 / 3.0, 1.0, 1.0), Color::from_rgb8(0, 0, 255));
        assert_eq!(hsv_color(0.0, 0.0, 1.0), Color::from_rgb8(255, 255, 255));
        // El matiz envuelve: hue=1.0 ≡ hue=0.0 (rojo).
        assert_eq!(hsv_color(1.0, 1.0, 1.0), hsv_color(0.0, 1.0, 1.0));
    }

    use llimphi_ui::llimphi_raster::peniko::Color;
}

#[cfg(test)]
mod tests_hueco {
    use super::etiqueta_flotante;
    use llimphi_ui::llimphi_compositor::{measure_text_node, mount};
    use llimphi_ui::llimphi_layout::{taffy, LayoutTree};
    use llimphi_ui::llimphi_text::Typesetter;

    /// Ancho computado del hueco/notch (el contenedor con fill del label pwd/git)
    /// para un `cwd` dado, con medición de texto real.
    fn ancho_hueco(cwd: &str) -> f32 {
        let theme = llimphi_theme::Theme::dark();
        let view = etiqueta_flotante(std::path::Path::new(cwd), &theme);
        let mut layout = LayoutTree::new();
        let mounted = mount(&mut layout, view);
        let mut ts = Typesetter::new();
        let computed = {
            let tmap = &mounted.text_measures;
            layout
                .compute_with_measure(mounted.root, (2000.0, 100.0), |nid, known, avail| {
                    match tmap.get(&nid) {
                        Some(tm) => measure_text_node(&mut ts, tm, known, avail),
                        None => taffy::Size::ZERO,
                    }
                })
                .expect("layout")
        };
        computed.rects.get(&mounted.root).map(|r| r.w).unwrap_or(0.0)
    }

    #[test]
    fn el_hueco_se_adapta_al_ancho_del_pwd() {
        // Paths fuera de $HOME y sin `.git`: el label es SÓLO el pwd (sin `~` ni
        // rama), así el ancho depende puramente del largo del texto.
        let corto = ancho_hueco("/a");
        let largo = ancho_hueco("/una/ruta/mucho/mas/larga/y/profunda/todavia/aqui");
        assert!(
            largo > corto + 60.0,
            "el hueco no se adaptó al pwd (corto={corto}, largo={largo})"
        );
        // Y ya no es la caja fija vieja (344 px): el corto es mucho menor.
        assert!(corto < 200.0, "el hueco corto sigue siendo anchote: {corto}");
    }
}
