//! Historial persistente de notificaciones sobre `sled`.
//!
//! Append-only: guarda *todo* lo recibido, no se borra al descartar un toast
//! (eso solo afecta el stack vivo en pantalla). Es el sustrato que leerán el
//! panel de historial y la futura capa de triage semántico/IA.

use std::path::PathBuf;

use crate::Notificacion;

/// Acceso al árbol de historial. Clonable y barato (sled es `Arc` por dentro).
#[derive(Clone)]
pub struct Store {
    // Se conserva para mantener viva la `Db` mientras exista un `Store`; sled
    // hace flush al dropearla.
    #[allow(dead_code)]
    db: sled::Db,
    tree: sled::Tree,
}

impl Store {
    /// Abre el historial en `$XDG_DATA_HOME/pata-notify` (persiste entre
    /// sesiones, a diferencia de `$XDG_RUNTIME_DIR`).
    pub fn open() -> anyhow::Result<Self> {
        let dir = data_dir();
        std::fs::create_dir_all(&dir)?;
        let db = sled::open(dir.join("historial"))?;
        let tree = db.open_tree("notificaciones")?;
        Ok(Self { db, tree })
    }

    /// Store efímero en memoria — fallback si `open` falla (disco lleno,
    /// permisos): el daemon sigue mostrando toasts aunque no persista.
    pub fn temporary() -> anyhow::Result<Self> {
        let db = sled::Config::new().temporary(true).open()?;
        let tree = db.open_tree("notificaciones")?;
        Ok(Self { db, tree })
    }

    /// Agrega una notificación al historial. La clave es
    /// `created_usec ++ id` big-endian, así `iter()` sale en orden temporal.
    pub fn append(&self, n: &Notificacion) -> anyhow::Result<()> {
        let mut key = n.created_usec.to_be_bytes().to_vec();
        key.extend_from_slice(&n.id.to_be_bytes());
        let val = postcard::to_stdvec(n)?;
        self.tree.insert(key, val)?;
        Ok(())
    }

    /// Devuelve el historial completo en orden temporal ascendente.
    pub fn list(&self) -> anyhow::Result<Vec<Notificacion>> {
        let mut out = Vec::new();
        for kv in self.tree.iter() {
            let (_, v) = kv?;
            out.push(postcard::from_bytes(&v)?);
        }
        Ok(out)
    }

    /// Vacía el historial.
    pub fn clear(&self) -> anyhow::Result<()> {
        self.tree.clear()?;
        Ok(())
    }
}

/// `$XDG_DATA_HOME/pata-notify`, con fallback a `~/.local/share/pata-notify`.
fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("pata-notify")
}
