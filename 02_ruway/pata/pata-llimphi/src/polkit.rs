//! Agente de autenticación **polkit** (`org.freedesktop.PolicyKit1`).
//!
//! Un escritorio necesita un agente gráfico que pida la contraseña cuando una
//! acción privilegiada lo requiere (el reloj de pata ya usa `pkexec`, p.ej.). Sin
//! agente, esas autenticaciones no tienen UI. pata registra el suyo, igual que es
//! el watcher del tray: corre en su **propio hilo** con un runtime tokio
//! current-thread (zbus es async, el bucle de pata es bloqueante — patrón de
//! `tray.rs`/`mirada-portal`).
//!
//! Flujo: polkitd llama a `BeginAuthentication` en nuestro objeto; el hilo manda
//! un [`PolkitRequest`] al bucle de UI (con un `oneshot` para la respuesta) y
//! espera. La UI muestra el diálogo de contraseña (reusa el campo con foco de
//! teclado del applet de red); al confirmar/cancelar responde por el `oneshot`.
//! Con la contraseña, el agente corre el helper setuid `polkit-agent-helper-1`,
//! que habla PAM y le dice el resultado a polkitd por el `cookie`. La contraseña
//! **no** se loguea ni pasa por la shell — va por el stdin del helper.
//!
//! Alcance: una autenticación a la vez (la típica). `CancelAuthentication` es
//! best-effort —hoy un no-op: el diálogo abierto sigue hasta que el usuario
//! responde o cancela, y un helper tardío simplemente falla—. Runtime no
//! verificable headless (norma de pata).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};

use tokio::sync::oneshot;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{interface, proxy};

/// Una solicitud de autenticación que el hilo del agente manda al bucle de UI.
pub struct PolkitRequest {
    /// El mensaje a mostrar (lo arma polkit: «Se requiere autenticación para…»).
    pub message: String,
    /// Por dónde la UI devuelve la contraseña (`Some`) o la cancelación (`None`).
    pub reply: oneshot::Sender<Option<String>>,
}

/// El asa que el bucle de pata conserva: drena las solicitudes pendientes.
pub struct PolkitHandle {
    rx: std::sync::mpsc::Receiver<PolkitRequest>,
}

impl PolkitHandle {
    /// Arranca el hilo del agente y lo registra con polkitd. Devuelve `None` sólo
    /// si no se pudo lanzar el hilo (la conexión/registro se intentan dentro; si
    /// fallan, el hilo termina y no hay agente, sin romper la barra).
    pub fn spawn() -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<PolkitRequest>();
        std::thread::Builder::new()
            .name("pata-polkit".into())
            .spawn(move || run(tx))
            .ok()?;
        Some(Self { rx })
    }

    /// La próxima solicitud pendiente, o `None`. No bloquea.
    pub fn try_recv(&self) -> Option<PolkitRequest> {
        self.rx.try_recv().ok()
    }
}

/// El hilo del agente: runtime tokio current-thread + bucle async.
fn run(tx: std::sync::mpsc::Sender<PolkitRequest>) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    rt.block_on(async move {
        if let Err(e) = registrar(tx).await {
            eprintln!("pata polkit · no se pudo registrar el agente ({e}); sin agente");
        }
    });
}

/// La interfaz del **Authority** de polkit que consumimos para registrarnos.
#[proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait Authority {
    /// Registra un agente para el `subject` dado (la sesión). `object_path` es el
    /// objeto que implementa `AuthenticationAgent`.
    fn register_authentication_agent(
        &self,
        subject: &(String, HashMap<String, OwnedValue>),
        locale: &str,
        object_path: &str,
    ) -> zbus::Result<()>;
}

/// El objeto que polkitd llama: implementa `AuthenticationAgent`.
struct Agent {
    /// Hacia el bucle de UI (clonado por cada `BeginAuthentication`).
    tx: std::sync::mpsc::Sender<PolkitRequest>,
}

#[interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Agent {
    /// polkitd pide autenticar una acción. Bloquea (async) hasta resolver:
    /// pedimos la contraseña a la UI y corremos el helper PAM.
    async fn begin_authentication(
        &self,
        _action_id: String,
        message: String,
        _icon_name: String,
        _details: HashMap<String, String>,
        cookie: String,
        identities: Vec<(String, HashMap<String, OwnedValue>)>,
    ) -> zbus::fdo::Result<()> {
        // 1) ¿Como qué usuario autenticamos? El primero unix-user (preferimos el
        //    nuestro si figura), resuelto a nombre.
        let user = elegir_usuario(&identities)
            .ok_or_else(|| zbus::fdo::Error::Failed("sin identidad unix-user".into()))?;

        // 2) Pedimos la contraseña al bucle de UI.
        let (rtx, rrx) = oneshot::channel();
        if self
            .tx
            .send(PolkitRequest { message, reply: rtx })
            .is_err()
        {
            return Err(zbus::fdo::Error::Failed("UI no disponible".into()));
        }
        let pw = match rrx.await {
            Ok(Some(pw)) => pw,
            // Cancelado o UI caída: la autenticación no se completó.
            _ => return Err(zbus::fdo::Error::Failed("cancelado".into())),
        };

        // 3) Corremos el helper PAM con la contraseña. El bloqueo de E/S del
        //    helper es breve; lo hacemos en un hilo para no trabar el runtime.
        let ok = tokio::task::spawn_blocking(move || correr_helper(&user, &cookie, &pw))
            .await
            .unwrap_or(false);
        if ok {
            Ok(())
        } else {
            Err(zbus::fdo::Error::Failed("autenticación fallida".into()))
        }
    }

    /// polkitd cancela una autenticación en vuelo. Como atendemos de a una y la
    /// respuesta va por el `oneshot`, no hace falta más: si la UI ya cerró, el
    /// `BeginAuthentication` correspondiente terminará al recibir la cancelación.
    async fn cancel_authentication(&self, _cookie: String) {}
}

/// Registra el agente: conecta al **bus de sistema**, sirve el objeto y llama a
/// `RegisterAuthenticationAgent` con el subject de la sesión. Mantiene la
/// conexión viva hasta que el proceso termina.
async fn registrar(tx: std::sync::mpsc::Sender<PolkitRequest>) -> zbus::Result<()> {
    const OBJ: &str = "/tawasuyu/pata/PolkitAgent";
    let session_id = std::env::var("XDG_SESSION_ID")
        .map_err(|_| zbus::Error::Failure("sin XDG_SESSION_ID (¿sesión sin logind?)".into()))?;

    let conn = zbus::connection::Builder::system()?
        .serve_at(OBJ, Agent { tx })?
        .build()
        .await?;

    // subject = ("unix-session", {"session-id": <id>}).
    let mut detalles: HashMap<String, OwnedValue> = HashMap::new();
    detalles.insert(
        "session-id".to_string(),
        Value::from(session_id).try_to_owned()?,
    );
    let subject = ("unix-session".to_string(), detalles);

    let authority = AuthorityProxy::new(&conn).await?;
    authority
        .register_authentication_agent(&subject, "es_AR.UTF-8", OBJ)
        .await?;

    // Quedarse vivo atendiendo llamadas del Authority.
    std::future::pending::<()>().await;
    Ok(())
}

/// Elige el usuario con el que autenticar entre las `identities`: si nuestro uid
/// (de `$USER`/`getuid`) figura, ése; si no, el primer `unix-user`.
fn elegir_usuario(identities: &[(String, HashMap<String, OwnedValue>)]) -> Option<String> {
    let mut primero = None;
    let yo = std::env::var("USER").ok();
    for (kind, det) in identities {
        if kind != "unix-user" {
            continue;
        }
        let Some(uid) = det.get("uid").and_then(uid_de_value) else {
            continue;
        };
        let Some(name) = username_for_uid(uid) else {
            continue;
        };
        if Some(&name) == yo.as_ref() {
            return Some(name);
        }
        if primero.is_none() {
            primero = Some(name);
        }
    }
    primero
}

/// Lee un uid (`u32`) de un `Value` de polkit (suele venir como `u32`).
fn uid_de_value(v: &OwnedValue) -> Option<u32> {
    u32::try_from(v).ok()
}

/// Resuelve un uid a nombre de usuario vía `getent passwd <uid>` (primer campo).
/// Sin depender de libc; `None` si no se pudo.
fn username_for_uid(uid: u32) -> Option<String> {
    let out = std::process::Command::new("getent")
        .args(["passwd", &uid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let linea = String::from_utf8_lossy(&out.stdout);
    linea.split(':').next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// El prefijo de una línea del helper indica qué espera.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HelperLine {
    /// Pide un secreto (contraseña): hay que responder por stdin.
    Prompt,
    /// Autenticación exitosa.
    Success,
    /// Autenticación fallida.
    Failure,
    /// Mensaje informativo / de error: se ignora.
    Info,
}

/// Clasifica una línea de stdout del helper. (Pura, testeable.)
pub(crate) fn clasificar_linea(linea: &str) -> HelperLine {
    let l = linea.trim_end();
    if l.starts_with("PAM_PROMPT_ECHO_OFF") || l.starts_with("PAM_PROMPT_ECHO_ON") {
        HelperLine::Prompt
    } else if l == "SUCCESS" {
        HelperLine::Success
    } else if l == "FAILURE" {
        HelperLine::Failure
    } else {
        HelperLine::Info
    }
}

/// Las rutas donde suele vivir el helper setuid según la distro.
const HELPER_PATHS: [&str; 3] = [
    "/usr/lib/polkit-1/polkit-agent-helper-1",
    "/usr/libexec/polkit-1/polkit-agent-helper-1",
    "/usr/lib/policykit-1/polkit-agent-helper-1",
];

/// Corre `polkit-agent-helper-1 <user>`: le pasa el `cookie` y responde sus
/// prompts PAM con `pw`. Devuelve `true` si el helper reportó `SUCCESS`.
fn correr_helper(user: &str, cookie: &str, pw: &str) -> bool {
    let Some(path) = HELPER_PATHS.iter().find(|p| std::path::Path::new(p).exists()) else {
        eprintln!("pata polkit · no encontré polkit-agent-helper-1");
        return false;
    };
    let mut child = match std::process::Command::new(path)
        .arg(user)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("pata polkit · no pude lanzar el helper: {e}");
            return false;
        }
    };
    let Some(mut stdin) = child.stdin.take() else { return false };
    let Some(stdout) = child.stdout.take() else { return false };

    // El helper espera el cookie como primera línea.
    if writeln!(stdin, "{cookie}").is_err() {
        return false;
    }
    let _ = stdin.flush();

    let mut reader = BufReader::new(stdout);
    let mut linea = String::new();
    let mut exito = false;
    loop {
        linea.clear();
        match reader.read_line(&mut linea) {
            Ok(0) => break, // EOF: el helper terminó
            Ok(_) => match clasificar_linea(&linea) {
                HelperLine::Prompt => {
                    if writeln!(stdin, "{pw}").is_err() {
                        break;
                    }
                    let _ = stdin.flush();
                }
                HelperLine::Success => {
                    exito = true;
                    break;
                }
                HelperLine::Failure => break,
                HelperLine::Info => {}
            },
            Err(_) => break,
        }
    }
    let _ = child.wait();
    exito
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clasifica_lineas_del_helper() {
        assert_eq!(clasificar_linea("PAM_PROMPT_ECHO_OFF Password: "), HelperLine::Prompt);
        assert_eq!(clasificar_linea("PAM_PROMPT_ECHO_ON Login: "), HelperLine::Prompt);
        assert_eq!(clasificar_linea("SUCCESS\n"), HelperLine::Success);
        assert_eq!(clasificar_linea("FAILURE\n"), HelperLine::Failure);
        assert_eq!(clasificar_linea("PAM_TEXT_INFO algo"), HelperLine::Info);
        assert_eq!(clasificar_linea("PAM_ERROR_MSG ups"), HelperLine::Info);
    }
}
