//! **Ágora** — el sustrato de confianza, en la barra: el fantasma «¿Le creo?».
//!
//! Ágora es la red de confianza soberana: identidades Ed25519, avales de
//! terceros (atestaciones), rotaciones y **revocaciones** de claves. Este módulo
//! lee —sólo lectura— el grafo persistido en disco (`~/.local/share/agora/
//! graph.json`, vía `agora-store` que **re-verifica cada firma** al cargar) y la
//! **libreta de petnames** (`libreta.postcard`, tu mapeo privado de nombres
//! memorables) en un hilo lento, y arma un snapshot con lo honesto que se puede
//! decir **sin estado vivo de gossip**:
//!
//! - un resumen de tu red (a cuánta gente le pusiste nombre, cuántos avales hay);
//! - las **revocaciones**, resueltas a nombre — y en particular las claves
//!   **comprometidas** de gente que nombraste, que son la alerta soberana: «la
//!   clave de Fulano fue reportada comprometida, cuidado».
//!
//! El veredicto per-claim «¿le creo a Fulano que es partera?» es interactivo
//! (necesita elegir un aval y una política de lector) y vive en la **app ágora**;
//! el fantasma no lo finge — abre la app para eso. Aquí sólo narra hechos en disco
//! y alerta de lo que amerita.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use agora_core::identity::IdentityId;
use agora_core::lifecycle::{RevReason, Revocation};
use agora_graph::TrustGraph;
use agora_petnames::Libreta;

/// Cadencia del hilo (segundos): la confianza cambia con gestos humanos y
/// re-verificar el grafo (firmas Ed25519) no es gratis — no hace falta rápido.
const CADENCIA: Duration = Duration::from_secs(120);

/// **¿Le creo? per-persona M-de-N (PLAN-AYLLU B4).** ¿Avalan al menos `min` de
/// MIS avalistas (`avalistas`, mi "N") que `sujeto` cumple el claim `predicado =
/// valor`? Delega en [`TrustGraph::le_creo`] contra el grafo en disco de pata,
/// revocation-aware. Es el mismo verbo que la firma del ayllu (A2): ¿respaldan M
/// de mis N? pata ya no tiene que mandar SIEMPRE a `agora-app` para la pregunta
/// simple —la responde con su propio grafo—; la elección fina de política (qué
/// avalistas, qué umbral por-claim) sigue viviendo en la app ágora.
pub fn le_creo(
    grafo: &TrustGraph,
    sujeto: IdentityId,
    predicado: &str,
    valor: &str,
    avalistas: &[IdentityId],
    min: usize,
    ahora: u64,
) -> bool {
    grafo.le_creo(sujeto, predicado, valor, avalistas, min, ahora).creo
}

/// Una revocación resuelta a nombre, lista para pintar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevocacionVista {
    /// Nombre memorable (petname), o el `display_name` del grafo, o el hex corto.
    pub nombre: String,
    /// La nombraste vos (está en tu libreta) — te toca de cerca.
    pub conocido: bool,
    /// Motivo en castellano llano.
    pub motivo: &'static str,
    /// Clave comprometida (el ROJO): revocación por filtración/robo, no retiro.
    pub comprometida: bool,
    /// Rige ahora (permanente, o suspensión temporal aún no vencida).
    pub vigente: bool,
}

/// Lo que el render necesita: el resumen de la red + las revocaciones + la señal
/// de salience del fantasma.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgoraSnapshot {
    /// Identidades registradas en tu grafo.
    pub personas: usize,
    /// Avales (atestaciones) que conoce el grafo.
    pub avales: usize,
    /// Cuánta gente nombraste en tu libreta (petnames).
    pub conocidos: usize,
    /// Revocaciones resueltas a nombre — las comprometidas y vigentes, primero.
    pub revocaciones: Vec<RevocacionVista>,
    /// Hay al menos una clave **comprometida y vigente** (dispara el fantasma).
    pub hay_alerta: bool,
}

/// Motivo de revocación en castellano llano.
fn motivo_humano(r: RevReason) -> &'static str {
    match r {
        RevReason::Compromised => "clave comprometida",
        RevReason::Retired => "se retiró",
        RevReason::Superseded => "reemplazó su clave",
    }
}

/// ¿La revocación rige en `ahora`? Permanente (`expires_at` None) ⇒ desde
/// `issued_at`. Suspensión temporal ⇒ sólo hasta que vence.
fn vigente(rev: &Revocation, ahora: u64) -> bool {
    match rev.expires_at {
        None => ahora >= rev.issued_at,
        Some(t) => ahora >= rev.issued_at && ahora < t,
    }
}

/// Resuelve la clave revocada a un nombre memorable. Prefiere tu petname; si no,
/// el `display_name` del grafo; si no, el hex corto de la clave. Puro y testeable.
pub fn nombre_de_clave(target_key: &[u8; 32], graph: &TrustGraph, libreta: &Libreta) -> (String, bool) {
    let id = IdentityId::from_public_key(target_key);
    if let Some(n) = libreta.nombre_de(id) {
        return (n.to_string(), true);
    }
    if let Some(ident) = graph.identity(id) {
        if !ident.display_name.trim().is_empty() {
            return (ident.display_name.clone(), false);
        }
    }
    (
        format!("{:02x}{:02x}{:02x}…", target_key[0], target_key[1], target_key[2]),
        false,
    )
}

/// Arma una [`RevocacionVista`] a partir de una revocación cruda. Puro.
pub fn ver_revocacion(rev: &Revocation, graph: &TrustGraph, libreta: &Libreta, ahora: u64) -> RevocacionVista {
    let (nombre, conocido) = nombre_de_clave(&rev.target_key, graph, libreta);
    RevocacionVista {
        nombre,
        conocido,
        motivo: motivo_humano(rev.reason),
        comprometida: matches!(rev.reason, RevReason::Compromised),
        vigente: vigente(rev, ahora),
    }
}

/// El asa del bucle de pata: drena el último snapshot por frame.
pub struct AgoraHandle {
    rx: Receiver<AgoraSnapshot>,
    ultimo: Option<AgoraSnapshot>,
}

impl AgoraHandle {
    /// Arranca el hilo si el directorio de ágora **ya existe** (`~/.local/share/
    /// agora`, o `$AGORA_HOME`). `None` si ágora no está en uso — así la barra no
    /// le crea directorios ni le muestra un fantasma a quien no teje confianza.
    pub fn spawn() -> Option<Self> {
        let dir = agora_dir()?;
        if !dir.exists() {
            return None; // ágora no inicializada: sin fantasma de confianza
        }
        let (tx, rx): (Sender<AgoraSnapshot>, Receiver<AgoraSnapshot>) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("pata-agora".into())
            .spawn(move || bucle(tx, dir))
            .ok()?;
        Some(Self { rx, ultimo: None })
    }

    /// El último snapshot (retiene el previo si no llegó uno nuevo).
    pub fn latest(&mut self) -> Option<&AgoraSnapshot> {
        while let Ok(s) = self.rx.try_recv() {
            self.ultimo = Some(s);
        }
        self.ultimo.as_ref()
    }
}

/// El directorio de datos de ágora (`$AGORA_HOME` o `~/.local/share/agora`), el
/// mismo que abre la app ágora (`directories::ProjectDirs` en Linux resuelve ahí).
fn agora_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AGORA_HOME") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/agora"))
}

/// Segundos Unix de ahora.
fn ahora_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Construye el snapshot leyendo el grafo + la libreta. Ordena las revocaciones
/// poniendo primero las comprometidas y vigentes (la alerta). Puro salvo el I/O.
fn construir(dir: &PathBuf) -> AgoraSnapshot {
    let graph = agora_store::load(&dir.join("graph.json")).unwrap_or_else(|_| TrustGraph::new());
    let libreta = std::fs::read(dir.join("libreta.postcard"))
        .ok()
        .and_then(|b| postcard::from_bytes::<Libreta>(&b).ok())
        .unwrap_or_default();
    let ahora = ahora_unix();

    let mut revocaciones: Vec<RevocacionVista> = graph
        .revocations()
        .iter()
        .map(|r| ver_revocacion(r, &graph, &libreta, ahora))
        .collect();
    // Las comprometidas y vigentes primero; dentro, las de gente que nombraste
    // antes que las ajenas (te tocan de cerca).
    revocaciones.sort_by_key(|r| {
        let urgencia = if r.comprometida && r.vigente { 0 } else if r.vigente { 1 } else { 2 };
        let cercania = if r.conocido { 0 } else { 1 };
        (urgencia, cercania)
    });
    let hay_alerta = revocaciones.iter().any(|r| r.comprometida && r.vigente);

    AgoraSnapshot {
        personas: graph.identity_count(),
        avales: graph.attestation_count(),
        conocidos: libreta.len(),
        revocaciones,
        hay_alerta,
    }
}

/// El hilo: arma un snapshot y lo emite cada [`CADENCIA`].
fn bucle(tx: Sender<AgoraSnapshot>, dir: PathBuf) {
    loop {
        let snap = construir(&dir);
        if tx.send(snap).is_err() {
            return; // pata soltó el handle
        }
        std::thread::sleep(CADENCIA);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agora_core::identity::{Identity, IdentityKind, Keypair};
    use agora_core::lifecycle::Revocation;

    /// Una revocación permanente por compromiso de una clave arbitraria.
    fn rev_comprometida(pk: [u8; 32], issued_at: u64) -> Revocation {
        // `create` firma el canónico; el set autorizador da igual aquí (no
        // verificamos el umbral, sólo leemos los campos).
        let kp = Keypair::from_seed([9u8; 32]);
        Revocation::create(pk, RevReason::Compromised, issued_at, None, &[&kp])
    }

    #[test]
    fn le_creo_m_de_n_desde_el_grafo_de_pata() {
        use agora_core::{Attestation, Claim};
        let partera = Keypair::from_seed([50; 32]).identity_id();
        let a1 = Keypair::from_seed([60; 32]);
        let a2 = Keypair::from_seed([61; 32]);
        let ajeno = Keypair::from_seed([70; 32]);
        let mis_avalistas = [a1.identity_id(), a2.identity_id()];

        let mut g = TrustGraph::new();
        g.add_attestation(Attestation::create(&a1, Claim::new(partera, "oficio", "partera", 1))).unwrap();
        g.add_attestation(Attestation::create(&ajeno, Claim::new(partera, "oficio", "partera", 1))).unwrap();

        // Un solo avalista mío avaló → le creo con umbral 1, no con 2.
        assert!(le_creo(&g, partera, "oficio", "partera", &mis_avalistas, 1, 1_000));
        assert!(!le_creo(&g, partera, "oficio", "partera", &mis_avalistas, 2, 1_000));
        // Cuando a2 también avala, alcanzo el umbral 2.
        g.add_attestation(Attestation::create(&a2, Claim::new(partera, "oficio", "partera", 1))).unwrap();
        assert!(le_creo(&g, partera, "oficio", "partera", &mis_avalistas, 2, 1_000));
    }

    #[test]
    fn vigencia_permanente_y_temporal() {
        let permanente = rev_comprometida([1u8; 32], 100);
        assert!(vigente(&permanente, 100));
        assert!(vigente(&permanente, 10_000));
        assert!(!vigente(&permanente, 50)); // antes de issued_at

        // Suspensión temporal: rige sólo en la ventana [issued, expires).
        let kp = Keypair::from_seed([13u8; 32]);
        let temporal = Revocation::create([2u8; 32], RevReason::Retired, 100, Some(500), &[&kp]);
        assert!(vigente(&temporal, 300));
        assert!(!vigente(&temporal, 500)); // ya venció → la clave vuelve a valer
        assert!(!vigente(&temporal, 600));
    }

    #[test]
    fn nombre_prefiere_petname_luego_display_luego_hex() {
        let kp = Keypair::from_seed([13u8; 32]);
        let ident = Identity {
            kind: IdentityKind::Person,
            public_key: kp.public_key(),
            display_name: "Yumaira Soldadora".into(),
        };
        let id = ident.id();
        let mut graph = TrustGraph::new();
        graph.register(ident);

        // Sin petname: cae al display_name del grafo.
        let libreta = Libreta::nueva();
        let (n, conocido) = nombre_de_clave(&kp.public_key(), &graph, &libreta);
        assert_eq!(n, "Yumaira Soldadora");
        assert!(!conocido);

        // Con petname: gana el nombre memorable local y marca `conocido`.
        let mut libreta = Libreta::nueva();
        libreta.nombrar(id, "Yuma").unwrap();
        let (n, conocido) = nombre_de_clave(&kp.public_key(), &graph, &libreta);
        assert_eq!(n, "Yuma");
        assert!(conocido);

        // Clave desconocida (ni grafo ni libreta): hex corto, no conocido.
        let (n, conocido) = nombre_de_clave(&[0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], &graph, &libreta);
        assert_eq!(n, "abcdef…");
        assert!(!conocido);
    }

    #[test]
    fn alerta_solo_con_comprometida_vigente() {
        let kp = Keypair::from_seed([13u8; 32]);
        let ident = Identity {
            kind: IdentityKind::Person,
            public_key: kp.public_key(),
            display_name: "Alguien".into(),
        };
        let mut graph = TrustGraph::new();
        graph.register(ident);
        let libreta = Libreta::nueva();

        let comprometida = rev_comprometida(kp.public_key(), 100);
        let v = ver_revocacion(&comprometida, &graph, &libreta, 1_000);
        assert!(v.comprometida && v.vigente);
        assert_eq!(v.motivo, "clave comprometida");
    }
}
