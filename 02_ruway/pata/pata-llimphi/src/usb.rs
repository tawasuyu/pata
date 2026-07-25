//! **USB / medios extraíbles** — el hueco reconocido del escritorio: vigilar la
//! inserción de un pendrive/disco y ofrecer montarlo, abrirlo y expulsarlo.
//!
//! Enumera con `lsblk -J` (una llamada da nombre/etiqueta/tamaño/punto de montaje/
//! removible) en un hilo lento; el fantasma sale saliente cuando hay un extraíble
//! **sin montar** (recién insertado). Monta/desmonta/expulsa con `udisksctl`
//! (udisks2): sin root, sin sudo — el polkit de escritorio lo autoriza, y monta
//! en `/run/media/$USER`. Reusa la política de la suite (nahual usa el mismo
//! `udisksctl` para su montaje rw).

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

/// Cadencia del hilo (segundos): lo bastante ágil para notar una inserción sin
/// castigar la batería sondeando `lsblk` a cada rato.
const CADENCIA: Duration = Duration::from_secs(4);

/// Una partición extraíble montable, lista para pintar.
#[derive(Clone, Debug, PartialEq)]
pub struct UsbParticion {
    /// Ruta del dispositivo de la partición (`/dev/sdb1`).
    pub dev: String,
    /// Ruta del disco padre (`/dev/sdb`) — para expulsar (power-off) el medio.
    pub disco: String,
    /// Etiqueta del volumen, o el nombre si no tiene.
    pub etiqueta: String,
    /// Tamaño legible (`14,4 GB`).
    pub tam: String,
    /// Dónde está montada, o `None` si no lo está.
    pub montada_en: Option<String>,
}

/// Lo que el render necesita: las particiones extraíbles + si hay alguna sin montar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsbSnapshot {
    pub particiones: Vec<UsbParticion>,
    /// `true` si hay al menos un extraíble sin montar (salience del fantasma).
    pub hay_sin_montar: bool,
}

/// Formatea bytes a un tamaño legible (base 1000, como las etiquetas de los discos).
fn tam_legible(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// Parsea la salida de `lsblk -J -b -o NAME,LABEL,SIZE,MOUNTPOINT,RM,TYPE,PATH`:
/// devuelve las **particiones de discos removibles**. Puro y testeable.
pub fn parse_lsblk(json: &str) -> Vec<UsbParticion> {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let Some(devs) = v.get("blockdevices").and_then(|d| d.as_array()) else {
        return out;
    };
    for disco in devs {
        // Sólo discos removibles (rm == 1 / true). lsblk emite `rm` como bool o num.
        let removible = disco.get("rm").map(es_verdadero).unwrap_or(false);
        if !removible {
            continue;
        }
        // Una ranura de lector de tarjetas VACÍA sigue siendo un disco removible
        // para lsblk, pero con `size: 0` y sin hijos. Sin este corte se colaba
        // como «volumen de 0 B sin montar», y como `hay_sin_montar` es un `any`,
        // el fantasma del USB quedaba encendido para siempre ofreciendo montar
        // una ranura donde no hay nada. Medido en metal el 2026-07-24: dos
        // ranuras (`/dev/sda`, `/dev/sdb`) del lector de esta laptop.
        if tam_de(disco) == 0 && disco.get("children").and_then(|c| c.as_array()).is_none() {
            continue;
        }
        let disco_path = disco.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
        // Las particiones son los hijos de tipo `part`; si el disco no tiene tabla
        // (formateado entero, o cifrado de punta a punta) el propio disco es la
        // unidad montable.
        let parts: Vec<&serde_json::Value> = disco
            .get("children")
            .and_then(|c| c.as_array())
            .map(|hijos| {
                hijos
                    .iter()
                    .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("part"))
                    .collect()
            })
            .unwrap_or_default();
        if parts.is_empty() {
            // Sin hijos `part`: o no hay tabla, o los hijos son un mapper
            // (`crypt`/`lvm`) colgando directo del disco. En ambos casos la
            // unidad montable es el disco mismo — antes este caso se perdía
            // entero (el `match` exigía hijos de tipo `part` y, si los había de
            // otro tipo, no emitía nada).
            if let Some(up) = particion_de(disco, &disco_path) {
                out.push(up);
            }
            continue;
        }
        for p in parts {
            if let Some(up) = particion_de(p, &disco_path) {
                out.push(up);
            }
        }
    }
    out
}

/// El `size` de un nodo de lsblk, que puede venir numérico (`-b`) o como cadena.
fn tam_de(p: &serde_json::Value) -> u64 {
    p.get("size")
        .and_then(|x| x.as_u64())
        .or_else(|| p.get("size").and_then(|x| x.as_str()).and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

/// El punto de montaje **efectivo** de un nodo: el suyo, o —si no tiene— el del
/// primer descendiente que sí lo tenga.
///
/// Un volumen cifrado no se monta en la partición sino en el mapper que cuelga
/// de ella (`sdc2` → `crypt luks-…` → `/run/media/…`), y con LVM pasa lo mismo.
/// Mirando sólo el nodo `part`, un USB cifrado YA montado se reportaba como «sin
/// montar»: el fantasma se encendía y el menú ofrecía «Montar» sobre algo que ya
/// lo estaba. Esa topología es la del disco `sdc` de esta laptop (metal
/// 2026-07-24), que fue de donde salió el caso.
fn montaje_efectivo(p: &serde_json::Value) -> Option<String> {
    if let Some(m) = p.get("mountpoint").and_then(|x| x.as_str()).filter(|s| !s.is_empty()) {
        return Some(m.to_string());
    }
    p.get("children")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
        .find_map(montaje_efectivo)
}

/// `rm` de lsblk puede venir como `true`/`false` o `1`/`0`.
fn es_verdadero(v: &serde_json::Value) -> bool {
    v.as_bool().unwrap_or(false) || v.as_u64().map(|n| n == 1).unwrap_or(false)
        || v.as_str().map(|s| s == "1" || s == "true").unwrap_or(false)
}

/// Construye una [`UsbParticion`] de un nodo de partición (o disco entero).
fn particion_de(p: &serde_json::Value, disco: &str) -> Option<UsbParticion> {
    let dev = p.get("path").and_then(|x| x.as_str())?.to_string();
    let name = p.get("name").and_then(|x| x.as_str()).unwrap_or("");
    let etiqueta = p
        .get("label")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(name)
        .to_string();
    let bytes = tam_de(p);
    let montada_en = montaje_efectivo(p);
    Some(UsbParticion {
        dev,
        disco: disco.to_string(),
        etiqueta,
        tam: tam_legible(bytes),
        montada_en,
    })
}

// ── Acciones (udisksctl, sin root) ──────────────────────────────────────────

/// Monta la partición `dev` (rw) vía `udisksctl` (en `/run/media/$USER`).
pub fn montar(dev: &str) {
    crate::desacoplar(std::process::Command::new("udisksctl").args(["mount", "-b", dev]).spawn());
}

/// Desmonta la partición `dev`.
pub fn desmontar(dev: &str) {
    crate::desacoplar(std::process::Command::new("udisksctl").args(["unmount", "-b", dev]).spawn());
}

/// Expulsa (apaga) el medio del disco `disco` — seguro para retirar.
pub fn expulsar(disco: &str) {
    crate::desacoplar(std::process::Command::new("udisksctl").args(["power-off", "-b", disco]).spawn());
}

/// El asa del bucle de pata: drena el último snapshot por frame.
pub struct UsbHandle {
    rx: Receiver<UsbSnapshot>,
    ultimo: Option<UsbSnapshot>,
}

impl UsbHandle {
    /// Arranca el hilo si `lsblk` está disponible. `None` si no (sin USB widget).
    pub fn spawn() -> Option<Self> {
        if !hay_lsblk() {
            return None;
        }
        let (tx, rx): (Sender<UsbSnapshot>, Receiver<UsbSnapshot>) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("pata-usb".into())
            .spawn(move || bucle(tx))
            .ok()?;
        Some(Self { rx, ultimo: None })
    }

    /// El último snapshot (retiene el previo si no llegó uno nuevo).
    pub fn latest(&mut self) -> Option<&UsbSnapshot> {
        while let Ok(s) = self.rx.try_recv() {
            self.ultimo = Some(s);
        }
        self.ultimo.as_ref()
    }
}

/// `true` si `lsblk` está en el PATH.
fn hay_lsblk() -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg("command -v lsblk")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Corre `lsblk -J` y arma el snapshot. `None` si falló.
fn construir() -> Option<UsbSnapshot> {
    let out = std::process::Command::new("lsblk")
        .args(["-J", "-b", "-o", "NAME,LABEL,SIZE,MOUNTPOINT,RM,TYPE,PATH"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let json = String::from_utf8_lossy(&out.stdout);
    let particiones = parse_lsblk(&json);
    let hay_sin_montar = particiones.iter().any(|p| p.montada_en.is_none());
    Some(UsbSnapshot { particiones, hay_sin_montar })
}

/// El hilo: arma un snapshot y lo emite cada [`CADENCIA`].
fn bucle(tx: Sender<UsbSnapshot>) {
    loop {
        let snap = construir().unwrap_or_default();
        if tx.send(snap).is_err() {
            return;
        }
        std::thread::sleep(CADENCIA);
    }
}

/// Punto de montaje `~/.local/share`… no aplica; helper para abrir un punto en el
/// gestor de archivos (nahual/xdg-open).
pub fn abrir(punto: &str) -> String {
    // Reusa el open genérico de la suite; se dispara con `spawn_cmd`.
    format!("xdg-open {}", crate::shell_quote(punto))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tam_legible_redondea() {
        assert_eq!(tam_legible(512), "512 B");
        assert_eq!(tam_legible(14_400_000_000), "14.4 GB");
    }

    #[test]
    fn parse_lsblk_toma_solo_particiones_removibles() {
        // Un disco interno (rm 0) con root montado + un pendrive (rm 1) con una
        // partición sin montar y otra montada.
        let json = r#"{
          "blockdevices": [
            {"name":"sda","rm":false,"type":"disk","path":"/dev/sda","children":[
              {"name":"sda1","label":"root","size":500000000000,"mountpoint":"/","rm":false,"type":"part","path":"/dev/sda1"}
            ]},
            {"name":"sdb","rm":true,"type":"disk","path":"/dev/sdb","children":[
              {"name":"sdb1","label":"KINGSTON","size":14400000000,"mountpoint":null,"rm":true,"type":"part","path":"/dev/sdb1"},
              {"name":"sdb2","label":"DATA","size":1000000000,"mountpoint":"/run/media/x/DATA","rm":true,"type":"part","path":"/dev/sdb2"}
            ]}
          ]
        }"#;
        let ps = parse_lsblk(json);
        assert_eq!(ps.len(), 2, "sólo las dos particiones del pendrive removible");
        assert_eq!(ps[0].etiqueta, "KINGSTON");
        assert_eq!(ps[0].dev, "/dev/sdb1");
        assert_eq!(ps[0].disco, "/dev/sdb");
        assert_eq!(ps[0].tam, "14.4 GB");
        assert!(ps[0].montada_en.is_none());
        assert_eq!(ps[1].montada_en.as_deref(), Some("/run/media/x/DATA"));
    }

    #[test]
    fn disco_removible_sin_tabla_es_montable_entero() {
        let json = r#"{"blockdevices":[
          {"name":"sdc","label":"BACKUP","size":2000000000,"mountpoint":null,"rm":1,"type":"disk","path":"/dev/sdc"}
        ]}"#;
        let ps = parse_lsblk(json);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].etiqueta, "BACKUP");
        assert_eq!(ps[0].dev, "/dev/sdc");
    }

    /// Una ranura de lector de tarjetas vacía es un disco removible de 0 B sin
    /// hijos. Se colaba como volumen «sin montar» y dejaba el fantasma del USB
    /// encendido de por vida. Fixture tomado tal cual del `lsblk` de la laptop
    /// (metal 2026-07-24: `/dev/sda` y `/dev/sdb` del lector interno).
    #[test]
    fn ranura_de_lector_vacia_no_es_un_volumen() {
        let json = r#"{"blockdevices":[
          {"name":"sda","label":null,"size":0,"mountpoint":null,"rm":true,"type":"disk","path":"/dev/sda"},
          {"name":"sdb","label":null,"size":0,"mountpoint":null,"rm":true,"type":"disk","path":"/dev/sdb"}
        ]}"#;
        let ps = parse_lsblk(json);
        assert!(ps.is_empty(), "las ranuras vacías no son volúmenes montables: {ps:?}");
        assert!(!ps.iter().any(|p| p.montada_en.is_none()), "no debe encender el fantasma");
    }

    /// Con cifrado, el montaje NO está en la partición sino en el mapper que
    /// cuelga de ella. Antes se reportaba «sin montar» un USB cifrado ya montado.
    /// Fixture con la forma real de `sdc2 → luks-… → /run/media/…` de la laptop.
    #[test]
    fn particion_cifrada_hereda_el_montaje_de_su_mapper() {
        let json = r#"{"blockdevices":[
          {"name":"sdc","label":null,"size":2000365289472,"mountpoint":null,"rm":true,"type":"disk","path":"/dev/sdc","children":[
            {"name":"sdc2","label":null,"size":532094648320,"mountpoint":null,"rm":true,"type":"part","path":"/dev/sdc2","children":[
              {"name":"luks-11ff80df","label":null,"size":532077871104,"mountpoint":"/run/media/sergio/143f1cb0","rm":false,"type":"crypt","path":"/dev/mapper/luks-11ff80df"}
            ]}
          ]}
        ]}"#;
        let ps = parse_lsblk(json);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].dev, "/dev/sdc2");
        assert_eq!(ps[0].montada_en.as_deref(), Some("/run/media/sergio/143f1cb0"));
    }

    /// Un removible cifrado de punta a punta (sin tabla de particiones) cuelga el
    /// mapper directo del disco. El `match` viejo veía hijos, no encontraba
    /// ninguno de tipo `part` y no emitía **nada**: el volumen desaparecía.
    #[test]
    fn disco_removible_cifrado_entero_no_desaparece() {
        let json = r#"{"blockdevices":[
          {"name":"sdd","label":null,"size":64000000000,"mountpoint":null,"rm":true,"type":"disk","path":"/dev/sdd","children":[
            {"name":"luks-caja","label":null,"size":63900000000,"mountpoint":"/run/media/sergio/caja","rm":false,"type":"crypt","path":"/dev/mapper/luks-caja"}
          ]}
        ]}"#;
        let ps = parse_lsblk(json);
        assert_eq!(ps.len(), 1, "el disco entero es la unidad montable");
        assert_eq!(ps[0].dev, "/dev/sdd");
        assert_eq!(ps[0].montada_en.as_deref(), Some("/run/media/sergio/caja"));
    }

    /// Contrato contra el `lsblk` **instalado en esta máquina**, no contra un
    /// fixture: los flags que usa [`construir`] siguen dando el JSON que
    /// [`parse_lsblk`] entiende. Es lo que atrapa una deriva de formato de lsblk
    /// (el riesgo que la cola de verificación anotaba para `udisksctl`). Se salta
    /// solo donde no haya `lsblk`.
    #[test]
    fn el_lsblk_instalado_sigue_hablando_el_formato_que_parseamos() {
        let Ok(out) = std::process::Command::new("lsblk")
            .args(["-J", "-b", "-o", "NAME,LABEL,SIZE,MOUNTPOINT,RM,TYPE,PATH"])
            .output()
        else {
            return; // sin lsblk (sandbox/CI pelado): nada que contrastar
        };
        if !out.status.success() {
            return;
        }
        let json = String::from_utf8_lossy(&out.stdout);
        let v: serde_json::Value = serde_json::from_str(&json).expect("lsblk -J emitió JSON válido");
        assert!(v.get("blockdevices").and_then(|d| d.as_array()).is_some(), "falta blockdevices");
        // No aserta el CONTENIDO (depende del hardware), sí las invariantes que
        // el render da por ciertas: toda unidad tiene device y tamaño legible.
        for p in parse_lsblk(&json) {
            assert!(p.dev.starts_with("/dev/"), "device raro: {p:?}");
            assert!(!p.etiqueta.is_empty(), "etiqueta vacía: {p:?}");
            assert!(!p.tam.is_empty(), "tamaño vacío: {p:?}");
            assert!(p.disco.starts_with("/dev/"), "disco padre raro: {p:?}");
        }
    }
}
