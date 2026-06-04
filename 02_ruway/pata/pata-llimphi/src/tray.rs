//! Bandeja del sistema (`tray`) vía **StatusNotifierItem**, el protocolo D-Bus de
//! KDE/freedesktop que usan los applets modernos (nm-applet, blueman, clientes de
//! chat…).
//!
//! pata actúa como **watcher + host**: posee el nombre well-known
//! `org.kde.StatusNotifierWatcher` y atiende a las apps que registran su item.
//! Como el bucle de pata es bloqueante (sctk) y zbus es async, el tray corre en su
//! **propio hilo** con un runtime tokio current-thread (el workspace fija zbus con
//! la feature `tokio`, no la blocking — mismo patrón que `mirada-portal`). Comparte
//! el snapshot de items con el bucle por `Arc<Mutex>` y recibe los clicks por un
//! canal (como el exec asíncrono del Quake).
//!
//! Alcance del MVP (todo runtime, no verificable sin un Hyprland real):
//! - **Enumera** los items y los **activa** al click (`Activate(0,0)`).
//! - **Íconos**: resuelve el `IconPixmap` (ARGB32 que manda la app por D-Bus) y,
//!   si no hay, busca el `IconName` como PNG en los directorios estándar de íconos
//!   (búsqueda acotada: hicolor + pixmaps, sólo PNG, sin parsear `index.theme` ni
//!   SVG). Si nada resuelve, cae a una etiqueta de texto (título o id).
//! - **No** emite las señales del watcher (sólo le importan a *otros* hosts) ni
//!   provee fallback si ya hay un watcher corriendo: si el nombre está tomado, el
//!   tray queda vacío y se loguea (el caso de pata como barra única no lo tiene).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use zbus::message::Header;
use zbus::{interface, proxy};

/// Un ícono ya decodificado a RGBA8, listo para que el render lo envuelva en una
/// `peniko::Image`. El render no toca D-Bus ni decodifica; sólo pinta.
#[derive(Clone, Debug)]
pub struct TrayIcon {
    /// Ancho en píxeles.
    pub width: u32,
    /// Alto en píxeles.
    pub height: u32,
    /// Píxeles RGBA8 (`width*height*4` bytes).
    pub rgba: Vec<u8>,
}

/// Lo que el render necesita de cada item del tray. `key` (`"bus|path"`) rutea la
/// activación de vuelta al hilo del tray.
#[derive(Clone, Debug)]
pub struct TrayItem {
    /// Clave estable `"bus|path"` para la activación.
    pub key: String,
    /// Texto a pintar (título, o id si no hay título).
    pub label: String,
    /// Estado SNI (`Active` / `Passive` / `NeedsAttention`).
    pub status: String,
    /// Ícono ya decodificado, o `None` si no se pudo resolver (cae a texto).
    pub icon: Option<TrayIcon>,
}

/// Estado compartido con la interfaz del watcher: los items registrados como
/// `(key, bus, path)`. Lo escribe la interfaz (en el runtime del tray) y lo lee el
/// bucle de refresco; de ahí el `Mutex`.
#[derive(Default)]
struct WatcherState {
    items: Vec<(String, String, String)>,
}

/// Órdenes del bucle de pata hacia el hilo del tray. (Soltar el [`TrayHandle`]
/// cierra el canal, lo que termina el hilo: no hace falta una variante de parada.)
enum TrayCmd {
    /// Activar el item con esa `key` (click).
    Activate(String),
}

/// El asa que el bucle de pata conserva: lee el snapshot de items y manda clicks.
pub struct TrayHandle {
    items: Arc<Mutex<Vec<TrayItem>>>,
    tx: mpsc::UnboundedSender<TrayCmd>,
}

impl TrayHandle {
    /// Arranca el hilo del tray. Devuelve `None` sólo si no se pudo lanzar el hilo;
    /// la conexión D-Bus se intenta dentro (si falla, el hilo termina y el tray
    /// queda vacío, sin romper la barra).
    pub fn spawn() -> Option<Self> {
        let items: Arc<Mutex<Vec<TrayItem>>> = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::unbounded_channel::<TrayCmd>();
        let items_hilo = items.clone();
        std::thread::Builder::new()
            .name("pata-tray".into())
            .spawn(move || run_tray(items_hilo, rx))
            .ok()?;
        Some(Self { items, tx })
    }

    /// El snapshot actual de items para el render.
    pub fn items(&self) -> Vec<TrayItem> {
        self.items.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Pide activar el item `key` (no bloquea; el hilo del tray hace la llamada).
    pub fn activate(&self, key: String) {
        let _ = self.tx.send(TrayCmd::Activate(key));
    }
}

/// La interfaz `org.kde.StatusNotifierWatcher` que pata expone. Las apps llaman a
/// `RegisterStatusNotifierItem`; guardamos el item normalizado en el estado
/// compartido. Métodos síncronos: zbus los atiende en el runtime del tray.
struct Watcher {
    state: Arc<Mutex<WatcherState>>,
}

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    /// Una app registra su item. `service` puede ser un nombre de bus, una ruta de
    /// objeto (con el bus = remitente) o la forma combinada `"bus/path"`.
    fn register_status_notifier_item(&self, service: &str, #[zbus(header)] hdr: Header<'_>) {
        let sender = hdr.sender().map(|s| s.to_string());
        if let Some((bus, path)) = split_service(service, sender.as_deref()) {
            let key = format!("{bus}|{path}");
            let mut st = self.state.lock().unwrap();
            if !st.items.iter().any(|(k, _, _)| *k == key) {
                st.items.push((key, bus, path));
            }
        }
    }

    /// Otro host se registra. pata es su propio host, así que no hace falta nada.
    fn register_status_notifier_host(&self, _service: &str) {}

    /// La lista de items registrados, en la forma `"bus/path"` que esperan los
    /// hosts que consulten el watcher.
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .items
            .iter()
            .map(|(_, b, p)| format!("{b}{p}"))
            .collect()
    }

    /// Siempre `true`: pata provee el host, así que las apps deben usar SNI.
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    /// Versión del protocolo (0, como la implementación de referencia).
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
}

/// Cliente del item de una app: leemos sus atributos para pintarlo y lo activamos
/// al click.
#[proxy(interface = "org.kde.StatusNotifierItem", assume_defaults = false)]
trait StatusNotifierItem {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn title(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn icon_name(&self) -> zbus::Result<String>;
    /// Íconos embebidos: lista de `(ancho, alto, ARGB32)`. La app los manda por el
    /// bus; no requieren buscar en el tema.
    #[zbus(property)]
    fn icon_pixmap(&self) -> zbus::Result<Vec<(i32, i32, Vec<u8>)>>;
    #[zbus(property)]
    fn status(&self) -> zbus::Result<String>;
    /// Click primario sobre el item.
    fn activate(&self, x: i32, y: i32) -> zbus::Result<()>;
}

/// El hilo del tray: levanta un runtime tokio current-thread y corre el bucle
/// async. Si no hay runtime o D-Bus, termina (tray vacío).
fn run_tray(items: Arc<Mutex<Vec<TrayItem>>>, rx: mpsc::UnboundedReceiver<TrayCmd>) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    rt.block_on(bucle_tray(items, rx));
}

/// El bucle async: arma el watcher y, hasta que cierren el canal, atiende los
/// clicks (respuesta inmediata) o refresca el snapshot de items cada ~1s.
async fn bucle_tray(items: Arc<Mutex<Vec<TrayItem>>>, mut rx: mpsc::UnboundedReceiver<TrayCmd>) {
    let state = Arc::new(Mutex::new(WatcherState::default()));
    let Some(conn) = build_watcher(state.clone()).await else {
        return; // sin D-Bus o nombre tomado: tray vacío
    };
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(TrayCmd::Activate(key)) => activar(&conn, &state, &key).await,
                None => break, // se soltó el TrayHandle
            },
            _ = tick.tick() => {}
        }
        refrescar(&conn, &state, &items).await;
    }
}

/// Construye la conexión de sesión sirviendo el watcher y tomando su nombre.
/// `None` si no hay bus de sesión o el nombre ya está tomado por otro watcher.
async fn build_watcher(state: Arc<Mutex<WatcherState>>) -> Option<zbus::Connection> {
    let res = zbus::connection::Builder::session()
        .ok()?
        .serve_at("/StatusNotifierWatcher", Watcher { state })
        .ok()?
        .name("org.kde.StatusNotifierWatcher")
        .ok()?
        .build()
        .await;
    match res {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("pata tray · no se pudo ser StatusNotifierWatcher ({e}); ¿ya hay uno? tray vacío");
            None
        }
    }
}

/// Reconstruye el snapshot de items leyendo cada uno por D-Bus; poda los que ya no
/// responden (su app se cerró).
async fn refrescar(
    conn: &zbus::Connection,
    state: &Arc<Mutex<WatcherState>>,
    items_out: &Arc<Mutex<Vec<TrayItem>>>,
) {
    let registrados = state.lock().unwrap().items.clone();
    let mut snapshot = Vec::new();
    let mut vivos = Vec::new();
    for (key, bus, path) in registrados {
        if let Some((label, status, icon)) = leer_item(conn, &bus, &path).await {
            snapshot.push(TrayItem {
                key: key.clone(),
                label,
                status,
                icon,
            });
            vivos.push((key, bus, path));
        }
    }
    state.lock().unwrap().items = vivos;
    *items_out.lock().unwrap() = snapshot;
}

/// Lee `(label, status, icon)` de un item. La etiqueta es el título, o el id, o el
/// nombre del ícono —lo primero no vacío—. El ícono sale del `IconPixmap` o, si no,
/// del `IconName` resuelto a PNG. `None` si no se puede leer ni el label ni el id:
/// la app se fue y hay que podar el item.
async fn leer_item(
    conn: &zbus::Connection,
    bus: &str,
    path: &str,
) -> Option<(String, String, Option<TrayIcon>)> {
    let proxy = item_proxy(conn, bus, path).await?;
    let title = proxy.title().await.ok().filter(|s| !s.is_empty());
    let id = proxy.id().await.ok().filter(|s| !s.is_empty());
    let icon_name = proxy.icon_name().await.ok().filter(|s| !s.is_empty());
    let label = title.or(id).or_else(|| icon_name.clone())?;
    let status = proxy
        .status()
        .await
        .ok()
        .unwrap_or_else(|| "Active".to_string());

    // Ícono: primero el pixmap embebido (no necesita tema); si no, el nombre.
    let icon = proxy
        .icon_pixmap()
        .await
        .ok()
        .and_then(pixmap_a_icono)
        .or_else(|| icon_name.as_deref().and_then(buscar_icono_png));

    Some((label, status, icon))
}

/// Activa (click primario) el item con esa `key`, si sigue registrado.
async fn activar(conn: &zbus::Connection, state: &Arc<Mutex<WatcherState>>, key: &str) {
    let reg = state
        .lock()
        .unwrap()
        .items
        .iter()
        .find(|(k, _, _)| k == key)
        .cloned();
    if let Some((_, bus, path)) = reg {
        if let Some(proxy) = item_proxy(conn, &bus, &path).await {
            let _ = proxy.activate(0, 0).await;
        }
    }
}

/// Arma un proxy al item de `bus`/`path`.
async fn item_proxy<'a>(
    conn: &zbus::Connection,
    bus: &str,
    path: &str,
) -> Option<StatusNotifierItemProxy<'a>> {
    StatusNotifierItemProxy::builder(conn)
        .destination(bus.to_string())
        .ok()?
        .path(path.to_string())
        .ok()?
        .build()
        .await
        .ok()
}

/// Tamaño de ícono que preferimos para la barra (px). Elegimos el pixmap más
/// cercano a esto para no clonar mapas de 256px en cada frame.
const ICONO_OBJETIVO: i64 = 24;

/// Convierte la lista de `IconPixmap` (ARGB32) en un [`TrayIcon`] RGBA8. Elige el
/// pixmap de tamaño más cercano a [`ICONO_OBJETIVO`] entre los válidos. `None` si
/// la lista viene vacía o ningún entry tiene datos consistentes.
fn pixmap_a_icono(pixmaps: Vec<(i32, i32, Vec<u8>)>) -> Option<TrayIcon> {
    let (w, h, data) = pixmaps
        .into_iter()
        .filter(|(w, h, d)| {
            *w > 0 && *h > 0 && d.len() >= (*w as usize) * (*h as usize) * 4
        })
        .min_by_key(|(w, _, _)| (*w as i64 - ICONO_OBJETIVO).abs())?;
    let (w, h) = (w as u32, h as u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    // ARGB32 en orden de red (big-endian) → en memoria los bytes son [A,R,G,B];
    // peniko espera RGBA8, así que reordenamos a [R,G,B,A].
    for px in data.chunks_exact(4) {
        rgba.extend_from_slice(&[px[1], px[2], px[3], px[0]]);
    }
    Some(TrayIcon {
        width: w,
        height: h,
        rgba,
    })
}

/// Busca el `IconName` como PNG en los directorios estándar y lo decodifica. Si el
/// nombre ya es una ruta absoluta, la usa directo. Búsqueda acotada: hicolor (por
/// tamaños) + `/usr/share/pixmaps`, sólo PNG, sin `index.theme` ni SVG.
fn buscar_icono_png(name: &str) -> Option<TrayIcon> {
    if name.is_empty() {
        return None;
    }
    if name.starts_with('/') {
        return decodificar_png(&PathBuf::from(name));
    }
    for ruta in rutas_candidatas(name) {
        if let Some(ic) = decodificar_png(&ruta) {
            return Some(ic);
        }
    }
    None
}

/// Las rutas PNG donde puede vivir un ícono temático `name`, en orden de
/// preferencia (tamaños chicos primero, para una barra).
fn rutas_candidatas(name: &str) -> Vec<PathBuf> {
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        bases.push(PathBuf::from(&home).join(".local/share"));
        bases.push(PathBuf::from(&home).join(".icons"));
    }
    let datadirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    bases.extend(datadirs.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));

    let sizes = ["32x32", "24x24", "22x22", "16x16", "48x48"];
    let mut rutas = Vec::new();
    for base in &bases {
        for size in sizes {
            rutas.push(
                base.join("icons/hicolor")
                    .join(size)
                    .join("apps")
                    .join(format!("{name}.png")),
            );
        }
    }
    // Último recurso: el cajón plano de pixmaps.
    rutas.push(PathBuf::from(format!("/usr/share/pixmaps/{name}.png")));
    rutas
}

/// Decodifica un PNG a [`TrayIcon`] RGBA8, o `None` si no existe / no es válido.
fn decodificar_png(ruta: &std::path::Path) -> Option<TrayIcon> {
    let img = image::open(ruta).ok()?.to_rgba8();
    let (width, height) = (img.width(), img.height());
    Some(TrayIcon {
        width,
        height,
        rgba: img.into_raw(),
    })
}

/// Normaliza el argumento de `RegisterStatusNotifierItem` a `(bus, path)`:
/// - empieza con `/` → es una ruta de objeto, el bus es el remitente (Ayatana);
/// - tiene un `/` interno → forma combinada `"bus/path"`;
/// - si no → es un nombre de bus, con la ruta por defecto `/StatusNotifierItem` (KDE).
fn split_service(service: &str, sender: Option<&str>) -> Option<(String, String)> {
    if service.starts_with('/') {
        Some((sender?.to_string(), service.to_string()))
    } else if let Some(idx) = service.find('/') {
        Some((service[..idx].to_string(), service[idx..].to_string()))
    } else {
        Some((service.to_string(), "/StatusNotifierItem".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_service_ruta_usa_el_remitente_como_bus() {
        // Ayatana/AppIndicator: registra la ruta, el bus es el remitente.
        assert_eq!(
            split_service("/org/ayatana/NotificationItem/app", Some(":1.42")),
            Some((
                ":1.42".to_string(),
                "/org/ayatana/NotificationItem/app".to_string()
            ))
        );
        // Ruta sin remitente conocido: no se puede ubicar.
        assert_eq!(split_service("/foo", None), None);
    }

    #[test]
    fn split_service_nombre_de_bus_usa_ruta_por_defecto() {
        // KDE: registra el nombre de bus, ruta por defecto.
        assert_eq!(
            split_service("org.kde.StatusNotifierItem-1234-1", Some(":1.9")),
            Some((
                "org.kde.StatusNotifierItem-1234-1".to_string(),
                "/StatusNotifierItem".to_string()
            ))
        );
    }

    #[test]
    fn split_service_forma_combinada_se_parte() {
        assert_eq!(
            split_service(":1.9/StatusNotifierItem", None),
            Some((":1.9".to_string(), "/StatusNotifierItem".to_string()))
        );
    }
}
