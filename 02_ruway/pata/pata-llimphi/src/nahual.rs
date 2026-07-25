//! `nahual` — el **front universal de nahual hospedado** en pata, como un
//! drawer Quake (mismo patrón que [`crate::shuma`]).
//!
//! pata no reimplementa un explorador: monta el módulo [`nahual_module`] —el
//! mismo motor de `nahual-shell`, sobre `nahual-source-core`— y le rutea
//! eventos. El árbol POSIX (y, montando una fuente, las Mónadas del daemon,
//! imágenes wawa o `.zip`) se navega con las **mismas acciones** que la app
//! standalone: lista/detalle/iconos, breadcrumb, filtro, abrir-con. El trabajo
//! async (miniaturas, lanzar apps) lo ejecuta el host con su `Handle`, vía los
//! [`nahual_module::Effect`]s que el `update` del módulo devuelve.
//!
//! El estado de interacción del drawer (abierto, animación) vive aquí; el
//! **contenido** es el módulo, en [`NahualState::inner`].

use std::sync::{Arc, Mutex};

use llimphi_motion::Tween;
use llimphi_theme::Theme;
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, FlexDirection, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::View;

use crate::Msg;

/// Alto máximo del drawer, como fracción de la pantalla.
const DRAWER_FRAC: f32 = 0.6;

/// Estado de la carga del daemon de Mónadas (que es **cara**: descubrimiento
/// por broker + consulta inicial, hasta ~5 s). Se monta en un worker; este
/// enum gobierna que se intente una sola vez y refleja el progreso.
#[derive(Clone, Debug, PartialEq)]
pub enum DaemonLoad {
    /// Aún no se intentó.
    Idle,
    /// El worker está descubriendo/conectando.
    Loading,
    /// Montado: las Mónadas vivas están en la pila del módulo.
    Mounted,
    /// Falló (sin daemon, broker caído…). El usuario se queda en POSIX.
    Failed(String),
}

/// Estado del drawer del front universal. El contenido —el módulo— se
/// inicializa perezosamente la primera vez que se abre, **siempre en POSIX**
/// (no bloquea). Las Mónadas del daemon se montan aparte, en un worker, y se
/// empujan sobre la pila cuando están listas (ver [`DaemonLoad`]).
pub struct NahualState {
    /// `true` cuando el drawer está desplegado.
    pub open: bool,
    /// El módulo hospedado. `None` hasta la primera apertura.
    pub inner: Option<nahual_module::State>,
    /// Animación de despliegue `0..1`.
    pub anim: Tween<f32>,
    /// Progreso de la carga del daemon de Mónadas.
    pub daemon: DaemonLoad,
    /// Canal lateral worker→UI: el `Navigator` del daemon, listo para montar.
    /// No viaja por el `Msg` (no es `Clone`); el worker lo deja aquí y dispara
    /// un `Msg` liviano para que el hilo de UI lo tome y lo monte.
    pub slot: Arc<Mutex<Option<nahual_module::Navigator>>>,
}

impl Default for NahualState {
    fn default() -> Self {
        Self {
            open: false,
            inner: None,
            anim: Tween::idle(0.0),
            daemon: DaemonLoad::Idle,
            slot: Arc::new(Mutex::new(None)),
        }
    }
}

impl NahualState {
    /// Inicializa el módulo si hace falta, parado en `$HOME` (o `/`). Idempotente.
    pub fn ensure(&mut self) {
        if self.inner.is_none() {
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("/"));
            self.inner = Some(nahual_module::State::posix(&home));
        }
    }

    /// `true` si el drawer debe pintarse (abierto o aún animando el cierre).
    pub fn visible(&self) -> bool {
        self.open || self.anim.value() > 0.01
    }
}

/// El drawer del front universal como overlay: panel anclado abajo + scrim que
/// cierra al click. Mismo esqueleto que [`crate::shuma::drawer_overlay`]. El
/// menú "abrir con…" del módulo se apila por encima del panel.
pub fn drawer_overlay(state: &NahualState, screen: (i32, i32), theme: &Theme) -> Option<View<Msg>> {
    if !state.visible() {
        return None;
    }
    let inner = state.inner.as_ref()?;
    let t = state.anim.value().clamp(0.0, 1.0);
    let (sw, sh) = screen;
    let alto = (sh as f32 * DRAWER_FRAC * t).max(1.0);

    // El cuerpo es el módulo: su `view` trae breadcrumb + lista/detalle/iconos
    // y pinta clicks que vuelven como `Msg::Nahual(..)` por el `lift`.
    let body = nahual_module::view(inner, theme, Msg::Nahual);

    let mut hijos = vec![body];
    // Menú "abrir con…" por encima, si el módulo lo tiene abierto.
    if let Some(menu) = nahual_module::context_overlay(
        inner,
        theme,
        (sw as f32, alto),
        Msg::Nahual,
    ) {
        hijos.push(menu);
    }

    let panel = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            right: length(0.0_f32),
            top: auto(),
            bottom: length(0.0_f32),
        },
        size: Size { width: percent(1.0_f32), height: length(alto) },
        flex_direction: FlexDirection::Column,
        ..Default::default()
    })
    .fill(theme.bg_app)
    // Absorbe los clicks del borde para que no cierren el drawer; NahualAnim
    // es un no-op de re-render.
    .on_click(Msg::NahualAnim)
    .children(hijos);

    let scrim = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            top: length(0.0_f32),
            right: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_app)
    .alpha(0.55 * t)
    .on_click(Msg::NahualToggle)
    .children(vec![panel]);

    Some(scrim)
}
