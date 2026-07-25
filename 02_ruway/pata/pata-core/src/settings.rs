//! El marco como configuración editable.
//!
//! `pata-core` ya es declarativo (un [`Config`] es datos puros), así que su
//! esquema [`allichay`] es casi un reflejo del modelo: una sección por
//! superficie + los settings generales. El renderizador lo pinta con dientes y
//! controles; los cambios vuelven por [`Configurable::apply`], que muta el
//! modelo. La persistencia (escribir el TOML) la hace `pata-config::save` en el
//! lado `std` — aquí sólo vive la lógica `no_std`.
//!
//! v1 cubre los **escalares** de cada superficie (tipo, borde, grosor, padding,
//! gap, autohide, ancho de panel) y la zona horaria. Editar la **lista** de
//! superficies y de widgets (agregar/quitar/reordenar) queda para v2.

use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use allichay::{AllichayError, Configurable, EnumOption, Field, FieldPath, FieldValue, Schema, Section};

use crate::config::{Anchor, Config, Surface, SurfaceKind};

/// Prefijo de los ids de sección de cada superficie: `surface0`, `surface1`, …
const SURFACE_PREFIX: &str = "surface";

impl SurfaceKind {
    /// Id estable (coincide con el `serde(rename_all = "lowercase")`).
    fn as_id(&self) -> &'static str {
        match self {
            SurfaceKind::Bar => "bar",
            SurfaceKind::Panel => "panel",
            SurfaceKind::Dock => "dock",
            SurfaceKind::Sidebar => "sidebar",
            SurfaceKind::Background => "background",
        }
    }
    fn from_id(id: &str) -> Option<Self> {
        match id {
            "bar" => Some(SurfaceKind::Bar),
            "panel" => Some(SurfaceKind::Panel),
            "dock" => Some(SurfaceKind::Dock),
            "sidebar" => Some(SurfaceKind::Sidebar),
            "background" => Some(SurfaceKind::Background),
            _ => None,
        }
    }
    fn options() -> Vec<EnumOption> {
        vec![
            EnumOption::new("bar", "Barra"),
            EnumOption::new("panel", "Panel"),
            EnumOption::new("dock", "Dock"),
            EnumOption::new("sidebar", "Sidebar"),
        ]
    }
    /// Glifo del diente de la superficie. Emoji que la fuente bundle sí tiene
    /// (los geométricos salen tofu); el rótulo "Superficie N" distingue cada una.
    fn glyph(&self) -> &'static str {
        "🎛"
    }
}

impl Anchor {
    fn as_id(&self) -> &'static str {
        match self {
            Anchor::Top => "top",
            Anchor::Bottom => "bottom",
            Anchor::Left => "left",
            Anchor::Right => "right",
        }
    }
    fn from_id(id: &str) -> Option<Self> {
        match id {
            "top" => Some(Anchor::Top),
            "bottom" => Some(Anchor::Bottom),
            "left" => Some(Anchor::Left),
            "right" => Some(Anchor::Right),
            _ => None,
        }
    }
    fn options() -> Vec<EnumOption> {
        vec![
            EnumOption::new("top", "Arriba"),
            EnumOption::new("bottom", "Abajo"),
            EnumOption::new("left", "Izquierda"),
            EnumOption::new("right", "Derecha"),
        ]
    }
}

/// El esquema de una superficie: sus campos escalares. `pub` para que el panel
/// pueda rescatar estos parámetros (grosor/autoesconder/padding/…) al editor de una
/// barra de biblioteca, sin duplicar sus definiciones.
pub fn surface_section(index: usize, s: &Surface) -> Section {
    let id = format!("{SURFACE_PREFIX}{index}");
    let title = format!("Superficie {}", index + 1);
    // El BORDE (posición) ya NO va aquí: se elige en la lista de Barras. Aquí van
    // las configuraciones PARTICULARES, y el formulario cambia según el TIPO de
    // superficie (cada tipo tiene capacidades distintas).
    let mut sec = Section::new(id, title)
        .icon(s.kind.glyph())
        .help("Configuración particular de esta superficie (su tipo decide el formulario).")
        .field(Field::dropdown("kind", "Tipo", s.kind.as_id(), SurfaceKind::options()));

    // Grosor: el alto de una barra / el ancho del rail de un sidebar / el dock.
    // No aplica a Panel ni Background (ocupan otra cosa).
    if matches!(s.kind, SurfaceKind::Bar | SurfaceKind::Dock | SurfaceKind::Sidebar) {
        let etiqueta = if s.kind == SurfaceKind::Sidebar { "Ancho del rail" } else { "Grosor" };
        sec = sec.field(Field::slider("thickness", etiqueta, s.thickness as f64, 16.0, 200.0, 1.0));
    }
    // Ancho del panel que despliega un diente del sidebar.
    if s.kind == SurfaceKind::Sidebar {
        sec = sec
            .field(Field::slider("panel_width", "Ancho del panel desplegado", s.panel_width as f64, 120.0, 600.0, 10.0))
            // Eje **Espacio** del PANEL: reserva su ancho al desplegarse o flota como
            // overlay. (La franja de los dientes la gobierna 'Autoesconder', aparte.)
            .field(Field::dropdown(
                "reserve",
                "Panel: acople al escritorio",
                match s.reserve {
                    None => "auto",
                    Some(true) => "reserva",
                    Some(false) => "flota",
                },
                vec![
                    allichay::EnumOption::new("auto", "Automático (global)"),
                    allichay::EnumOption::new("reserva", "Reserva su ancho"),
                    allichay::EnumOption::new("flota", "Flota encima"),
                ],
            ));
    }
    // Autoesconder: barras/docks/sidebars que reaparecen al rozar el borde.
    if matches!(s.kind, SurfaceKind::Bar | SurfaceKind::Dock | SurfaceKind::Sidebar) {
        sec = sec.field(Field::toggle("autohide", "Autoesconder", s.autohide));
    }
    // Espaciado interno: aplica a las que alinean widgets en línea.
    if matches!(s.kind, SurfaceKind::Bar | SurfaceKind::Dock | SurfaceKind::Sidebar) {
        sec = sec
            .field(Field::slider("padding", "Padding", s.padding as f64, 0.0, 48.0, 1.0))
            .field(Field::slider("gap", "Separación entre widgets", s.gap as f64, 0.0, 48.0, 1.0));
    }
    // Pincel del fondo: común a todas (incluso Panel/Background).
    sec = sec
        .field(Field::slider("opacity", "Opacidad del fondo", s.opacity as f64, 0.0, 1.0, 0.05))
        .field(Field::slider("radius", "Esquinas redondeadas", s.radius as f64, 0.0, 32.0, 1.0));
    // Margen al borde (look flotante) — no para el fondo de pantalla.
    if s.kind != SurfaceKind::Background {
        sec = sec.field(Field::slider("margin", "Margen al borde (flotante)", s.margin as f64, 0.0, 48.0, 1.0));
    }
    sec
}

impl Configurable for Config {
    fn schema(&self) -> Schema {
        let mut schema = Schema::new().section(
            // El cajón/shell del panel (shuma). La zona horaria NO va aquí — es
            // del sistema (Sistema→reloj), no de pata.
            Section::new("general", "Shuma")
                .icon("▦")
                .field(
                    Field::slider(
                        "shuma_height",
                        "Shuma · alto del drawer",
                        self.general.shuma_height as f64,
                        0.1,
                        0.95,
                        0.05,
                    )
                    .help("Fracción de la pantalla que despliega el drawer del shell."),
                )
                .field(
                    Field::text("shuma_bg", "Shuma · color de fondo", self.general.shuma_bg.clone())
                        .help("Hex #rrggbb del fondo del drawer. Vacío = el del tema."),
                )
                .field(
                    Field::text("shuma_key", "Shuma · tecla de apertura", self.general.shuma_key.clone())
                        .help("Ej. F12. Default Alt+Enter. El grab global lo aplica el atajo de mirada."),
                ),
        );
        // Disposición del sidebar, derivada del tema/vista (default global; cada
        // sidebar la puede pisar). Es lo que también alterna el multiswitch del
        // control mutable del sidebar.
        schema = schema.section(
            Section::new("sidebar", "Sidebar")
                .icon("▤")
                .field(
                    Field::toggle(
                        "dientes_outside",
                        "Dientes afuera (franja saliente)",
                        self.general.sidebar_dientes_outside,
                    )
                    .help("Off = rail adentro (overlay estilo cosmos). On = rail afuera."),
                )
                .field(
                    Field::toggle("docked", "Espacio: panel fijo", self.general.sidebar_docked)
                        .help("On = el PANEL del sidebar reserva su ancho al desplegarse; Off = flota por encima. (La franja de los dientes la gobierna 'Autoesconder', no esto.)"),
                )
                .field(
                    Field::toggle(
                        "diente_dos_pasos",
                        "Dientes de dos pasos",
                        self.general.diente_dos_pasos,
                    )
                    .help("On = el primer click activa el diente y el segundo despliega su panel."),
                ),
        );
        for (i, s) in self.surfaces.iter().enumerate() {
            schema = schema.section(surface_section(i, s));
        }
        schema
    }

    fn apply(&mut self, path: &FieldPath, value: FieldValue) -> Result<(), AllichayError> {
        let segs = path.segments();
        let unknown = || AllichayError::UnknownPath(path.to_string());
        match segs {
            [section, field] if section == "general" => match field.as_str() {
                "timezone" => {
                    if let Some(s) = value.as_str() {
                        self.general.timezone = s.to_string();
                    }
                    Ok(())
                }
                "shuma_height" => {
                    if let Some(v) = value.as_float() {
                        self.general.shuma_height = (v as f32).clamp(0.1, 0.95);
                    }
                    Ok(())
                }
                "shuma_bg" => {
                    if let Some(s) = value.as_str() {
                        self.general.shuma_bg = s.to_string();
                    }
                    Ok(())
                }
                "shuma_key" => {
                    if let Some(s) = value.as_str() {
                        self.general.shuma_key = s.to_string();
                    }
                    Ok(())
                }
                _ => Err(unknown()),
            },
            [section, field] if section == "sidebar" => match field.as_str() {
                "dientes_outside" => {
                    if let Some(b) = value.as_bool() {
                        self.general.sidebar_dientes_outside = b;
                    }
                    Ok(())
                }
                "docked" => {
                    if let Some(b) = value.as_bool() {
                        self.general.sidebar_docked = b;
                    }
                    Ok(())
                }
                "diente_dos_pasos" => {
                    if let Some(b) = value.as_bool() {
                        self.general.diente_dos_pasos = b;
                    }
                    Ok(())
                }
                _ => Err(unknown()),
            },
            [section, field] if section.starts_with(SURFACE_PREFIX) => {
                let idx: usize = section[SURFACE_PREFIX.len()..]
                    .parse()
                    .map_err(|_| unknown())?;
                let surf = self.surfaces.get_mut(idx).ok_or_else(unknown)?;
                apply_surface_field(surf, field, value)
            }
            _ => Err(unknown()),
        }
    }
}

/// Aplica un campo escalar a una superficie (grosor/padding/gap/autohide/opacity/…).
/// `pub` para reusarlo al editar una barra de la biblioteca desde el panel.
pub fn apply_surface_field(
    surf: &mut Surface,
    field: &str,
    value: FieldValue,
) -> Result<(), AllichayError> {
    match field {
        "kind" => {
            if let Some(k) = value.as_str().and_then(SurfaceKind::from_id) {
                surf.kind = k;
            }
        }
        "anchor" => {
            if let Some(a) = value.as_str().and_then(Anchor::from_id) {
                surf.anchor = a;
            }
        }
        "thickness" => {
            if let Some(v) = value.as_float() {
                surf.thickness = v as f32;
            }
        }
        "padding" => {
            if let Some(v) = value.as_float() {
                surf.padding = v as f32;
            }
        }
        "gap" => {
            if let Some(v) = value.as_float() {
                surf.gap = v as f32;
            }
        }
        "autohide" => {
            if let Some(b) = value.as_bool() {
                surf.autohide = b;
            }
        }
        "panel_width" => {
            if let Some(v) = value.as_float() {
                surf.panel_width = v as f32;
            }
        }
        "opacity" => {
            if let Some(v) = value.as_float() {
                surf.opacity = (v as f32).clamp(0.0, 1.0);
            }
        }
        "radius" => {
            if let Some(v) = value.as_float() {
                surf.radius = (v as f32).max(0.0);
            }
        }
        "margin" => {
            if let Some(v) = value.as_float() {
                surf.margin = (v as f32).max(0.0);
            }
        }
        "reserve" => {
            surf.reserve = match value.as_str() {
                Some("reserva") => Some(true),
                Some("flota") => Some(false),
                _ => None, // "auto"
            };
        }
        _ => return Err(AllichayError::UnknownPath(field.to_string())),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_refleja_superficies() {
        let cfg = Config::preset();
        let schema = cfg.schema();
        // general + sidebar (disposición) + una sección por superficie.
        assert_eq!(schema.sections.len(), 2 + cfg.surfaces.len());
        assert_eq!(schema.sections[0].id, "general");
        assert_eq!(schema.sections[1].id, "sidebar");
        assert_eq!(schema.sections[2].id, "surface0");
    }

    #[test]
    fn apply_edita_grosor_de_una_barra() {
        let mut cfg = Config::preset();
        let path = FieldPath::from("surface0.thickness");
        cfg.apply(&path, FieldValue::Float(48.0)).unwrap();
        assert_eq!(cfg.surfaces[0].thickness, 48.0);
    }

    #[test]
    fn apply_cambia_tipo_y_borde() {
        let mut cfg = Config::preset();
        cfg.apply(&"surface0.kind".into(), FieldValue::Enum("dock".into()))
            .unwrap();
        cfg.apply(&"surface0.anchor".into(), FieldValue::Enum("left".into()))
            .unwrap();
        assert_eq!(cfg.surfaces[0].kind, SurfaceKind::Dock);
        assert_eq!(cfg.surfaces[0].anchor, Anchor::Left);
    }

    #[test]
    fn apply_timezone() {
        let mut cfg = Config::preset();
        cfg.apply(&"general.timezone".into(), FieldValue::Text("America/Lima".into()))
            .unwrap();
        assert_eq!(cfg.general.timezone, "America/Lima");
    }

    #[test]
    fn apply_ruta_desconocida_es_error() {
        let mut cfg = Config::preset();
        assert!(cfg.apply(&"surface9.thickness".into(), FieldValue::Float(1.0)).is_err());
        assert!(cfg.apply(&"general.nope".into(), FieldValue::Bool(true)).is_err());
        assert!(cfg.apply(&"sidebar.nope".into(), FieldValue::Bool(true)).is_err());
    }

    #[test]
    fn apply_toggles_de_disposicion_del_sidebar() {
        let mut cfg = Config::preset();
        // Defaults del tema/vista: adentro, docked, un paso.
        assert!(!cfg.general.sidebar_dientes_outside);
        assert!(cfg.general.sidebar_docked);
        assert!(!cfg.general.diente_dos_pasos);
        cfg.apply(&"sidebar.dientes_outside".into(), FieldValue::Bool(true)).unwrap();
        cfg.apply(&"sidebar.docked".into(), FieldValue::Bool(false)).unwrap();
        cfg.apply(&"sidebar.diente_dos_pasos".into(), FieldValue::Bool(true)).unwrap();
        assert!(cfg.general.sidebar_dientes_outside);
        assert!(!cfg.general.sidebar_docked);
        assert!(cfg.general.diente_dos_pasos);
    }
}
