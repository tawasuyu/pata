//! El modelo de widget de `pata`: **lógica de datos sin pincel**.
//!
//! Un [`WidgetSpec`] (lo que el config declara) se materializa en un objeto
//! [`Widget`] vivo. El widget no sabe dibujar: cada `tick` refresca su estado a
//! partir de un [`WidgetCtx`] —un snapshot agnóstico del sistema que el host
//! muestrea (reloj, CPU, RAM, volumen, brillo…)— y `view` emite un
//! [`WidgetView`], un view-model que describe *qué* mostrar (texto, medidor,
//! placeholder) sin decir *cómo*. El frontend (Llimphi en Linux, framebuffer en
//! wawa) traduce ese view-model a su pincel.
//!
//! La frontera está donde tiene que estar: el core es `no_std` y determinista,
//! así que **no lee el reloj ni los contadores del kernel** —eso son syscalls—.
//! El host los muestrea y los entrega en el [`WidgetCtx`]; el core sólo formatea
//! y compone. Misma lógica de datos para los dos mundos; cada uno aporta su
//! sampler y su pincel.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::config::WidgetSpec;

/// Tope de núcleos que un [`WidgetCtx`] reporta a la vez. 64 cubre cualquier
/// máquina de escritorio (y muchas de servidor) sin tocar la asignación dinámica
/// — el core es `no_std` y el ctx queda `Copy`.
pub const MAX_CORES: usize = 64;

/// Lectura del reloj descompuesta. El host la rellena desde su fuente de tiempo
/// (en Linux, la zona horaria de `general.timezone`); el core sólo la formatea.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClockReading {
    /// Año con siglo (p. ej. `2026`).
    pub year: u16,
    /// Mes `1..=12`.
    pub month: u8,
    /// Día del mes `1..=31`.
    pub day: u8,
    /// Día de la semana, `0` = domingo … `6` = sábado.
    pub weekday: u8,
    /// Hora `0..=23`.
    pub hour: u8,
    /// Minuto `0..=59`.
    pub minute: u8,
    /// Segundo `0..=59`.
    pub second: u8,
}

/// Modo de teselado del escritorio activo, para el **indicador de layout**
/// estilo dwm. Un enum `Copy` (no String) para que [`WidgetCtx`] siga barato;
/// el host mapea el slug que reporta el WM con [`LayoutGlyph::from_slug`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayoutGlyph {
    /// Sin compositor que reporte (o slug desconocido): el widget se oculta.
    #[default]
    Unknown,
    MasterStack,
    Monocle,
    Grid,
    Columns,
    Rows,
    CenteredMaster,
    Spiral,
}

impl LayoutGlyph {
    /// Mapea el slug que reporta el WM (`mirada-ctl workspaces … layout=<slug>`).
    pub fn from_slug(s: &str) -> Self {
        match s.trim() {
            "master-stack" => Self::MasterStack,
            "monocle" => Self::Monocle,
            "grid" => Self::Grid,
            "columns" => Self::Columns,
            "rows" => Self::Rows,
            "centered-master" => Self::CenteredMaster,
            "spiral" => Self::Spiral,
            _ => Self::Unknown,
        }
    }

    /// El símbolo ASCII del layout, estilo dwm (sin emojis que caigan a *tofu*).
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Unknown => "",
            Self::MasterStack => "[]=",
            Self::Monocle => "[M]",
            Self::Grid => "[#]",
            Self::Columns => "|||",
            Self::Rows => "===",
            Self::CenteredMaster => "|M|",
            Self::Spiral => "(@)",
        }
    }
}

/// El snapshot del sistema que alimenta a los widgets en cada `tick`. El host
/// lo muestrea (vía sysfs/PulseAudio en Linux, vía el kernel en wawa) y lo pasa
/// por referencia: el core no toca el SO. Todos los campos arrancan en cero/vacío,
/// así que un frontend puede llenar sólo lo que le importe. (Ya no es `Copy`:
/// lleva el título de la ventana enfocada, que es un `String`.)
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetCtx {
    /// Hora actual ya descompuesta.
    pub clock: ClockReading,
    /// Uso de CPU, fracción `0.0..=1.0`.
    pub cpu: f32,
    /// Uso de RAM, fracción `0.0..=1.0`.
    pub ram: f32,
    /// RAM usada en MiB (para la leyenda del medidor).
    pub ram_used_mb: u32,
    /// RAM total en MiB.
    pub ram_total_mb: u32,
    /// Volumen, fracción `0.0..=1.0`.
    pub volume: f32,
    /// `true` si el audio está silenciado.
    pub muted: bool,
    /// Brillo de pantalla, fracción `0.0..=1.0`.
    pub brightness: f32,
    /// Longitud eclíptica del Sol en grados `0..360` — la posición zodiacal. El
    /// host la computa (en Linux, vía una efeméride; ver `pata-llimphi::sampler`);
    /// el widget [`Astro`] la mapea a signo + grado.
    pub sun_longitude_deg: f32,
    /// Fase lunar como fracción del ciclo sinódico `0.0..=1.0`: `0` = luna nueva,
    /// `0.5` = llena, de vuelta a `1` = nueva.
    pub moon_phase: f32,
    /// Escritorio virtual activo, **1-based** (`1..=workspace_count`). `0` =
    /// desconocido (no hay compositor que lo reporte): el switcher se oculta. El
    /// host lo muestrea del WM (en Linux, `mirada-ctl workspaces`).
    pub active_workspace: u8,
    /// Cuántos escritorios virtuales hay. `0` = desconocido.
    pub workspace_count: u8,
    /// Máscara de escritorios **ocupados** (con al menos una ventana): el bit `i`
    /// (desde el menos significativo) marca el escritorio `i + 1`. Cubre hasta 16
    /// escritorios — de sobra para los 9 de mirada.
    pub workspace_occupied: u16,
    /// Máscara de escritorios visibles en **otro monitor** (no el enfocado):
    /// bit `i` = escritorio `i + 1` que está abierto en otra pantalla. El
    /// switcher los marca aparte (cliquearlos lleva el foco a ese monitor). `0`
    /// en mono-monitor o sin datos.
    pub workspace_others: u16,
    /// Uso por núcleo lógico, fracción `0.0..=1.0`. Sólo los primeros
    /// `cpu_cores_n` son válidos; el resto queda en cero. Lo llena el host
    /// (en Linux, leyendo todas las líneas `cpuN` de `/proc/stat`); el widget
    /// [`CpuCores`] lo proyecta como un racimo de mini-medidores.
    pub cpu_cores: [f32; MAX_CORES],
    /// Cantidad de núcleos lógicos detectados (`0..=MAX_CORES`). Si es 0 el
    /// widget [`CpuCores`] cae a [`WidgetView::Empty`].
    pub cpu_cores_n: u8,
    /// Modo de teselado del escritorio activo, para el indicador de layout.
    /// `Unknown` (default) = sin WM que reporte → el widget se oculta.
    pub layout: LayoutGlyph,
    /// Título de la ventana enfocada, para el widget de título estilo dwm.
    /// Vacío = sin foco (o sin WM) → el widget se oculta.
    pub focused_title: String,
    /// Código corto de la **distribución de teclado activa** (`"ES"`, `"US"`…),
    /// para el indicador de layout de teclado. Vacío = una sola distribución (o
    /// sin WM que reporte) → el widget [`KbdLayout`] se oculta. El host lo
    /// muestrea del WM (en Linux, `kbd=` de `mirada-ctl workspaces`).
    pub keyboard_layout: String,
    /// Escritorio activo **por monitor**: `(nombre_conector, escritorio_1based)`.
    /// El host lo muestrea del WM (en Linux, `outputs=DP-1:3,HDMI-A-1:4` de
    /// `mirada-ctl workspaces`). Vacío en mono-monitor o con un WM que no lo
    /// reporte. Lo consume el frontend layer-shell para pintar en **cada barra**
    /// el escritorio de su propio monitor (no el del monitor enfocado): resuelve
    /// el nombre del output de la barra y sobreescribe `active_workspace` /
    /// `workspace_others` con la vista de ese monitor.
    pub output_workspaces: Vec<(String, u8)>,
    /// **Hogar** de cada escritorio con dueño: `(escritorio_1based,
    /// nombre_conector del monitor al que pertenece)`. Del WM (`homes=` de
    /// `mirada-ctl workspaces`). Un conector que no aparezca en
    /// [`output_workspaces`](Self::output_workspaces) está desconectado: sus
    /// escritorios andan *prestados* al monitor enfocado hasta que vuelva.
    /// Vacío con un WM que no lo reporte.
    pub workspace_homes: Vec<(u8, String)>,
    /// Nombre del conector del monitor con el **foco** del sistema (hay uno
    /// solo). Del WM (`focus=` de `mirada-ctl workspaces`). Vacío si no reporta.
    pub focused_output: String,
}

impl Default for WidgetCtx {
    fn default() -> Self {
        Self {
            clock: ClockReading::default(),
            cpu: 0.0,
            ram: 0.0,
            ram_used_mb: 0,
            ram_total_mb: 0,
            volume: 0.0,
            muted: false,
            brightness: 0.0,
            sun_longitude_deg: 0.0,
            moon_phase: 0.0,
            active_workspace: 0,
            workspace_count: 0,
            workspace_occupied: 0,
            workspace_others: 0,
            cpu_cores: [0.0_f32; MAX_CORES],
            cpu_cores_n: 0,
            layout: LayoutGlyph::Unknown,
            focused_title: String::new(),
            keyboard_layout: String::new(),
            output_workspaces: Vec::new(),
            workspace_homes: Vec::new(),
            focused_output: String::new(),
        }
    }
}

/// Tamaño visual de un medidor. El frontend mapea cada nivel a su grilla
/// (ancho/alto de la barra y tamaño del texto); con `Small` además es razonable
/// pasar a `MeterOrient::Vertical` para que entre en una barra angosta o un
/// dock columnar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MeterSize {
    /// Chico: barra mínima, leyenda corta o ausente. Ideal para `Vertical`.
    Small,
    /// Tamaño actual del marco (el default histórico).
    #[default]
    Medium,
    /// Grande: barra ancha y leyenda con cuerpo más grande — para paneles
    /// flotantes o docks con espacio de sobra.
    Large,
}

impl MeterSize {
    /// Parsea `"small"`/`"sm"` / `"medium"`/`"md"` / `"large"`/`"lg"` (insensible
    /// a mayúsculas), o `None` si no cuadra — el spec cae al default.
    pub fn from_str(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("small") || s.eq_ignore_ascii_case("sm") {
            Some(Self::Small)
        } else if s.eq_ignore_ascii_case("medium") || s.eq_ignore_ascii_case("md") {
            Some(Self::Medium)
        } else if s.eq_ignore_ascii_case("large") || s.eq_ignore_ascii_case("lg") {
            Some(Self::Large)
        } else {
            None
        }
    }
}

/// Eje del medidor. `Horizontal` (default) pinta la barra de izquierda a
/// derecha; `Vertical` la levanta de abajo hacia arriba — el modo natural para
/// una barra/sidebar columnar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MeterOrient {
    /// Barra acostada (default).
    #[default]
    Horizontal,
    /// Barra parada — sube con el valor.
    Vertical,
}

impl MeterOrient {
    /// Parsea `"horizontal"`/`"h"`/`"row"` o `"vertical"`/`"v"`/`"col"`/
    /// `"column"` (insensible a mayúsculas).
    pub fn from_str(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("horizontal") || s.eq_ignore_ascii_case("h") || s.eq_ignore_ascii_case("row") {
            Some(Self::Horizontal)
        } else if s.eq_ignore_ascii_case("vertical") || s.eq_ignore_ascii_case("v") || s.eq_ignore_ascii_case("col") || s.eq_ignore_ascii_case("column") {
            Some(Self::Vertical)
        } else {
            None
        }
    }
}

/// El view-model que un widget emite: describe qué pintar sin atarse a ningún
/// pincel. El frontend hace el match y lo traduce a su backend gráfico.
#[derive(Debug, Clone, PartialEq)]
pub enum WidgetView {
    /// Nada que pintar (un widget que aún no tiene datos).
    Empty,
    /// Una línea de texto: el reloj, una etiqueta.
    Text(String),
    /// Texto compacto con tooltip aparte: lo que va en el chip vs. lo que va
    /// en el tooltip son distintos. El glifo del signo zodiacal va en `text`
    /// (chip apretado) y nombre + grado + fase lunar en `tooltip`.
    TextRich {
        /// Lo que se pinta en el chip de la barra (tipicamente un glifo).
        text: String,
        /// Lectura completa que se muestra al posar el cursor.
        tooltip: String,
        /// Fracción `0.0..=1.0` para un **medidor circular** que rodea el glifo
        /// (p. ej. el grado dentro del signo zodiacal, 0..30°). `0.0` = sin anillo.
        ring: f32,
    },
    /// Un medidor: `fraction` en `0.0..=1.0`, una `caption` ya formateada y una
    /// `label` opcional (el nombre corto, p. ej. `"CPU"`).
    Meter {
        /// Etiqueta corta, o `None` si el widget la oculta.
        label: Option<String>,
        /// Fracción `0.0..=1.0` que el frontend pinta como barra/arco.
        fraction: f32,
        /// Leyenda ya formateada (`"42%"`, `"3.2G"`, `"muted"`).
        caption: String,
        /// Tamaño visual sugerido (mapeado a px por el frontend).
        size: MeterSize,
        /// Eje de la barra (horizontal/vertical).
        orient: MeterOrient,
    },
    /// Un racimo de medidores —típicamente un mini-medidor por núcleo de CPU—
    /// que el frontend pinta como un mosaico/columna (estilo systemmonitor de
    /// KDE). El `fractions` viene en orden estable; `caption` es el agregado
    /// (p. ej. `"42% (8)"`).
    Cores {
        /// Etiqueta corta del racimo, o `None`.
        label: Option<String>,
        /// Fracción `0..1` por núcleo, en el orden del sistema.
        fractions: Vec<f32>,
        /// Leyenda agregada (promedio + cantidad), ya formateada.
        caption: String,
        /// Tamaño visual sugerido.
        size: MeterSize,
        /// Eje del racimo (filas vs. columnas de mini-barras).
        orient: MeterOrient,
    },
    /// Un selector de escritorios virtuales: una celda por escritorio, la activa
    /// resaltada y las ocupadas con un realce tenue. El frontend pinta la fila y
    /// cablea el click de cada celda a "ir a ese escritorio".
    Workspaces {
        /// Escritorio activo, **1-based** (`1..=count`).
        active: u8,
        /// Total de escritorios a pintar.
        count: u8,
        /// Máscara de ocupados (bit `i` → escritorio `i + 1`).
        occupied: u16,
        /// Máscara de escritorios visibles en **otro monitor** (bit `i` →
        /// escritorio `i + 1`). El frontend los marca distinto.
        others: u16,
    },
    /// Fase lunar — fracción del ciclo sinódico (`0`/`1` = nueva, `0.5` = llena)
    /// + el nombre de la fase para el tooltip. El frontend pinta el disco con
    /// **shapes** (un círculo iluminado desplazado contra un fondo oscuro): los
    /// glifos emoji 🌑..🌘 caen a *tofu* en cualquier máquina sin Noto Color
    /// Emoji y arruinan la lectura. Mejor dibujarla.
    Moon {
        /// Fracción del ciclo `0..=1`.
        phase: f32,
        /// Nombre de la fase (para el tooltip).
        name: String,
    },
    /// Un widget cuyo `kind` el core no implementa todavía: el frontend pinta un
    /// chip tenue con este nombre. Permite encodear la visión completa del marco
    /// (start_button, tray, astro…) antes de que cada widget exista.
    Placeholder(String),
}

/// Un widget vivo: refresca su estado en cada `tick` y emite su view-model en
/// `view`. La lógica de datos vive aquí; el dibujo, en el frontend.
pub trait Widget {
    /// Refresca el estado interno con el snapshot del sistema.
    fn tick(&mut self, ctx: &WidgetCtx);
    /// El view-model actual.
    fn view(&self) -> WidgetView;
}

/// Reloj: formatea [`ClockReading`] según una cadena estilo `strftime` reducida.
///
/// Tokens soportados (suficientes para una barra): `%H %M %S` (hora/min/seg a
/// dos dígitos), `%I %p` (12h + AM/PM), `%d %m %Y %y` (día/mes/año), `%%`
/// (porcentaje literal). Cualquier otro carácter pasa tal cual. No es un
/// `strftime` completo a propósito: nombres de mes/día localizados los resuelve
/// `rimay-localize` aguas arriba, no este core.
#[derive(Debug, Clone)]
pub struct Clock {
    format: String,
    text: String,
}

impl Clock {
    /// Construye desde el spec leyendo la prop `format` (default `"%H:%M"`).
    pub fn from_spec(spec: &WidgetSpec) -> Self {
        Self {
            format: spec.str_prop("format", "%H:%M").to_string(),
            text: String::new(),
        }
    }
}

impl Widget for Clock {
    fn tick(&mut self, ctx: &WidgetCtx) {
        self.text = format_time(&self.format, &ctx.clock);
    }

    fn view(&self) -> WidgetView {
        if self.text.is_empty() {
            WidgetView::Empty
        } else {
            WidgetView::Text(self.text.clone())
        }
    }
}

/// De dónde saca su valor un [`Meter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterSource {
    /// Uso de CPU.
    Cpu,
    /// Uso de RAM (leyenda en GiB usados/total).
    Ram,
    /// Volumen de audio (leyenda `"muted"` si está silenciado).
    Volume,
    /// Brillo de pantalla.
    Brightness,
}

impl MeterSource {
    /// La etiqueta corta por defecto de la fuente.
    fn label_por_defecto(&self) -> &'static str {
        match self {
            MeterSource::Cpu => "CPU",
            MeterSource::Ram => "RAM",
            MeterSource::Volume => "VOL",
            MeterSource::Brightness => "BRI",
        }
    }
}

/// Lee `size`/`orientation` del spec, con defaults razonables: si la
/// `orientation` no se nombra pero el tamaño es `Small`, asumimos `Vertical`
/// (entra en una barra angosta sin caption). El resto cae a horizontal/medium.
fn size_orient_de(spec: &WidgetSpec) -> (MeterSize, MeterOrient) {
    let size = MeterSize::from_str(spec.str_prop("size", "")).unwrap_or_default();
    // Default global: vertical. La barra es horizontal, así que un medidor
    // horizontal "consume ancho" sin necesidad — la columna vertical entra en
    // el alto de la barra y aprovecha el espacio. Para forzar horizontal hay
    // que escribir `orientation = "horizontal"` explícito.
    let orient_explicit = MeterOrient::from_str(spec.str_prop("orientation", ""));
    let orient = orient_explicit.unwrap_or(MeterOrient::Vertical);
    (size, orient)
}

/// Medidor genérico: lee una fracción `0..1` del [`WidgetCtx`] según su
/// [`MeterSource`] y arma una leyenda. Cubre cpu/ram/volumen/brillo con la
/// misma lógica; el frontend decide si lo pinta como barra, arco o ícono.
#[derive(Debug, Clone)]
pub struct Meter {
    source: MeterSource,
    label: Option<String>,
    fraction: f32,
    caption: String,
    size: MeterSize,
    orient: MeterOrient,
}

impl Meter {
    /// Construye un medidor de `source` leyendo del spec:
    /// - `label` (string): override de la etiqueta corta;
    /// - `show_label` (bool, default `true`): si es `false`, oculta la etiqueta;
    /// - `size` (string, default `"medium"`): `"small"` / `"medium"` / `"large"`;
    /// - `orientation` (string): `"horizontal"` (default) o `"vertical"`. Si no
    ///   se nombra y `size = "small"`, asumimos vertical — un medidor chico
    ///   horizontal pierde la leyenda al cuantizar.
    pub fn from_spec(source: MeterSource, spec: &WidgetSpec) -> Self {
        let label = if spec.bool_prop("show_label", true) {
            Some(
                spec.str_prop("label", source.label_por_defecto())
                    .to_string(),
            )
        } else {
            None
        };
        let (size, orient) = size_orient_de(spec);
        Self {
            source,
            label,
            fraction: 0.0,
            caption: String::new(),
            size,
            orient,
        }
    }
}

impl Widget for Meter {
    fn tick(&mut self, ctx: &WidgetCtx) {
        match self.source {
            MeterSource::Cpu => {
                self.fraction = ctx.cpu;
                self.caption = porcentaje(ctx.cpu);
            }
            MeterSource::Ram => {
                self.fraction = ctx.ram;
                self.caption = leyenda_memoria(ctx.ram_used_mb, ctx.ram_total_mb);
            }
            MeterSource::Volume => {
                self.fraction = ctx.volume;
                self.caption = if ctx.muted {
                    "muted".to_string()
                } else {
                    porcentaje(ctx.volume)
                };
            }
            MeterSource::Brightness => {
                self.fraction = ctx.brightness;
                self.caption = porcentaje(ctx.brightness);
            }
        }
    }

    fn view(&self) -> WidgetView {
        WidgetView::Meter {
            label: self.label.clone(),
            fraction: self.fraction.clamp(0.0, 1.0),
            caption: self.caption.clone(),
            size: self.size,
            orient: self.orient,
        }
    }
}

/// Medidor multinúcleo (`cpu_cores` / `cpu_cores_meter`): pinta una mini-barra
/// por core lógico, en el orden del sistema. Toma el snapshot por core que el
/// host muestrea en `ctx.cpu_cores[..ctx.cpu_cores_n]` y agrega el promedio en
/// la leyenda (`"42% (8)"`). Es el equivalente del *System Load Viewer* de KDE.
///
/// Props:
/// - `label` (string, default `"CPU"`), `show_label` (bool, default `true`).
/// - `size` (string, default `"medium"`), `orientation` (string, default
///   `"horizontal"` / `"vertical"` si size = small).
/// - `max` (número entero, default 0 = todos): tope de cores a pintar; los
///   sobrantes se ignoran (útil para barras muy estrechas).
#[derive(Debug, Clone)]
pub struct CpuCores {
    label: Option<String>,
    fractions: Vec<f32>,
    caption: String,
    size: MeterSize,
    orient: MeterOrient,
    max: usize,
}

impl CpuCores {
    /// Construye un racimo desde el spec — ver doc del struct.
    pub fn from_spec(spec: &WidgetSpec) -> Self {
        let label = if spec.bool_prop("show_label", true) {
            Some(spec.str_prop("label", "CPU").to_string())
        } else {
            None
        };
        let (size, orient) = size_orient_de(spec);
        let max = spec.num_prop("max", 0.0).max(0.0) as usize;
        Self {
            label,
            fractions: Vec::new(),
            caption: String::new(),
            size,
            orient,
            max,
        }
    }
}

impl Widget for CpuCores {
    fn tick(&mut self, ctx: &WidgetCtx) {
        let n = (ctx.cpu_cores_n as usize).min(MAX_CORES);
        let n = if self.max > 0 { n.min(self.max) } else { n };
        self.fractions.clear();
        let mut acc = 0.0_f32;
        for i in 0..n {
            let f = ctx.cpu_cores[i].clamp(0.0, 1.0);
            self.fractions.push(f);
            acc += f;
        }
        if n == 0 {
            self.caption = String::new();
        } else {
            let prom = acc / n as f32;
            self.caption = format!("{} ({})", porcentaje(prom), n);
        }
    }

    fn view(&self) -> WidgetView {
        if self.fractions.is_empty() {
            WidgetView::Empty
        } else {
            WidgetView::Cores {
                label: self.label.clone(),
                fractions: self.fractions.clone(),
                caption: self.caption.clone(),
                size: self.size,
                orient: self.orient,
            }
        }
    }
}

/// Los doce signos del zodíaco con su glifo, en orden desde Aries (0°). Los
/// nombres van en español (regla del repo); el glifo es el símbolo astrológico.
const SIGNOS: [(&str, &str); 12] = [
    ("Aries", "♈"),
    ("Tauro", "♉"),
    ("Géminis", "♊"),
    ("Cáncer", "♋"),
    ("Leo", "♌"),
    ("Virgo", "♍"),
    ("Libra", "♎"),
    ("Escorpio", "♏"),
    ("Sagitario", "♐"),
    ("Capricornio", "♑"),
    ("Acuario", "♒"),
    ("Piscis", "♓"),
];

/// Las ocho fases lunares con su glifo, en orden desde la nueva. El índice se
/// saca de la fracción del ciclo (`moon_phase * 8`, redondeado).
const FASES_LUNA: [(&str, &str); 8] = [
    ("Nueva", "🌑"),
    ("Creciente", "🌒"),
    ("Cuarto creciente", "🌓"),
    ("Gibosa creciente", "🌔"),
    ("Llena", "🌕"),
    ("Gibosa menguante", "🌖"),
    ("Cuarto menguante", "🌗"),
    ("Menguante", "🌘"),
];

/// Widget astral del Sol: el glifo del signo zodiacal en el chip y, en el
/// tooltip, nombre + grado dentro del signo. La fase lunar tiene su propio
/// widget (ver [`Moon`]) — antes la mezclaba este; ahora la separación es
/// limpia: un dibujito por chip. La efeméride la resuelve el host y la entrega
/// en el [`WidgetCtx`]; este widget sólo mapea grados a signo, con aritmética
/// entera (`core` no tiene `floor`/`round` de punto flotante).
///
/// Props:
/// - `degree` (bool, default `true`): incluye el grado en el tooltip.
/// - `name` (bool, default `true`): incluye el nombre del signo en el tooltip.
#[derive(Debug, Clone)]
pub struct Astro {
    show_degree: bool,
    show_name: bool,
    glyph: String,
    tooltip: String,
    /// Fracción del grado dentro del signo (0..30° → 0.0..1.0), para el anillo.
    ring: f32,
}

impl Astro {
    /// Construye desde el spec leyendo `degree` / `name`.
    pub fn from_spec(spec: &WidgetSpec) -> Self {
        Self {
            show_degree: spec.bool_prop("degree", true),
            show_name: spec.bool_prop("name", true),
            glyph: String::new(),
            tooltip: String::new(),
            ring: 0.0,
        }
    }
}

impl Widget for Astro {
    fn tick(&mut self, ctx: &WidgetCtx) {
        let lon = ((ctx.sun_longitude_deg as i32) % 360 + 360) % 360;
        let (nombre, glifo) = SIGNOS[(lon / 30) as usize % 12];
        let grado = lon % 30;
        self.ring = grado as f32 / 30.0;

        self.glyph = glifo.to_string();
        let mut tip = String::new();
        if self.show_name {
            tip.push_str(nombre);
        }
        if self.show_degree {
            if !tip.is_empty() {
                tip.push(' ');
            }
            tip.push_str(&format!("{grado}°"));
        }
        if tip.is_empty() {
            tip.push_str(glifo);
        }
        self.tooltip = tip;
    }

    fn view(&self) -> WidgetView {
        if self.glyph.is_empty() {
            WidgetView::Empty
        } else {
            WidgetView::TextRich {
                text: self.glyph.clone(),
                tooltip: self.tooltip.clone(),
                ring: self.ring,
            }
        }
    }
}

/// Widget del ciclo lunar: emite [`WidgetView::Moon`] con la fracción del ciclo
/// y el nombre de la fase. El frontend la pinta con shapes (un disco iluminado
/// desplazado contra un fondo oscuro) — antes emitía el glifo emoji 🌑..🌘,
/// pero en cualquier máquina sin Noto Color Emoji salía como cuadrado tofu.
/// La fase la entrega el host vía [`WidgetCtx::moon_phase`] (`0..1`).
#[derive(Debug, Clone, Default)]
pub struct Moon {
    phase: f32,
    name: String,
    primed: bool,
}

impl Moon {
    /// Construye con los defaults (no tiene props aún).
    pub fn from_spec(_spec: &WidgetSpec) -> Self {
        Self::default()
    }
}

impl Widget for Moon {
    fn tick(&mut self, ctx: &WidgetCtx) {
        let frac = ctx.moon_phase.clamp(0.0, 1.0);
        // El nombre se saca del bin más cercano de las 8 fases canónicas, para
        // que el tooltip diga "Llena" / "Creciente" / … sin números.
        let idx = ((frac * 8.0 + 0.5) as usize) % 8;
        let (nombre, _glifo) = FASES_LUNA[idx];
        self.phase = frac;
        self.name = nombre.to_string();
        self.primed = true;
    }

    fn view(&self) -> WidgetView {
        if !self.primed {
            WidgetView::Empty
        } else {
            WidgetView::Moon {
                phase: self.phase,
                name: self.name.clone(),
            }
        }
    }
}

/// Botón de inicio: el ancla del menú de aplicaciones. Por ahora sólo muestra su
/// etiqueta (`label`, default `"⊞"`); cablear su acción —abrir el lanzador—
/// llega cuando el marco rutee clicks (Fase 7, junto al Quake de shuma).
#[derive(Debug, Clone)]
pub struct StartButton {
    label: String,
}

impl StartButton {
    /// Construye desde el spec leyendo la prop `label`.
    pub fn from_spec(spec: &WidgetSpec) -> Self {
        Self {
            label: spec.str_prop("label", "⊞").to_string(),
        }
    }
}

impl Widget for StartButton {
    fn tick(&mut self, _ctx: &WidgetCtx) {}

    fn view(&self) -> WidgetView {
        WidgetView::Text(self.label.clone())
    }
}

/// Selector de escritorios virtuales (*workspace switcher*): refleja el estado
/// del WM —escritorio activo y cuáles tienen ventanas— que el host muestrea y
/// entrega en el [`WidgetCtx`]. El core sólo transcribe ese estado a un
/// view-model; el frontend lo pinta como una fila de celdas clickeables y, al
/// click, le pide al WM saltar a ese escritorio.
///
/// Si el host no reporta escritorios (`workspace_count == 0`, p. ej. el WM aún
/// no respondió), cae a `fallback` celdas (prop `count`, default 9) para que el
/// control **siempre se vea** — un switcher invisible no sirve. Sólo desaparece
/// si `fallback` es 0 explícito.
#[derive(Debug, Clone)]
pub struct WorkspaceSwitcher {
    active: u8,
    count: u8,
    occupied: u16,
    others: u16,
    /// Celdas a pintar cuando el WM no reporta (prop `count`, default 9).
    fallback: u8,
}

impl Default for WorkspaceSwitcher {
    fn default() -> Self {
        Self { active: 0, count: 0, occupied: 0, others: 0, fallback: 9 }
    }
}

impl WorkspaceSwitcher {
    /// Construye desde el spec. Lee `count` = cuántas celdas mostrar mientras el
    /// WM no reporta (default 9, el `WORKSPACE_COUNT` de mirada).
    pub fn from_spec(spec: &WidgetSpec) -> Self {
        let fallback = spec.num_prop("count", 9.0).clamp(0.0, 16.0) as u8;
        Self { fallback, ..Self::default() }
    }
}

impl Widget for WorkspaceSwitcher {
    fn tick(&mut self, ctx: &WidgetCtx) {
        self.active = ctx.active_workspace;
        self.count = ctx.workspace_count;
        self.occupied = ctx.workspace_occupied;
        self.others = ctx.workspace_others;
    }

    fn view(&self) -> WidgetView {
        // El WM tiene prioridad; si no reportó nada todavía, usamos el fallback
        // para que el switcher sea visible y clickeable desde el arranque.
        let count = if self.count > 0 { self.count } else { self.fallback };
        if count == 0 {
            WidgetView::Empty
        } else {
            WidgetView::Workspaces {
                active: self.active.max(1),
                count,
                occupied: self.occupied,
                others: self.others,
            }
        }
    }
}

/// Indicador de **layout** estilo dwm: pinta el símbolo del modo de teselado
/// del escritorio activo (`[]=` maestra+pila, `[M]` monóculo, `(@)` espiral…).
/// El estado viene del WM por el [`WidgetCtx::layout`]; si no hay compositor que
/// reporte (`Unknown`), su `view` es [`WidgetView::Empty`] y desaparece.
#[derive(Debug, Clone, Default)]
pub struct LayoutIndicator {
    glyph: LayoutGlyph,
}

impl LayoutIndicator {
    /// Construye desde el spec (hoy sin props; el estado viene del WM).
    pub fn from_spec(_spec: &WidgetSpec) -> Self {
        Self::default()
    }
}

impl Widget for LayoutIndicator {
    fn tick(&mut self, ctx: &WidgetCtx) {
        self.glyph = ctx.layout;
    }

    fn view(&self) -> WidgetView {
        match self.glyph {
            LayoutGlyph::Unknown => WidgetView::Empty,
            g => WidgetView::Text(g.symbol().to_string()),
        }
    }
}

/// Título de la **ventana enfocada**, estilo barra de dwm/Hyprland. El texto
/// viene del WM por el [`WidgetCtx::focused_title`]; se trunca a `max` caracteres
/// (prop `max`, default 80) con una elipsis. Vacío = sin foco → desaparece.
#[derive(Debug, Clone)]
pub struct WindowTitle {
    title: String,
    max: usize,
}

impl WindowTitle {
    /// Construye desde el spec leyendo la prop `max` (largo máximo en caracteres).
    pub fn from_spec(spec: &WidgetSpec) -> Self {
        let max = spec.num_prop("max", 80.0).max(1.0) as usize;
        Self {
            title: String::new(),
            max,
        }
    }
}

impl Widget for WindowTitle {
    fn tick(&mut self, ctx: &WidgetCtx) {
        self.title = ctx.focused_title.clone();
    }

    fn view(&self) -> WidgetView {
        if self.title.is_empty() {
            return WidgetView::Empty;
        }
        // Trunca por caracteres (no por bytes: respeta UTF-8) y agrega elipsis.
        if self.title.chars().count() > self.max {
            let mut s: String = self.title.chars().take(self.max).collect();
            s.push('\u{2026}'); // …
            WidgetView::Text(s)
        } else {
            WidgetView::Text(self.title.clone())
        }
    }
}

/// Indicador de **distribución de teclado** (`ES`/`US`/`RU`…), estilo el
/// `keyboard` de waybar. El código viene del WM por [`WidgetCtx::keyboard_layout`]
/// (en mirada, la opción `grp:*toggle` rota entre las distribuciones de
/// `xkb_layout`). Vacío = una sola distribución (nada que indicar) o sin WM →
/// [`WidgetView::Empty`] y desaparece.
#[derive(Debug, Clone, Default)]
pub struct KbdLayout {
    code: String,
}

impl KbdLayout {
    /// Construye desde el spec (hoy sin props; el estado viene del WM).
    pub fn from_spec(_spec: &WidgetSpec) -> Self {
        Self::default()
    }
}

impl Widget for KbdLayout {
    fn tick(&mut self, ctx: &WidgetCtx) {
        self.code = ctx.keyboard_layout.clone();
    }

    fn view(&self) -> WidgetView {
        if self.code.is_empty() {
            WidgetView::Empty
        } else {
            WidgetView::Text(self.code.clone())
        }
    }
}

/// Widget de relleno para un `kind` que el core no implementa todavía. Su `view`
/// es siempre un [`WidgetView::Placeholder`] con el nombre del kind.
#[derive(Debug, Clone)]
pub struct Placeholder {
    kind: String,
}

impl Placeholder {
    /// Un placeholder que muestra `kind`.
    pub fn new(kind: impl Into<String>) -> Self {
        Self { kind: kind.into() }
    }
}

impl Widget for Placeholder {
    fn tick(&mut self, _ctx: &WidgetCtx) {}

    fn view(&self) -> WidgetView {
        WidgetView::Placeholder(self.kind.clone())
    }
}

/// Materializa un [`WidgetSpec`] en un [`Widget`] vivo. Los `kind`s que el core
/// ya implementa (reloj y medidores) se construyen con su lógica; el resto cae a
/// un [`Placeholder`] —el conjunto de kinds es abierto, así que esto nunca
/// falla—. Los widgets que dependen de IPC o crates externos (`window_list`,
/// `astro`, `tray`, `shuma_input`) llegan en fases posteriores.
pub fn build(spec: &WidgetSpec) -> Box<dyn Widget> {
    match spec.kind.as_str() {
        "clock" => Box::new(Clock::from_spec(spec)),
        "cpu_meter" => Box::new(Meter::from_spec(MeterSource::Cpu, spec)),
        "cpu_cores" | "cpu_cores_meter" => Box::new(CpuCores::from_spec(spec)),
        "ram_meter" => Box::new(Meter::from_spec(MeterSource::Ram, spec)),
        "volume" => Box::new(Meter::from_spec(MeterSource::Volume, spec)),
        "brightness" => Box::new(Meter::from_spec(MeterSource::Brightness, spec)),
        "astro" => Box::new(Astro::from_spec(spec)),
        "moon" => Box::new(Moon::from_spec(spec)),
        "start_button" => Box::new(StartButton::from_spec(spec)),
        "workspaces" | "workspace_switcher" => Box::new(WorkspaceSwitcher::from_spec(spec)),
        "layout" | "layout_indicator" => Box::new(LayoutIndicator::from_spec(spec)),
        "keyboard_layout" | "kbd_layout" => Box::new(KbdLayout::from_spec(spec)),
        "window_title" | "title" => Box::new(WindowTitle::from_spec(spec)),
        _ => Box::new(Placeholder::new(&spec.kind)),
    }
}

/// Materializa una lista de specs (un slot completo) de una pasada.
pub fn build_all(specs: &[WidgetSpec]) -> Vec<Box<dyn Widget>> {
    specs.iter().map(build).collect()
}

/// Formatea un [`ClockReading`] con la cadena `fmt` (subconjunto de `strftime`,
/// ver [`Clock`]).
fn format_time(fmt: &str, t: &ClockReading) -> String {
    let mut out = String::with_capacity(fmt.len() + 4);
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('H') => empuja_dos(&mut out, t.hour),
            Some('M') => empuja_dos(&mut out, t.minute),
            Some('S') => empuja_dos(&mut out, t.second),
            Some('I') => {
                let h12 = match t.hour % 12 {
                    0 => 12,
                    h => h,
                };
                empuja_dos(&mut out, h12);
            }
            Some('p') => out.push_str(if t.hour < 12 { "AM" } else { "PM" }),
            Some('d') => empuja_dos(&mut out, t.day),
            Some('m') => empuja_dos(&mut out, t.month),
            Some('Y') => out.push_str(&format!("{:04}", t.year)),
            Some('y') => empuja_dos(&mut out, (t.year % 100) as u8),
            Some('%') => out.push('%'),
            // Token desconocido: lo dejamos literal (`%` + el carácter).
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// Empuja `n` como dos dígitos con cero a la izquierda.
fn empuja_dos(out: &mut String, n: u8) {
    out.push_str(&format!("{:02}", n));
}

/// Una fracción `0..1` como porcentaje entero: `0.42 → "42%"`.
fn porcentaje(frac: f32) -> String {
    // `f32::round` vive en `std`; aquí (no_std) redondeamos a mano. El valor es
    // siempre ≥ 0 (fracción clampeada), así que `+ 0.5` y truncar basta.
    let pct = (frac.clamp(0.0, 1.0) * 100.0 + 0.5) as i32;
    format!("{}%", pct)
}

/// Leyenda de memoria `"usado/total"` en GiB con un decimal: `"3.2/15.5G"`.
fn leyenda_memoria(used_mb: u32, total_mb: u32) -> String {
    let used = used_mb as f32 / 1024.0;
    let total = total_mb as f32 / 1024.0;
    format!("{:.1}/{:.1}G", used, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Prop;

    fn ctx() -> WidgetCtx {
        WidgetCtx {
            clock: ClockReading {
                year: 2026,
                month: 6,
                day: 1,
                weekday: 1,
                hour: 14,
                minute: 7,
                second: 9,
            },
            cpu: 0.42,
            ram: 0.5,
            ram_used_mb: 3277, // ~3.2 GiB
            ram_total_mb: 15872,
            volume: 0.75,
            muted: false,
            brightness: 0.3,
            ..WidgetCtx::default()
        }
    }

    #[test]
    fn layout_indicator_pinta_el_simbolo_dwm_y_se_oculta_sin_wm() {
        let mut w = LayoutIndicator::from_spec(&WidgetSpec::new("layout"));
        // Sin WM (Unknown) → oculto.
        w.tick(&WidgetCtx::default());
        assert_eq!(w.view(), WidgetView::Empty);
        // Con un layout reportado → su símbolo.
        let c = WidgetCtx {
            layout: LayoutGlyph::Spiral,
            ..WidgetCtx::default()
        };
        w.tick(&c);
        assert_eq!(w.view(), WidgetView::Text("(@)".to_string()));
    }

    #[test]
    fn layout_glyph_mapea_los_slugs_de_mirada() {
        assert_eq!(LayoutGlyph::from_slug("master-stack"), LayoutGlyph::MasterStack);
        assert_eq!(LayoutGlyph::from_slug("centered-master"), LayoutGlyph::CenteredMaster);
        assert_eq!(LayoutGlyph::from_slug("garabato"), LayoutGlyph::Unknown);
        assert_eq!(LayoutGlyph::MasterStack.symbol(), "[]=");
    }

    #[test]
    fn window_title_muestra_y_trunca_el_foco() {
        // Sin foco → oculto.
        let mut w = WindowTitle::from_spec(&WidgetSpec::new("window_title"));
        w.tick(&WidgetCtx::default());
        assert_eq!(w.view(), WidgetView::Empty);
        // Con título corto → tal cual.
        let c = WidgetCtx {
            focused_title: "foot".to_string(),
            ..WidgetCtx::default()
        };
        w.tick(&c);
        assert_eq!(w.view(), WidgetView::Text("foot".to_string()));
        // Trunca a `max` con elipsis (respeta caracteres).
        let mut corto =
            WindowTitle::from_spec(&WidgetSpec::new("window_title").with("max", Prop::Num(3.0)));
        let c = WidgetCtx {
            focused_title: "abcdef".to_string(),
            ..WidgetCtx::default()
        };
        corto.tick(&c);
        assert_eq!(corto.view(), WidgetView::Text("abc\u{2026}".to_string()));
    }

    #[test]
    fn reloj_formatea_default_y_segundos() {
        let mut c = Clock::from_spec(&WidgetSpec::new("clock"));
        c.tick(&ctx());
        assert_eq!(c.view(), WidgetView::Text("14:07".to_string()));

        let mut con_seg = Clock::from_spec(
            &WidgetSpec::new("clock").with("format", Prop::Str("%H:%M:%S".to_string())),
        );
        con_seg.tick(&ctx());
        assert_eq!(con_seg.view(), WidgetView::Text("14:07:09".to_string()));
    }

    #[test]
    fn reloj_12h_fecha_y_literales() {
        let spec = WidgetSpec::new("clock")
            .with("format", Prop::Str("%d/%m/%Y %I:%M %p".to_string()));
        let mut c = Clock::from_spec(&spec);
        c.tick(&ctx());
        assert_eq!(c.view(), WidgetView::Text("01/06/2026 02:07 PM".to_string()));

        // %% literal y token desconocido pasa tal cual.
        let mut raro = Clock::from_spec(
            &WidgetSpec::new("clock").with("format", Prop::Str("%H%% %q".to_string())),
        );
        raro.tick(&ctx());
        assert_eq!(raro.view(), WidgetView::Text("14% %q".to_string()));
    }

    #[test]
    fn medianoche_en_12h_es_las_12() {
        let mut t = ctx();
        t.clock.hour = 0;
        let mut c = Clock::from_spec(
            &WidgetSpec::new("clock").with("format", Prop::Str("%I %p".to_string())),
        );
        c.tick(&t);
        assert_eq!(c.view(), WidgetView::Text("12 AM".to_string()));
    }

    #[test]
    fn cpu_meter_emite_fraccion_y_porcentaje() {
        let mut m = Meter::from_spec(MeterSource::Cpu, &WidgetSpec::new("cpu_meter"));
        m.tick(&ctx());
        assert_eq!(
            m.view(),
            WidgetView::Meter {
                label: Some("CPU".to_string()),
                fraction: 0.42,
                caption: "42%".to_string(),
                size: MeterSize::Medium,
                // Default orient: Vertical (la barra es horizontal; columnas
                // entran mejor que filas).
                orient: MeterOrient::Vertical,
            }
        );
    }

    #[test]
    fn meter_lee_size_orient_y_small_implica_vertical() {
        // Small sin orientación explícita → vertical (cabe en una barra angosta).
        let spec = WidgetSpec::new("cpu_meter").with("size", Prop::Str("small".into()));
        let mut m = Meter::from_spec(MeterSource::Cpu, &spec);
        m.tick(&ctx());
        match m.view() {
            WidgetView::Meter { size, orient, .. } => {
                assert_eq!(size, MeterSize::Small);
                assert_eq!(orient, MeterOrient::Vertical);
            }
            v => panic!("esperaba Meter, vino {v:?}"),
        }
        // Override explícito de orientation gana sobre la heurística.
        let spec = WidgetSpec::new("cpu_meter")
            .with("size", Prop::Str("small".into()))
            .with("orientation", Prop::Str("horizontal".into()));
        let mut m = Meter::from_spec(MeterSource::Cpu, &spec);
        m.tick(&ctx());
        match m.view() {
            WidgetView::Meter { orient, .. } => assert_eq!(orient, MeterOrient::Horizontal),
            v => panic!("esperaba Meter, vino {v:?}"),
        }
        // Large sin orientación → vertical (default global).
        let spec = WidgetSpec::new("cpu_meter").with("size", Prop::Str("LARGE".into()));
        let m = Meter::from_spec(MeterSource::Cpu, &spec);
        assert_eq!(
            (m.size, m.orient),
            (MeterSize::Large, MeterOrient::Vertical)
        );
    }

    #[test]
    fn cpu_cores_pinta_un_mini_medidor_por_core() {
        let mut c = ctx();
        c.cpu_cores_n = 4;
        c.cpu_cores[0] = 0.10;
        c.cpu_cores[1] = 0.50;
        c.cpu_cores[2] = 0.30;
        c.cpu_cores[3] = 0.90;
        let mut w = CpuCores::from_spec(&WidgetSpec::new("cpu_cores"));
        w.tick(&c);
        match w.view() {
            WidgetView::Cores { fractions, caption, label, .. } => {
                assert_eq!(fractions, vec![0.10, 0.50, 0.30, 0.90]);
                assert_eq!(caption, "45% (4)");
                assert_eq!(label, Some("CPU".to_string()));
            }
            v => panic!("esperaba Cores, vino {v:?}"),
        }
    }

    #[test]
    fn cpu_cores_sin_datos_es_empty() {
        let w = CpuCores::from_spec(&WidgetSpec::new("cpu_cores"));
        assert_eq!(w.view(), WidgetView::Empty);
    }

    #[test]
    fn cpu_cores_respeta_max_tope() {
        let mut c = ctx();
        c.cpu_cores_n = 8;
        for i in 0..8 {
            c.cpu_cores[i] = 0.5;
        }
        let spec = WidgetSpec::new("cpu_cores").with("max", Prop::Int(4));
        let mut w = CpuCores::from_spec(&spec);
        w.tick(&c);
        match w.view() {
            WidgetView::Cores { fractions, caption, .. } => {
                assert_eq!(fractions.len(), 4);
                assert_eq!(caption, "50% (4)");
            }
            v => panic!("esperaba Cores, vino {v:?}"),
        }
    }

    #[test]
    fn ram_meter_leyenda_en_gib() {
        let mut m = Meter::from_spec(MeterSource::Ram, &WidgetSpec::new("ram_meter"));
        m.tick(&ctx());
        match m.view() {
            WidgetView::Meter { caption, .. } => assert_eq!(caption, "3.2/15.5G"),
            v => panic!("esperaba Meter, vino {v:?}"),
        }
    }

    #[test]
    fn volumen_muteado_dice_muted() {
        let mut t = ctx();
        t.muted = true;
        let mut m = Meter::from_spec(MeterSource::Volume, &WidgetSpec::new("volume"));
        m.tick(&t);
        match m.view() {
            WidgetView::Meter { caption, .. } => assert_eq!(caption, "muted"),
            v => panic!("esperaba Meter, vino {v:?}"),
        }
    }

    #[test]
    fn meter_oculta_label_con_show_label_false() {
        let spec = WidgetSpec::new("cpu_meter").with("show_label", Prop::Bool(false));
        let mut m = Meter::from_spec(MeterSource::Cpu, &spec);
        m.tick(&ctx());
        match m.view() {
            WidgetView::Meter { label, .. } => assert_eq!(label, None),
            v => panic!("esperaba Meter, vino {v:?}"),
        }
    }

    #[test]
    fn meter_label_override() {
        let spec = WidgetSpec::new("cpu_meter").with("label", Prop::Str("Proc".to_string()));
        let m = Meter::from_spec(MeterSource::Cpu, &spec);
        match m.view() {
            WidgetView::Meter { label, .. } => assert_eq!(label, Some("Proc".to_string())),
            v => panic!("esperaba Meter, vino {v:?}"),
        }
    }

    #[test]
    fn workspace_switcher_sin_compositor_cae_al_fallback() {
        // Sin estado del WM, el switcher NO desaparece: muestra `fallback` celdas
        // (default 9) para ser visible y clickeable desde el arranque.
        let mut w = WorkspaceSwitcher::from_spec(&WidgetSpec::new("workspaces"));
        w.tick(&ctx()); // el ctx de prueba no setea campos de workspace
        assert_eq!(
            w.view(),
            WidgetView::Workspaces { active: 1, count: 9, occupied: 0, others: 0 }
        );
    }

    #[test]
    fn workspace_switcher_count_cero_explicito_oculta() {
        // `count = 0` en el spec apaga el fallback: el widget desaparece.
        let mut w =
            WorkspaceSwitcher::from_spec(&WidgetSpec::new("workspaces").with("count", Prop::Num(0.0)));
        w.tick(&ctx());
        assert_eq!(w.view(), WidgetView::Empty);
    }

    #[test]
    fn workspace_switcher_transcribe_estado_del_wm() {
        let mut c = ctx();
        c.active_workspace = 2;
        c.workspace_count = 9;
        c.workspace_occupied = 0b0000_0101; // escritorios 1 y 3 ocupados
        c.workspace_others = 0b0001_0000; // escritorio 5 en otro monitor
        let mut w = WorkspaceSwitcher::from_spec(&WidgetSpec::new("workspace_switcher"));
        w.tick(&c);
        assert_eq!(
            w.view(),
            WidgetView::Workspaces {
                active: 2,
                count: 9,
                occupied: 0b0000_0101,
                others: 0b0001_0000,
            }
        );
    }

    #[test]
    fn build_despacha_el_workspace_switcher() {
        // Ambos alias materializan el mismo widget; sin estado del WM caen al
        // fallback de 9 celdas (en vez de desaparecer).
        for kind in ["workspaces", "workspace_switcher"] {
            let w = build(&WidgetSpec::new(kind));
            assert_eq!(
                w.view(),
                WidgetView::Workspaces { active: 1, count: 9, occupied: 0, others: 0 }
            );
        }
    }

    #[test]
    fn kind_desconocido_cae_a_placeholder() {
        // window_list/tray/shuma_input todavía no son builtin (IPC/shell pendiente).
        let w = build(&WidgetSpec::new("window_list"));
        assert_eq!(w.view(), WidgetView::Placeholder("window_list".to_string()));
        assert_eq!(
            build(&WidgetSpec::new("tray")).view(),
            WidgetView::Placeholder("tray".to_string())
        );
    }

    #[test]
    fn astro_mapea_longitud_a_signo_y_grado() {
        let mut ctx = ctx();
        // 132° → 132/30 = 4 → Leo; 132 % 30 = 12°.
        ctx.sun_longitude_deg = 132.0;
        ctx.moon_phase = 0.0; // nueva (ya no afecta a Astro)
        let mut a = Astro::from_spec(&WidgetSpec::new("astro"));
        a.tick(&ctx);
        assert_eq!(
            a.view(),
            WidgetView::TextRich {
                text: "♌".to_string(),
                tooltip: "Leo 12°".to_string(),
                ring: 12.0 / 30.0,
            }
        );
    }

    #[test]
    fn astro_normaliza_longitud_negativa() {
        let mut ctx = ctx();
        // -30° normaliza a 330° → Piscis (330/30 = 11), grado 0.
        ctx.sun_longitude_deg = -30.0;
        let mut a = Astro::from_spec(&WidgetSpec::new("astro"));
        a.tick(&ctx);
        assert_eq!(
            a.view(),
            WidgetView::TextRich {
                text: "♓".to_string(),
                tooltip: "Piscis 0°".to_string(),
                ring: 0.0,
            }
        );
    }

    #[test]
    fn astro_respeta_props_degree_name() {
        let mut ctx = ctx();
        ctx.sun_longitude_deg = 5.0; // Aries 5°
        let spec = WidgetSpec::new("astro")
            .with("degree", Prop::Bool(false))
            .with("name", Prop::Bool(false));
        let mut a = Astro::from_spec(&spec);
        a.tick(&ctx);
        // Sin nombre ni grado, el tooltip cae al glifo (sin info útil).
        assert_eq!(
            a.view(),
            WidgetView::TextRich {
                text: "♈".to_string(),
                tooltip: "♈".to_string(),
                ring: 5.0 / 30.0,
            }
        );
    }

    #[test]
    fn moon_emite_phase_y_nombre_de_fase() {
        let mut ctx = ctx();
        ctx.moon_phase = 0.5; // llena → idx 4
        let mut m = Moon::from_spec(&WidgetSpec::new("moon"));
        m.tick(&ctx);
        assert_eq!(
            m.view(),
            WidgetView::Moon {
                phase: 0.5,
                name: "Llena".to_string(),
            }
        );
    }

    #[test]
    fn start_button_muestra_label_default_y_override() {
        let def = build(&WidgetSpec::new("start_button"));
        assert_eq!(def.view(), WidgetView::Text("⊞".to_string()));
        let custom = StartButton::from_spec(
            &WidgetSpec::new("start_button").with("label", Prop::Str("Inicio".to_string())),
        );
        assert_eq!(custom.view(), WidgetView::Text("Inicio".to_string()));
    }

    #[test]
    fn build_despacha_los_builtins() {
        let specs = [
            WidgetSpec::new("clock"),
            WidgetSpec::new("cpu_meter"),
            WidgetSpec::new("ram_meter"),
            WidgetSpec::new("volume"),
            WidgetSpec::new("brightness"),
        ];
        let mut widgets = build_all(&specs);
        for w in widgets.iter_mut() {
            w.tick(&ctx());
        }
        // El reloj da texto; los cuatro medidores dan Meter.
        assert!(matches!(widgets[0].view(), WidgetView::Text(_)));
        for w in &widgets[1..] {
            assert!(matches!(w.view(), WidgetView::Meter { .. }));
        }
    }

    #[test]
    fn build_despacha_cpu_cores() {
        for kind in ["cpu_cores", "cpu_cores_meter"] {
            let w = build(&WidgetSpec::new(kind));
            assert_eq!(w.view(), WidgetView::Empty); // sin ctx
        }
    }
}
