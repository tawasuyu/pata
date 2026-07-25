//! Espejo del **switcher de ventanas** de mirada (Alt-Tab), Plan B.
//!
//! El compositor publica el estado del switcher a `$XDG_RUNTIME_DIR/mirada-switcher`
//! (texto tab-separado, ver `mirada-compositor/src/switcher.rs::export_text`) y lo
//! borra al cerrar. pata lo lee en su latido —se lo despierta el frame-callback
//! que el compositor manda en cada cambio del switcher— y, cuando está desplegado
//! como árbol, lo pinta en su sidebar con el cursor de selección.
//!
//! Formato (una directiva por línea):
//! - `tree\t{0|1}` — 1 si está desplegado como árbol (Alt pegado).
//! - `sel\t{i}` — índice seleccionado dentro de `items`.
//! - `group\t{ws}\t{start}\t{len}` — un escritorio: rango `[start, start+len)`
//!   de `items` que le pertenece (sólo en árbol).
//! - `item\t{id}\t{label}` — una ventana; `label` es el resto de la línea.

use std::path::PathBuf;

/// El estado del switcher espejado desde mirada.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AltabView {
    /// `true` si está desplegado como árbol (Alt pegado); `false` = alternar
    /// plano (tap rápido, que pata no alcanza a mostrar).
    pub tree: bool,
    /// Índice seleccionado en `items`.
    pub sel: usize,
    /// Grupos por escritorio: `(ws 1-based, start, len)` en `items`. Vacío en
    /// modo plano.
    pub groups: Vec<(usize, usize, usize)>,
    /// Ventanas en orden de `items`: `(id, etiqueta)`.
    pub items: Vec<(u32, String)>,
}

impl AltabView {
    /// El escritorio (1-based) del ítem `i`, si cae en algún grupo.
    pub fn ws_de(&self, i: usize) -> Option<usize> {
        self.groups
            .iter()
            .find(|(_, start, len)| i >= *start && i < *start + *len)
            .map(|(ws, _, _)| *ws)
    }
}

/// La ruta del archivo runtime que publica mirada. Debe coincidir con
/// `mirada-compositor/src/switcher.rs::export_path`.
pub fn export_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("mirada-switcher"),
        None => PathBuf::from("/tmp/mirada-switcher"),
    }
}

/// Lee y parsea el estado del switcher, o `None` si el archivo no existe (no hay
/// switcher activo) o está corrupto.
pub fn read() -> Option<AltabView> {
    let txt = std::fs::read_to_string(export_path()).ok()?;
    parse(&txt)
}

/// Parsea el texto tab-separado. `None` si no hay ninguna línea válida.
pub fn parse(txt: &str) -> Option<AltabView> {
    let mut v = AltabView::default();
    let mut vio_algo = false;
    for linea in txt.lines() {
        // Separá la directiva del resto; el label de `item` es el resto ENTERO
        // (puede traer tabs), de ahí no usar un `splitn` global.
        let Some((dir, rest)) = linea.split_once('\t') else {
            continue;
        };
        match dir {
            "tree" => {
                v.tree = rest == "1";
                vio_algo = true;
            }
            "sel" => {
                if let Ok(n) = rest.parse() {
                    v.sel = n;
                    vio_algo = true;
                }
            }
            "group" => {
                let mut g = rest.splitn(3, '\t');
                let ws = g.next().and_then(|s| s.parse().ok());
                let start = g.next().and_then(|s| s.parse().ok());
                let len = g.next().and_then(|s| s.parse().ok());
                if let (Some(ws), Some(start), Some(len)) = (ws, start, len) {
                    v.groups.push((ws, start, len));
                    vio_algo = true;
                }
            }
            "item" => {
                // `id\tlabel`; el label es el resto tras el id (con tabs).
                let (id, label) = match rest.split_once('\t') {
                    Some((id, label)) => (id.parse().ok(), label.to_string()),
                    None => (rest.parse().ok(), String::new()),
                };
                if let Some(id) = id {
                    v.items.push((id, label));
                    vio_algo = true;
                }
            }
            _ => {}
        }
    }
    vio_algo.then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsea_un_arbol_de_dos_escritorios() {
        let txt = "tree\t1\nsel\t2\ngroup\t1\t0\t2\ngroup\t2\t2\t1\n\
                   item\t5\tFirefox\nitem\t7\tterminal\nitem\t9\tCosmos\n";
        let v = parse(txt).unwrap();
        assert!(v.tree);
        assert_eq!(v.sel, 2);
        assert_eq!(v.groups, vec![(1, 0, 2), (2, 2, 1)]);
        assert_eq!(v.items.len(), 3);
        assert_eq!(v.items[0], (5, "Firefox".to_string()));
        // El ítem seleccionado (2) cae en el escritorio 2.
        assert_eq!(v.ws_de(v.sel), Some(2));
        assert_eq!(v.ws_de(0), Some(1));
    }

    #[test]
    fn label_con_tab_no_rompe() {
        // Un título con un tab adentro: el label es el resto de la línea.
        let v = parse("item\t3\tDoc\tsin guardar\n").unwrap();
        assert_eq!(v.items[0], (3, "Doc\tsin guardar".to_string()));
    }

    #[test]
    fn vacio_o_corrupto_es_none() {
        assert!(parse("").is_none());
        assert!(parse("basura sin tabs\n").is_none());
    }

    #[test]
    fn modo_plano_sin_grupos() {
        let v = parse("tree\t0\nsel\t1\nitem\t1\tA\nitem\t2\tB\n").unwrap();
        assert!(!v.tree);
        assert!(v.groups.is_empty());
        assert_eq!(v.ws_de(0), None);
    }
}
