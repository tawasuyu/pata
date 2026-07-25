//! Estado de Bluetooth para el widget `bluetooth` (gemelo del applet de red).
//!
//! Como la red o el clima, es **dato del host** en su **propio hilo**: sondea el
//! controlador y los dispositivos emparejados y publica la foto por un canal. La
//! fuente es `bluetoothctl` (BlueZ) en modo no interactivo, sin sumar un cliente
//! D-Bus al árbol — mismo patrón defensivo que `network` con `nmcli`. Si no está,
//! el widget queda en `available=false` (icono tenue) sin romper la barra.
//!
//! Alcance: enciende/apaga el controlador, **escanea** dispositivos nuevos
//! ([`scan`]), **empareja** uno nuevo ([`pair`]: pair→trust→connect) y conecta/
//! desconecta emparejados. Lo único que resta para paridad total es el **agente
//! de PIN**: los dispositivos que piden passkey/confirmación necesitan un agente
//! BlueZ que muestre el PIN (metal + un diálogo); los «just works» (audífonos,
//! ratones) emparejan sin él.

use std::collections::HashSet;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// Un dispositivo Bluetooth conocido, para el popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtDevice {
    /// La dirección MAC (clave para conectar/desconectar/emparejar).
    pub mac: String,
    /// Nombre legible.
    pub name: String,
    /// `true` si está conectado ahora.
    pub connected: bool,
    /// `true` si está emparejado (si no, es un descubrimiento del scan → «Emparejar»).
    pub paired: bool,
    /// Batería del dispositivo `0..=100`, si BlueZ la reporta (auriculares, mouse…).
    pub battery: Option<u8>,
}

/// La foto del Bluetooth que el hilo publica.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BtState {
    /// `true` si `bluetoothctl` respondió (hay controlador/BlueZ).
    pub available: bool,
    /// `true` si el controlador está encendido.
    pub powered: bool,
    /// Dispositivos emparejados, los conectados primero.
    pub devices: Vec<BtDevice>,
}

// ============================================================
// Parsers puros (testeables sin BlueZ)
// ============================================================

/// `true` si `bluetoothctl show` reporta `Powered: yes`.
pub fn parse_powered(out: &str) -> bool {
    out.lines().any(|l| {
        let l = l.trim();
        l.starts_with("Powered:") && l.ends_with("yes")
    })
}

/// Parsea `bluetoothctl devices [Paired]` → `(mac, nombre)` por línea
/// `Device <MAC> <Nombre>`. Descarta líneas que no calzan.
pub fn parse_devices(out: &str) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for l in out.lines() {
        let l = l.trim();
        let Some(rest) = l.strip_prefix("Device ") else {
            continue;
        };
        let mut it = rest.splitn(2, ' ');
        let Some(mac) = it.next() else { continue };
        if mac.is_empty() {
            continue;
        }
        let name = it.next().unwrap_or(mac).trim().to_string();
        v.push((mac.to_string(), name));
    }
    v
}

/// El conjunto de MAC conectadas, de `bluetoothctl devices Connected`.
pub fn parse_connected(out: &str) -> HashSet<String> {
    parse_devices(out).into_iter().map(|(mac, _)| mac).collect()
}

/// Extrae la batería de `bluetoothctl info <mac>`: la línea
/// `Battery Percentage: 0x64 (100)`. `None` si no la reporta.
pub fn parse_battery(out: &str) -> Option<u8> {
    for l in out.lines() {
        let l = l.trim();
        if let Some(rest) = l.strip_prefix("Battery Percentage:") {
            // El valor decimal va entre paréntesis: `0x64 (100)`.
            if let Some(dentro) = rest.split('(').nth(1).and_then(|s| s.split(')').next()) {
                if let Ok(pct) = dentro.trim().parse::<u8>() {
                    return Some(pct.min(100));
                }
            }
        }
    }
    None
}

/// Arma la lista ordenada a partir de TODOS los dispositivos conocidos, marcando
/// emparejado/conectado y adjuntando la batería. Orden: conectados, luego
/// emparejados, luego descubrimientos (sin emparejar); por nombre dentro de cada
/// grupo. `battery` mapea MAC→porcentaje (sólo para los que la reportan).
pub fn build_devices(
    all: Vec<(String, String)>,
    paired: &HashSet<String>,
    connected: &HashSet<String>,
    battery: &std::collections::HashMap<String, u8>,
) -> Vec<BtDevice> {
    let mut devs: Vec<BtDevice> = all
        .into_iter()
        .map(|(mac, name)| BtDevice {
            connected: connected.contains(&mac),
            paired: paired.contains(&mac),
            battery: battery.get(&mac).copied(),
            mac,
            name,
        })
        .collect();
    devs.sort_by(|a, b| {
        let rank = |d: &BtDevice| if d.connected { 0 } else if d.paired { 1 } else { 2 };
        rank(a)
            .cmp(&rank(b))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    devs
}

// ============================================================
// Acciones (fire-and-forget)
// ============================================================

/// Conecta el dispositivo `mac` (`bluetoothctl connect`). No bloquea.
pub fn connect(mac: &str) {
    spawn(&["connect", mac]);
}

/// Desconecta el dispositivo `mac` (`bluetoothctl disconnect`). No bloquea.
pub fn disconnect(mac: &str) {
    spawn(&["disconnect", mac]);
}

/// Enciende/apaga el controlador (`bluetoothctl power on|off`). No bloquea.
pub fn set_power(on: bool) {
    spawn(&["power", if on { "on" } else { "off" }]);
}

/// Lanza un **scan** acotado (12 s) para descubrir dispositivos nuevos: los que
/// aparezcan quedan en la lista conocida de BlueZ y salen en el próximo muestreo.
/// No bloquea (el `--timeout` corta solo).
pub fn scan() {
    crate::desacoplar(std::process::Command::new("bluetoothctl")
        .args(["--timeout", "12", "scan", "on"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn());
}

/// **Empareja** un dispositivo nuevo: pair → trust → connect, en secuencia (los
/// que piden PIN/confirmación siguen necesitando el agente del sistema). No
/// bloquea el bucle de UI; corre en una shell aparte.
pub fn pair(mac: &str) {
    // MAC de BlueZ: hex y `:`, sin metacaracteres de shell.
    crate::spawn_cmd(&format!(
        "bluetoothctl pair {mac}; bluetoothctl trust {mac}; bluetoothctl connect {mac}"
    ));
}

fn spawn(args: &[&str]) {
    crate::desacoplar(std::process::Command::new("bluetoothctl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn());
}

// ============================================================
// Muestreo en el hilo
// ============================================================

/// Corre `bluetoothctl <args>` con tope de tiempo; `None` si no está o falla.
fn run(args: &[&str]) -> Option<String> {
    use std::io::Read;
    use std::time::Instant;
    const PLAZO: Duration = Duration::from_secs(5);
    let mut child = std::process::Command::new("bluetoothctl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let inicio = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut buf = String::new();
                child.stdout.take()?.read_to_string(&mut buf).ok()?;
                return Some(buf);
            }
            Ok(None) => {
                if inicio.elapsed() >= PLAZO {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

/// Una lectura completa. `None` si bluetoothctl no responde.
fn sample() -> Option<BtState> {
    let show = run(&["show"])?;
    let powered = parse_powered(&show);
    // TODOS los conocidos (emparejados + descubiertos por el scan), + los sets de
    // emparejados y conectados para clasificar.
    let all = parse_devices(&run(&["devices"]).unwrap_or_default());
    let paired: HashSet<String> = parse_devices(&run(&["devices", "Paired"]).unwrap_or_default())
        .into_iter()
        .map(|(m, _)| m)
        .collect();
    let connected = parse_connected(&run(&["devices", "Connected"]).unwrap_or_default());
    // Batería sólo de los conectados (leer `info` de cada uno es caro).
    let mut battery = std::collections::HashMap::new();
    for mac in &connected {
        if let Some(info) = run(&["info", mac]) {
            if let Some(pct) = parse_battery(&info) {
                battery.insert(mac.clone(), pct);
            }
        }
    }
    // Si `devices` sin filtro no lista nada (BlueZ viejo), cae a los emparejados.
    let all = if all.is_empty() {
        parse_devices(&run(&["devices", "Paired"]).unwrap_or_default())
    } else {
        all
    };
    Some(BtState {
        available: true,
        powered,
        devices: build_devices(all, &paired, &connected, &battery),
    })
}

/// El feed de Bluetooth corriendo en su propio hilo.
pub struct BluetoothHandle {
    rx: Receiver<BtState>,
}

impl BluetoothHandle {
    /// Arranca el hilo. Refresca cada ~5 s (15 s si bluetoothctl no responde).
    pub fn spawn() -> Self {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("pata-bluetooth".into())
            .spawn(move || loop {
                let (estado, espera) = match sample() {
                    Some(s) => (s, Duration::from_secs(5)),
                    None => (BtState::default(), Duration::from_secs(15)),
                };
                if tx.send(estado).is_err() {
                    break;
                }
                std::thread::sleep(espera);
            })
            .ok();
        Self { rx }
    }

    /// La lectura más reciente (drena la cola), o `None` si no llegó nada nuevo.
    pub fn latest(&self) -> Option<BtState> {
        let mut last = None;
        while let Ok(s) = self.rx.try_recv() {
            last = Some(s);
        }
        last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powered_de_show() {
        assert!(parse_powered("Controller AA\n\tPowered: yes\n\tDiscoverable: no"));
        assert!(!parse_powered("\tPowered: no"));
        assert!(!parse_powered("sin nada"));
    }

    #[test]
    fn devices_conectados_emparejados_y_descubiertos() {
        // Sony (emparejado, no conectado), Magic Mouse (conectado), Teclado nuevo
        // (descubierto, no emparejado).
        let all = parse_devices(
            "Device AA:BB:CC:DD:EE:FF Sony WH-1000XM4\n\
             Device 11:22:33:44:55:66 Magic Mouse\n\
             Device 99:88:77:66:55:44 Teclado nuevo",
        );
        assert_eq!(all.len(), 3);
        let paired: HashSet<String> =
            ["AA:BB:CC:DD:EE:FF".to_string(), "11:22:33:44:55:66".to_string()].into_iter().collect();
        let connected = parse_connected("Device 11:22:33:44:55:66 Magic Mouse");
        let mut battery = std::collections::HashMap::new();
        battery.insert("11:22:33:44:55:66".to_string(), 85u8);
        let built = build_devices(all, &paired, &connected, &battery);
        // Orden: conectado, emparejado, descubierto.
        assert_eq!(built[0].name, "Magic Mouse");
        assert!(built[0].connected && built[0].paired);
        assert_eq!(built[0].battery, Some(85));
        assert_eq!(built[1].name, "Sony WH-1000XM4");
        assert!(built[1].paired && !built[1].connected);
        assert_eq!(built[2].name, "Teclado nuevo");
        assert!(!built[2].paired);
    }

    #[test]
    fn parsea_bateria() {
        assert_eq!(parse_battery("\tBattery Percentage: 0x64 (100)"), Some(100));
        assert_eq!(parse_battery("Battery Percentage: 0x2a (42)"), Some(42));
        assert_eq!(parse_battery("sin batería"), None);
    }
}
