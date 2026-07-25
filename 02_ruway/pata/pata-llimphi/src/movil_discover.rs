//! Discover de presencia de los **equipos móviles** del tejido: por cada cuenta
//! móvil marcada «automática», pata pregunta al censo del tejido (`tejido flota
//! --json`) si está en línea AHORA y lo muestra como si fuera un equipo local —el
//! par móvil del [`flota_discover`](crate::flota_discover) para servidores SSH—.
//!
//! A diferencia de la flota SSH (docker/servicios, cuyo caído ES alarma), que un
//! equipo del tejido esté offline es NORMAL (tu teléfono apagado): es un **censo**,
//! no una alarma. Best-effort: si no hay roster o el binario `tejido` no está en
//! el PATH, queda inerte (todos offline / sin datos).

use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// Cada cuánto se re-sondea el censo del tejido (levantar el nodo card-net cuesta).
const REFRESH: Duration = Duration::from_secs(30);

/// Una cuenta móvil que pata vigila (las marcadas «automática»).
#[derive(Clone)]
pub struct MovilConn {
    pub id: String,
    pub label: String,
    /// Pubkey por-device (hex) que la liga a un equipo del roster; vacío = sin parear.
    pub device_hex: String,
}

/// El estado observado de un equipo móvil en el censo.
pub struct MovilObs {
    pub id: String,
    pub label: String,
    /// `true` si el equipo respondió el censo (está en línea).
    pub online: bool,
    /// Nombre reportado por el equipo (su hostname), si lo dio.
    pub nombre: Option<String>,
    /// `true` si la cuenta no tiene `device_hex` (no se pareó todavía).
    pub sin_parear: bool,
}

/// Una entrada del censo `tejido flota --json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CensoEntry {
    pub device: String,
    pub nombre: Option<String>,
    pub online: bool,
}

/// Feed de censo móvil en su propio hilo. `latest()` drena la última tanda.
pub struct MovilDiscoverHandle {
    rx: Receiver<Vec<MovilObs>>,
}

impl MovilDiscoverHandle {
    /// `conns` = las cuentas móviles automáticas a vigilar. Inerte si está vacío
    /// (no arranca el hilo).
    pub fn spawn(conns: Vec<MovilConn>) -> Option<Self> {
        if conns.is_empty() {
            return None;
        }
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("pata-movil-discover".into())
            .spawn(move || loop {
                let censo = run_censo();
                let obs = mapear(&conns, &censo);
                if tx.send(obs).is_err() {
                    break; // el panel se cerró
                }
                std::thread::sleep(REFRESH);
            })
            .ok()?;
        Some(Self { rx })
    }

    /// Drena el canal y devuelve la última tanda de observaciones (o `None` si no
    /// hubo ninguna nueva desde el último `latest`).
    pub fn latest(&self) -> Option<Vec<MovilObs>> {
        let mut last = None;
        while let Ok(v) = self.rx.try_recv() {
            last = Some(v);
        }
        last
    }
}

/// Corre `tejido flota --json` y parsea su salida. Vec vacío si el binario no
/// está, no hay roster, o la salida no parsea (best-effort, nunca panica).
fn run_censo() -> Vec<CensoEntry> {
    match std::process::Command::new("tejido").args(["flota", "--json"]).output() {
        Ok(o) if o.status.success() => parse_censo_json(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

/// Parsea la salida JSON de `tejido flota --json` (un array de objetos
/// `{device, nombre, online, ...}`) a [`CensoEntry`]s. Tolerante: ignora entradas
/// mal formadas y devuelve vacío ante un JSON inválido.
pub fn parse_censo_json(s: &str) -> Vec<CensoEntry> {
    let val: serde_json::Value = match serde_json::from_str(s.trim()) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = val.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|e| {
            let device = e.get("device")?.as_str()?.to_string();
            let online = e.get("online").and_then(|v| v.as_bool()).unwrap_or(false);
            let nombre = e.get("nombre").and_then(|v| v.as_str()).map(str::to_string);
            Some(CensoEntry { device, nombre, online })
        })
        .collect()
}

/// Mapea cada cuenta móvil al censo por `device_hex` (case-insensitive). Una
/// cuenta sin `device_hex` queda «sin parear»; una que no aparece en el censo,
/// offline.
pub fn mapear(conns: &[MovilConn], censo: &[CensoEntry]) -> Vec<MovilObs> {
    conns
        .iter()
        .map(|c| {
            let dev = c.device_hex.trim();
            if dev.is_empty() {
                return MovilObs {
                    id: c.id.clone(),
                    label: c.label.clone(),
                    online: false,
                    nombre: None,
                    sin_parear: true,
                };
            }
            let hit = censo.iter().find(|e| e.device.eq_ignore_ascii_case(dev));
            MovilObs {
                id: c.id.clone(),
                label: c.label.clone(),
                online: hit.map(|e| e.online).unwrap_or(false),
                nombre: hit.and_then(|e| e.nombre.clone()),
                sin_parear: false,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsea_censo_json_valido() {
        let s = r#"[
            {"device":"aa01","nombre":"telefono","online":true,"soy_yo":false},
            {"device":"bb02","nombre":null,"online":false,"soy_yo":false}
        ]"#;
        let c = parse_censo_json(s);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0], CensoEntry { device: "aa01".into(), nombre: Some("telefono".into()), online: true });
        assert_eq!(c[1], CensoEntry { device: "bb02".into(), nombre: None, online: false });
    }

    #[test]
    fn json_invalido_o_vacio_no_panica() {
        assert!(parse_censo_json("").is_empty());
        assert!(parse_censo_json("no soy json").is_empty());
        assert!(parse_censo_json("{}").is_empty(), "un objeto no-array = vacío");
        assert!(parse_censo_json("[]").is_empty());
    }

    #[test]
    fn mapea_cuentas_por_device_hex() {
        let conns = vec![
            MovilConn { id: "tel".into(), label: "Teléfono".into(), device_hex: "AA01".into() },
            MovilConn { id: "tab".into(), label: "Tablet".into(), device_hex: "cc99".into() },
            MovilConn { id: "sinp".into(), label: "Nuevo".into(), device_hex: "".into() },
        ];
        let censo = vec![
            CensoEntry { device: "aa01".into(), nombre: Some("mi-tel".into()), online: true },
            // cc99 no aparece → offline pero pareado.
        ];
        let obs = mapear(&conns, &censo);
        assert_eq!(obs.len(), 3);
        // Teléfono: matchea (case-insensitive) → online con nombre.
        assert!(obs[0].online && !obs[0].sin_parear);
        assert_eq!(obs[0].nombre.as_deref(), Some("mi-tel"));
        // Tablet: pareada pero ausente del censo → offline.
        assert!(!obs[1].online && !obs[1].sin_parear);
        // Nuevo: sin device_hex → sin parear.
        assert!(obs[2].sin_parear && !obs[2].online);
    }
}
