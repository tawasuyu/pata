//! `nouser` — el **plano de datos** del sidebar navegador (Fase 11c).
//!
//! El sidebar de `pata` muestra las **Mónadas** de nouser y sus archivos en un
//! navegador conmutable árbol/grafo ([`llimphi_widget_navigator`]). nouser es la
//! **fuente autoritativa** de qué archivos componen una Mónada (no el filesystem
//! por su cuenta — decisión del autor); por eso el nivel de archivos se resuelve
//! por el query de nouser (`chasqui_card::query`) y no leyendo directorios.
//!
//! Este módulo:
//! - descubre el socket del daemon (broker brahman → fallback al default path),
//!   igual que `chasqui-explorer-llimphi`;
//! - consulta `list_monads` (poll liviano) y `resolve_monad` (miembros bajo
//!   demanda al expandir una Mónada);
//! - construye el bosque de [`NavNode`]s que el widget pinta, manteniendo el
//!   estado de UI (modo, selección, expansión, diente desplegado) en el caller.
//!
//! La asignación de [`NavId`] es **determinista** (hash del `MonadId`/path) para
//! que la expansión y la selección sobrevivan a un re-poll sin parpadear.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use card_sidecar::{await_provider_blocking, build_consumer_card};
use chasqui_card::query::client as qclient;
use chasqui_card::query::{
    transport, FileView, ListMonadsResponse, MonadView, FLOW_MONAD_LIST, FLOW_TYPE_NAME,
};
use chasqui_card::MonadId;
use llimphi_widget_navigator::{NavId, NavKind, NavMode, NavNode};

/// Timeout para descubrir el provider por el broker.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
/// Timeout de un query single-shot al daemon.
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
/// Cada cuánto se repolea `list_monads`.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

// =====================================================================
// Mapeo NavId → qué representa
// =====================================================================

/// Qué representa un nodo del navegador, para resolver miembros (al expandir una
/// Mónada) y para abrir con la app que corresponda (Fase 11d).
#[derive(Debug, Clone)]
pub enum NavTarget {
    /// Una Mónada de nouser, por su id.
    Monad(MonadId),
    /// Un archivo miembro, por su ruta.
    File(String),
}

/// Hash FNV-1a de 64 bits — determinista y sin dependencias, suficiente para
/// derivar un [`NavId`] estable de un identificador opaco.
fn fnv1a(tag: u8, bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    h ^= tag as u64;
    h = h.wrapping_mul(0x0000_0100_0000_01b3);
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// [`NavId`] de una Mónada (tag 1).
fn monad_nav_id(id: &MonadId) -> NavId {
    fnv1a(1, &id.to_bytes())
}

/// [`NavId`] del placeholder "cargando…" de una Mónada aún no resuelta (tag 2).
fn placeholder_nav_id(id: &MonadId) -> NavId {
    fnv1a(2, &id.to_bytes())
}

/// [`NavId`] de un archivo, por su ruta (tag 3).
fn file_nav_id(path: &str) -> NavId {
    fnv1a(3, path.as_bytes())
}

/// El último componente de una ruta (su "nombre"), o la ruta entera si no tiene
/// separadores. Para la etiqueta de la fila — el path completo va al tooltip /
/// al abrir.
fn file_label(path: &str) -> String {
    path.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(path).to_string()
}

// =====================================================================
// Estado del navegador
// =====================================================================

/// Estado del sidebar navegador. Vive en el `Model` del frontend; el widget es
/// render-only y lo consulta cada `view`.
pub struct NavState {
    /// Diente desplegado: `(surface_idx, tab_idx)`. `None` = rail colapsado, sin
    /// panel.
    pub open: Option<(usize, usize)>,
    /// Modo de visualización activo (compartido entre dientes).
    pub mode: NavMode,
    /// Nodo seleccionado (resaltado).
    pub selected: Option<NavId>,
    /// Nodos rama expandidos.
    pub expanded: HashSet<NavId>,
    /// Offset de scroll del panel (px).
    pub scroll: f32,
    /// El bosque a pintar (Mónadas como raíces, archivos como hijos).
    pub roots: Vec<NavNode>,
    /// Qué representa cada [`NavId`] (para resolver/abrir).
    pub targets: HashMap<NavId, NavTarget>,
    /// Mónadas vivas del último poll (vista slim).
    monads: Vec<MonadView>,
    /// Miembros ya resueltos por Mónada (cache, llenado bajo demanda).
    members: HashMap<MonadId, Vec<FileView>>,
    /// Socket del daemon, cacheado entre polls (`None` fuerza re-descubrimiento).
    pub socket: Option<PathBuf>,
    /// Último error de descubrimiento/query (para mostrar en el panel).
    pub error: Option<String>,
    /// Menú "Abrir con…" abierto sobre un archivo (su [`NavId`]). `None` = sin
    /// menú. Las opciones se precomputan al abrirlo ([`NavState::open_menu`]) para
    /// que el render no toque el registro de apps.
    pub menu: Option<NavId>,
    /// Apps nativas que ofrece el menú abierto: `(app_id, label)`. El render las
    /// pinta como filas "Abrir con <label>"; siempre se les suma "el sistema".
    pub menu_options: Vec<(String, String)>,
}

impl Default for NavState {
    fn default() -> Self {
        Self {
            open: None,
            mode: NavMode::Tree,
            selected: None,
            expanded: HashSet::new(),
            scroll: 0.0,
            roots: Vec::new(),
            targets: HashMap::new(),
            monads: Vec::new(),
            members: HashMap::new(),
            socket: None,
            error: None,
            menu: None,
            menu_options: Vec::new(),
        }
    }
}

impl NavState {
    /// `true` si el diente `(si, ti)` está desplegado ahora.
    pub fn is_open(&self, si: usize, ti: usize) -> bool {
        self.open == Some((si, ti))
    }

    /// Activa/repliega el diente `(si, ti)`: si ya estaba abierto lo cierra, si
    /// no, lo abre (cerrando cualquier otro).
    pub fn toggle_tab(&mut self, si: usize, ti: usize) {
        self.close_menu(); // un cambio de diente descarta el menú "Abrir con…"
        if self.open == Some((si, ti)) {
            self.open = None;
        } else {
            self.open = Some((si, ti));
            self.scroll = 0.0;
        }
    }

    /// La ruta del archivo que representa `id`, si es un archivo. `None` para
    /// Mónadas (no tienen una ruta única).
    pub fn file_path(&self, id: NavId) -> Option<&str> {
        match self.targets.get(&id) {
            Some(NavTarget::File(p)) => Some(p.as_str()),
            _ => None,
        }
    }

    /// Abre el menú "Abrir con…" sobre `id` con las `options` (app_id, label) ya
    /// resueltas por el caller (que tiene el registro de apps).
    pub fn open_menu(&mut self, id: NavId, options: Vec<(String, String)>) {
        self.menu = Some(id);
        self.menu_options = options;
    }

    /// Cierra el menú "Abrir con…".
    pub fn close_menu(&mut self) {
        self.menu = None;
        self.menu_options.clear();
    }

    /// Si `id` es una Mónada todavía sin miembros resueltos, devuelve su id para
    /// que el caller dispare el `resolve_monad`. `None` en caso contrario.
    pub fn needs_resolve(&self, id: NavId) -> Option<MonadId> {
        match self.targets.get(&id) {
            Some(NavTarget::Monad(mid)) if !self.members.contains_key(mid) => Some(*mid),
            _ => None,
        }
    }

    /// Aplica una respuesta de `list_monads`: reemplaza la lista de Mónadas y
    /// reconstruye el bosque (preservando miembros ya resueltos).
    pub fn apply_monads(&mut self, resp: ListMonadsResponse) {
        self.monads = resp.monads;
        // Descarta del cache las Mónadas que ya no existen, para no acumular.
        let vivos: HashSet<MonadId> = self.monads.iter().map(|m| m.id).collect();
        self.members.retain(|id, _| vivos.contains(id));
        self.error = None;
        self.rebuild();
    }

    /// Aplica los miembros resueltos de una Mónada y reconstruye el bosque.
    pub fn apply_members(&mut self, monad: MonadId, members: Vec<FileView>) {
        self.members.insert(monad, members);
        self.rebuild();
    }

    /// Reconstruye `roots` + `targets` desde `monads` + `members`. Una Mónada con
    /// `cardinality > 0` aún no resuelta lleva un hijo placeholder "…" para que
    /// muestre el chevron y se pueda expandir (carga perezosa).
    fn rebuild(&mut self) {
        let mut roots = Vec::with_capacity(self.monads.len());
        let mut targets = HashMap::new();
        for mv in &self.monads {
            let mid = monad_nav_id(&mv.id);
            targets.insert(mid, NavTarget::Monad(mv.id));
            let children = if let Some(files) = self.members.get(&mv.id) {
                files
                    .iter()
                    .map(|f| {
                        let fid = file_nav_id(&f.path);
                        targets.insert(fid, NavTarget::File(f.path.clone()));
                        NavNode::leaf(fid, file_label(&f.path), NavKind::File)
                    })
                    .collect()
            } else if mv.cardinality > 0 {
                vec![NavNode::leaf(placeholder_nav_id(&mv.id), "…", NavKind::Other)]
            } else {
                Vec::new()
            };
            let label = if mv.label.is_empty() {
                "(sin nombre)".to_string()
            } else {
                mv.label.clone()
            };
            roots.push(NavNode::branch(mid, label, NavKind::Monad, children));
        }
        self.roots = roots;
        self.targets = targets;
    }
}

// =====================================================================
// Queries (corren en un thread vía Handle::spawn, no bloquean el UI)
// =====================================================================

/// Resultado de un poll de `list_monads`.
#[derive(Clone, Debug)]
pub enum PollOutcome {
    /// El daemon respondió: socket usado + Mónadas.
    Ok {
        socket: PathBuf,
        resp: Box<ListMonadsResponse>,
    },
    /// No se pudo descubrir/consultar; mensaje para el panel.
    Failed(String),
}

/// Descubre el socket (broker → fallback default path) y pide `list_monads`.
/// Reusa `prior_socket` si está cacheado (evita re-descubrir cada poll).
pub fn poll(prior_socket: Option<PathBuf>) -> PollOutcome {
    let socket = match prior_socket {
        Some(p) => p,
        None => match resolve_socket() {
            Ok(p) => p,
            Err(e) => return PollOutcome::Failed(e),
        },
    };
    match qclient::list_monads(&socket, QUERY_TIMEOUT) {
        Ok(resp) => PollOutcome::Ok {
            socket,
            resp: Box::new(resp),
        },
        Err(e) => PollOutcome::Failed(format!("query a {}: {e}", socket.display())),
    }
}

/// Resultado de resolver los miembros de una Mónada.
#[derive(Clone, Debug)]
pub enum MembersOutcome {
    Ok {
        monad: MonadId,
        members: Vec<FileView>,
    },
    Failed(String),
}

/// Pide los archivos miembros de `monad` al daemon en `socket`.
pub fn resolve(socket: PathBuf, monad: MonadId) -> MembersOutcome {
    match qclient::resolve_monad(&socket, monad, QUERY_TIMEOUT) {
        Ok(resp) => MembersOutcome::Ok {
            monad,
            members: resp.members,
        },
        Err(e) => MembersOutcome::Failed(format!("resolve_monad: {e}")),
    }
}

/// Resuelve el socket del daemon: primero el broker brahman (Card consumer +
/// `await_provider_blocking`), luego el default path si el broker no responde.
/// Idéntico a `chasqui-explorer-llimphi`.
fn resolve_socket() -> Result<PathBuf, String> {
    let card = build_consumer_card("pata-llimphi", FLOW_MONAD_LIST, FLOW_TYPE_NAME);
    match await_provider_blocking(card, DISCOVERY_TIMEOUT) {
        Ok(p) => Ok(p),
        Err(broker_err) => {
            let fallback = transport::default_socket_path();
            if fallback.exists() {
                Ok(fallback)
            } else {
                Err(format!(
                    "broker: {broker_err}; fallback {} no existe",
                    fallback.display()
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chasqui_card::query::EngineInfo;
    use ulid::Ulid;

    fn monad_view(label: &str, cardinality: u32) -> MonadView {
        MonadView {
            id: Ulid::new(),
            label: label.into(),
            summary: String::new(),
            keywords: Vec::new(),
            cardinality,
            entropy: 0.0,
            dominant_lens: Default::default(),
            path_hint: None,
            centroid_model: None,
        }
    }

    fn list_resp(monads: Vec<MonadView>) -> ListMonadsResponse {
        ListMonadsResponse {
            engine: EngineInfo {
                id: Ulid::new(),
                label: "test".into(),
                watching: None,
            },
            monads,
        }
    }

    #[test]
    fn nav_id_determinista_y_separado_por_tag() {
        let id = Ulid::new();
        assert_eq!(monad_nav_id(&id), monad_nav_id(&id));
        // El placeholder y la Mónada no colisionan (tags distintos).
        assert_ne!(monad_nav_id(&id), placeholder_nav_id(&id));
        // Dos rutas distintas → ids distintos.
        assert_ne!(file_nav_id("/a/x.rs"), file_nav_id("/a/y.rs"));
    }

    #[test]
    fn file_label_toma_el_ultimo_componente() {
        assert_eq!(file_label("/proj/src/lib.rs"), "lib.rs");
        assert_eq!(file_label("solo.txt"), "solo.txt");
        assert_eq!(file_label("/dir/"), "/dir/");
    }

    #[test]
    fn apply_monads_construye_raices_con_placeholder_si_hay_cardinalidad() {
        let mut st = NavState::default();
        let m = monad_view("src", 3);
        let mid = m.id;
        st.apply_monads(list_resp(vec![m]));
        assert_eq!(st.roots.len(), 1);
        // Aún sin resolver: tiene un hijo placeholder → chevron visible.
        assert!(st.roots[0].has_children());
        assert_eq!(st.roots[0].children.len(), 1);
        // La Mónada necesita resolverse al expandir.
        let nav = monad_nav_id(&mid);
        assert_eq!(st.needs_resolve(nav), Some(mid));
    }

    #[test]
    fn monada_vacia_no_tiene_chevron() {
        let mut st = NavState::default();
        st.apply_monads(list_resp(vec![monad_view("vacia", 0)]));
        assert!(!st.roots[0].has_children());
    }

    #[test]
    fn apply_members_reemplaza_placeholder_por_archivos() {
        let mut st = NavState::default();
        let m = monad_view("src", 2);
        let mid = m.id;
        st.apply_monads(list_resp(vec![m]));
        let files = vec![
            FileView {
                id: Ulid::new(),
                path: "/p/lib.rs".into(),
                size: 1,
                extension: Some("rs".into()),
                mtime_ms: 0,
            },
            FileView {
                id: Ulid::new(),
                path: "/p/main.rs".into(),
                size: 1,
                extension: Some("rs".into()),
                mtime_ms: 0,
            },
        ];
        st.apply_members(mid, files);
        assert_eq!(st.roots[0].children.len(), 2);
        assert_eq!(st.roots[0].children[0].label, "lib.rs");
        // Ya resuelta: no vuelve a pedir.
        assert_eq!(st.needs_resolve(monad_nav_id(&mid)), None);
        // El target del archivo apunta a su ruta completa.
        let fid = file_nav_id("/p/lib.rs");
        matches!(st.targets.get(&fid), Some(NavTarget::File(p)) if p == "/p/lib.rs");
    }

    #[test]
    fn apply_monads_descarta_miembros_de_monadas_muertas() {
        let mut st = NavState::default();
        let m = monad_view("a", 1);
        let mid = m.id;
        st.apply_monads(list_resp(vec![m]));
        st.apply_members(mid, vec![]);
        assert!(st.members.contains_key(&mid));
        // Re-poll sin esa Mónada: su cache se purga.
        st.apply_monads(list_resp(vec![monad_view("b", 1)]));
        assert!(!st.members.contains_key(&mid));
    }

    #[test]
    fn toggle_tab_abre_y_cierra() {
        let mut st = NavState::default();
        assert!(!st.is_open(0, 0));
        st.toggle_tab(0, 0);
        assert!(st.is_open(0, 0));
        // Abrir otro diente cierra el anterior.
        st.toggle_tab(0, 1);
        assert!(st.is_open(0, 1));
        assert!(!st.is_open(0, 0));
        // Re-clic en el abierto lo cierra.
        st.toggle_tab(0, 1);
        assert_eq!(st.open, None);
    }
}
