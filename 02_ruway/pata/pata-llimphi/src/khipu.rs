//! **Khipu** — la captura rápida de notas *que se desvanecen*, desde la barra.
//!
//! Un khipu es una nota con **física temporal**: nace con masa 1.0, decae con el
//! tiempo (vida media de una semana) y sube con cada acceso; cuando su masa cae
//! bajo el **horizonte** pasa al archivo (no se borra, deja de estar a la vista).
//! *Olvidar es la feature.* El fantasma de la barra titila cuando una nota está a
//! punto de caer del horizonte — la última chance de reforzarla.
//!
//! Reusa las primitivas reales de khipu (`khipu-core` para el modelo `Note`/
//! `NoteStore`, `khipu-gravity` para el decay/refuerzo/horizonte), pero sobre un
//! **store soberano propio** en `~/.local/share/khipu/pata-quick.json` (JSON): la
//! captura de la barra es liviana e independiente de la app khipu (que persiste su
//! propio `notes.bin` con embeddings). Misma física, distinto cajón.

use std::path::PathBuf;

use khipu_core::{NoteId, NoteStore};
use khipu_gravity::Gravity;

/// Bajo esta masa la nota está **a punto de caer** del horizonte (entre el
/// horizonte y el doble): la señal de salience del fantasma. El horizonte lo fija
/// `khipu-gravity` (0.10 por defecto); esto es 2× ese valor.
const MARGEN_CAIDA: f32 = 2.0;

/// Una nota lista para pintar: id, título, masa efectiva (ya decaída a *ahora*) y
/// si está por caer del horizonte.
#[derive(Clone, Debug, PartialEq)]
pub struct NotaVista {
    pub id: NoteId,
    pub titulo: String,
    /// Masa efectiva `0..~` decaída al instante de la consulta.
    pub masa: f32,
    /// `true` si está sobre el horizonte pero por caer (última chance).
    pub por_caer: bool,
}

/// Lo que el render necesita: las notas visibles (sobre el horizonte) ordenadas
/// por masa ascendente (las más moribundas primero) y si alguna está por caer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KhipuSnapshot {
    pub notas: Vec<NotaVista>,
    /// `true` si alguna nota visible está por caer del horizonte (salience).
    pub hay_por_caer: bool,
}

/// El store soberano de khipu de la barra: la física + las notas + su archivo.
pub struct KhipuStore {
    store: NoteStore,
    gravity: Gravity,
    path: PathBuf,
}

impl KhipuStore {
    /// Abre (o crea vacío) el store en `~/.local/share/khipu/pata-quick.json`.
    /// Nunca falla: si el archivo no existe o no parsea, arranca vacío.
    pub fn open() -> Self {
        let path = ruta_store();
        let store = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<NoteStore>(&b).ok())
            .unwrap_or_default();
        Self {
            store,
            gravity: Gravity::default(),
            path: path.unwrap_or_else(|| PathBuf::from("pata-quick.json")),
        }
    }

    /// Masa efectiva de una nota decaída a `now` (sin mutar el store).
    fn masa_efectiva(&self, mass: f32, last_access: u64, now: u64) -> f32 {
        let dt = now.saturating_sub(last_access) as f32;
        self.gravity.decay(mass, dt)
    }

    /// El snapshot para pintar a `now` (segundos Unix): las notas **sobre el
    /// horizonte**, ordenadas por masa ascendente, y si alguna está por caer.
    pub fn snapshot(&self, now: u64) -> KhipuSnapshot {
        let horizonte = self.gravity.params.horizon;
        let mut notas: Vec<NotaVista> = self
            .store
            .iter()
            .filter_map(|n| {
                let masa = self.masa_efectiva(n.mass, n.last_access, now);
                if masa < horizonte {
                    return None; // ya cayó al archivo — no se muestra
                }
                Some(NotaVista {
                    id: n.id,
                    titulo: if n.title.trim().is_empty() {
                        primera_linea(&n.body)
                    } else {
                        n.title.clone()
                    },
                    masa,
                    por_caer: masa < horizonte * MARGEN_CAIDA,
                })
            })
            .collect();
        notas.sort_by(|a, b| a.masa.partial_cmp(&b.masa).unwrap_or(std::cmp::Ordering::Equal));
        let hay_por_caer = notas.iter().any(|n| n.por_caer);
        KhipuSnapshot { notas, hay_por_caer }
    }

    /// Anota una nota nueva (título = primera línea, cuerpo = todo) y persiste.
    /// Ignora texto en blanco.
    pub fn jot(&mut self, texto: &str, now: u64) {
        let texto = texto.trim();
        if texto.is_empty() {
            return;
        }
        let titulo = primera_linea(texto);
        self.store.create(titulo, texto.to_string(), Vec::new(), now);
        self.persist();
    }

    /// Refuerza una nota (le sube la masa y marca el acceso), reviviéndola. Fija la
    /// masa a `reinforce(masa_efectiva)` para que el refuerzo cuente desde su masa
    /// real de ahora, no la vieja guardada. Persiste.
    pub fn reinforce(&mut self, id: NoteId, now: u64) {
        let actual = self.store.get(id).map(|n| (n.mass, n.last_access));
        if let Some((mass, last)) = actual {
            let efectiva = self.masa_efectiva(mass, last, now);
            let nueva = self.gravity.reinforce(efectiva);
            self.store.set_mass(id, nueva);
            self.store.touch(id, now);
            self.persist();
        }
    }

    /// Guarda el store a disco (JSON, escritura atómica por tmp+rename). Silencioso.
    fn persist(&self) {
        let Ok(bytes) = serde_json::to_vec_pretty(&self.store) else {
            return;
        };
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }
}

/// `~/.local/share/khipu/pata-quick.json` (respeta `XDG_DATA_HOME`), o `None` si
/// no se puede resolver `HOME`.
fn ruta_store() -> Option<PathBuf> {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("khipu").join("pata-quick.json"))
}

/// La primera línea no vacía de un texto, recortada a 60 caracteres (el título).
fn primera_linea(texto: &str) -> String {
    let l = texto.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if l.chars().count() > 60 {
        l.chars().take(59).chain(std::iter::once('…')).collect()
    } else {
        l.to_string()
    }
}

/// Segundos Unix de ahora (helper del host).
pub fn ahora_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_vacio() -> KhipuStore {
        KhipuStore {
            store: NoteStore::new(),
            gravity: Gravity::default(),
            path: PathBuf::from("/tmp/pata-khipu-test-noexiste.json"),
        }
    }

    #[test]
    fn jot_y_snapshot_muestra_la_nota_fresca() {
        let mut k = store_vacio();
        let now = 1_000_000;
        k.store.create("Comprar pan", "Comprar pan\ny leche", Vec::new(), now);
        let snap = k.snapshot(now);
        assert_eq!(snap.notas.len(), 1);
        assert_eq!(snap.notas[0].titulo, "Comprar pan");
        // Recién nacida: masa ~1.0, no por caer.
        assert!(snap.notas[0].masa > 0.9);
        assert!(!snap.notas[0].por_caer);
        assert!(!snap.hay_por_caer);
    }

    #[test]
    fn una_nota_vieja_esta_por_caer_o_cae() {
        let mut k = store_vacio();
        let born = 1_000_000u64;
        k.store.create("Vieja", "Vieja", Vec::new(), born);
        // 4 vidas medias después (~28 días): masa ≈ 1/16 = 0.0625 < horizonte 0.10.
        let now = born + (4.0 * 7.0 * 24.0 * 3600.0) as u64;
        let snap = k.snapshot(now);
        assert!(snap.notas.is_empty(), "ya debería haber caído del horizonte");
        // A 3 vidas medias (~21 días): masa ≈ 0.125, sobre el horizonte pero por caer.
        let casi = born + (3.0 * 7.0 * 24.0 * 3600.0) as u64;
        let snap2 = k.snapshot(casi);
        assert_eq!(snap2.notas.len(), 1);
        assert!(snap2.notas[0].por_caer, "0.125 debería marcar por_caer");
        assert!(snap2.hay_por_caer);
    }

    #[test]
    fn reinforce_revive_la_nota() {
        let mut k = store_vacio();
        let born = 1_000_000u64;
        let id = k.store.create("Revivir", "Revivir", Vec::new(), born);
        let casi = born + (3.0 * 7.0 * 24.0 * 3600.0) as u64;
        assert!(k.snapshot(casi).notas[0].por_caer);
        k.reinforce(id, casi);
        // Tras reforzar, la masa subió el boost (0.4) desde ~0.125 → ~0.525.
        let snap = k.snapshot(casi);
        assert!(!snap.notas[0].por_caer, "reforzada ya no está por caer");
        assert!(snap.notas[0].masa > 0.4);
    }
}
