//! Lado **shell** del rail hospedado: pata escucha el socket, acumula los dientes
//! que registran las apps y les reenvía las activaciones.

use std::collections::HashMap;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::{read_frame, socket_path, write_frame, AppMsg, HostedTooth, ShellMsg};

/// Una app registrada: su título, sus dientes y la mitad de escritura de su
/// conexión (para mandarle `Activate`).
struct AppReg {
    title: String,
    teeth: Vec<HostedTooth>,
    write: UnixStream,
}

/// El estado compartido entre el hilo aceptador/lectores y el bucle de UI.
struct Shared {
    apps: Mutex<HashMap<String, AppReg>>,
    /// Se incrementa en cada cambio (alta/baja/update) para que el host detecte
    /// "hay algo nuevo que pintar" sin difear el mapa.
    revision: AtomicU64,
}

/// El servidor del rail hospedado. Vive en pata; arranca su hilo aceptador en
/// [`HostServer::spawn`] y se consulta desde el bucle de UI.
pub struct HostServer {
    shared: Arc<Shared>,
}

impl HostServer {
    /// Bindea el socket y arranca el hilo aceptador. `None` si no se puede bindear
    /// (otro pata ya escucha, o sin permisos) — el rail hospedado queda inactivo
    /// sin romper el resto del marco.
    pub fn spawn() -> Option<HostServer> {
        let path = socket_path();
        // Limpia un socket viejo de un pata que murió (best-effort). Si hay otro
        // pata vivo, el bind de abajo fallará y devolvemos None.
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("pata host · no pude bindear {}: {e}", path.display());
                return None;
            }
        };

        let shared = Arc::new(Shared {
            apps: Mutex::new(HashMap::new()),
            revision: AtomicU64::new(0),
        });

        let shared_accept = shared.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let shared = shared_accept.clone();
                // Un lector por conexión.
                std::thread::spawn(move || handle_conn(stream, shared));
            }
        });

        Some(HostServer { shared })
    }

    /// Snapshot (clonado) del título + dientes de `app_id`, si está registrada.
    /// Para que el host lo pinte sin retener el lock.
    pub fn snapshot(&self, app_id: &str) -> Option<(String, Vec<HostedTooth>)> {
        let apps = self.shared.apps.lock().ok()?;
        apps.get(app_id).map(|r| (r.title.clone(), r.teeth.clone()))
    }

    /// `true` si alguna app tiene dientes registrados (para saber si vale la pena
    /// mirar el foco).
    pub fn any_registered(&self) -> bool {
        self.shared
            .apps
            .lock()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    }

    /// Le manda `Activate{tooth}` a `app_id`. `true` si se escribió.
    pub fn activate(&self, app_id: &str, tooth: u32) -> bool {
        let Ok(mut apps) = self.shared.apps.lock() else {
            return false;
        };
        let Some(reg) = apps.get_mut(app_id) else {
            return false;
        };
        write_frame(&mut reg.write, &ShellMsg::Activate { tooth }).is_ok()
    }

    /// Contador de revisión: cambia cuando un alta/baja/update tocó el mapa.
    pub fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Relaxed)
    }
}

/// Atiende una conexión: lee `AppMsg`s y mantiene el mapa. Al cerrarse el stream
/// (EOF) o recibir `Bye`, da de baja la app.
fn handle_conn(stream: UnixStream, shared: Arc<Shared>) {
    // La mitad de escritura (clonada) viaja al mapa para mandar `Activate`; la de
    // lectura se queda en este hilo.
    let write = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let mut read = stream;
    let mut my_app_id: Option<String> = None;

    loop {
        match read_frame::<AppMsg>(&mut read) {
            Ok(AppMsg::Register {
                app_id,
                title,
                teeth,
            }) => {
                let write = match write.try_clone() {
                    Ok(w) => w,
                    Err(_) => return,
                };
                if let Ok(mut apps) = shared.apps.lock() {
                    apps.insert(app_id.clone(), AppReg { title, teeth, write });
                }
                my_app_id = Some(app_id);
                bump(&shared);
            }
            Ok(AppMsg::Update { teeth }) => {
                if let Some(id) = &my_app_id {
                    if let Ok(mut apps) = shared.apps.lock() {
                        if let Some(reg) = apps.get_mut(id) {
                            reg.teeth = teeth;
                        }
                    }
                    bump(&shared);
                }
            }
            Ok(AppMsg::Bye) | Err(_) => {
                // Bye explícito o EOF/error: damos de baja y salimos.
                if let Some(id) = &my_app_id {
                    if let Ok(mut apps) = shared.apps.lock() {
                        apps.remove(id);
                    }
                    bump(&shared);
                }
                return;
            }
        }
    }
}

fn bump(shared: &Arc<Shared>) {
    shared.revision.fetch_add(1, Ordering::Relaxed);
}
