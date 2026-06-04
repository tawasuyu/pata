//! El esquema declarativo de `pata`.
//!
//! El archivo de config manda; el código no asume superficies ni widgets que
//! el config no nombre. Un [`Config`] es **una lista de [`Surface`]s** —no un
//! único panel—: el usuario despliega tantas barras, paneles y docks como
//! quiera, ancla cada uno a un borde, y reparte los widgets en sus slots
//! (`start` / `center` / `end`) con total libertad.
//!
//! El modelo es agnóstico del formato en disco: en Linux un loader TOML
//! deserializa directo a estos tipos (vía `serde`); en wawa el config llega por
//! akasha. Los valores de propiedad de cada widget se guardan como [`Prop`],
//! un enum cerrado y `no_std` —ni `toml::Value` ni nada atado a una
//! plataforma—.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// El borde de la pantalla al que se ancla una superficie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Anchor {
    /// Pegada al borde superior — la posición clásica de una barra de menú.
    #[default]
    Top,
    /// Pegada al borde inferior — donde suele ir el dock o el shell.
    Bottom,
    /// Pegada al borde izquierdo (superficie vertical).
    Left,
    /// Pegada al borde derecho (superficie vertical).
    Right,
}

impl Anchor {
    /// `true` si la superficie se extiende horizontalmente (top/bottom): su
    /// grosor es alto y sus slots se reparten en X. `false` para left/right.
    pub fn es_horizontal(&self) -> bool {
        matches!(self, Anchor::Top | Anchor::Bottom)
    }
}

/// Qué clase de superficie del marco se está describiendo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum SurfaceKind {
    /// Barra fina pegada a un borde (estilo waybar/eww): tres slots
    /// (`start` / `center` / `end`) con widgets en línea. Reserva su franja al
    /// compositor para que las ventanas no la tapen.
    #[default]
    Bar,
    /// Panel: un área donde flotan [`FloatingCard`]s posicionadas en píxeles
    /// (estilo conky). No reserva franja; vive sobre el escritorio.
    Panel,
    /// Dock: lanzadores y/o ventanas abiertas, centrado en su borde y del
    /// tamaño justo de su contenido. Puede autoesconderse.
    Dock,
    /// Sidebar acoplable: un **rail de dientes** vertical pegado a un borde
    /// (left/right). Cada diente ([`SidebarTab`]) despliega un panel con su
    /// widget de contenido (lógica de launcher: colapsado = sólo el rail,
    /// activar un diente despliega su panel sobre el escritorio). El rail
    /// reserva su grosor como una barra; si `autohide`, no reserva y reaparece
    /// al rozar el borde. El panel desplegado flota, no reserva.
    Sidebar,
}

/// El valor de una propiedad de widget, agnóstico del formato en disco. El
/// loader de cada plataforma (TOML, RON, akasha) deserializa a esto; los
/// widgets lo leen con los helpers de [`WidgetSpec`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(untagged))]
pub enum Prop {
    /// Booleano (`true` / `false`).
    Bool(bool),
    /// Entero — se prueba antes que [`Prop::Num`] para no perder la
    /// distinción int/float de formatos como TOML.
    Int(i64),
    /// Número de punto flotante.
    Num(f64),
    /// Cadena de texto.
    Str(String),
}

impl Prop {
    /// La prop como `&str`, si lo es.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Prop::Str(s) => Some(s),
            _ => None,
        }
    }

    /// La prop como número: un [`Prop::Int`] se promueve a `f64`.
    pub fn as_num(&self) -> Option<f64> {
        match self {
            Prop::Num(n) => Some(*n),
            Prop::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    /// La prop como booleano, si lo es.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Prop::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// Una entrada de widget: `kind` (qué widget) + props arbitrarias. El conjunto
/// de `kind`s es **abierto** —el frontend despacha por string y cae a un
/// placeholder si no lo conoce—, así que agregar un widget no toca el core.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct WidgetSpec {
    /// El identificador del widget builtin: `"clock"`, `"volume"`,
    /// `"astro"`, `"start_button"`, `"window_list"`, `"tray"`,
    /// `"shuma_input"`, …
    pub kind: String,
    /// Props que cada widget interpreta a su gusto (formato del reloj, hotkey
    /// del quake, etc.). Las claves no reconocidas se conservan.
    #[cfg_attr(feature = "serde", serde(default, flatten))]
    pub props: BTreeMap<String, Prop>,
}

impl WidgetSpec {
    /// Un widget sin props.
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            props: BTreeMap::new(),
        }
    }

    /// El mismo widget con una prop añadida (encadenable).
    pub fn with(mut self, key: impl Into<String>, value: Prop) -> Self {
        self.props.insert(key.into(), value);
        self
    }

    /// Lee una prop string. Devuelve `default` si falta o no es string.
    pub fn str_prop<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.props.get(key).and_then(Prop::as_str).unwrap_or(default)
    }

    /// Lee una prop numérica. Devuelve `default` si falta o no es número.
    pub fn num_prop(&self, key: &str, default: f64) -> f64 {
        self.props.get(key).and_then(Prop::as_num).unwrap_or(default)
    }

    /// Lee una prop booleana. Devuelve `default` si falta o no es booleana.
    pub fn bool_prop(&self, key: &str, default: bool) -> bool {
        self.props.get(key).and_then(Prop::as_bool).unwrap_or(default)
    }
}

/// Una tarjeta posicionada en píxeles dentro de un panel (estilo conky).
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct FloatingCard {
    /// Origen X en píxeles desde la esquina superior-izquierda del panel.
    pub x: f32,
    /// Origen Y en píxeles.
    pub y: f32,
    /// Ancho en píxeles.
    pub w: f32,
    /// Alto en píxeles.
    pub h: f32,
    /// Título opcional (chip arriba a la izquierda).
    #[cfg_attr(feature = "serde", serde(default))]
    pub title: Option<String>,
    /// Widgets apilados dentro de la tarjeta.
    #[cfg_attr(feature = "serde", serde(default))]
    pub widgets: Vec<WidgetSpec>,
}

/// Un diente de un [`SurfaceKind::Sidebar`]: una pestaña vertical del rail que,
/// al activarse, despliega un panel con su widget de contenido.
///
/// El `icon` es un identificador que el frontend mapea a su glifo/dibujo (igual
/// que el `kind` de un widget: conjunto abierto, cae a un default si no lo
/// conoce). El `label` rotula el panel desplegado y el tooltip del diente. El
/// `content` es un [`WidgetSpec`] —típicamente `kind = "navigator"`— que el
/// frontend pinta en el panel; el modelo no asume qué es.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SidebarTab {
    /// Identificador del icono del diente (`"files"`, `"monads"`, `"search"`…).
    #[cfg_attr(feature = "serde", serde(default))]
    pub icon: String,
    /// Rótulo del panel desplegado y tooltip del diente.
    #[cfg_attr(feature = "serde", serde(default))]
    pub label: String,
    /// El widget que se pinta en el panel cuando el diente está activo.
    #[cfg_attr(feature = "serde", serde(default))]
    pub content: WidgetSpec,
}

impl SidebarTab {
    /// Un diente con icono, rótulo y widget de contenido.
    pub fn new(icon: impl Into<String>, label: impl Into<String>, content: WidgetSpec) -> Self {
        Self {
            icon: icon.into(),
            label: label.into(),
            content,
        }
    }
}

/// Una superficie del marco: una barra, un panel, un dock o un sidebar anclado
/// a un borde.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Surface {
    /// Bar, Panel o Dock.
    #[cfg_attr(feature = "serde", serde(default))]
    pub kind: SurfaceKind,
    /// A qué borde se ancla.
    #[cfg_attr(feature = "serde", serde(default))]
    pub anchor: Anchor,
    /// Grosor en píxeles: alto para superficies horizontales (top/bottom),
    /// ancho para verticales (left/right). Un dock puede ignorarlo y crecer
    /// con su contenido.
    #[cfg_attr(feature = "serde", serde(default = "default_thickness"))]
    pub thickness: f32,
    /// Si `true`, la superficie se esconde y reaparece al hover/hotkey. El
    /// shell (`shuma_input`) vive típicamente en una barra inferior con esto.
    #[cfg_attr(feature = "serde", serde(default))]
    pub autohide: bool,
    /// Padding interno (px) entre el borde y el primer widget.
    #[cfg_attr(feature = "serde", serde(default = "default_padding"))]
    pub padding: f32,
    /// Separación (px) entre widgets adyacentes.
    #[cfg_attr(feature = "serde", serde(default = "default_gap"))]
    pub gap: f32,
    /// Slot inicial: pegado al inicio del eje (izquierda / arriba).
    #[cfg_attr(feature = "serde", serde(default))]
    pub start: Vec<WidgetSpec>,
    /// Slot central: centrado en el eje.
    #[cfg_attr(feature = "serde", serde(default))]
    pub center: Vec<WidgetSpec>,
    /// Slot final: pegado al final del eje (derecha / abajo).
    #[cfg_attr(feature = "serde", serde(default))]
    pub end: Vec<WidgetSpec>,
    /// Para `kind = panel`: las tarjetas flotantes que contiene.
    #[cfg_attr(feature = "serde", serde(default))]
    pub cards: Vec<FloatingCard>,
    /// Monitor al que anclar la superficie (nombre del conector, ej.
    /// `"HDMI-A-1"` o `"DP-1"`). Vacío = el compositor elige el primario.
    /// El backend `wlr-layer-shell` pasa este `wl_output` a
    /// `create_layer_surface`; si el nombre no matchea ninguno conectado,
    /// también cae al primario y se loguea un aviso.
    #[cfg_attr(feature = "serde", serde(default))]
    pub output: String,
    /// Para `kind = sidebar`: los dientes del rail. Cada uno despliega su panel.
    #[cfg_attr(feature = "serde", serde(default))]
    pub tabs: Vec<SidebarTab>,
    /// Para `kind = sidebar`: ancho (px) del panel que despliega un diente. El
    /// rail mismo usa `thickness`; el panel flota a su lado con este ancho.
    #[cfg_attr(feature = "serde", serde(default = "default_panel_width"))]
    pub panel_width: f32,
}

impl Default for Surface {
    fn default() -> Self {
        Self {
            kind: SurfaceKind::default(),
            anchor: Anchor::default(),
            thickness: default_thickness(),
            autohide: false,
            padding: default_padding(),
            gap: default_gap(),
            start: Vec::new(),
            center: Vec::new(),
            end: Vec::new(),
            cards: Vec::new(),
            output: String::new(),
            tabs: Vec::new(),
            panel_width: default_panel_width(),
        }
    }
}

impl Surface {
    /// Una barra anclada a `anchor`, con los slots vacíos.
    pub fn bar(anchor: Anchor) -> Self {
        Self {
            kind: SurfaceKind::Bar,
            anchor,
            ..Self::default()
        }
    }

    /// Un dock anclado a `anchor`.
    pub fn dock(anchor: Anchor) -> Self {
        Self {
            kind: SurfaceKind::Dock,
            anchor,
            ..Self::default()
        }
    }

    /// Un sidebar acoplable anclado a `anchor` (left/right), con el rail vacío.
    /// El grosor por defecto es el ancho del rail de dientes.
    pub fn sidebar(anchor: Anchor) -> Self {
        Self {
            kind: SurfaceKind::Sidebar,
            anchor,
            thickness: default_rail_thickness(),
            ..Self::default()
        }
    }
}

/// Settings transversales a todas las superficies.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct General {
    /// Zona horaria del reloj. `"auto"` la detecta del sistema; también acepta
    /// nombres IANA. La sincronización NTP no es de pata: la da el SO.
    #[cfg_attr(feature = "serde", serde(default = "default_timezone"))]
    pub timezone: String,
}

impl Default for General {
    fn default() -> Self {
        Self {
            timezone: default_timezone(),
        }
    }
}

/// El marco completo: settings generales + las superficies a desplegar.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Config {
    #[cfg_attr(feature = "serde", serde(default))]
    pub general: General,
    /// Las superficies del escritorio, en orden de declaración.
    #[cfg_attr(feature = "serde", serde(default))]
    pub surfaces: Vec<Surface>,
}

impl Config {
    /// El marco por defecto cuando no hay config: una barra superior con el
    /// botón de inicio y el reloj a la izquierda, la lista de ventanas al
    /// centro, y el racimo de indicadores (astro, clipboard, volumen, brillo,
    /// tray, medidores) más el input del shell a la derecha; y un dock inferior
    /// autoescondible. Referencia widgets que aún no existen como builtin —el
    /// frontend cae a un placeholder— para encodear la visión completa.
    pub fn preset() -> Self {
        let mut top = Surface::bar(Anchor::Top);
        top.start = vec![WidgetSpec::new("start_button"), WidgetSpec::new("clock")];
        top.center = vec![WidgetSpec::new("window_list")];
        top.end = vec![
            WidgetSpec::new("astro"),
            WidgetSpec::new("clipboard"),
            WidgetSpec::new("volume"),
            WidgetSpec::new("brightness"),
            WidgetSpec::new("tray"),
            WidgetSpec::new("ram_meter"),
            WidgetSpec::new("cpu_meter"),
        ];

        let mut shell = Surface::bar(Anchor::Bottom);
        shell.autohide = true;
        shell.thickness = 40.0;
        shell.center = vec![
            WidgetSpec::new("shuma_input").with("hotkey", Prop::Str("F12".to_string()))
        ];

        Self {
            general: General::default(),
            surfaces: vec![top, shell],
        }
    }
}

fn default_thickness() -> f32 {
    32.0
}
fn default_padding() -> f32 {
    12.0
}
fn default_gap() -> f32 {
    16.0
}
fn default_rail_thickness() -> f32 {
    44.0
}
fn default_panel_width() -> f32 {
    280.0
}
fn default_timezone() -> String {
    "auto".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_horizontalidad() {
        assert!(Anchor::Top.es_horizontal());
        assert!(Anchor::Bottom.es_horizontal());
        assert!(!Anchor::Left.es_horizontal());
        assert!(!Anchor::Right.es_horizontal());
    }

    #[test]
    fn defaults_de_anchor_y_kind() {
        assert_eq!(Anchor::default(), Anchor::Top);
        assert_eq!(SurfaceKind::default(), SurfaceKind::Bar);
    }

    #[test]
    fn prop_helpers_y_promocion_int_a_num() {
        let w = WidgetSpec::new("clock")
            .with("format", Prop::Str("%H:%M".to_string()))
            .with("size", Prop::Int(14))
            .with("ratio", Prop::Num(0.5))
            .with("flag", Prop::Bool(true));
        assert_eq!(w.str_prop("format", "?"), "%H:%M");
        // Int se promueve a f64 al leer como número.
        assert_eq!(w.num_prop("size", 0.0), 14.0);
        assert_eq!(w.num_prop("ratio", 0.0), 0.5);
        assert!(w.bool_prop("flag", false));
        // Defaults cuando la clave falta o el tipo no calza.
        assert_eq!(w.str_prop("nope", "def"), "def");
        assert_eq!(w.num_prop("format", -1.0), -1.0);
    }

    #[test]
    fn preset_tiene_barra_top_y_shell_bottom() {
        let cfg = Config::preset();
        assert_eq!(cfg.surfaces.len(), 2);
        let top = &cfg.surfaces[0];
        assert_eq!(top.anchor, Anchor::Top);
        assert_eq!(top.kind, SurfaceKind::Bar);
        assert_eq!(top.start[0].kind, "start_button");
        assert!(top.end.iter().any(|w| w.kind == "astro"));

        let shell = &cfg.surfaces[1];
        assert_eq!(shell.anchor, Anchor::Bottom);
        assert!(shell.autohide);
        assert_eq!(shell.center[0].kind, "shuma_input");
        assert_eq!(shell.center[0].str_prop("hotkey", "?"), "F12");
    }

    #[test]
    fn config_default_esta_vacio() {
        // Default (derive) = sin superficies; `preset()` es el marco poblado.
        let cfg = Config::default();
        assert!(cfg.surfaces.is_empty());
        assert_eq!(cfg.general.timezone, "auto");
    }

    #[test]
    fn surface_constructores() {
        assert_eq!(Surface::bar(Anchor::Left).anchor, Anchor::Left);
        assert_eq!(Surface::dock(Anchor::Bottom).kind, SurfaceKind::Dock);
    }

    #[test]
    fn sidebar_lleva_dientes_y_ancho_de_panel() {
        let mut sb = Surface::sidebar(Anchor::Left);
        sb.tabs.push(SidebarTab::new(
            "monads",
            "Mónadas",
            WidgetSpec::new("navigator").with("source", Prop::Str("nouser".to_string())),
        ));
        sb.tabs
            .push(SidebarTab::new("files", "Archivos", WidgetSpec::new("navigator")));
        assert_eq!(sb.kind, SurfaceKind::Sidebar);
        assert_eq!(sb.anchor, Anchor::Left);
        // El rail por defecto es fino; el panel desplegado es ancho.
        assert_eq!(sb.thickness, 44.0);
        assert_eq!(sb.panel_width, 280.0);
        assert_eq!(sb.tabs.len(), 2);
        assert_eq!(sb.tabs[0].icon, "monads");
        assert_eq!(sb.tabs[0].content.kind, "navigator");
        assert_eq!(sb.tabs[0].content.str_prop("source", "?"), "nouser");
    }
}
