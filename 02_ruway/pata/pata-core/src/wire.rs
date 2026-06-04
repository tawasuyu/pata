//! Representación **postcard-safe** del marco para akasha (wawa).
//!
//! El modelo de [`Config`](crate::Config) está afinado para el loader TOML de
//! Linux: `WidgetSpec.props` usa `#[serde(flatten)]` (para que las props vivan al
//! nivel del `kind`) y [`Prop`] es `#[serde(untagged)]` (para que TOML infiera el
//! tipo). Ambos atributos **rompen postcard** —el codec de akasha—, que no es
//! auto-descriptivo y no soporta `deserialize_any`.
//!
//! Este módulo define un espejo *plano y etiquetado* del modelo: [`WireConfig`]
//! es exactamente la misma información, pero serializable con cualquier formato
//! binario no auto-descriptivo. El kernel de wawa serializa el `WireConfig` con
//! postcard, lo guarda en el grafo direccionado por contenido y lo lee de vuelta
//! —el config del marco viaja por akasha como cualquier otro objeto—. En Linux
//! el camino TOML sigue usando `Config` directo; este espejo es sólo para el
//! cruce a wawa.
//!
//! Las conversiones son **sin pérdida**: `Config → WireConfig → Config` devuelve
//! el mismo modelo (lo fija un test).

use alloc::string::String;
use alloc::vec::Vec;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::config::{
    Anchor, Config, FloatingCard, General, Prop, SidebarTab, Surface, SurfaceKind, WidgetSpec,
};

/// Valor de propiedad **etiquetado** (a diferencia de [`Prop`], que es
/// `untagged`): así postcard puede deserializarlo sin auto-descripción.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum WireProp {
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
}

impl From<&Prop> for WireProp {
    fn from(p: &Prop) -> Self {
        match p {
            Prop::Bool(b) => WireProp::Bool(*b),
            Prop::Int(i) => WireProp::Int(*i),
            Prop::Num(n) => WireProp::Num(*n),
            Prop::Str(s) => WireProp::Str(s.clone()),
        }
    }
}

impl From<WireProp> for Prop {
    fn from(p: WireProp) -> Self {
        match p {
            WireProp::Bool(b) => Prop::Bool(b),
            WireProp::Int(i) => Prop::Int(i),
            WireProp::Num(n) => Prop::Num(n),
            WireProp::Str(s) => Prop::Str(s),
        }
    }
}

/// Espejo de [`WidgetSpec`] con las props como **lista ordenada** (no `flatten`).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct WireWidget {
    pub kind: String,
    pub props: Vec<(String, WireProp)>,
}

impl From<&WidgetSpec> for WireWidget {
    fn from(w: &WidgetSpec) -> Self {
        Self {
            kind: w.kind.clone(),
            props: w.props.iter().map(|(k, v)| (k.clone(), WireProp::from(v))).collect(),
        }
    }
}

impl From<WireWidget> for WidgetSpec {
    fn from(w: WireWidget) -> Self {
        let mut spec = WidgetSpec::new(w.kind);
        for (k, v) in w.props {
            spec = spec.with(k, Prop::from(v));
        }
        spec
    }
}

fn a_wire(specs: &[WidgetSpec]) -> Vec<WireWidget> {
    specs.iter().map(WireWidget::from).collect()
}

fn de_wire(wires: Vec<WireWidget>) -> Vec<WidgetSpec> {
    wires.into_iter().map(WidgetSpec::from).collect()
}

/// Espejo de [`FloatingCard`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct WireCard {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub title: Option<String>,
    pub widgets: Vec<WireWidget>,
}

impl From<&FloatingCard> for WireCard {
    fn from(c: &FloatingCard) -> Self {
        Self {
            x: c.x,
            y: c.y,
            w: c.w,
            h: c.h,
            title: c.title.clone(),
            widgets: a_wire(&c.widgets),
        }
    }
}

impl From<WireCard> for FloatingCard {
    fn from(c: WireCard) -> Self {
        FloatingCard {
            x: c.x,
            y: c.y,
            w: c.w,
            h: c.h,
            title: c.title,
            widgets: de_wire(c.widgets),
        }
    }
}

/// Espejo de [`SidebarTab`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct WireTab {
    pub icon: String,
    pub label: String,
    pub content: WireWidget,
}

impl From<&SidebarTab> for WireTab {
    fn from(t: &SidebarTab) -> Self {
        Self {
            icon: t.icon.clone(),
            label: t.label.clone(),
            content: WireWidget::from(&t.content),
        }
    }
}

impl From<WireTab> for SidebarTab {
    fn from(t: WireTab) -> Self {
        SidebarTab {
            icon: t.icon,
            label: t.label,
            content: WidgetSpec::from(t.content),
        }
    }
}

/// Espejo de [`Surface`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct WireSurface {
    pub kind: SurfaceKind,
    pub anchor: Anchor,
    pub thickness: f32,
    pub autohide: bool,
    pub padding: f32,
    pub gap: f32,
    pub start: Vec<WireWidget>,
    pub center: Vec<WireWidget>,
    pub end: Vec<WireWidget>,
    pub cards: Vec<WireCard>,
    pub output: String,
    pub tabs: Vec<WireTab>,
    pub panel_width: f32,
}

impl From<&Surface> for WireSurface {
    fn from(s: &Surface) -> Self {
        Self {
            kind: s.kind,
            anchor: s.anchor,
            thickness: s.thickness,
            autohide: s.autohide,
            padding: s.padding,
            gap: s.gap,
            start: a_wire(&s.start),
            center: a_wire(&s.center),
            end: a_wire(&s.end),
            cards: s.cards.iter().map(WireCard::from).collect(),
            output: s.output.clone(),
            tabs: s.tabs.iter().map(WireTab::from).collect(),
            panel_width: s.panel_width,
        }
    }
}

impl From<WireSurface> for Surface {
    fn from(s: WireSurface) -> Self {
        Surface {
            kind: s.kind,
            anchor: s.anchor,
            thickness: s.thickness,
            autohide: s.autohide,
            padding: s.padding,
            gap: s.gap,
            start: de_wire(s.start),
            center: de_wire(s.center),
            end: de_wire(s.end),
            cards: s.cards.into_iter().map(FloatingCard::from).collect(),
            output: s.output,
            tabs: s.tabs.into_iter().map(SidebarTab::from).collect(),
            panel_width: s.panel_width,
        }
    }
}

/// Espejo postcard-safe de [`Config`]: la misma información, sin `flatten` ni
/// `untagged`. Es lo que viaja por akasha en wawa.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct WireConfig {
    pub general: General,
    pub surfaces: Vec<WireSurface>,
}

impl From<&Config> for WireConfig {
    fn from(c: &Config) -> Self {
        Self {
            general: c.general.clone(),
            surfaces: c.surfaces.iter().map(WireSurface::from).collect(),
        }
    }
}

impl From<WireConfig> for Config {
    fn from(c: WireConfig) -> Self {
        Config {
            general: c.general,
            surfaces: c.surfaces.into_iter().map(Surface::from).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Prop, SidebarTab};
    use crate::{Config, Surface, WidgetSpec};
    use alloc::string::ToString;

    /// Un config con todos los rincones poblados (props de cada tipo, una
    /// tarjeta flotante) para que el round-trip cubra el caso difícil.
    fn config_rico() -> Config {
        let mut top = Surface::bar(Anchor::Top);
        top.thickness = 32.0;
        top.start = alloc::vec![WidgetSpec::new("start_button").with("label", Prop::Str("⊞".to_string()))];
        top.center = alloc::vec![WidgetSpec::new("clock")
            .with("format", Prop::Str("%H:%M".to_string()))
            .with("size", Prop::Int(14))
            .with("ratio", Prop::Num(0.5))
            .with("flag", Prop::Bool(true))];
        top.end = alloc::vec![WidgetSpec::new("ram_meter")];

        let mut panel = Surface::default();
        panel.kind = SurfaceKind::Panel;
        panel.cards = alloc::vec![FloatingCard {
            x: 40.0,
            y: 80.0,
            w: 220.0,
            h: 110.0,
            title: Some("sistema".to_string()),
            widgets: alloc::vec![WidgetSpec::new("cpu_meter"), WidgetSpec::new("ram_meter")],
        }];

        let mut sidebar = Surface::sidebar(Anchor::Left);
        sidebar.panel_width = 300.0;
        sidebar.tabs = alloc::vec![
            SidebarTab::new(
                "monads",
                "Mónadas",
                WidgetSpec::new("navigator").with("source", Prop::Str("nouser".to_string())),
            ),
            SidebarTab::new("files", "Archivos", WidgetSpec::new("navigator")),
        ];

        let mut cfg = Config::default();
        cfg.surfaces.push(top);
        cfg.surfaces.push(panel);
        cfg.surfaces.push(sidebar);
        cfg
    }

    #[test]
    fn round_trip_sin_perdida() {
        let cfg = config_rico();
        let wire = WireConfig::from(&cfg);
        let vuelta: Config = wire.into();
        assert_eq!(cfg, vuelta);
    }

    #[test]
    fn round_trip_postcard() {
        // El caso real de akasha: serializar con postcard (no auto-descriptivo,
        // sin soporte de flatten/untagged) y volver. Falla con `Config` directo;
        // funciona con `WireConfig`.
        let cfg = config_rico();
        let wire = WireConfig::from(&cfg);
        let bytes = postcard::to_allocvec(&wire).expect("postcard serializa el wire");
        let wire2: WireConfig = postcard::from_bytes(&bytes).expect("postcard deserializa el wire");
        let vuelta: Config = wire2.into();
        assert_eq!(cfg, vuelta);
    }
}
