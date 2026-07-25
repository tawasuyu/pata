//! Salud de la flota (matilda) para el escritorio: resume el estado runtime
//! —contenedores, servicios, vhosts— de la máquina **local** y de los hosts
//! **remotos** en un puñado de números y nombres. Alimenta tres superficies de
//! pata:
//!
//! - el **control fantasma** del cabezal de shuma ([`crate::shuma`]): aparece
//!   sólo cuando algo se cayó (contenedor parado / servicio fallado / host
//!   inalcanzable), igual que el fantasma de CPU o batería;
//! - la **marquesina** del input ([`crate::marquesina`]): narra el aviso al
//!   ciclar entre las fuentes de estado del escritorio;
//! - una fila del **centro de control** ([`crate::render::control`]).
//!
//! Read-only: nunca aplica cambios (eso es del CLI matilda). El muestreo local
//! corre en su propio hilo a cadencia lenta —`docker ps`/`systemctl` son
//! subprocesos—; lo remoto reutiliza el discover SSH que ya vive en
//! [`crate::flota_discover`].

use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use matilda_discover::{discover_runtime, RuntimeState, ServiceState};

use crate::flota_discover::HostObs;

/// Cadencia del muestreo runtime local. `docker ps` + `systemctl` son
/// subprocesos: no vale la pena correrlos a 1 Hz como el sampler de sistema.
const REFRESH: Duration = Duration::from_secs(6);

/// Cuántos nombres caídos se enumeran en el aviso antes de resumir con un conteo.
const MAX_NOMBRES: usize = 3;

/// Muestreo runtime **local** en su propio hilo. `latest()` drena la última foto.
/// Inerte (no se crea el hilo) si la máquina no tiene nada monitoreable —sin
/// docker/podman ni nginx—: un escritorio de a pie no paga un hilo ocioso.
pub struct MatildaLocalHandle {
    rx: Receiver<RuntimeState>,
}

impl MatildaLocalHandle {
    /// Arranca el muestreo si la máquina es monitoreable; si no, `None`.
    pub fn spawn() -> Option<Self> {
        if !local_monitoreable() {
            return None;
        }
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("pata-matilda-local".into())
            .spawn(move || loop {
                // `discover_runtime` es puro-lectura (docker ps -a + systemctl +
                // ls nginx); si docker no está, la lista queda vacía, no es error.
                if tx.send(discover_runtime()).is_err() {
                    break; // la app se fue
                }
                std::thread::sleep(REFRESH);
            })
            .ok()?;
        Some(Self { rx })
    }

    /// La última foto runtime (drena la cola quedándose con la más nueva).
    pub fn latest(&self) -> Option<RuntimeState> {
        let mut last = None;
        while let Ok(v) = self.rx.try_recv() {
            last = Some(v);
        }
        last
    }
}

/// `true` si vale la pena muestrear la máquina local: hay `docker`/`podman` en el
/// `PATH` o existe el árbol de sitios de nginx. Sin nada de eso,
/// `discover_runtime` devolvería listas vacías en cada tick — puro gasto.
fn local_monitoreable() -> bool {
    binario_en_path("docker")
        || binario_en_path("podman")
        || std::path::Path::new("/etc/nginx/sites-enabled").is_dir()
}

/// Busca un ejecutable en el `PATH` (sin depender de `which`).
fn binario_en_path(bin: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(bin).is_file())
}

/// Resumen de salud de un nodo (la máquina local o un host remoto).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaludNodo {
    /// Nombre del nodo: `local` o el nombre del host del inventario.
    pub nombre: String,
    /// `true` si el nodo respondió (local siempre; remoto = SSH alcanzable).
    pub alcanzable: bool,
    /// Contenedores vivos.
    pub up: usize,
    /// Contenedores parados/muertos.
    pub down: usize,
    /// Servicios caídos (fallados en local; declarados inactivos en remoto).
    pub svc_caidos: usize,
    /// Nombres de lo caído (contenedores + servicios), para el aviso/tooltip.
    pub caidos: Vec<String>,
}

impl SaludNodo {
    /// `true` si el nodo tiene algún problema: inalcanzable, contenedor caído o
    /// servicio caído.
    pub fn hay_problema(&self) -> bool {
        !self.alcanzable || self.down > 0 || self.svc_caidos > 0
    }
}

/// Salud combinada de la flota: la máquina local + los hosts remotos observados.
/// Pura: se recomputa cada tick a partir del último runtime local
/// ([`MatildaLocalHandle::latest`]) y del discover remoto
/// ([`crate::flota_discover`]). `None` si no hay nada que monitorear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaludFlota {
    pub nodos: Vec<SaludNodo>,
}

impl SaludFlota {
    /// Combina la foto local y la remota en un resumen por nodo. `None` si no hay
    /// ningún nodo con datos (ni contenedores/servicios locales ni hosts remotos).
    pub fn compute(local: Option<&RuntimeState>, remoto: Option<&[HostObs]>) -> Option<Self> {
        let mut nodos = Vec::new();

        // Nodo local: sólo si el runtime trajo algo (docker/systemd presentes).
        if let Some(rt) = local {
            if !rt.containers.is_empty() || !rt.services.is_empty() {
                let mut caidos: Vec<String> = rt
                    .containers
                    .iter()
                    .filter(|c| !c.state.is_up())
                    .map(|c| c.name.clone())
                    .collect();
                // Servicios locales: `discover_services` sólo lista running+failed,
                // así que un fallado es un problema real (no un inactive cualquiera).
                let fallados: Vec<&str> = rt
                    .services
                    .iter()
                    .filter(|s| s.state == ServiceState::Failed)
                    .map(|s| s.name.as_str())
                    .collect();
                let svc_caidos = fallados.len();
                caidos.extend(fallados.iter().map(|s| s.to_string()));
                nodos.push(SaludNodo {
                    nombre: "local".into(),
                    alcanzable: true,
                    up: rt.up_count(),
                    down: rt.down_count(),
                    svc_caidos,
                    caidos,
                });
            }
        }

        // Nodos remotos: uno por host observado por SSH.
        if let Some(hosts) = remoto {
            for h in hosts {
                if !h.reachable {
                    nodos.push(SaludNodo {
                        nombre: h.name.clone(),
                        alcanzable: false,
                        up: 0,
                        down: 0,
                        svc_caidos: 0,
                        caidos: Vec::new(),
                    });
                    continue;
                }
                let up = h.containers.iter().filter(|c| c.state.is_up()).count();
                let down = h.containers.len() - up;
                let mut caidos: Vec<String> = h
                    .containers
                    .iter()
                    .filter(|c| !c.state.is_up())
                    .map(|c| c.name.clone())
                    .collect();
                // Servicios declarados observados como inactivos.
                let inactivos: Vec<&str> =
                    h.services.iter().filter(|s| !s.active).map(|s| s.unit.as_str()).collect();
                let svc_caidos = inactivos.len();
                caidos.extend(inactivos.iter().map(|s| s.to_string()));
                nodos.push(SaludNodo {
                    nombre: h.name.clone(),
                    alcanzable: true,
                    up,
                    down,
                    svc_caidos,
                    caidos,
                });
            }
        }

        if nodos.is_empty() {
            None
        } else {
            Some(Self { nodos })
        }
    }

    /// Contenedores vivos en todos los nodos.
    pub fn total_up(&self) -> usize {
        self.nodos.iter().map(|n| n.up).sum()
    }

    /// Contenedores caídos en todos los nodos.
    pub fn total_down(&self) -> usize {
        self.nodos.iter().map(|n| n.down).sum()
    }

    /// Servicios caídos en todos los nodos.
    pub fn svc_caidos(&self) -> usize {
        self.nodos.iter().map(|n| n.svc_caidos).sum()
    }

    /// Hosts remotos que no respondieron por SSH.
    pub fn inalcanzables(&self) -> usize {
        self.nodos.iter().filter(|n| !n.alcanzable).count()
    }

    /// `true` si algún nodo tiene un problema (el gate del control fantasma).
    pub fn hay_problema(&self) -> bool {
        self.nodos.iter().any(|n| n.hay_problema())
    }

    /// Severidad para el color del fantasma: `0` sano, `1` aviso (servicio
    /// caído), `2` grave (contenedor caído u host inalcanzable).
    pub fn severidad(&self) -> u8 {
        if self.inalcanzables() > 0 || self.total_down() > 0 {
            2
        } else if self.svc_caidos() > 0 {
            1
        } else {
            0
        }
    }

    /// Un aviso corto para la marquesina, o `None` si todo está sano. Nombra lo
    /// caído cuando son pocos; si son muchos, lo resume con un conteo. Los hosts
    /// inalcanzables (lo más grave) van primero.
    pub fn resumen(&self) -> Option<String> {
        if !self.hay_problema() {
            return None;
        }
        let inalcanzables: Vec<&str> = self
            .nodos
            .iter()
            .filter(|n| !n.alcanzable)
            .map(|n| n.nombre.as_str())
            .collect();
        // Nombres caídos, prefijando el host si es remoto (`web@srv1`).
        let mut nombres: Vec<String> = Vec::new();
        for n in self.nodos.iter().filter(|n| n.alcanzable) {
            for c in &n.caidos {
                if n.nombre == "local" {
                    nombres.push(c.clone());
                } else {
                    nombres.push(format!("{c}@{}", n.nombre));
                }
            }
        }

        let mut partes: Vec<String> = Vec::new();
        match inalcanzables.len() {
            0 => {}
            1 => partes.push(format!("{} inalcanzable", inalcanzables[0])),
            n => partes.push(format!("{n} hosts inalcanzables")),
        }
        let total = nombres.len();
        if total > 0 && total <= MAX_NOMBRES {
            partes.push(format!(
                "{} caído{}",
                nombres.join(", "),
                if total == 1 { "" } else { "s" }
            ));
        } else if total > MAX_NOMBRES {
            partes.push(format!("{total} caídos"));
        }

        if partes.is_empty() {
            None
        } else {
            Some(format!("flota: {}", partes.join(" · ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matilda_discover::{ContainerStatus, RunState, ServiceStatus};

    fn cont(name: &str, up: bool) -> ContainerStatus {
        ContainerStatus {
            name: name.into(),
            image: "img".into(),
            state: if up { RunState::Running } else { RunState::Exited },
            status: if up { "Up 2 hours".into() } else { "Exited (0)".into() },
            ports: String::new(),
        }
    }

    fn svc(name: &str, state: ServiceState) -> ServiceStatus {
        ServiceStatus { name: name.into(), state, sub: String::new(), description: String::new() }
    }

    #[test]
    fn sin_datos_es_none() {
        assert!(SaludFlota::compute(None, None).is_none());
        // Runtime local vacío (sin docker/systemd) → no aporta nodo.
        let vacio = RuntimeState::default();
        assert!(SaludFlota::compute(Some(&vacio), Some(&[])).is_none());
    }

    #[test]
    fn local_todo_sano_sin_aviso() {
        let rt = RuntimeState {
            containers: vec![cont("web", true), cont("db", true)],
            services: vec![svc("sshd.service", ServiceState::Active)],
            vhosts: vec![],
        };
        let s = SaludFlota::compute(Some(&rt), None).unwrap();
        assert_eq!(s.total_up(), 2);
        assert_eq!(s.total_down(), 0);
        assert!(!s.hay_problema());
        assert_eq!(s.severidad(), 0);
        assert!(s.resumen().is_none());
    }

    #[test]
    fn contenedor_caido_se_nombra() {
        let rt = RuntimeState {
            containers: vec![cont("web", true), cont("db", false)],
            services: vec![],
            vhosts: vec![],
        };
        let s = SaludFlota::compute(Some(&rt), None).unwrap();
        assert!(s.hay_problema());
        assert_eq!(s.severidad(), 2);
        assert_eq!(s.total_down(), 1);
        let r = s.resumen().unwrap();
        assert!(r.contains("db"), "esperaba nombrar el caído: {r}");
        assert!(r.contains("caído"));
    }

    #[test]
    fn servicio_fallado_es_aviso() {
        let rt = RuntimeState {
            containers: vec![cont("web", true)],
            services: vec![svc("nginx.service", ServiceState::Failed)],
            vhosts: vec![],
        };
        let s = SaludFlota::compute(Some(&rt), None).unwrap();
        assert_eq!(s.svc_caidos(), 1);
        assert_eq!(s.severidad(), 1); // servicio caído sin contenedor caído = aviso
        assert!(s.resumen().unwrap().contains("nginx.service"));
    }

    #[test]
    fn host_inalcanzable_manda() {
        let hosts = vec![HostObs {
            name: "srv2".into(),
            reachable: false,
            containers: vec![],
            vhosts: vec![],
            services: vec![],
        }];
        let s = SaludFlota::compute(None, Some(&hosts)).unwrap();
        assert_eq!(s.inalcanzables(), 1);
        assert!(s.hay_problema());
        let r = s.resumen().unwrap();
        assert!(r.contains("srv2") && r.contains("inalcanzable"), "{r}");
    }

    #[test]
    fn remoto_prefija_host_en_el_nombre() {
        let hosts = vec![HostObs {
            name: "srv1".into(),
            reachable: true,
            containers: vec![cont("api", false)],
            vhosts: vec![],
            services: vec![],
        }];
        let s = SaludFlota::compute(None, Some(&hosts)).unwrap();
        assert_eq!(s.total_down(), 1);
        assert!(s.resumen().unwrap().contains("api@srv1"));
    }

    #[test]
    fn muchos_caidos_se_resumen_por_conteo() {
        let containers: Vec<ContainerStatus> =
            (0..7).map(|i| cont(&format!("c{i}"), false)).collect();
        let rt = RuntimeState { containers, services: vec![], vhosts: vec![] };
        let s = SaludFlota::compute(Some(&rt), None).unwrap();
        let r = s.resumen().unwrap();
        assert!(r.contains("7 caídos"), "esperaba conteo, no nombres: {r}");
    }
}
