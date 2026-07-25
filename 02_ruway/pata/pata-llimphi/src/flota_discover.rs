//! Discover **remoto read-only** de la flota: por cada host del inventario,
//! conecta por SSH (matilda-linker) y observa su estado real (`docker ps` +
//! `ls` de nginx), para el «drift» del panel Flota. Hilo + tokio; **sólo
//! comandos de lectura** — nunca aplica cambios (eso es del CLI matilda).
//! Auth: la clave default del usuario (`~/.ssh/id_ed25519` → `id_rsa`). Inerte
//! si no hay hosts, clave o conexión (marca el host como «no alcanzable»).

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use matilda_discover::{
    parse_docker_ps, parse_nginx_sites, parse_service_states, remote_service_probe_command,
    ContainerStatus, ObservedService, DOCKER_PS_FORMAT,
};
use matilda_linker::{Linker, SshAuth, SshConfig};

/// Cada cuánto se re-descubre cada host (la conexión SSH es cara).
const REFRESH: Duration = Duration::from_secs(30);

/// Datos de conexión de un host, extraídos del inventario de matilda.
#[derive(Clone)]
pub struct HostConn {
    pub name: String,
    pub address: String,
    pub user: String,
    pub port: u16,
}

/// El estado real observado de un host (o que no se pudo alcanzar).
pub struct HostObs {
    pub name: String,
    pub reachable: bool,
    pub containers: Vec<ContainerStatus>,
    pub vhosts: Vec<String>,
    /// Servicios systemd declarados, con su estado real (enabled/active).
    pub services: Vec<ObservedService>,
}

/// Feed de discover remoto en su propio hilo. `latest()` drena la última tanda.
pub struct FlotaDiscoverHandle {
    rx: Receiver<Vec<HostObs>>,
}

impl FlotaDiscoverHandle {
    /// `hosts` = a quién conectar; `service_units` = los servicios systemd
    /// declarados en el inventario, que se sondean en cada host (estado real).
    pub fn spawn(hosts: Vec<HostConn>, service_units: Vec<String>) -> Self {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("pata-flota-discover".into())
            .spawn(move || {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                let key = default_key_path();
                let probe = {
                    let refs: Vec<&str> = service_units.iter().map(|s| s.as_str()).collect();
                    remote_service_probe_command(&refs)
                };
                loop {
                    let mut out = Vec::with_capacity(hosts.len());
                    for h in &hosts {
                        let cfg = SshConfig {
                            host: h.address.clone(),
                            port: h.port,
                            user: h.user.clone(),
                            auth: SshAuth::Key { path: key.clone(), passphrase: None },
                            keepalive_secs: 15,
                        };
                        let probe = probe.clone();
                        let obs = rt.block_on(async {
                            let linker = Linker::connect(&cfg).await.ok()?;
                            let ps = linker
                                .exec(&format!("docker ps -a --format '{DOCKER_PS_FORMAT}'"))
                                .await
                                .ok();
                            let containers = ps.map(|t| parse_docker_ps(&t)).unwrap_or_default();
                            let vh = linker
                                .exec("ls -1 /etc/nginx/sites-enabled 2>/dev/null")
                                .await
                                .ok();
                            let vhosts = vh.map(|t| parse_nginx_sites(&t)).unwrap_or_default();
                            // Servicios systemd (sólo si el inventario declara alguno).
                            let services = if probe.is_empty() {
                                Vec::new()
                            } else {
                                linker
                                    .exec(&probe)
                                    .await
                                    .ok()
                                    .map(|t| parse_service_states(&t))
                                    .unwrap_or_default()
                            };
                            Some((containers, vhosts, services))
                        });
                        out.push(match obs {
                            Some((containers, vhosts, services)) => HostObs {
                                name: h.name.clone(),
                                reachable: true,
                                containers,
                                vhosts,
                                services,
                            },
                            None => HostObs {
                                name: h.name.clone(),
                                reachable: false,
                                containers: Vec::new(),
                                vhosts: Vec::new(),
                                services: Vec::new(),
                            },
                        });
                    }
                    if tx.send(out).is_err() {
                        break; // la app se fue
                    }
                    std::thread::sleep(REFRESH);
                }
            })
            .ok();
        Self { rx }
    }

    pub fn latest(&self) -> Option<Vec<HostObs>> {
        let mut last = None;
        while let Ok(v) = self.rx.try_recv() {
            last = Some(v);
        }
        last
    }
}

/// La clave SSH default del usuario: `~/.ssh/id_ed25519`, o `id_rsa` si no está.
fn default_key_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let ed = home.join(".ssh/id_ed25519");
    if ed.exists() {
        ed
    } else {
        home.join(".ssh/id_rsa")
    }
}
