//! Estado de red para el widget `network` (el applet de Wi-Fi/Ethernet).
//!
//! Como el clima o el tray, es **dato del host** que el frontend muestrea aparte
//! del view-model de core: corre en su **propio hilo** (consultar al
//! NetworkManager puede tardar) y publica la última lectura por un canal. La
//! fuente es `nmcli` (NetworkManager), invocado en modo terse (`-t`), sin agregar
//! una dependencia de D-Bus al árbol — mismo patrón defensivo que `weather` con
//! `curl` o el sampler con `wpctl`. Si `nmcli` no está, el widget queda en
//! [`NetStatus::Sin`] (icono tenue) sin romper la barra.
//!
//! El render traduce el [`NetState`] a un **dibujo del nivel de señal** (barras
//! ascendentes) y el popup lista los SSID disponibles para conectarse.
//!
//! **Alcance**: enumera redes, conecta a una guardada/abierta, desconecta,
//! conmuta la radio y pide **contraseña** para una red segura nueva (campo con
//! foco de teclado en el popup → [`connect_with`]; ver `render/network.rs`
//! `password_rows` y `Msg::NetworkPassword*` en `lib.rs`). Como respaldo, una red
//! segura sin perfil con contraseña vacía cae al agente de secretos del sistema.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// La conexión activa, derivada de lo que reporta NetworkManager.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NetStatus {
    /// Cable conectado.
    Ethernet,
    /// Wi-Fi conectado a `ssid` con intensidad `signal` (0..=100).
    Wifi {
        /// El SSID de la red activa.
        ssid: String,
        /// Intensidad de señal 0..=100.
        signal: u8,
    },
    /// Radio Wi-Fi apagada (rfkill / `nmcli radio wifi off`).
    WifiOff,
    /// Sin conexión (radio encendida pero sin asociar, y sin cable).
    #[default]
    Desconectado,
    /// No hay NetworkManager (nmcli ausente o sin responder).
    Sin,
}

/// Un punto de acceso Wi-Fi visible, para el popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiAp {
    /// El nombre de la red.
    pub ssid: String,
    /// Intensidad de señal 0..=100.
    pub signal: u8,
    /// `true` si la red pide credenciales (no es abierta).
    pub secure: bool,
    /// `true` si es la red a la que estamos conectados.
    pub active: bool,
}

/// El tipo de una conexión guardada (perfil de NetworkManager).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnKind {
    /// Red Wi-Fi guardada.
    Wifi,
    /// VPN (OpenVPN/WireGuard/…): togglear con un botón.
    Vpn,
    /// Conexión cableada.
    Ethernet,
    /// Otro (bridge, tun, bond…).
    Otro,
}

/// El **alcance** de un túnel: qué tráfico entra en él. Se deriva de la tabla
/// de rutas del sistema (`/proc/net/route`, legible sin privilegio) para el
/// dispositivo del túnel activo — no de sus `.conf` (root-only). Contesta la
/// pregunta que importa al prender una VPN: «¿me agarra toda la máquina o sólo
/// un pedazo?».
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelScope {
    /// El túnel captura **todo** el tráfico (ruta por defecto `0.0.0.0/0`, o el
    /// par `/1` que usa OpenVPN para pisar el default sin borrarlo).
    Full,
    /// Sólo estas subredes (CIDR) entran al túnel; el resto sale directo.
    Split(Vec<String>),
}

impl TunnelScope {
    /// Etiqueta corta para la insignia del applet.
    pub fn label(&self) -> String {
        match self {
            TunnelScope::Full => "Todo el tráfico".to_string(),
            TunnelScope::Split(cidrs) => match cidrs.len() {
                0 => "Sin rutas".to_string(),
                1 => format!("Sólo {}", cidrs[0]),
                n => format!("Sólo {n} redes"),
            },
        }
    }
}

/// Un perfil de conexión **guardado** en NetworkManager. Deja conectar sin
/// re-escanear, togglear una VPN, y olvidar (borrar) el perfil.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedConn {
    /// Nombre del perfil (lo usan `connection up/down/delete id <name>`).
    pub name: String,
    /// Su tipo (para el ícono y para saber si es una VPN).
    pub kind: ConnKind,
    /// `true` si está activa ahora.
    pub active: bool,
    /// Dispositivo de red asociado si está activa (`wg0`, `tun0`, …). Lo usa el
    /// sampler para cruzar con la tabla de rutas y derivar el alcance.
    pub device: Option<String>,
    /// Alcance del túnel, si es una VPN activa cuyo alcance se pudo derivar.
    /// `None` para redes normales o túneles inactivos (su `.conf` es root-only).
    pub scope: Option<TunnelScope>,
    /// UUID del perfil (para resolver `secondaries`, que se guardan por UUID).
    pub uuid: Option<String>,
    /// Nombres de las conexiones que esta red **levanta automáticamente** al
    /// conectar (NM `connection.secondaries`) — el disparador *detectado* de la
    /// saga túnel: «al llegar a la oficina, sube la VPN de la oficina». Vacío si
    /// no declara ninguna. Lo llena el sampler sólo para la red activa.
    pub raises: Vec<String>,
}

/// Detalles de la conexión activa (para mostrar «bajo el capó»).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetDetails {
    /// Dirección IPv4 con prefijo (`192.168.1.5/24`), si hay.
    pub ip: Option<String>,
    /// Gateway por defecto.
    pub gateway: Option<String>,
    /// Primer servidor DNS.
    pub dns: Option<String>,
}

/// La foto de la red que el hilo publica: estado actual + radio + lista de redes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetState {
    /// La conexión activa.
    pub status: NetStatus,
    /// `true` si la radio Wi-Fi está habilitada.
    pub wifi_enabled: bool,
    /// Redes Wi-Fi visibles, la activa primero, luego por señal descendente.
    pub networks: Vec<WifiAp>,
    /// Perfiles guardados (Wi-Fi/VPN/ethernet) — VPNs y redes conocidas.
    pub saved: Vec<SavedConn>,
    /// Detalles de la conexión activa (IP/gateway/DNS).
    pub details: NetDetails,
}

// ============================================================
// Parsers puros (testeables sin red)
// ============================================================

/// Parte una línea terse de `nmcli -t` respetando el escape `\:` (un SSID puede
/// contener `:`). NetworkManager escapa `:` y `\` como `\:` y `\\`.
fn split_terse(line: &str) -> Vec<String> {
    let mut campos = Vec::new();
    let mut actual = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                // El siguiente carácter es literal (`\:` → `:`, `\\` → `\`).
                if let Some(n) = chars.next() {
                    actual.push(n);
                }
            }
            ':' => {
                campos.push(std::mem::take(&mut actual));
            }
            _ => actual.push(c),
        }
    }
    campos.push(actual);
    campos
}

/// Parsea la salida de `nmcli -t -f ACTIVE,SSID,SIGNAL,SECURITY device wifi`.
/// Deduplica por SSID quedándose con la entrada de mayor señal (o la activa);
/// descarta SSID vacíos (redes ocultas). Ordena activa primero, luego por señal.
pub fn parse_wifi_list(out: &str) -> Vec<WifiAp> {
    let mut aps: Vec<WifiAp> = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let campos = split_terse(line);
        if campos.len() < 4 {
            continue;
        }
        let active = campos[0].trim().eq_ignore_ascii_case("yes");
        let ssid = campos[1].trim().to_string();
        if ssid.is_empty() {
            continue;
        }
        let signal: u8 = campos[2].trim().parse().unwrap_or(0).min(100);
        // SECURITY vacío (o "--") = red abierta.
        let sec = campos[3].trim();
        let secure = !sec.is_empty() && sec != "--";
        // Dedup: si ya está el SSID, conservamos el mejor (activo o más fuerte).
        if let Some(prev) = aps.iter_mut().find(|a| a.ssid == ssid) {
            if active || signal > prev.signal {
                prev.signal = signal.max(prev.signal);
                prev.active = prev.active || active;
                prev.secure = secure;
            }
            continue;
        }
        aps.push(WifiAp {
            ssid,
            signal,
            secure,
            active,
        });
    }
    aps.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then(b.signal.cmp(&a.signal))
            .then(a.ssid.cmp(&b.ssid))
    });
    aps
}

/// Parsea `nmcli -t radio wifi` → `true` si dice `enabled`.
pub fn parse_radio(out: &str) -> bool {
    out.trim().eq_ignore_ascii_case("enabled")
}

/// Parsea `nmcli -t -f TYPE,STATE device status` → `true` si hay un `ethernet`
/// en estado `connected`.
pub fn parse_ethernet_connected(out: &str) -> bool {
    out.lines().any(|l| {
        let campos = split_terse(l.trim());
        campos.len() >= 2
            && campos[0].trim().eq_ignore_ascii_case("ethernet")
            && campos[1].trim().starts_with("connected")
    })
}

/// Mapea el TYPE de nmcli a un [`ConnKind`].
fn conn_kind(tipo: &str) -> ConnKind {
    let t = tipo.trim();
    if t.contains("wireless") || t == "wifi" {
        ConnKind::Wifi
    } else if t == "vpn" || t == "wireguard" || t == "tun" {
        ConnKind::Vpn
    } else if t.contains("ethernet") || t == "802-3-ethernet" {
        ConnKind::Ethernet
    } else {
        ConnKind::Otro
    }
}

/// Parsea `nmcli -t -f NAME,TYPE,ACTIVE,DEVICE,UUID connection show`: los perfiles
/// guardados. Descarta los `loopback`/`bridge` internos. Ordena VPNs primero,
/// luego por nombre. Deduplica por nombre (nmcli puede repetir con distintos
/// device). Los campos `DEVICE`/`UUID` son opcionales (formatos viejos de 3/4
/// campos siguen parseando, con `device`/`uuid = None`).
pub fn parse_connections(out: &str) -> Vec<SavedConn> {
    let mut conns: Vec<SavedConn> = Vec::new();
    for line in out.lines() {
        let campos = split_terse(line.trim());
        if campos.len() < 3 {
            continue;
        }
        let name = campos[0].trim().to_string();
        if name.is_empty() {
            continue;
        }
        let kind = conn_kind(&campos[1]);
        // Los cableados internos / puentes / loopback no aportan al panel.
        if matches!(kind, ConnKind::Otro) && (campos[1].contains("loopback") || campos[1].contains("bridge")) {
            continue;
        }
        let active = campos[2].trim().eq_ignore_ascii_case("yes");
        // DEVICE (4º campo): sólo relevante si la conexión está activa. nmcli
        // pone `--` (o vacío) cuando no hay device.
        let limpio = |c: &str| -> Option<String> {
            let t = c.trim();
            (!t.is_empty() && t != "--").then(|| t.to_string())
        };
        let device = campos.get(3).and_then(|d| limpio(d));
        let uuid = campos.get(4).and_then(|u| limpio(u));
        if let Some(prev) = conns.iter_mut().find(|c| c.name == name) {
            prev.active = prev.active || active;
            if prev.device.is_none() {
                prev.device = device;
            }
            if prev.uuid.is_none() {
                prev.uuid = uuid;
            }
            continue;
        }
        conns.push(SavedConn { name, kind, active, device, scope: None, uuid, raises: Vec::new() });
    }
    conns.sort_by(|a, b| {
        let va = matches!(a.kind, ConnKind::Vpn);
        let vb = matches!(b.kind, ConnKind::Vpn);
        vb.cmp(&va).then(b.active.cmp(&a.active)).then(a.name.cmp(&b.name))
    });
    conns
}

/// Parsea `nmcli -t -f IP4.ADDRESS,IP4.GATEWAY,IP4.DNS device show`: toma el
/// primer valor no vacío de cada clave (la conexión con IP). Las claves de nmcli
/// vienen como `IP4.ADDRESS[1]:192.168.1.5/24`.
pub fn parse_details(out: &str) -> NetDetails {
    let mut d = NetDetails::default();
    for line in out.lines() {
        let line = line.trim();
        let Some((key, val)) = line.split_once(':') else { continue };
        let val = val.trim();
        if val.is_empty() {
            continue;
        }
        if key.starts_with("IP4.ADDRESS") && d.ip.is_none() {
            d.ip = Some(val.to_string());
        } else if key.starts_with("IP4.GATEWAY") && d.gateway.is_none() {
            d.gateway = Some(val.to_string());
        } else if key.starts_with("IP4.DNS") && d.dns.is_none() {
            d.dns = Some(val.to_string());
        }
    }
    d
}

/// Parsea `nmcli -t -f connection.secondaries connection show <name>`: los UUIDs
/// de las conexiones que esta red levanta automáticamente al conectar. El terse
/// viene como `connection.secondaries:uuid1,uuid2` (o el valor pelado). Descarta
/// vacíos y `--`.
pub fn parse_secondaries(out: &str) -> Vec<String> {
    let val = out.lines().next().unwrap_or("").trim();
    let val = val.strip_prefix("connection.secondaries:").unwrap_or(val);
    val.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "--")
        .map(str::to_string)
        .collect()
}

// ============================================================
// Alcance de túnel (derivado de la tabla de rutas, sin privilegio)
// ============================================================

/// Una ruta de `/proc/net/route` que nos importa: por qué interfaz sale y a qué
/// destino/máscara. Los campos vienen en hex **little-endian** (así los imprime
/// el kernel), por eso `to_le_bytes` reconstruye los octetos en orden normal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteEntry {
    /// Interfaz de salida (`wg0`, `wlan0`, …).
    pub iface: String,
    /// Destino tal como lo imprime `/proc/net/route` (little-endian).
    pub dest_le: u32,
    /// Máscara idem.
    pub mask_le: u32,
}

impl RouteEntry {
    /// Longitud de prefijo (bits de la máscara). El popcount no depende del
    /// orden de bytes.
    pub fn prefix(&self) -> u8 {
        self.mask_le.count_ones() as u8
    }
    /// `true` si es la ruta por defecto (`0.0.0.0/0`).
    pub fn is_default(&self) -> bool {
        self.dest_le == 0 && self.mask_le == 0
    }
    /// El destino como CIDR legible (`10.0.0.0/24`).
    pub fn cidr(&self) -> String {
        let b = self.dest_le.to_le_bytes();
        format!("{}.{}.{}.{}/{}", b[0], b[1], b[2], b[3], self.prefix())
    }
}

/// Parsea `/proc/net/route`. Salta el encabezado y las líneas cortas.
pub fn parse_proc_net_route(contenido: &str) -> Vec<RouteEntry> {
    let mut out = Vec::new();
    for linea in contenido.lines() {
        let campos: Vec<&str> = linea.split_whitespace().collect();
        // Iface Destination Gateway Flags RefCnt Use Metric Mask ...
        if campos.len() < 8 || campos[0] == "Iface" {
            continue;
        }
        let (Ok(dest_le), Ok(mask_le)) = (
            u32::from_str_radix(campos[1], 16),
            u32::from_str_radix(campos[7], 16),
        ) else {
            continue;
        };
        out.push(RouteEntry {
            iface: campos[0].to_string(),
            dest_le,
            mask_le,
        });
    }
    out
}

/// Deriva el [`TunnelScope`] a partir de las rutas que salen por `iface`.
/// `Full` si captura todo (ruta por defecto, o el par `0.0.0.0/1`+`128.0.0.0/1`
/// que usa OpenVPN); `Split` con las subredes concretas si no. `None` si `iface`
/// no tiene rutas (no aporta alcance, p. ej. túnel recién levantado sin rutas).
pub fn scope_for_iface(rutas: &[RouteEntry], iface: &str) -> Option<TunnelScope> {
    let propias: Vec<&RouteEntry> = rutas.iter().filter(|r| r.iface == iface).collect();
    if propias.is_empty() {
        return None;
    }
    // 128.0.0.0 en little-endian (octeto alto primero) = 0x80.
    const MITAD_ALTA: u32 = 0x80;
    let cubre_todo = propias.iter().any(|r| r.is_default())
        || (propias.iter().any(|r| r.prefix() == 1 && r.dest_le == 0)
            && propias.iter().any(|r| r.prefix() == 1 && r.dest_le == MITAD_ALTA));
    if cubre_todo {
        return Some(TunnelScope::Full);
    }
    let mut cidrs: Vec<String> = propias.iter().map(|r| r.cidr()).collect();
    cidrs.sort();
    cidrs.dedup();
    Some(TunnelScope::Split(cidrs))
}

/// Deriva el [`NetStatus`] a partir de las tres lecturas: radio, cable y APs.
/// Prioridad: radio apagada → `WifiOff`; Wi-Fi asociado → `Wifi`; cable →
/// `Ethernet`; si no → `Desconectado`.
pub fn derive_status(wifi_enabled: bool, ethernet: bool, aps: &[WifiAp]) -> NetStatus {
    if let Some(activo) = aps.iter().find(|a| a.active) {
        return NetStatus::Wifi {
            ssid: activo.ssid.clone(),
            signal: activo.signal,
        };
    }
    if ethernet {
        return NetStatus::Ethernet;
    }
    if !wifi_enabled {
        return NetStatus::WifiOff;
    }
    NetStatus::Desconectado
}

// ============================================================
// Tráfico (para las microbarras del fantasma de red)
// ============================================================

/// Suma `(rx, tx)` en bytes de todas las interfaces reales de `/proc/net/dev`
/// (salta `lo`). El caller guarda el par anterior y deriva la tasa.
pub fn trafico_totales() -> Option<(u64, u64)> {
    let contenido = std::fs::read_to_string("/proc/net/dev").ok()?;
    Some(parse_proc_net_dev(&contenido))
}

/// Parser puro de `/proc/net/dev`: cada línea de interfaz es
/// `iface: rx_bytes ... (8 campos) tx_bytes ...`. Ignora `lo` y encabezados.
pub fn parse_proc_net_dev(contenido: &str) -> (u64, u64) {
    let (mut rx, mut tx) = (0u64, 0u64);
    for linea in contenido.lines().skip(2) {
        let Some((iface, resto)) = linea.split_once(':') else { continue };
        if iface.trim() == "lo" {
            continue;
        }
        let campos: Vec<&str> = resto.split_whitespace().collect();
        if campos.len() >= 9 {
            rx = rx.saturating_add(campos[0].parse().unwrap_or(0));
            tx = tx.saturating_add(campos[8].parse().unwrap_or(0));
        }
    }
    (rx, tx)
}

/// Normaliza una tasa (bytes/s) a `0..1` en **escala logarítmica**: 1 KB/s ya
/// asoma, ~12 MB/s satura. Así la microbarra "respira" tanto con un ping como
/// con una descarga, sin que lo chico sea invisible.
pub fn trafico_frac(bytes_por_seg: f64) -> f32 {
    if bytes_por_seg <= 0.0 {
        return 0.0;
    }
    const PISO: f64 = 512.0; // por debajo de esto, nada que mostrar
    const TECHO: f64 = 12_500_000.0; // ~100 Mbps
    ((bytes_por_seg / PISO).ln() / (TECHO / PISO).ln()).clamp(0.0, 1.0) as f32
}

// ============================================================
// Acciones (fire-and-forget, como spawn_cmd)
// ============================================================

/// Conecta a la red `ssid` (`nmcli device wifi connect`). Usa el perfil guardado
/// o el agente de secretos del sistema para la contraseña; no bloquea.
pub fn connect(ssid: &str) {
    crate::desacoplar(std::process::Command::new("nmcli")
        .args(["device", "wifi", "connect", ssid])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn());
}

/// Conecta a `ssid` con una contraseña explícita
/// (`nmcli device wifi connect <ssid> password <pw>`). Con `pw` vacío cae a
/// [`connect`] (perfil guardado / agente). No bloquea; la contraseña va por
/// argumentos al subproceso nmcli (no por la shell), sin quoting frágil.
pub fn connect_with(ssid: &str, pw: &str) {
    if pw.is_empty() {
        return connect(ssid);
    }
    crate::desacoplar(std::process::Command::new("nmcli")
        .args(["device", "wifi", "connect", ssid, "password", pw])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn());
}

/// Baja la conexión activa con ese `ssid` (`nmcli connection down`). No bloquea.
pub fn disconnect(ssid: &str) {
    crate::desacoplar(std::process::Command::new("nmcli")
        .args(["connection", "down", "id", ssid])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn());
}

/// Levanta un perfil guardado por nombre (`nmcli connection up id <name>`) — para
/// VPNs y redes guardadas. No bloquea.
pub fn conn_up(name: &str) {
    crate::desacoplar(std::process::Command::new("nmcli")
        .args(["connection", "up", "id", name])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn());
}

/// **Olvida** (borra) un perfil guardado (`nmcli connection delete id <name>`).
/// No bloquea.
pub fn forget(name: &str) {
    crate::desacoplar(std::process::Command::new("nmcli")
        .args(["connection", "delete", "id", name])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn());
}

/// Enciende/apaga la radio Wi-Fi (`nmcli radio wifi on|off`). No bloquea.
pub fn set_wifi_radio(on: bool) {
    crate::desacoplar(std::process::Command::new("nmcli")
        .args(["radio", "wifi", if on { "on" } else { "off" }])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn());
}

// ============================================================
// Muestreo síncrono (corre en el hilo)
// ============================================================

/// Corre `nmcli <args>` con un tope de tiempo y devuelve su stdout, o `None` si
/// nmcli no está, falla, o se pasa del plazo (la red puede colgar).
fn run_nmcli(args: &[&str]) -> Option<String> {
    use std::io::Read;
    use std::time::Instant;
    const PLAZO: Duration = Duration::from_secs(6);
    let mut child = std::process::Command::new("nmcli")
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

/// Una lectura completa del estado de la red. `None` si nmcli no responde.
fn sample() -> Option<NetState> {
    // La lista de Wi-Fi y la radio son las consultas esenciales; el cable es
    // best-effort (su ausencia no invalida la lectura).
    let radio_out = run_nmcli(&["-t", "radio", "wifi"])?;
    let wifi_enabled = parse_radio(&radio_out);
    let wifi_out = run_nmcli(&["-t", "-f", "ACTIVE,SSID,SIGNAL,SECURITY", "device", "wifi"])
        .unwrap_or_default();
    let networks = parse_wifi_list(&wifi_out);
    let eth_out = run_nmcli(&["-t", "-f", "TYPE,STATE", "device", "status"]).unwrap_or_default();
    let ethernet = parse_ethernet_connected(&eth_out);
    // Perfiles guardados (VPNs/redes conocidas) y detalles de la conexión activa.
    let conns_out = run_nmcli(&["-t", "-f", "NAME,TYPE,ACTIVE,DEVICE,UUID", "connection", "show"]).unwrap_or_default();
    let mut saved = parse_connections(&conns_out);
    // Enriquecer el alcance de los túneles VPN activos con la tabla de rutas
    // (legible sin privilegio, a diferencia de los `.conf` en /etc/wireguard).
    if saved.iter().any(|c| c.active && matches!(c.kind, ConnKind::Vpn)) {
        if let Ok(rutas_txt) = std::fs::read_to_string("/proc/net/route") {
            let rutas = parse_proc_net_route(&rutas_txt);
            for c in saved.iter_mut() {
                if c.active && matches!(c.kind, ConnKind::Vpn) {
                    if let Some(dev) = c.device.as_deref() {
                        c.scope = scope_for_iface(&rutas, dev);
                    }
                }
            }
        }
    }
    // Disparador DETECTADO: la red activa (Wi-Fi/cable) puede declarar
    // `secondaries` — VPN que NM levanta sola al conectar. Lo resolvemos a
    // nombres y lo mostramos como surfacing, sin motor de detección propio.
    let mapa_uuid: HashMap<String, String> = saved
        .iter()
        .filter_map(|c| c.uuid.clone().map(|u| (u, c.name.clone())))
        .collect();
    let activas: Vec<String> = saved
        .iter()
        .filter(|c| c.active && matches!(c.kind, ConnKind::Wifi | ConnKind::Ethernet))
        .map(|c| c.name.clone())
        .collect();
    for name in activas {
        let Some(sec_out) = run_nmcli(&["-t", "-f", "connection.secondaries", "connection", "show", &name])
        else {
            continue;
        };
        let nombres: Vec<String> = parse_secondaries(&sec_out)
            .into_iter()
            .map(|u| mapa_uuid.get(&u).cloned().unwrap_or(u))
            .collect();
        if let Some(c) = saved.iter_mut().find(|c| c.name == name) {
            c.raises = nombres;
        }
    }
    let det_out = run_nmcli(&["-t", "-f", "IP4.ADDRESS,IP4.GATEWAY,IP4.DNS", "device", "show"]).unwrap_or_default();
    let details = parse_details(&det_out);
    Some(NetState {
        status: derive_status(wifi_enabled, ethernet, &networks),
        wifi_enabled,
        networks,
        saved,
        details,
    })
}

/// El feed de red corriendo en su propio hilo. Publica la última lectura por un
/// canal; el frontend la drena con [`NetworkHandle::latest`] por frame.
pub struct NetworkHandle {
    rx: Receiver<NetState>,
}

impl NetworkHandle {
    /// Arranca el hilo. Refresca cada ~5 s. Si nmcli no responde, publica una
    /// lectura `Sin` (icono tenue) y reintenta más espaciado.
    pub fn spawn() -> Self {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("pata-network".into())
            .spawn(move || loop {
                let (estado, espera) = match sample() {
                    Some(s) => (s, Duration::from_secs(5)),
                    None => (
                        NetState {
                            status: NetStatus::Sin,
                            ..Default::default()
                        },
                        Duration::from_secs(15),
                    ),
                };
                if tx.send(estado).is_err() {
                    break; // la app se fue
                }
                std::thread::sleep(espera);
            })
            .ok();
        Self { rx }
    }

    /// La lectura más reciente (drena la cola), o `None` si no llegó nada nuevo.
    /// No bloquea.
    pub fn latest(&self) -> Option<NetState> {
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
    fn split_respeta_escape() {
        assert_eq!(split_terse("yes:MiRed:72:WPA2"), vec!["yes", "MiRed", "72", "WPA2"]);
        // Un SSID con `:` viene escapado como `\:`.
        assert_eq!(
            split_terse(r"no:Red\:rara:40:WPA2"),
            vec!["no", "Red:rara", "40", "WPA2"]
        );
    }

    #[test]
    fn parsea_lista_wifi_dedup_y_orden() {
        let out = "\
yes:CasaWifi:65:WPA2
no:Vecino:80:WPA2
no:CasaWifi:50:WPA2
no::55:WPA2
no:Abierta:30:";
        let aps = parse_wifi_list(out);
        // La oculta (SSID vacío) se descarta.
        assert_eq!(aps.len(), 3);
        // La activa va primero pese a menor señal.
        assert_eq!(aps[0].ssid, "CasaWifi");
        assert!(aps[0].active);
        // La abierta no es segura.
        let abierta = aps.iter().find(|a| a.ssid == "Abierta").unwrap();
        assert!(!abierta.secure);
        // El resto, por señal descendente.
        assert_eq!(aps[1].ssid, "Vecino");
    }

    #[test]
    fn radio_enabled() {
        assert!(parse_radio("enabled\n"));
        assert!(!parse_radio("disabled"));
        assert!(!parse_radio("missing"));
    }

    #[test]
    fn ethernet_conectado() {
        assert!(parse_ethernet_connected("ethernet:connected\nwifi:disconnected"));
        assert!(parse_ethernet_connected("ethernet:connected (externally)"));
        assert!(!parse_ethernet_connected("ethernet:unavailable\nwifi:connected"));
        assert!(!parse_ethernet_connected("wifi:connected"));
    }

    #[test]
    fn parsea_conexiones_guardadas_vpn_primero() {
        // Formato NAME:TYPE:ACTIVE:DEVICE:UUID (la VPN activa trae su device).
        let out = "\
Casa:802-11-wireless:yes:wlan0:aaa-111
MiVPN:wireguard:yes:wg0:bbb-222
Oficina:802-11-wireless:no:--:ccc-333
Cableada:802-3-ethernet:no::
lo:loopback:no:lo:ddd";
        let cs = parse_connections(out);
        // loopback descartado.
        assert_eq!(cs.len(), 4);
        // VPN primero.
        assert_eq!(cs[0].name, "MiVPN");
        assert_eq!(cs[0].kind, ConnKind::Vpn);
        // Device parseado sólo cuando aporta (no `--` ni vacío).
        assert_eq!(cs[0].device.as_deref(), Some("wg0"));
        assert_eq!(cs[0].uuid.as_deref(), Some("bbb-222"));
        let casa = cs.iter().find(|c| c.name == "Casa").unwrap();
        assert_eq!(casa.device.as_deref(), Some("wlan0"));
        assert_eq!(casa.uuid.as_deref(), Some("aaa-111"));
        assert_eq!(cs.iter().find(|c| c.name == "Oficina").unwrap().device, None);
        assert_eq!(cs.iter().find(|c| c.name == "Cableada").unwrap().device, None);
        // El scope y los raises arrancan vacíos (los llena el sampler).
        assert!(cs.iter().all(|c| c.scope.is_none() && c.raises.is_empty()));
    }

    #[test]
    fn parsea_secondaries_con_y_sin_prefijo() {
        // Con el prefijo terse de nmcli.
        assert_eq!(
            parse_secondaries("connection.secondaries:aaa-111,bbb-222"),
            vec!["aaa-111".to_string(), "bbb-222".to_string()]
        );
        // Valor pelado.
        assert_eq!(parse_secondaries("aaa-111"), vec!["aaa-111".to_string()]);
        // Sin secondaries: vacío (nmcli suele dar el campo vacío o `--`).
        assert!(parse_secondaries("connection.secondaries:").is_empty());
        assert!(parse_secondaries("--").is_empty());
        assert!(parse_secondaries("").is_empty());
    }

    #[test]
    fn parsea_conexiones_formato_viejo_3_campos() {
        // Sin DEVICE: sigue parseando, device = None.
        let cs = parse_connections("Casa:802-11-wireless:yes\nMiVPN:vpn:no");
        assert_eq!(cs.len(), 2);
        assert!(cs.iter().all(|c| c.device.is_none()));
    }

    #[test]
    fn parsea_proc_net_route() {
        // Encabezado + default por wlan0 + LAN por wlan0.
        let out = "\
Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT
wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0
wlan0\t0001A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0";
        let rs = parse_proc_net_route(out);
        assert_eq!(rs.len(), 2);
        assert!(rs[0].is_default());
        // 0001A8C0 LE → 192.168.1.0, máscara 00FFFFFF → /24.
        assert_eq!(rs[1].cidr(), "192.168.1.0/24");
        assert_eq!(rs[1].prefix(), 24);
    }

    #[test]
    fn scope_full_por_ruta_default() {
        let rutas = vec![
            RouteEntry { iface: "wg0".into(), dest_le: 0, mask_le: 0 },
            RouteEntry { iface: "wlan0".into(), dest_le: 0, mask_le: 0 },
        ];
        assert_eq!(scope_for_iface(&rutas, "wg0"), Some(TunnelScope::Full));
    }

    #[test]
    fn scope_full_por_par_slash1_openvpn() {
        // OpenVPN pisa el default con 0.0.0.0/1 + 128.0.0.0/1 (máscara /1 = 0x80).
        let rutas = vec![
            RouteEntry { iface: "tun0".into(), dest_le: 0x00, mask_le: 0x80 },
            RouteEntry { iface: "tun0".into(), dest_le: 0x80, mask_le: 0x80 },
        ];
        assert_eq!(scope_for_iface(&rutas, "tun0"), Some(TunnelScope::Full));
    }

    #[test]
    fn scope_split_solo_subredes() {
        // AllowedIPs = 10.0.0.0/24 → una ruta específica, sin default.
        // 10.0.0.0 en LE (octeto alto primero en el byte bajo) = 0x0A.
        let rutas = vec![RouteEntry { iface: "wg0".into(), dest_le: 0x0A, mask_le: 0x00FFFFFF }];
        let sc = scope_for_iface(&rutas, "wg0");
        assert_eq!(sc, Some(TunnelScope::Split(vec!["10.0.0.0/24".into()])));
        assert_eq!(sc.unwrap().label(), "Sólo 10.0.0.0/24");
    }

    #[test]
    fn scope_none_si_iface_ausente() {
        let rutas = vec![RouteEntry { iface: "wlan0".into(), dest_le: 0, mask_le: 0 }];
        assert_eq!(scope_for_iface(&rutas, "wg0"), None);
    }

    #[test]
    fn scope_label_muchas_redes() {
        let sc = TunnelScope::Split(vec!["10.0.0.0/24".into(), "192.168.9.0/24".into()]);
        assert_eq!(sc.label(), "Sólo 2 redes");
        assert_eq!(TunnelScope::Full.label(), "Todo el tráfico");
    }

    #[test]
    fn parsea_detalles_ip_gateway_dns() {
        let out = "\
IP4.ADDRESS[1]:192.168.1.5/24
IP4.GATEWAY:192.168.1.1
IP4.DNS[1]:1.1.1.1
IP4.DNS[2]:8.8.8.8";
        let d = parse_details(out);
        assert_eq!(d.ip.as_deref(), Some("192.168.1.5/24"));
        assert_eq!(d.gateway.as_deref(), Some("192.168.1.1"));
        assert_eq!(d.dns.as_deref(), Some("1.1.1.1")); // el primero
    }

    #[test]
    fn parsea_proc_net_dev_saltando_lo() {
        let out = "Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 1000 10 0 0 0 0 0 0 1000 10 0 0 0 0 0 0
  wlan0: 5000 50 0 0 0 0 0 0 2000 20 0 0 0 0 0 0
   eth0: 300 3 0 0 0 0 0 0 700 7 0 0 0 0 0 0";
        assert_eq!(parse_proc_net_dev(out), (5300, 2700));
    }

    #[test]
    fn trafico_frac_escala_log() {
        assert_eq!(trafico_frac(0.0), 0.0);
        assert_eq!(trafico_frac(100.0), 0.0); // bajo el piso
        let chico = trafico_frac(10_000.0);
        let grande = trafico_frac(5_000_000.0);
        assert!(chico > 0.1 && chico < grande && grande < 1.0);
        assert_eq!(trafico_frac(50_000_000.0), 1.0); // saturado
    }

    #[test]
    fn deriva_estado() {
        let activa = vec![WifiAp {
            ssid: "X".into(),
            signal: 70,
            secure: true,
            active: true,
        }];
        assert_eq!(
            derive_status(true, false, &activa),
            NetStatus::Wifi { ssid: "X".into(), signal: 70 }
        );
        // Sin Wi-Fi asociado pero con cable.
        assert_eq!(derive_status(true, true, &[]), NetStatus::Ethernet);
        // Radio apagada y sin cable.
        assert_eq!(derive_status(false, false, &[]), NetStatus::WifiOff);
        // Radio encendida, sin asociar, sin cable.
        assert_eq!(derive_status(true, false, &[]), NetStatus::Desconectado);
    }
}
