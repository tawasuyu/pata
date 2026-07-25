//! `pata-core` — el modelo del marco del escritorio.
//!
//! `pata` (quechua: borde, repisa, andén) es la capa que dibuja el *marco* del
//! escritorio: las **barras**, los **paneles** y el **dock** que rodean a las
//! ventanas, y los **widgets** que viven dentro. No es el compositor (eso es
//! `mirada`) ni el shell (eso es `shuma`): es el chrome configurable que ambos
//! mundos comparten.
//!
//! Este crate es sólo el **modelo**, deliberadamente tonto y portable:
//!
//! - [`config`] — el esquema declarativo. Un [`Config`] es una lista de
//!   [`Surface`]s (bar/panel/dock), cada una anclada a un borde y con widgets
//!   colocables en sus slots. Es lo que un archivo de config (TOML en Linux,
//!   akasha en wawa) materializa.
//! - [`layout`] — la geometría: resuelve un `Config` + pantalla en superficies
//!   colocadas en píxeles + el área de trabajo que queda para las ventanas.
//! - [`widget`] — la lógica de datos de cada widget: un [`config::WidgetSpec`]
//!   se materializa en un [`widget::Widget`] vivo que, alimentado por un
//!   snapshot del sistema, emite un view-model agnóstico del pincel.
//!
//! No pinta, no toca el SO, no sabe quién lo renderiza. Por eso es `no_std` +
//! `alloc`: el mismo modelo sirve al frontend Llimphi sobre Linux y al kernel
//! launcher de wawa (`x86_64-unknown-none`), que aporta su propio allocator.
//! La regla del repo: si un tipo se comparte entre kernel y userspace, vive
//! sin `std`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

/// Efemérides de baja precisión (Sol/Luna) para el widget astral. Matemática
/// pura `no_std`; el host computa el día juliano y llama.
pub mod astro;
/// El árbitro del **diente vivo**: a partir de señales del sistema decide qué
/// debe mostrar un diente multifuncional (música/volumen/CPU/batería/reposo).
/// Puro: no pinta ni toca el SO. Lo pinta el frontend.
pub mod atencion;
pub mod config;
pub mod layout;
/// Modelo puro de una **transferencia en curso** (copiar/mover archivos,
/// descargas…): a partir de muestras de bytes hechos + reloj, computa velocidad
/// suavizada, ETA e historial de velocidad para el sparkline. No pinta ni toca el
/// SO — el host lo alimenta y el frontend lo dibuja. Es el cómputo **central** que
/// convierte los avances crudos que emiten las apps (p. ej. nahual) en una vista
/// unificada de progreso.
pub mod progreso;
/// `impl allichay::Configurable for Config` — vuelve el marco editable por UI.
pub mod settings;
pub mod widget;
/// Espejo postcard-safe del modelo, para el cruce a wawa por akasha. Sólo con la
/// feature `serde` (el kernel la activa; el camino TOML de Linux no lo necesita).
#[cfg(feature = "serde")]
pub mod wire;

pub use config::{
    widget_catalog, widgets_for_surface, Anchor, Config, FloatingCard, General, Prop, SidebarTab,
    Surface, SurfaceKind, TabsSource, WidgetCatalogEntry, WidgetSpec,
};
pub use layout::{resolve, Frame, Placed, Rect};
pub use widget::{
    build, build_all, Astro, Clock, ClockReading, Meter, MeterSource, Placeholder, StartButton,
    Widget, WidgetCtx, WidgetView,
};
