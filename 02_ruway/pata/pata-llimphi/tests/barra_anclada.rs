//! La barra fina **no debe estirarse** cuando su surface todavía mide de más.
//!
//! Al replegar el drawer de shuma, pata pide `set_size(0, barra)` y sigue
//! pintando hasta que llega el `configure` con el alto nuevo. Esos frames se
//! dibujan con el alto VIEJO (pantalla completa), y `bar_view` —que es
//! 100%×100% de su contenedor— estiraba ahí la franja del input a media
//! pantalla: el parpadeo al guardar el drawer, tanto más largo cuanto más lenta
//! la máquina. `bar_view_anclada` ancla la franja a su borde con su alto real.
//!
//! Certificación numérica (sin PNG): se computa el layout a 1920×1080 y se
//! miden los rects.

use llimphi_ui::llimphi_compositor::{measure_text_node, mount};
use llimphi_ui::llimphi_layout::{taffy, LayoutTree, Rect};
use llimphi_ui::llimphi_text::Typesetter;
use llimphi_ui::View;
use pata_llimphi::{render, shuma, Msg, SlotWidget, SurfaceWidgets};

/// Ancho de la surface (irrelevante para el alto, pero realista).
const W: f32 = 1920.0;
/// Alto VIEJO de la surface: el drawer ya se cerró pero el `configure` que la
/// encoge todavía no llegó.
const H: f32 = 1080.0;
/// Grosor real de la barra fina.
const BARRA: f32 = 40.0;

/// Computa el layout y devuelve `(rect de la raíz, rects de todo lo demás)`.
fn medir(view: View<Msg>) -> (Rect, Vec<Rect>) {
    let mut layout = LayoutTree::new();
    let mounted = mount(&mut layout, view);
    let mut ts = Typesetter::new();
    let computed = {
        let tmap = &mounted.text_measures;
        layout
            .compute_with_measure(mounted.root, (W, H), |nid, known, avail| {
                match tmap.get(&nid) {
                    Some(tm) => measure_text_node(&mut ts, tm, known, avail),
                    None => taffy::Size::ZERO,
                }
            })
            .expect("layout")
    };
    let raiz = computed.get(mounted.root).expect("rect de la raíz");
    let resto = computed
        .rects
        .iter()
        .filter(|(nid, _)| **nid != mounted.root)
        .map(|(_, r)| *r)
        .collect();
    (raiz, resto)
}

/// Barra inferior con el input de shuma, como la del metal.
fn surface() -> pata_core::Surface {
    pata_core::Surface {
        kind: pata_core::SurfaceKind::Bar,
        anchor: pata_core::Anchor::Bottom,
        thickness: BARRA,
        ..Default::default()
    }
}

fn widgets() -> SurfaceWidgets {
    SurfaceWidgets {
        start: Vec::new(),
        center: vec![SlotWidget::Shuma],
        end: Vec::new(),
    }
}

/// La causa: el cuerpo de `bar_view` —el nodo que lleva el fondo de la barra—
/// es la RAÍZ y mide 100%×100%, así que en una surface aún crecida se pinta a
/// pantalla completa. Esto no es un bug de `bar_view` (en su surface fina es lo
/// correcto): documenta por qué el `draw` no puede usarla en ese hueco.
#[test]
fn bar_view_llena_la_surface_crecida() {
    let (s, sw) = (surface(), widgets());
    let st = shuma::ShumaState::default();
    let data = render::BarData::default();
    let theme = llimphi_theme::Theme::dark();
    let (raiz, _resto) = medir(render::bar_view(&s, &sw, &st, &data, &theme));
    assert!(
        (raiz.h - H).abs() < 1.0,
        "el cuerpo de la barra llena la surface entera (la causa del parpadeo): {raiz:?}"
    );
}

/// El fix: anclada, nada supera el grosor real de la barra y la franja queda
/// pegada al borde inferior (anchor Bottom).
#[test]
fn bar_view_anclada_respeta_el_grosor_y_el_borde() {
    let (s, sw) = (surface(), widgets());
    let st = shuma::ShumaState::default();
    let data = render::BarData::default();
    let theme = llimphi_theme::Theme::dark();
    let (raiz, resto) = medir(render::bar_view_anclada(&s, &sw, &st, &data, &theme, BARRA));

    // La raíz sí ocupa la surface entera — es el contenedor transparente.
    assert!((raiz.h - H).abs() < 1.0, "la raíz debe cubrir la surface: {raiz:?}");

    // Nada más puede pasarse del grosor de la barra: si algo mide de más, es la
    // franja estirada volviendo por la ventana.
    let alto_max = resto.iter().fold(0.0_f32, |a, r| a.max(r.h));
    assert!(
        alto_max <= BARRA + 1.0,
        "algo se estiró por encima de la barra ({alto_max} px > {BARRA})"
    );

    // Y la franja está pegada abajo (anchor Bottom), con su alto completo.
    let franja = resto
        .iter()
        .find(|r| (r.h - BARRA).abs() < 1.0 && (r.w - W).abs() < 1.0)
        .expect("la franja de la barra a ancho completo");
    assert!(
        (franja.y - (H - BARRA)).abs() < 1.0,
        "la franja debe quedar al borde inferior: {franja:?}"
    );
}

/// Con anchor Top la franja se pega arriba.
#[test]
fn bar_view_anclada_arriba_va_al_tope() {
    let mut s = surface();
    s.anchor = pata_core::Anchor::Top;
    let sw = widgets();
    let st = shuma::ShumaState::default();
    let data = render::BarData::default();
    let theme = llimphi_theme::Theme::dark();
    let (_raiz, resto) = medir(render::bar_view_anclada(&s, &sw, &st, &data, &theme, BARRA));
    let franja = resto
        .iter()
        .find(|r| (r.h - BARRA).abs() < 1.0 && (r.w - W).abs() < 1.0)
        .expect("la franja de la barra a ancho completo");
    assert!(franja.y.abs() < 1.0, "la franja debe quedar al tope: {franja:?}");
}
