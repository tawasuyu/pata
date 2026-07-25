//! **Tampu** — el común de objetos, en la barra: qué prestaste, qué tienes en
//! custodia y qué devolución venció.
//!
//! Tampu registra cada objeto del común con una **cadena de custodia firmada**
//! (Ed25519): quién lo aportó, quién lo tomó y con qué plazo blando, quién lo
//! devolvió. La cripto no sanciona —hace *legible*—: un plazo vencido no multa,
//! sólo habilita a asentar una falta (§5). Este módulo lee ese almacén (archivos
//! en `~/.local/share/tampu/`, sin lock — se puede leer mientras la app tampu
//! corre) en un hilo lento y arma un snapshot con lo que **te** involucra:
//! objetos en tu mano y objetos que aportaste, con sus devoluciones vencidas y
//! cualquier anomalía dura (manipulación / hueco de custodia) sobre lo tuyo.
//!
//! Es **de sólo lectura**: los gestos (Tomo/Devuelvo/Falta) se asientan en la app
//! tampu. La barra sólo narra y alerta.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use tampu_core::{Actor, ClavePublica, Informe};

/// Cadencia del hilo (segundos): el común cambia con gestos humanos, no rápido.
const CADENCIA: Duration = Duration::from_secs(90);

/// De qué lado del común está un objeto respecto de vos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lado {
    /// Lo tienes en la mano ahora (eres el custodio actual).
    Tengo,
    /// Lo aportaste vos (nació de tu firma); lo tiene alguien (o el común).
    Aporte,
}

/// Un objeto del común que te involucra, listo para pintar.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjetoVista {
    pub descripcion: String,
    pub lado: Lado,
    /// Línea de detalle: plazo, quién lo tiene, o el motivo de alarma.
    pub detalle: String,
    /// La devolución venció (vos debés devolver, o alguien te debe devolver).
    pub vencido: bool,
    /// Anomalía dura (manipulación / hueco) sobre un objeto tuyo — el ROJO.
    pub anomalia: bool,
}

/// Lo que el render necesita: los objetos que te involucran + las señales de
/// salience del fantasma.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TampuSnapshot {
    /// Objetos en tu custodia (Tengo) y aportados por vos (Aporte), en ese orden.
    pub objetos: Vec<ObjetoVista>,
    /// Alguna devolución venció (en cualquier dirección).
    pub hay_vencido: bool,
    /// Alguna anomalía dura sobre un objeto tuyo.
    pub hay_anomalia: bool,
}

/// Nombre legible de un actor (para «lo tiene …»). Una identidad soberana se
/// muestra por los primeros bytes de su clave (sin petnames aquí); una tarjeta o
/// un testimonio, por lo que declaran.
fn nombre_actor(a: &Actor) -> String {
    match a {
        Actor::Identidad(pk) => format!("{:02x}{:02x}{:02x}…", pk[0], pk[1], pk[2]),
        Actor::TarjetaNfc(_) => "una tarjeta".to_string(),
        Actor::Testimonio { de, .. } => de.clone(),
    }
}

/// Clasifica un objeto respecto de `mi_pk`. Devuelve `None` si el objeto no te
/// involucra (ni lo tienes ni lo aportaste). Puro y testeable.
pub fn clasificar(
    descripcion: &str,
    aportante: ClavePublica,
    informe: &Informe,
    mi_pk: ClavePublica,
    ahora: u64,
) -> Option<ObjetoVista> {
    let lo_tengo = matches!(&informe.custodio_actual, Some(Actor::Identidad(pk)) if *pk == mi_pk);
    let lo_aporte = aportante == mi_pk;
    let vencido = informe.vencido(ahora);
    // Sólo la manipulación DURA (firma inválida, actor suplantado…) alarma; los
    // huecos/bifurcaciones son estados representables, no ROJOS (§ tampu-core).
    let anomalia = informe.manipulada();

    if lo_tengo {
        let detalle = if vencido {
            "devolución vencida".to_string()
        } else if let Some(p) = informe.plazo_actual {
            let dias = (p.saturating_sub(ahora) / 86_400) as i64;
            if dias <= 0 { "devolver hoy".to_string() } else { format!("devolver en {dias} días") }
        } else {
            "en tu custodia".to_string()
        };
        Some(ObjetoVista { descripcion: descripcion.to_string(), lado: Lado::Tengo, detalle, vencido, anomalia })
    } else if lo_aporte {
        let detalle = if anomalia {
            "⚠ manipulación en la cadena".to_string()
        } else if informe.retirado {
            "retirado del común".to_string()
        } else if let Some(a) = &informe.custodio_actual {
            let quien = nombre_actor(a);
            if vencido { format!("lo tiene {quien} · vencido") } else { format!("lo tiene {quien}") }
        } else {
            "en el común".to_string()
        };
        Some(ObjetoVista { descripcion: descripcion.to_string(), lado: Lado::Aporte, detalle, vencido, anomalia })
    } else {
        None
    }
}

/// El asa del bucle de pata: drena el último snapshot por frame.
pub struct TampuHandle {
    rx: Receiver<TampuSnapshot>,
    ultimo: Option<TampuSnapshot>,
}

impl TampuHandle {
    /// Arranca el hilo si hay un almacén tampu **ya inicializado** (`~/.local/
    /// share/tampu`, o `$TAMPU_HOME`). `None` si tampu no está en uso — así la
    /// barra no le crea directorios a quien no usa el común.
    pub fn spawn() -> Option<Self> {
        let home = tampu_home()?;
        if !home.exists() {
            return None; // tampu no inicializado: sin fantasma del común
        }
        let (tx, rx): (Sender<TampuSnapshot>, Receiver<TampuSnapshot>) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("pata-tampu".into())
            .spawn(move || bucle(tx))
            .ok()?;
        Some(Self { rx, ultimo: None })
    }

    /// El último snapshot (retiene el previo si no llegó uno nuevo).
    pub fn latest(&mut self) -> Option<&TampuSnapshot> {
        while let Ok(s) = self.rx.try_recv() {
            self.ultimo = Some(s);
        }
        self.ultimo.as_ref()
    }
}

/// La raíz del almacén tampu (`$TAMPU_HOME` o `~/.local/share/tampu`).
fn tampu_home() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TAMPU_HOME") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/tampu"))
}

/// Segundos Unix de ahora.
fn ahora_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Construye el snapshot leyendo el almacén. `None` si no hay identidad activa
/// (no hay «vos» para clasificar) — el fantasma no aplica.
fn construir() -> Option<TampuSnapshot> {
    let alm = tampu_store::Almacen::abrir().ok()?;
    let (_, kp) = alm.keypair_activo().ok()?;
    let mi_pk = kp.public_key();
    let ahora = ahora_unix();

    let mut objetos: Vec<ObjetoVista> = Vec::new();
    for id in alm.listar_objetos().ok()? {
        let Ok(cadena) = alm.cargar_cadena(id) else { continue };
        let inf = cadena.verificar();
        let nc = &cadena.nacimiento.core;
        if let Some(v) = clasificar(&nc.descripcion, nc.aportante, &inf, mi_pk, ahora) {
            objetos.push(v);
        }
    }
    // Los que te comprometen (vencidos/anomalías) primero; dentro, Tengo antes que
    // Aporte (lo que vos debés, antes que lo que te deben).
    objetos.sort_by_key(|o| {
        let urgencia = if o.anomalia { 0 } else if o.vencido { 1 } else { 2 };
        let lado = if o.lado == Lado::Tengo { 0 } else { 1 };
        (urgencia, lado)
    });
    let hay_vencido = objetos.iter().any(|o| o.vencido);
    let hay_anomalia = objetos.iter().any(|o| o.anomalia);
    Some(TampuSnapshot { objetos, hay_vencido, hay_anomalia })
}

/// El hilo: arma un snapshot y lo emite cada [`CADENCIA`]. Si el almacén no da
/// nada (sin identidad, error de lectura), emite uno vacío.
fn bucle(tx: Sender<TampuSnapshot>) {
    loop {
        let snap = construir().unwrap_or_default();
        if tx.send(snap).is_err() {
            return; // pata soltó el handle
        }
        std::thread::sleep(CADENCIA);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tampu_core::{Anomalia, IdObjeto, IdPunto};

    fn informe_base() -> Informe {
        Informe {
            objeto: IdObjeto([0u8; 32]),
            anomalias: Vec::new(),
            custodio_actual: None,
            ubicacion_actual: None,
            plazo_actual: None,
            retirado: false,
            entradas_validas: 0,
        }
    }

    #[test]
    fn lo_que_tengo_vencido_marca_vencido() {
        let yo = [7u8; 32];
        let mut inf = informe_base();
        inf.custodio_actual = Some(Actor::Identidad(yo));
        inf.plazo_actual = Some(1_000);
        // ahora = 2000 > plazo 1000 ⇒ vencido.
        let v = clasificar("Taladro", [9u8; 32], &inf, yo, 2_000).expect("me involucra");
        assert_eq!(v.lado, Lado::Tengo);
        assert!(v.vencido);
        assert_eq!(v.detalle, "devolución vencida");
    }

    #[test]
    fn lo_que_aporte_lo_tiene_otro() {
        let yo = [7u8; 32];
        let otro = [3u8; 32];
        let mut inf = informe_base();
        inf.custodio_actual = Some(Actor::Identidad(otro));
        // aportante == yo, custodio == otro ⇒ Aporte.
        let v = clasificar("Escalera", yo, &inf, yo, 100).expect("me involucra");
        assert_eq!(v.lado, Lado::Aporte);
        assert!(v.detalle.starts_with("lo tiene 030303"));
        assert!(!v.vencido);
    }

    #[test]
    fn objeto_ajeno_no_me_involucra() {
        let inf = {
            let mut i = informe_base();
            i.custodio_actual = Some(Actor::Identidad([1u8; 32]));
            i
        };
        // ni lo tengo ([7]) ni lo aporté ([2]).
        assert!(clasificar("Ajeno", [2u8; 32], &inf, [7u8; 32], 0).is_none());
    }

    #[test]
    fn manipulacion_dura_en_lo_mio_se_marca_pero_un_hueco_no() {
        let yo = [7u8; 32];
        // Firma inválida = manipulación dura ⇒ anomalía.
        let mut dura = informe_base();
        dura.anomalias.push(Anomalia::FirmaInvalida { indice: 0 });
        assert!(clasificar("Libro", yo, &dura, yo, 0).unwrap().anomalia);
        // Hueco de custodia = representable, NO dura ⇒ sin alarma.
        let mut hueco = informe_base();
        hueco.anomalias.push(Anomalia::HuecoDeCustodia {
            indice: 0,
            esperado: [0u8; 32],
            hallado: [1u8; 32],
        });
        assert!(!clasificar("Libro", yo, &hueco, yo, 0).unwrap().anomalia);
    }
}
