//! **Perfiles** montados como diente del panel: el primer grupo de dientes del
//! sidebar. Un perfil es un tipo de configuración con **instancias** con nombre
//! (siempre hay una activa); su diente despliega un panel cuyo "sidebar" son esas
//! instancias como tabs, y al seleccionar una se muestra su contenido.
//!
//! El primer perfil es `pacha` (**contextos de usuario**): cada instancia es un
//! contexto (`"oficina"`, `"juegos"`…). Este módulo es sólo el **plano de datos**
//! (lee `pachas.ron` + el estado vivo de `pacha list`); el render del panel vive
//! en [`crate::render::sidebar`]. La activación se delega al daemon
//! ([`crate::Msg::SwitchPacha`] → `pacha switch <id>`); aquí no se muta nada.

use std::collections::BTreeMap;
use std::path::PathBuf;

use pacha_core::{Catalog, FsHome, OnLeave};

/// Una **instancia** de un perfil `pacha` para el panel: identidad + estado vivo
/// (activa / ciclo de vida) + un resumen legible del contenido del contexto. Todo
/// derivado del catálogo (`pachas.ron`) y del marcador de `pacha list`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PachaInfo {
    /// Slug estable del contexto (`"oficina"`).
    pub id: String,
    /// Nombre visible.
    pub label: String,
    /// El contexto que el usuario está usando ahora (marcador `●` de `pacha list`).
    pub active: bool,
    /// Ciclo de vida legible (activo / en fondo / en pausa / apagado).
    pub lifecycle: String,
    /// Apps de la receta del contexto (comando + si va aislada de FS).
    pub apps: Vec<AppLinea>,
    /// Vista/keymap del compositor a aplicar, si el contexto la fija.
    pub vista: Option<String>,
    /// Overlay de config del SO: `(clave, valor)` legibles (tema/acento/idioma…).
    pub overlay: Vec<(String, String)>,
    /// Política de recursos (cgroups v2): `(clave, valor)` legibles.
    pub resources: Vec<(String, String)>,
    /// Qué pasa al dejar el contexto (fondo / pausa / cerrar).
    pub on_leave: String,
    /// Cuántos sets de dotfiles materializa al activarse.
    pub dotfiles: usize,
    /// Si snapshotea las apps vivas para reabrirlas la próxima vez.
    pub persist: bool,
}

/// Una app de la receta de un contexto, ya resumida para la UI.
#[derive(Clone, Debug, PartialEq)]
pub struct AppLinea {
    /// Comando completo (`"puriy --profile oficina"`).
    pub command: String,
    /// `true` si la app corre con `$HOME` aislado (tmpfs / dotfiles).
    pub aislada: bool,
}

/// Lee las instancias del perfil `pacha`: el catálogo (`pachas.ron`) joineado con
/// el estado vivo (`pacha list`). Tolerante: sin catálogo o sin binario devuelve
/// lo que haya (lista vacía si nada). El orden es el del catálogo.
pub fn read_pacha_infos() -> Vec<PachaInfo> {
    infos_from(&load_catalog(), &read_estado())
}

/// El join **puro** catálogo × estado vivo → instancias (testeable sin disco ni
/// subproceso). `estado` mapea `id → (activo, ciclo)`; los contextos ausentes del
/// estado quedan «apagado».
fn infos_from(cat: &Catalog, estado: &BTreeMap<String, (bool, String)>) -> Vec<PachaInfo> {
    cat.iter()
        .map(|p| {
            let (active, lifecycle) = estado
                .get(&p.id)
                .cloned()
                .unwrap_or((false, "apagado".to_string()));
            PachaInfo {
                id: p.id.clone(),
                label: if p.label.is_empty() { p.id.clone() } else { p.label.clone() },
                active,
                lifecycle,
                apps: p
                    .apps
                    .iter()
                    .map(|a| AppLinea {
                        command: a.command.clone(),
                        aislada: a
                            .fs_profile
                            .as_ref()
                            .map(|f| !matches!(f.home, FsHome::Heredar))
                            .unwrap_or(false),
                    })
                    .collect(),
                vista: p.vista.clone(),
                overlay: overlay_resumen(p.overlay.as_ref()),
                resources: resources_resumen(&p.resources),
                on_leave: on_leave_es(p.on_leave),
                dotfiles: p.dotfiles.len(),
                persist: p.persist,
            }
        })
        .collect()
}

/// Path del catálogo `~/.config/pacha/pachas.ron` (respeta `XDG_CONFIG_HOME`).
/// Espeja `pacha_manager::paths::catalog_path` sin arrastrar la dependencia.
fn catalog_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("pacha").join("pachas.ron"))
}

/// Carga el catálogo de contextos; vacío si no existe o no parsea (nunca falla).
fn load_catalog() -> Catalog {
    let Some(p) = catalog_path() else { return Catalog::new() };
    match std::fs::read_to_string(&p) {
        Ok(s) => Catalog::from_ron(&s).unwrap_or_else(|_| Catalog::new()),
        Err(_) => Catalog::new(),
    }
}

/// El estado vivo por contexto: `id → (activo, ciclo_de_vida)`, vía `pacha list`.
/// Formato de cada línea: `«● »? id  <Lifecycle>  label`. Tolerante: mapa vacío si
/// el binario/daemon no están.
fn read_estado() -> BTreeMap<String, (bool, String)> {
    let mut m = BTreeMap::new();
    let Ok(out) = std::process::Command::new("pacha").arg("list").output() else {
        return m;
    };
    if !out.status.success() {
        return m;
    }
    let texto = String::from_utf8_lossy(&out.stdout);
    for linea in texto.lines() {
        let activo = linea.trim_start().starts_with('●');
        // Tokens descartando el marcador: [id, lifecycle, label…].
        let mut toks = linea.split_whitespace().filter(|t| *t != "●");
        let Some(id) = toks.next() else { continue };
        let ciclo = toks.next().map(lifecycle_es).unwrap_or_else(|| "—".to_string());
        m.insert(id.to_string(), (activo, ciclo));
    }
    m
}

/// Traduce el `Debug` del `Lifecycle` de pacha (`Active`/`Background`/…) a español.
fn lifecycle_es(raw: &str) -> String {
    match raw {
        "Active" => "activo",
        "Background" => "en fondo",
        "Paused" => "en pausa",
        "Closed" => "apagado",
        otro => otro,
    }
    .to_string()
}

/// El `OnLeave` en español para el resumen del panel.
fn on_leave_es(v: OnLeave) -> String {
    match v {
        OnLeave::Background => "queda en fondo",
        OnLeave::Pause => "se pausa",
        OnLeave::Close => "se cierra",
    }
    .to_string()
}

/// Resumen legible del overlay de config (los campos que fija).
fn overlay_resumen(ov: Option<&pacha_core::WawaOverlay>) -> Vec<(String, String)> {
    let mut v = Vec::new();
    let Some(ov) = ov else { return v };
    if let Some(t) = &ov.theme_variant {
        v.push(("Tema".to_string(), t.clone()));
    }
    if let Some(a) = &ov.accent {
        v.push(("Acento".to_string(), a.clone()));
    }
    if let Some(l) = &ov.lang {
        v.push(("Idioma".to_string(), l.clone()));
    }
    if let Some(h24) = ov.timefmt_24h {
        v.push(("Reloj".to_string(), if h24 { "24 h".into() } else { "12 h".into() }));
    }
    if !ov.modules.is_empty() {
        v.push(("Módulos".to_string(), format!("{} ajustes", ov.modules.len())));
    }
    v
}

/// Resumen legible de la política de recursos (cgroups v2).
fn resources_resumen(r: &pacha_core::ResourcePolicy) -> Vec<(String, String)> {
    let mut v = Vec::new();
    if let Some(w) = r.cpu_weight {
        v.push(("CPU peso".to_string(), w.to_string()));
    }
    if let Some(w) = r.io_weight {
        v.push(("IO peso".to_string(), w.to_string()));
    }
    if let Some(m) = r.mem_max {
        v.push(("RAM máx".to_string(), format!("{} MiB", m / (1024 * 1024))));
    }
    if let Some(cores) = &r.cpu_affinity {
        v.push(("Cores".to_string(), format!("{} fijados", cores.len())));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_traduce_y_cae_a_original() {
        assert_eq!(lifecycle_es("Active"), "activo");
        assert_eq!(lifecycle_es("Background"), "en fondo");
        assert_eq!(lifecycle_es("Raro"), "Raro");
    }

    #[test]
    fn on_leave_en_espanol() {
        assert_eq!(on_leave_es(OnLeave::Background), "queda en fondo");
        assert_eq!(on_leave_es(OnLeave::Close), "se cierra");
    }

    #[test]
    fn overlay_solo_lista_lo_fijado() {
        let mut ov = pacha_core::WawaOverlay::default();
        assert!(overlay_resumen(Some(&ov)).is_empty());
        ov.accent = Some("#ff8800".to_string());
        let r = overlay_resumen(Some(&ov));
        assert_eq!(r, vec![("Acento".to_string(), "#ff8800".to_string())]);
    }

    #[test]
    fn join_catalogo_estado_marca_activo_y_resume() {
        let mut cat = Catalog::new();
        let mut oficina = pacha_core::Pacha::new("oficina", "Oficina");
        oficina.apps.push(pacha_core::AppSpec::new("puriy --profile oficina", "puriy"));
        oficina.vista = Some("mirada".to_string());
        cat.upsert(oficina);
        cat.upsert(pacha_core::Pacha::new("juegos", "Juegos"));

        let mut estado = BTreeMap::new();
        estado.insert("oficina".to_string(), (true, "activo".to_string()));

        let infos = infos_from(&cat, &estado);
        assert_eq!(infos.len(), 2);
        let of = infos.iter().find(|i| i.id == "oficina").unwrap();
        assert!(of.active);
        assert_eq!(of.lifecycle, "activo");
        assert_eq!(of.vista.as_deref(), Some("mirada"));
        assert_eq!(of.apps.len(), 1);
        // Un contexto ausente del estado vivo queda apagado, no activo.
        let jg = infos.iter().find(|i| i.id == "juegos").unwrap();
        assert!(!jg.active);
        assert_eq!(jg.lifecycle, "apagado");
    }

    #[test]
    fn resources_resume_pesos_y_ram() {
        let r = pacha_core::ResourcePolicy {
            cpu_weight: Some(500),
            mem_max: Some(2 * 1024 * 1024 * 1024),
            ..Default::default()
        };
        let s = resources_resumen(&r);
        assert!(s.contains(&("CPU peso".to_string(), "500".to_string())));
        assert!(s.contains(&("RAM máx".to_string(), "2048 MiB".to_string())));
    }
}
