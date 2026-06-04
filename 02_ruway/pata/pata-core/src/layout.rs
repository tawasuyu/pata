//! Resolución geométrica del marco: de [`Config`] + pantalla a superficies
//! colocadas en píxeles + el **área de trabajo** que queda libre.
//!
//! Es pura geometría —`no_std`, determinista, sin servidor gráfico—. Dos
//! consumidores la necesitan:
//!
//! - el **frontend** (Llimphi / framebuffer wawa), para saber dónde pintar cada
//!   barra/dock/panel;
//! - el **compositor** (`mirada`), para saber qué franja reservar: el
//!   [`Frame::work_area`] es exactamente el rectángulo donde teselar las
//!   ventanas, ya descontadas las barras sólidas.
//!
//! Reglas de reserva:
//! - una **Bar** no-`autohide` reserva su grosor del borde y encoge el área;
//! - una **Bar** `autohide`, un **Dock** y un **Panel** *no* reservan: flotan
//!   sobre el escritorio (su rect se calcula, pero el área de trabajo no cambia).

use alloc::vec::Vec;

use crate::config::{Anchor, Config, SurfaceKind};

/// Un rectángulo en píxeles de pantalla. Origen `(0,0)` arriba-izquierda; `x`
/// crece a la derecha, `y` hacia abajo. Propio de `pata` —no depende de
/// `mirada`— para que el marco sea independiente del compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// `true` si tiene ancho y alto positivos.
    pub fn es_visible(&self) -> bool {
        self.w > 0 && self.h > 0
    }
}

/// Una superficie ya colocada: su índice en `config.surfaces` y su rect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    /// Índice dentro de [`Config::surfaces`], para recuperar sus widgets.
    pub index: usize,
    /// Rectángulo en píxeles donde va la superficie.
    pub rect: Rect,
    /// `true` si reservó franja (encogió el área de trabajo).
    pub reserva: bool,
}

/// El resultado de resolver el marco: las superficies colocadas y el área que
/// queda para las ventanas.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Superficies en el mismo orden que `config.surfaces`.
    pub surfaces: Vec<Placed>,
    /// Lo que queda libre tras reservar las barras sólidas — donde el
    /// compositor tesela las ventanas.
    pub work_area: Rect,
}

/// La franja de grosor `t` pegada al borde `anchor` de `area`.
fn strip(area: Rect, anchor: Anchor, t: i32) -> Rect {
    let t = t.max(0);
    match anchor {
        Anchor::Top => Rect::new(area.x, area.y, area.w, t.min(area.h)),
        Anchor::Bottom => {
            let t = t.min(area.h);
            Rect::new(area.x, area.y + area.h - t, area.w, t)
        }
        Anchor::Left => Rect::new(area.x, area.y, t.min(area.w), area.h),
        Anchor::Right => {
            let t = t.min(area.w);
            Rect::new(area.x + area.w - t, area.y, t, area.h)
        }
    }
}

/// `area` tras descontarle la franja de grosor `t` del borde `anchor`.
fn shrink(area: Rect, anchor: Anchor, t: i32) -> Rect {
    let t = t.max(0);
    match anchor {
        Anchor::Top => {
            let t = t.min(area.h);
            Rect::new(area.x, area.y + t, area.w, area.h - t)
        }
        Anchor::Bottom => Rect::new(area.x, area.y, area.w, (area.h - t).max(0)),
        Anchor::Left => {
            let t = t.min(area.w);
            Rect::new(area.x + t, area.y, area.w - t, area.h)
        }
        Anchor::Right => Rect::new(area.x, area.y, (area.w - t).max(0), area.h),
    }
}

/// Resuelve el marco sobre una pantalla. Recorre las superficies en orden: las
/// barras sólidas se apilan reservando franja (la segunda barra del mismo borde
/// va pegada a la primera); las `autohide`, docks y paneles flotan sin reservar.
pub fn resolve(config: &Config, screen: Rect) -> Frame {
    let mut work = screen;
    let mut surfaces = Vec::with_capacity(config.surfaces.len());

    for (index, s) in config.surfaces.iter().enumerate() {
        let t = s.thickness as i32;
        let (rect, reserva) = match s.kind {
            // Una barra y el rail de un sidebar reservan igual: su grosor pegado
            // al borde, salvo que sean autohide (entonces flotan sin reservar).
            // El panel que despliega un diente del sidebar no es parte de
            // `resolve` —flota sobre el área de trabajo como un drawer de
            // launcher, lo maneja el frontend—.
            SurfaceKind::Bar | SurfaceKind::Sidebar => {
                let r = strip(work, s.anchor, t);
                if s.autohide {
                    (r, false)
                } else {
                    work = shrink(work, s.anchor, t);
                    (r, true)
                }
            }
            // Dock: franja pegada al borde del área actual, sin reservar.
            SurfaceKind::Dock => (strip(work, s.anchor, t), false),
            // Panel: ocupa el área libre como lienzo de sus tarjetas, sin reservar.
            SurfaceKind::Panel => (work, false),
        };
        surfaces.push(Placed {
            index,
            rect,
            reserva,
        });
    }

    Frame {
        surfaces,
        work_area: work,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Surface, WidgetSpec};

    fn pantalla() -> Rect {
        Rect::new(0, 0, 1920, 1080)
    }

    #[test]
    fn barra_top_reserva_su_franja() {
        let mut cfg = Config::default();
        let mut top = Surface::bar(Anchor::Top);
        top.thickness = 32.0;
        cfg.surfaces.push(top);

        let f = resolve(&cfg, pantalla());
        assert_eq!(f.surfaces[0].rect, Rect::new(0, 0, 1920, 32));
        assert!(f.surfaces[0].reserva);
        // El área de trabajo arranca 32px más abajo.
        assert_eq!(f.work_area, Rect::new(0, 32, 1920, 1048));
    }

    #[test]
    fn barra_autohide_no_reserva() {
        let mut cfg = Config::default();
        let mut shell = Surface::bar(Anchor::Bottom);
        shell.thickness = 40.0;
        shell.autohide = true;
        cfg.surfaces.push(shell);

        let f = resolve(&cfg, pantalla());
        // El rect de la barra existe, pegado al pie…
        assert_eq!(f.surfaces[0].rect, Rect::new(0, 1080 - 40, 1920, 40));
        assert!(!f.surfaces[0].reserva);
        // …pero el área de trabajo es la pantalla entera (flota encima).
        assert_eq!(f.work_area, pantalla());
    }

    #[test]
    fn top_solida_mas_shell_autohide_solo_reserva_la_top() {
        // El caso del preset: barra top sólida + shell inferior autohide.
        let cfg = Config::preset();
        let f = resolve(&cfg, pantalla());
        // top reserva 32; shell no reserva.
        assert!(f.surfaces[0].reserva);
        assert!(!f.surfaces[1].reserva);
        assert_eq!(f.work_area, Rect::new(0, 32, 1920, 1048));
    }

    #[test]
    fn dos_barras_top_se_apilan() {
        let mut cfg = Config::default();
        let mut a = Surface::bar(Anchor::Top);
        a.thickness = 24.0;
        let mut b = Surface::bar(Anchor::Top);
        b.thickness = 30.0;
        cfg.surfaces.push(a);
        cfg.surfaces.push(b);

        let f = resolve(&cfg, pantalla());
        assert_eq!(f.surfaces[0].rect, Rect::new(0, 0, 1920, 24));
        // La segunda va pegada bajo la primera.
        assert_eq!(f.surfaces[1].rect, Rect::new(0, 24, 1920, 30));
        assert_eq!(f.work_area, Rect::new(0, 54, 1920, 1080 - 54));
    }

    #[test]
    fn barras_verticales_reservan_ancho() {
        let mut cfg = Config::default();
        let mut left = Surface::bar(Anchor::Left);
        left.thickness = 48.0;
        cfg.surfaces.push(left);

        let f = resolve(&cfg, pantalla());
        assert_eq!(f.surfaces[0].rect, Rect::new(0, 0, 48, 1080));
        assert_eq!(f.work_area, Rect::new(48, 0, 1920 - 48, 1080));
    }

    #[test]
    fn dock_no_reserva_y_se_pega_al_borde() {
        let mut cfg = Config::default();
        cfg.surfaces.push({
            let mut d = Surface::dock(Anchor::Bottom);
            d.thickness = 64.0;
            d
        });
        let f = resolve(&cfg, pantalla());
        assert_eq!(f.surfaces[0].rect, Rect::new(0, 1080 - 64, 1920, 64));
        assert!(!f.surfaces[0].reserva);
        assert_eq!(f.work_area, pantalla());
    }

    #[test]
    fn panel_ocupa_el_area_libre_sin_reservar() {
        let mut cfg = Config::default();
        // Una barra top sólida + un panel: el panel toma el área ya descontada.
        let mut top = Surface::bar(Anchor::Top);
        top.thickness = 32.0;
        cfg.surfaces.push(top);
        let mut panel = Surface::default();
        panel.kind = SurfaceKind::Panel;
        panel.center.push(WidgetSpec::new("ram_meter"));
        cfg.surfaces.push(panel);

        let f = resolve(&cfg, pantalla());
        assert_eq!(f.surfaces[1].rect, Rect::new(0, 32, 1920, 1048));
        assert!(!f.surfaces[1].reserva);
        assert_eq!(f.work_area, Rect::new(0, 32, 1920, 1048));
    }

    #[test]
    fn sidebar_reserva_su_rail_como_una_barra_vertical() {
        let mut cfg = Config::default();
        let mut sb = Surface::sidebar(Anchor::Left);
        sb.thickness = 44.0;
        cfg.surfaces.push(sb);

        let f = resolve(&cfg, pantalla());
        // El rail toma una franja vertical fina pegada a la izquierda…
        assert_eq!(f.surfaces[0].rect, Rect::new(0, 0, 44, 1080));
        assert!(f.surfaces[0].reserva);
        // …y el área de trabajo arranca 44px a la derecha (el panel desplegado
        // flota encima, no entra en `resolve`).
        assert_eq!(f.work_area, Rect::new(44, 0, 1920 - 44, 1080));
    }

    #[test]
    fn sidebar_autohide_no_reserva() {
        let mut cfg = Config::default();
        let mut sb = Surface::sidebar(Anchor::Right);
        sb.thickness = 44.0;
        sb.autohide = true;
        cfg.surfaces.push(sb);

        let f = resolve(&cfg, pantalla());
        assert_eq!(f.surfaces[0].rect, Rect::new(1920 - 44, 0, 44, 1080));
        assert!(!f.surfaces[0].reserva);
        assert_eq!(f.work_area, pantalla());
    }

    #[test]
    fn sin_superficies_el_area_es_la_pantalla() {
        let f = resolve(&Config::default(), pantalla());
        assert!(f.surfaces.is_empty());
        assert_eq!(f.work_area, pantalla());
    }
}
