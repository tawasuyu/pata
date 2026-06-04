//! `pata-host` — el **rail hospedado**: el protocolo por el que una app le presta
//! sus "dientes" (su sidebar) al marco `pata` mientras tiene foco, y recibe de
//! vuelta qué diente activó el usuario.
//!
//! La idea (visión del autor): una app como `cosmos` puede dejar de pintar su
//! propio rail y quedar como **puro lienzo**; sus herramientas aparecen en el rail
//! global de pata cuando la app está enfocada. Al clickear un diente en pata, el
//! comando vuelve a la app, que muestra ese panel sobre su propio canvas. pata
//! sólo hospeda el **rail** (los dientes) — no los paneles ricos de la app.
//!
//! ## Transporte
//!
//! Un socket Unix dedicado ([`socket_path`], default
//! `$XDG_RUNTIME_DIR/pata-sidebar.sock`). pata escucha ([`HostServer`]); las apps
//! se conectan ([`HostClient`]). Cada conexión es un stream con marco
//! **prefijo-de-longitud + postcard** (igual que `mirada-link`):
//!
//! ```text
//! app  → shell:  [u32 LE len][postcard AppMsg]…   (Register, Update, Bye)
//! shell → app:   [u32 LE len][postcard ShellMsg]… (Activate)
//! ```
//!
//! pata correlaciona el `app_id` que la app declara en `Register` con el `app_id`
//! del toplevel enfocado (que ya conoce vía wlr-foreign-toplevel). Cuando coinciden,
//! pinta los dientes de esa app; al clickear uno, le manda `Activate{tooth}`.

#![forbid(unsafe_code)]

use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::{de::DeserializeOwned, Deserialize, Serialize};

// =====================================================================
// Wire types
// =====================================================================

/// Un diente que la app presta al rail de pata: id opaco (asignado por la app),
/// nombre de icono (mismo vocabulario abierto que `pata_core::SidebarTab::icon`)
/// y etiqueta (tooltip).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostedTooth {
    /// Identificador del diente, opaco para pata; vuelve tal cual en [`ShellMsg::Activate`].
    pub id: u32,
    /// Nombre del icono (p. ej. `"folder"`, `"tools"`, `"astro"`).
    pub icon: String,
    /// Etiqueta corta (tooltip del diente).
    pub label: String,
}

impl HostedTooth {
    pub fn new(id: u32, icon: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id,
            icon: icon.into(),
            label: label.into(),
        }
    }
}

/// Mensaje de la **app hacia el shell**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppMsg {
    /// Alta: la app se presenta con su `app_id` (el mismo que reporta el
    /// compositor para su ventana), un título y sus dientes.
    Register {
        app_id: String,
        title: String,
        teeth: Vec<HostedTooth>,
    },
    /// Sus dientes cambiaron (mismo `app_id` implícito por la conexión).
    Update { teeth: Vec<HostedTooth> },
    /// Baja explícita (también se infiere al cerrarse la conexión).
    Bye,
}

/// Mensaje del **shell hacia la app**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellMsg {
    /// El usuario clickeó el diente `tooth` (el `id` que la app declaró). La app
    /// decide la semántica (típicamente: togglear ese panel sobre su canvas).
    Activate { tooth: u32 },
}

// =====================================================================
// Transporte
// =====================================================================

/// Variable de entorno para sobreescribir la ruta del socket.
pub const SOCKET_ENV: &str = "PATA_SIDEBAR_SOCKET";
/// Nombre por defecto del socket.
pub const SOCKET_NAME: &str = "pata-sidebar.sock";

/// Ruta canónica del socket del rail hospedado. Honra [`SOCKET_ENV`]; si no,
/// arma sobre `$XDG_RUNTIME_DIR` (con fallback al directorio temporal).
pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var(SOCKET_ENV) {
        return PathBuf::from(p);
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(SOCKET_NAME)
}

fn postcard_err(e: postcard::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

/// Escribe un mensaje con marco `[u32 LE len][postcard]`.
pub fn write_frame<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()> {
    let bytes = postcard::to_stdvec(msg).map_err(postcard_err)?;
    w.write_all(&(bytes.len() as u32).to_le_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

/// Lee un mensaje con marco `[u32 LE len][postcard]`. `Err(UnexpectedEof)` al
/// cerrarse la conexión limpiamente.
pub fn read_frame<T: DeserializeOwned>(r: &mut impl Read) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_le_bytes(len) as usize;
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    postcard::from_bytes(&buf).map_err(postcard_err)
}

#[cfg(unix)]
mod server;
#[cfg(unix)]
pub use server::HostServer;

#[cfg(unix)]
mod client;
#[cfg(unix)]
pub use client::HostClient;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_roundtrip_appmsg() {
        let m = AppMsg::Register {
            app_id: "gioser.cosmos".into(),
            title: "Cosmos".into(),
            teeth: vec![HostedTooth::new(1, "folder", "Árbol"), HostedTooth::new(2, "tools", "Herramientas")],
        };
        let bytes = postcard::to_stdvec(&m).unwrap();
        let back: AppMsg = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn wire_roundtrip_shellmsg() {
        let m = ShellMsg::Activate { tooth: 7 };
        let bytes = postcard::to_stdvec(&m).unwrap();
        assert_eq!(postcard::from_bytes::<ShellMsg>(&bytes).unwrap(), m);
    }

    #[test]
    fn frame_roundtrip_sobre_buffer() {
        let mut buf: Vec<u8> = Vec::new();
        let m = ShellMsg::Activate { tooth: 3 };
        write_frame(&mut buf, &m).unwrap();
        let mut cur = std::io::Cursor::new(buf);
        let back: ShellMsg = read_frame(&mut cur).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn socket_path_honra_env() {
        // No tocamos el entorno global del proceso de forma persistente; sólo
        // verificamos la forma del fallback.
        let p = socket_path();
        assert!(p.ends_with(SOCKET_NAME) || std::env::var(SOCKET_ENV).is_ok());
    }
}
