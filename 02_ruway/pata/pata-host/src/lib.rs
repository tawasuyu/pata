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
    /// Cuál de sus dientes está **activo** ahora (su panel desplegado sobre el
    /// canvas): `Some(id)` resalta ese diente en el rail de pata; `None` = nada
    /// desplegado (puro lienzo). Sin este mensaje los dientes hospedados van
    /// siempre inactivos (pata no conoce el estado interno de la app).
    SetActive { tooth: Option<u32> },
    /// Baja explícita (también se infiere al cerrarse la conexión).
    Bye,
    /// **Comando de shell** de un cliente que NO es una app de rail — hoy el
    /// compositor (mirada) desde una esquina caliente. No lleva `Register`: es
    /// un disparo suelto (conectar → un frame → cerrar). pata lo drena con
    /// [`HostServer::take_commands`] y lo traduce a una acción de su UI.
    Command(ShellCommand),
}

/// Comandos sueltos que un cliente externo (el compositor) le pide a pata sobre
/// su propia UI, por el mismo socket del rail. Separado de los dientes: no toca
/// el registro de apps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellCommand {
    /// Desplegar/replegar el drawer de shuma (como el hotkey / el toggle a mano).
    /// La esquina caliente top-center de mirada lo dispara: abre el vistazo y el
    /// mismo gesto lo cierra.
    ToggleShuma,
}

/// La ruta del socket de pata **de un usuario concreto** (por su uid), armada
/// SIN mirar el entorno del proceso actual: `/run/user/<uid>/pata-sidebar.sock`.
/// La necesita quien conecta con OTRO `XDG_RUNTIME_DIR` que pata — el caso del
/// compositor, que corre como root y pata como el usuario logueado. Si el usuario
/// lanzó pata con `PATA_SIDEBAR_SOCKET` a medida, esta ruta no lo refleja (caso
/// no estándar).
pub fn user_socket_path(uid: u32) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}")).join(SOCKET_NAME)
}

/// Cliente **one-shot** a un socket explícito: conecta, manda un [`ShellCommand`]
/// y cierra. Es la variante cross-uid — el compositor arma la ruta del usuario
/// con [`user_socket_path`] porque su `XDG_RUNTIME_DIR` (root) no es el de pata.
#[cfg(unix)]
pub fn send_command_to(path: &std::path::Path, cmd: ShellCommand) -> io::Result<()> {
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(path)?;
    write_frame(&mut stream, &AppMsg::Command(cmd))
}

/// Cliente one-shot al socket derivado del **entorno propio** (mismo usuario que
/// pata). Para cruzar de uid usa [`send_command_to`] con [`user_socket_path`].
#[cfg(unix)]
pub fn send_command(cmd: ShellCommand) -> io::Result<()> {
    send_command_to(&socket_path(), cmd)
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

/// `true` si pata está corriendo ahora (su socket existe en disco).
pub fn shell_present() -> bool {
    socket_path().exists()
}

/// **Decisión de delegación de sidebar** para una app: ¿debe entregarle sus
/// dientes a pata (rail hospedado) en vez de pintar su propio sidebar?
///
/// Default querido (2026-06-24): **sí, cuando pata está corriendo** —así hay una
/// sola barra de rails en toda la pantalla (la de pata) que absorbe el sidebar
/// de la app activa, sin duplicar. Si pata no está (app standalone), `false`: la
/// app pinta su propio sidebar como siempre. `env_var` (p.ej.
/// `COSMOS_DELEGATE_SIDEBAR`) es un **opt-out** explícito: `=0`/`false`/`no`
/// fuerza no-delegar aunque pata esté.
pub fn delegate_sidebar_default(env_var: &str) -> bool {
    if matches!(
        std::env::var(env_var).as_deref(),
        Ok("0") | Ok("false") | Ok("no")
    ) {
        return false;
    }
    shell_present()
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

// pata es un marco Linux/Wayland: no hay rail hospedado fuera de unix. El stub
// mantiene la API ([`HostClient::connect`] → siempre `None`) para que las apps
// que delegan su sidebar (p. ej. cosmos) compilen en Windows y sigan con su
// propio rail.
#[cfg(not(unix))]
mod client_stub;
#[cfg(not(unix))]
pub use client_stub::HostClient;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_roundtrip_appmsg() {
        let m = AppMsg::Register {
            app_id: "tawasuyu.cosmos".into(),
            title: "Cosmos".into(),
            teeth: vec![HostedTooth::new(1, "folder", "Árbol"), HostedTooth::new(2, "tools", "Herramientas")],
        };
        let bytes = postcard::to_stdvec(&m).unwrap();
        let back: AppMsg = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn wire_roundtrip_command() {
        let m = AppMsg::Command(ShellCommand::ToggleShuma);
        let bytes = postcard::to_stdvec(&m).unwrap();
        assert_eq!(postcard::from_bytes::<AppMsg>(&bytes).unwrap(), m);
    }

    #[test]
    fn wire_roundtrip_setactive() {
        for tooth in [Some(3u32), None] {
            let m = AppMsg::SetActive { tooth };
            let bytes = postcard::to_stdvec(&m).unwrap();
            assert_eq!(postcard::from_bytes::<AppMsg>(&bytes).unwrap(), m);
        }
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
