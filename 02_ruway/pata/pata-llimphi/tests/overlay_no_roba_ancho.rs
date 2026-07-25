//! El overlay de shuma (menús contextuales, dropdowns, modales) **flota**: no
//! puede participar del flujo del contenedor que lo hospeda.
//!
//! Regresión del 25-jul: el overlay se apilaba como un hermano más del canvas
//! en un contenedor sin `flex_direction` — o sea `Row`, el default de taffy —
//! así que al abrir el menú contextual de una pestaña los dos hijos se repartían
//! el ancho: *«el drawer se reduce a la mitad respecto a su width, y el menú
//! contextual aparece en el lado derecho»*.
//!
//! Certificación numérica (sin PNG): se computa el layout y se miden los rects.

use llimphi_ui::llimphi_compositor::{measure_text_node, mount};
use llimphi_ui::llimphi_layout::taffy::prelude::{length, percent, Size, Style};
use llimphi_ui::llimphi_layout::{taffy, LayoutTree, Rect};
use llimphi_ui::llimphi_text::Typesetter;
use llimphi_ui::View;
use pata_llimphi::{shuma, Msg};

const W: f32 = 1920.0;
const H: f32 = 500.0;

/// Computa el layout de `view` a W×H y devuelve `(rect de la raíz, el resto)`.
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
    let raiz = computed.get(mounted.root).expect("raíz");
    let resto = computed
        .rects
        .iter()
        .filter(|(nid, _)| **nid != mounted.root)
        .map(|(_, r)| *r)
        .collect();
    (raiz, resto)
}

/// Un cuerpo que pide el 100% de su contenedor, como el canvas de shuma.
fn cuerpo() -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
}

/// Algo del tamaño de un menú contextual.
fn menu() -> View<Msg> {
    View::new(Style {
        size: Size { width: length(220.0_f32), height: length(180.0_f32) },
        ..Default::default()
    })
}

/// El contenedor que usa el drawer: sin `flex_direction` explícito (Row).
fn contenedor(hijos: Vec<View<Msg>>) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// La causa, para que quede escrita: un overlay como hermano NORMAL se reparte
/// el ancho con el cuerpo. Este test documenta por qué no se puede hacer así.
#[test]
fn un_hermano_normal_le_roba_ancho_al_cuerpo() {
    let (_raiz, resto) = medir(contenedor(vec![cuerpo(), menu()]));
    let ancho_cuerpo = resto.iter().fold(0.0_f32, |a, r| a.max(r.w));
    assert!(
        ancho_cuerpo < W,
        "sin capa absoluta el cuerpo debería quedar comprimido, midió {ancho_cuerpo}"
    );
}

/// El fix: envuelto en `capa_absoluta`, el cuerpo conserva TODO el ancho.
#[test]
fn la_capa_absoluta_no_le_roba_ancho_al_cuerpo() {
    let (_raiz, resto) = medir(contenedor(vec![cuerpo(), shuma::capa_absoluta(menu())]));
    let cuerpo_completo = resto
        .iter()
        .any(|r| (r.w - W).abs() < 1.0 && (r.h - H).abs() < 1.0);
    assert!(
        cuerpo_completo,
        "el cuerpo tiene que seguir midiendo {W}×{H} con el menú abierto; rects: {resto:?}"
    );
}

/// Y la capa cubre la caja entera, que es lo que hace que el menú se pueda
/// anclar en coordenadas de pantalla (y que su clic-afuera tape todo).
#[test]
fn la_capa_absoluta_cubre_toda_la_caja() {
    let (_raiz, resto) = medir(contenedor(vec![cuerpo(), shuma::capa_absoluta(menu())]));
    let capa = resto
        .iter()
        .filter(|r| (r.w - W).abs() < 1.0 && (r.h - H).abs() < 1.0)
        .count();
    assert!(
        capa >= 2,
        "cuerpo y capa deberían medir ambos {W}×{H} (encontré {capa}); rects: {resto:?}"
    );
    // Y el menú queda anclado al origen de la capa, no desplazado por el flujo.
    let m = resto
        .iter()
        .find(|r| (r.w - 220.0).abs() < 1.0)
        .expect("el menú");
    assert!(m.x.abs() < 1.0 && m.y.abs() < 1.0, "el menú arranca en el origen: {m:?}");
}
