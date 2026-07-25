//! `pata-portapapeles` — el **gestor de portapapeles** (Klipper) de la suite.
//!
//! Sube el ring de sólo-texto en memoria de pata a un **historial persistente**
//! (sled) que además:
//! * guarda **texto e imágenes** (mime + bytes),
//! * **deduplica** moviendo al frente el clip repetido,
//! * deja **fijar** (pin) clips para que sobrevivan a la limpieza y al tope,
//! * **busca** por subcadena en los clips de texto,
//! * detecta **acciones** sobre un clip (URL/email/ruta → sugerencia).
//!
//! Núcleo puro y testeable; el widget de pata lo consume (muestrea `wl-paste`,
//! empuja aquí, y pinta el historial + las acciones).

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod acciones;
pub use acciones::{accion_para, AccionClip};

#[derive(Debug, Error)]
pub enum ClipError {
    #[error("sled: {0}")]
    Sled(#[from] sled::Error),
    #[error("serialización: {0}")]
    Bincode(#[from] bincode::Error),
}

/// Tope por defecto de entradas **no fijadas** en el historial.
pub const TOPE_DEFECTO: usize = 50;

/// El contenido de un clip.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Contenido {
    Texto(String),
    Imagen { mime: String, bytes: Vec<u8> },
}

impl Contenido {
    /// Preview de una línea para la UI.
    pub fn preview(&self) -> String {
        match self {
            Contenido::Texto(t) => t.lines().next().unwrap_or("").chars().take(120).collect(),
            Contenido::Imagen { mime, bytes } => format!("🖼 {} ({} bytes)", mime, bytes.len()),
        }
    }

    pub fn es_imagen(&self) -> bool {
        matches!(self, Contenido::Imagen { .. })
    }
}

/// Una entrada del historial.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entrada {
    /// Id monotónico creciente (también determina el orden temporal).
    pub id: u64,
    pub contenido: Contenido,
    /// Fijada: no la borran ni la limpieza ni el recorte por tope.
    pub fijada: bool,
    pub ts_usec: u64,
}

const T_ENTRADAS: &str = "entradas";
const T_META: &str = "meta";
const K_CONTADOR: &[u8] = b"contador";

/// El historial de portapapeles sobre `sled`.
pub struct Historial {
    entradas: sled::Tree,
    meta: sled::Tree,
    db: sled::Db,
    tope: usize,
}

impl Historial {
    /// Abre (o crea) el historial en `ruta`.
    pub fn abrir(ruta: impl AsRef<std::path::Path>) -> Result<Self, ClipError> {
        let db = sled::open(ruta)?;
        Ok(Self {
            entradas: db.open_tree(T_ENTRADAS)?,
            meta: db.open_tree(T_META)?,
            db,
            tope: TOPE_DEFECTO,
        })
    }

    pub fn con_tope(mut self, tope: usize) -> Self {
        self.tope = tope.max(1);
        self
    }

    fn siguiente_id(&self) -> Result<u64, ClipError> {
        // Contador persistente monotónico.
        let actual = self
            .meta
            .get(K_CONTADOR)?
            .map(|v| {
                let mut b = [0u8; 8];
                b.copy_from_slice(&v[..8.min(v.len())]);
                u64::from_be_bytes(b)
            })
            .unwrap_or(0);
        let nuevo = actual + 1;
        self.meta.insert(K_CONTADOR, &nuevo.to_be_bytes())?;
        Ok(nuevo)
    }

    /// Empuja un clip. Si el contenido ya existe, lo **mueve al frente** (nuevo
    /// id) en vez de duplicar, preservando su estado de fijado. Devuelve `true`
    /// si el clip es realmente nuevo o se reordenó. Tras insertar, recorta el
    /// historial al tope (respetando las fijadas).
    pub fn empujar(&self, contenido: Contenido, ts_usec: u64) -> Result<bool, ClipError> {
        // Texto vacío no entra.
        if let Contenido::Texto(t) = &contenido {
            if t.is_empty() {
                return Ok(false);
            }
        }
        // ¿Ya está? (dedup por contenido).
        let existente = self.buscar_igual(&contenido)?;
        let ya_al_frente = self
            .listar()?
            .first()
            .map(|e| e.contenido == contenido)
            .unwrap_or(false);
        if ya_al_frente {
            return Ok(false); // ya es el tope, nada que hacer
        }
        let fijada = existente
            .as_ref()
            .map(|e| e.fijada)
            .unwrap_or(false);
        if let Some(e) = existente {
            self.entradas.remove(e.id.to_be_bytes())?;
        }
        let id = self.siguiente_id()?;
        let entrada = Entrada {
            id,
            contenido,
            fijada,
            ts_usec,
        };
        self.entradas.insert(id.to_be_bytes(), bincode::serialize(&entrada)?)?;
        self.recortar()?;
        Ok(true)
    }

    fn buscar_igual(&self, contenido: &Contenido) -> Result<Option<Entrada>, ClipError> {
        for e in self.listar()? {
            if &e.contenido == contenido {
                return Ok(Some(e));
            }
        }
        Ok(None)
    }

    /// Todas las entradas, **más nueva primero**.
    pub fn listar(&self) -> Result<Vec<Entrada>, ClipError> {
        let mut out = Vec::new();
        for r in self.entradas.iter() {
            let (_, v) = r?;
            out.push(bincode::deserialize(&v)?);
        }
        out.sort_by(|a: &Entrada, b: &Entrada| b.id.cmp(&a.id));
        Ok(out)
    }

    /// Busca por subcadena (case-insensitive) en los clips de **texto**; las
    /// imágenes se incluyen sólo si la consulta está vacía.
    pub fn buscar(&self, consulta: &str) -> Result<Vec<Entrada>, ClipError> {
        let q = consulta.trim().to_lowercase();
        if q.is_empty() {
            return self.listar();
        }
        Ok(self
            .listar()?
            .into_iter()
            .filter(|e| match &e.contenido {
                Contenido::Texto(t) => t.to_lowercase().contains(&q),
                Contenido::Imagen { .. } => false,
            })
            .collect())
    }

    /// (Des)fija una entrada.
    pub fn fijar(&self, id: u64, fijada: bool) -> Result<(), ClipError> {
        if let Some(v) = self.entradas.get(id.to_be_bytes())? {
            let mut e: Entrada = bincode::deserialize(&v)?;
            e.fijada = fijada;
            self.entradas.insert(id.to_be_bytes(), bincode::serialize(&e)?)?;
        }
        Ok(())
    }

    /// Alterna el fijado de una entrada; devuelve el nuevo estado (o `None` si no
    /// existe). Lo usa el botón de pin del popup.
    pub fn alternar_fijado(&self, id: u64) -> Result<Option<bool>, ClipError> {
        if let Some(v) = self.entradas.get(id.to_be_bytes())? {
            let mut e: Entrada = bincode::deserialize(&v)?;
            e.fijada = !e.fijada;
            let nuevo = e.fijada;
            self.entradas.insert(id.to_be_bytes(), bincode::serialize(&e)?)?;
            Ok(Some(nuevo))
        } else {
            Ok(None)
        }
    }

    /// Borra una entrada (idempotente).
    pub fn borrar(&self, id: u64) -> Result<(), ClipError> {
        self.entradas.remove(id.to_be_bytes())?;
        Ok(())
    }

    /// Limpia el historial: borra las **no fijadas** (como Klipper). Las fijadas
    /// se conservan.
    pub fn limpiar(&self) -> Result<(), ClipError> {
        for e in self.listar()? {
            if !e.fijada {
                self.entradas.remove(e.id.to_be_bytes())?;
            }
        }
        Ok(())
    }

    /// Recorta las entradas **no fijadas** más viejas hasta respetar el tope. Las
    /// fijadas no cuentan ni se borran.
    fn recortar(&self) -> Result<(), ClipError> {
        let todas = self.listar()?; // nueva→vieja
        let no_fijadas: Vec<&Entrada> = todas.iter().filter(|e| !e.fijada).collect();
        if no_fijadas.len() <= self.tope {
            return Ok(());
        }
        for e in no_fijadas.into_iter().skip(self.tope) {
            self.entradas.remove(e.id.to_be_bytes())?;
        }
        Ok(())
    }

    /// Cuántas entradas hay.
    pub fn len(&self) -> usize {
        self.entradas.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entradas.is_empty()
    }

    pub fn flush(&self) -> Result<(), ClipError> {
        self.db.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist() -> Historial {
        let dir = std::env::temp_dir().join(format!(
            "pata-clip-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        Historial::abrir(dir).unwrap()
    }

    fn txt(s: &str) -> Contenido {
        Contenido::Texto(s.to_string())
    }

    #[test]
    fn empuja_y_lista_mas_nuevo_primero() {
        let h = hist();
        h.empujar(txt("uno"), 1).unwrap();
        h.empujar(txt("dos"), 2).unwrap();
        let l = h.listar().unwrap();
        assert_eq!(l[0].contenido, txt("dos"));
        assert_eq!(l[1].contenido, txt("uno"));
    }

    #[test]
    fn dedup_mueve_al_frente() {
        let h = hist();
        h.empujar(txt("a"), 1).unwrap();
        h.empujar(txt("b"), 2).unwrap();
        assert!(h.empujar(txt("a"), 3).unwrap()); // repetido: se reordena
        let l = h.listar().unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].contenido, txt("a"));
    }

    #[test]
    fn repetir_el_tope_no_hace_nada() {
        let h = hist();
        h.empujar(txt("a"), 1).unwrap();
        assert!(!h.empujar(txt("a"), 2).unwrap());
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn texto_vacio_no_entra() {
        let h = hist();
        assert!(!h.empujar(txt(""), 1).unwrap());
        assert_eq!(h.len(), 0);
    }

    #[test]
    fn tope_recorta_no_fijadas() {
        let h = hist().con_tope(3);
        for i in 0..6 {
            h.empujar(txt(&format!("c{i}")), i).unwrap();
        }
        assert_eq!(h.len(), 3);
        // Quedan las 3 más nuevas.
        let l = h.listar().unwrap();
        assert_eq!(l[0].contenido, txt("c5"));
    }

    #[test]
    fn fijada_sobrevive_al_tope_y_a_limpiar() {
        let h = hist().con_tope(2);
        let _ = h.empujar(txt("pin"), 1).unwrap();
        let id = h.listar().unwrap()[0].id;
        h.fijar(id, true).unwrap();
        // Empuja muchas no fijadas.
        for i in 0..5 {
            h.empujar(txt(&format!("x{i}")), 10 + i).unwrap();
        }
        assert!(h.listar().unwrap().iter().any(|e| e.contenido == txt("pin")));
        h.limpiar().unwrap();
        let l = h.listar().unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].contenido, txt("pin"));
    }

    #[test]
    fn imagenes_y_dedup_por_bytes() {
        let h = hist();
        let img = Contenido::Imagen { mime: "image/png".into(), bytes: vec![1, 2, 3] };
        h.empujar(img.clone(), 1).unwrap();
        h.empujar(txt("texto"), 2).unwrap();
        assert!(h.empujar(img.clone(), 3).unwrap()); // misma imagen → reordena
        let l = h.listar().unwrap();
        assert_eq!(l.len(), 2);
        assert!(l[0].contenido.es_imagen());
    }

    #[test]
    fn alternar_fijado_va_y_vuelve() {
        let h = hist();
        h.empujar(txt("a"), 1).unwrap();
        let id = h.listar().unwrap()[0].id;
        assert_eq!(h.alternar_fijado(id).unwrap(), Some(true));
        assert!(h.listar().unwrap()[0].fijada);
        assert_eq!(h.alternar_fijado(id).unwrap(), Some(false));
        assert!(!h.listar().unwrap()[0].fijada);
        assert_eq!(h.alternar_fijado(999).unwrap(), None);
    }

    #[test]
    fn busca_solo_texto() {
        let h = hist();
        h.empujar(txt("hola mundo"), 1).unwrap();
        h.empujar(txt("chau"), 2).unwrap();
        h.empujar(Contenido::Imagen { mime: "image/png".into(), bytes: vec![9] }, 3).unwrap();
        assert_eq!(h.buscar("mundo").unwrap().len(), 1);
        assert_eq!(h.buscar("").unwrap().len(), 3);
    }

    #[test]
    fn persiste_entre_aperturas() {
        let dir = std::env::temp_dir().join(format!(
            "pata-clip-persist-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        {
            let h = Historial::abrir(&dir).unwrap();
            h.empujar(txt("persistente"), 1).unwrap();
            h.flush().unwrap();
        }
        let h2 = Historial::abrir(&dir).unwrap();
        assert_eq!(h2.listar().unwrap()[0].contenido, txt("persistente"));
    }
}
