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
    /// Diente desplegado **por monitor**: conector (`"DP-1"`) → `(surface_idx,
    /// tab_idx)`. Cada monitor expande lo suyo, independiente de los demás —
    /// sin entrada = rail colapsado en ese monitor. El backend winit (una sola
    /// pantalla) usa la clave `""`.
    pub open: HashMap<String, (usize, usize)>,
    /// Diente SELECCIONADO (resaltado en el rail) por monitor, independiente de
    /// si su panel está desplegado. En modo un-paso sigue a [`Self::open`]; en
    /// modo dos-pasos el primer click lo fija sin abrir el panel.
    pub active: HashMap<String, (usize, usize)>,
    /// Modo de visualización activo (compartido entre dientes).
    pub mode: NavMode,
    /// Nodo seleccionado (resaltado).
    pub selected: Option<NavId>,
    /// Nodos rama expandidos.
    pub expanded: HashSet<NavId>,
    /// Offset de scroll del panel (px).
    pub scroll: f32,
    /// Texto del **buscador jerárquico** del sidebar (vacío = idle). Localiza
    /// contenido en el panel activo (resalta nodos/dientes que matchean). Es la
    /// consulta que "las apps reciben" del marco.
    pub search: String,
    /// El buscador tiene el foco de teclado (las teclas van a `search`, no al
    /// panel). Lo prende un click en la caja de búsqueda; Esc lo suelta/limpia.
    pub search_focused: bool,
    /// El **popover multiswitch** del control mutable está desplegado (opciones
    /// excluyentes de disposición del sidebar agrupadas).
    pub control_open: bool,
    /// De QUÉ sidebar (`si`) son las opciones abiertas. Lo fija el toggle: así,
    /// aunque no haya ningún diente desplegado, el drawer de ESE sidebar se
    /// muestra para hostear la card de opciones como ventana propia (no clipeada
    /// por el rail fino). `None` cuando `control_open` es `false`.
    pub control_si: Option<usize>,
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
    /// **Instancia seleccionada** del perfil `pacha` en el panel del diente perfil
    /// (el tab que se está viendo). `None` = seguir la instancia activa. Es sólo
    /// UI: no activa el contexto (eso es [`crate::Msg::SwitchPacha`]).
    pub pacha_sel: Option<String>,
    /// **Menú contextual de una ventana** (taskbar del diente-escritorio) abierto:
    /// el popup flotante anclado al cursor con las acciones de taskbar (traer al
    /// frente, cerrar, minimizar, mover a escritorio…). `None` = sin menú. Vive en
    /// la surface del drawer (como el popover de disposición), por eso guarda el
    /// `si` del sidebar y la posición de anclaje. Ver [`WinMenu`].
    pub win_menu: Option<WinMenu>,
}

/// Estado del **menú contextual de una ventana** abierto sobre una fila del
/// taskbar de un diente-escritorio. Se pinta como popup flotante en la surface
/// del drawer, anclado donde se hizo clic-derecho (coords de la surface del
/// drawer, que [`crate::View::on_right_click_screen`] entrega).
#[derive(Clone, Debug, PartialEq)]
pub struct WinMenu {
    /// Sidebar (`si`) en cuyo drawer vive el menú — para restaurar su input-region.
    pub si: usize,
    /// Escritorio cuyo taskbar disparó el menú (para «Cerrar las demás»).
    pub ws: u8,
    /// Ventana objetivo del menú (id de mirada).
    pub win_id: u32,
    /// Título de la ventana (cabecera del menú).
    pub title: String,
    /// Ancla del popup en coords de la surface del drawer (x, y del cursor).
    pub x: f32,
    pub y: f32,
}

impl Default for NavState {
    fn default() -> Self {
        Self {
            open: HashMap::new(),
            active: HashMap::new(),
            mode: NavMode::Tree,
            selected: None,
            expanded: HashSet::new(),
            scroll: 0.0,
            search: String::new(),
            search_focused: false,
            control_open: false,
            control_si: None,
            roots: Vec::new(),
            targets: HashMap::new(),
            monads: Vec::new(),
            members: HashMap::new(),
            socket: None,
            error: None,
            menu: None,
            menu_options: Vec::new(),
            pacha_sel: None,
            win_menu: None,
        }
    }
}

impl NavState {
    /// El diente desplegado en el monitor `out`, si hay.
    pub fn open_en(&self, out: &str) -> Option<(usize, usize)> {
        self.open.get(out).copied()
    }

    /// El diente seleccionado en el monitor `out`, si hay.
    pub fn active_en(&self, out: &str) -> Option<(usize, usize)> {
        self.active.get(out).copied()
    }

    /// `true` si el diente `(si, ti)` está desplegado en **algún** monitor.
    pub fn is_open(&self, si: usize, ti: usize) -> bool {
        self.open.values().any(|&v| v == (si, ti))
    }

    /// `true` si el diente `(si, ti)` está SELECCIONADO en algún monitor.
    /// En modo un-paso coincide con [`Self::is_open`]; en modo dos-pasos puede
    /// estar seleccionado con el panel replegado (primer click).
    pub fn is_active(&self, si: usize, ti: usize) -> bool {
        self.active.values().any(|&v| v == (si, ti))
    }

    /// Activa/repliega el diente `(si, ti)` del monitor `out` en modo **un
    /// paso**: si ya estaba abierto ahí lo cierra, si no, lo abre (cerrando
    /// cualquier otro de ESE monitor — los demás monitores no se tocan).
    /// `active` sigue a `open`.
    pub fn toggle_tab(&mut self, out: &str, si: usize, ti: usize) {
        self.close_menu(); // un cambio de diente descarta el menú "Abrir con…"
        if self.open.get(out) == Some(&(si, ti)) {
            self.open.remove(out);
            self.active.remove(out);
        } else {
            self.open.insert(out.to_string(), (si, ti));
            self.active.insert(out.to_string(), (si, ti));
            self.scroll = 0.0;
        }
    }

    /// Activa el diente `(si, ti)` del monitor `out` respetando el modo **dos
    /// pasos**: con `dos_pasos = false` cae a [`Self::toggle_tab`] (un click
    /// abre). Con `dos_pasos = true`, el primer click sólo SELECCIONA el diente
    /// (sin desplegar el panel); un segundo click —ya seleccionado—
    /// despliega/repliega su panel. Así el diente funciona "como botón que sólo
    /// expande al tocarlo estando activo, y a la primera sólo lo activa".
    pub fn activate_tab(&mut self, out: &str, si: usize, ti: usize, dos_pasos: bool) {
        if !dos_pasos {
            self.toggle_tab(out, si, ti);
            return;
        }
        self.close_menu();
        if self.active.get(out) != Some(&(si, ti)) {
            // Primer paso: seleccionar sin desplegar.
            self.active.insert(out.to_string(), (si, ti));
            self.open.remove(out);
            self.scroll = 0.0;
        } else if self.open.get(out) == Some(&(si, ti)) {
            // Segundo paso, ya desplegado: replegar (sigue seleccionado).
            self.open.remove(out);
        } else {
            // Segundo paso, replegado: desplegar.
            self.open.insert(out.to_string(), (si, ti));
            self.scroll = 0.0;
        }
    }

    /// Localiza jerárquicamente el texto del buscador ([`Self::search`]) en el
    /// bosque: expande los ancestros de todo nodo cuyo label lo contiene
    /// (case-insensitive) y **selecciona el primero** para resaltarlo. Con
    /// búsqueda vacía no toca nada. La llaman los handlers al cambiar `search`.
    pub fn apply_search(&mut self) {
        let q = self.search.trim().to_lowercase();
        if q.is_empty() {
            return;
        }
        fn walk(
            node: &NavNode,
            q: &str,
            path: &mut Vec<NavId>,
            expand: &mut HashSet<NavId>,
            first: &mut Option<NavId>,
        ) {
            if node.label.to_lowercase().contains(q) {
                if first.is_none() {
                    *first = Some(node.id);
                }
                for a in path.iter() {
                    expand.insert(*a);
                }
            }
            path.push(node.id);
            for c in &node.children {
                walk(c, q, path, expand, first);
            }
            path.pop();
        }
        let mut expand = HashSet::new();
        let mut first = None;
        let mut path = Vec::new();
        for r in &self.roots {
            walk(r, &q, &mut path, &mut expand, &mut first);
        }
        for id in expand {
            self.expanded.insert(id);
        }
        if first.is_some() {
            self.selected = first;
        }
    }

    /// `true` si algún nodo del bosque matchea el buscador — para resaltar el
    /// diente que contiene coincidencias. Con búsqueda vacía es `false`.
    pub fn search_hits_roots(&self) -> bool {
        let q = self.search.trim().to_lowercase();
        if q.is_empty() {
            return false;
        }
        fn any(node: &NavNode, q: &str) -> bool {
            node.label.to_lowercase().contains(q) || node.children.iter().any(|c| any(c, q))
        }
        self.roots.iter().any(|r| any(r, &q))
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

    /// Cierra el menú "Abrir con…" y también el menú contextual de ventana (un
    /// cambio de diente debe descartar cualquier popup contextual pendiente).
    pub fn close_menu(&mut self) {
        self.menu = None;
        self.menu_options.clear();
        self.win_menu = None;
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
        st.toggle_tab("", 0, 0);
        assert!(st.is_open(0, 0));
        assert!(st.is_active(0, 0)); // un-paso: active sigue a open
        // Abrir otro diente cierra el anterior.
        st.toggle_tab("", 0, 1);
        assert!(st.is_open(0, 1));
        assert!(!st.is_open(0, 0));
        // Re-clic en el abierto lo cierra (y deselecciona).
        st.toggle_tab("", 0, 1);
        assert!(st.open.is_empty());
        assert!(st.active.is_empty());
    }

    #[test]
    fn activate_tab_un_paso_equivale_a_toggle() {
        let mut st = NavState::default();
        st.activate_tab("", 0, 0, false);
        assert!(st.is_open(0, 0) && st.is_active(0, 0));
        st.activate_tab("", 0, 0, false);
        assert!(st.open.is_empty());
    }

    #[test]
    fn activate_tab_dos_pasos_primero_selecciona_luego_despliega() {
        let mut st = NavState::default();
        // Primer click: SELECCIONA sin desplegar.
        st.activate_tab("", 0, 0, true);
        assert!(st.is_active(0, 0));
        assert!(!st.is_open(0, 0), "primer paso no despliega el panel");
        // Segundo click (ya activo): despliega.
        st.activate_tab("", 0, 0, true);
        assert!(st.is_open(0, 0));
        // Tercer click (activo y desplegado): repliega, sigue seleccionado.
        st.activate_tab("", 0, 0, true);
        assert!(!st.is_open(0, 0));
        assert!(st.is_active(0, 0), "replegar no deselecciona en dos-pasos");
        // Click en OTRO diente: sólo lo selecciona (no despliega), y el anterior
        // deja de estar activo.
        st.activate_tab("", 0, 1, true);
        assert!(st.is_active(0, 1) && !st.is_open(0, 1));
        assert!(!st.is_active(0, 0));
    }

    #[test]
    fn apply_search_expande_ancestros_y_selecciona_el_primero() {
        let mut st = NavState::default();
        st.roots = vec![NavNode::branch(
            1,
            "src",
            NavKind::Monad,
            vec![
                NavNode::leaf(11, "lib.rs", NavKind::File),
                NavNode::leaf(12, "main.rs", NavKind::File),
            ],
        )];
        // Sin búsqueda: no toca nada.
        st.apply_search();
        assert!(st.expanded.is_empty());
        assert_eq!(st.selected, None);
        // Buscar "main": expande el ancestro (1) y selecciona el match (12).
        st.search = "main".into();
        st.apply_search();
        assert!(st.expanded.contains(&1), "expande el ancestro del match");
        assert_eq!(st.selected, Some(12));
        assert!(st.search_hits_roots());
    }

    #[test]
    fn search_hits_roots_es_case_insensitive_y_vacio_no_matchea() {
        let mut st = NavState::default();
        st.roots = vec![NavNode::leaf(1, "README", NavKind::File)];
        assert!(!st.search_hits_roots(), "búsqueda vacía no matchea");
        st.search = "readme".into();
        assert!(st.search_hits_roots());
        st.search = "zzz".into();
        assert!(!st.search_hits_roots());
    }
}
