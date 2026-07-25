//! Triage semántico del historial de notificaciones.
//!
//! Tres pasos sobre la lista de [`Notificacion`]:
//! 1. **Agrupar** por similitud coseno de embeddings (`rimay-verbo`): 20
//!    "build failed" colapsan en un grupo. Es clustering *por significado*, no
//!    por string — mako/dunst agrupan con regex; aquí con embeddings.
//! 2. **Clasificar** cada grupo contra [`Regla`]s semánticas: cada regla es un
//!    ejemplo prototípico; si el representante del grupo se le parece (coseno ≥
//!    umbral), aplica su [`Accion`] (priorizar / silenciar / sugerir).
//! 3. **Resumir** cada grupo multi-ítem con un LLM (`pluma-llm`), con fallback
//!    heurístico si no hay LLM.
//!
//! Es una capa *aparte*: lee el historial por D-Bus (ver el binario), no toca
//! el daemon. Y *sugiere* — no auto-ejecuta acciones; eso queda detrás de
//! reglas explícitas que el usuario autorice más adelante.

use std::path::PathBuf;

use pluma_llm_core::{ChatClient, ChatRequest};
use rimay_verbo_core::Provider;
use serde::{Deserialize, Serialize};

use pata_notify::Notificacion;

/// Umbral de coseno para que dos notificaciones caigan en el mismo grupo.
/// Alto: solo junta lo que de verdad habla de lo mismo.
const UMBRAL_CLUSTER: f32 = 0.82;

/// Qué hacer con un grupo que matchea una regla. No incluye "agrupar": el
/// clustering es incondicional; las reglas solo tiñen el grupo resultante.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Accion {
    /// Fija la prioridad del grupo (0 baja … 2 crítica).
    Priorizar(u8),
    /// Marca el grupo como ruido (no se muestra por defecto).
    Silenciar,
    /// Adjunta una acción sugerida (texto), sin ejecutarla.
    Sugerir(String),
    /// Acción a ejecutar: un comando de shell. Solo corre si `autorizado` es
    /// `true` Y se invoca el triage con `--aplicar` — auto-acción explícita,
    /// nunca por defecto.
    Ejecutar { comando: String, autorizado: bool },
}

/// Una regla semántica: matchea por parecido a un ejemplo, no por patrón.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Regla {
    pub nombre: String,
    /// Texto prototípico contra el que se mide la similitud.
    pub ejemplo: String,
    /// Coseno mínimo para considerar que el grupo cae bajo esta regla.
    pub umbral: f32,
    pub accion: Accion,
}

/// Un grupo de notificaciones afines, ya clasificado.
#[derive(Debug, Clone)]
pub struct Grupo {
    /// Título del grupo: resumen del LLM o heurístico.
    pub titulo: String,
    pub items: Vec<Notificacion>,
    /// Prioridad efectiva (de una regla, o el máximo de urgencia del grupo).
    pub prioridad: u8,
    /// Reglas que matchearon (por nombre), para trazabilidad.
    pub reglas: Vec<String>,
    pub sugerencia: Option<String>,
    /// Comando a ejecutar `(comando, autorizado)`, si una regla `Ejecutar`
    /// matcheó. Solo corre con `aplicar` + `autorizado`.
    pub ejecutar: Option<(String, bool)>,
    pub silenciado: bool,
}

/// El resultado del triage.
#[derive(Debug, Clone, Default)]
pub struct Digest {
    pub grupos: Vec<Grupo>,
}

impl Digest {
    /// Grupos visibles (no silenciados), ordenados por prioridad descendente.
    pub fn visibles(&self) -> Vec<&Grupo> {
        let mut v: Vec<&Grupo> = self.grupos.iter().filter(|g| !g.silenciado).collect();
        v.sort_by(|a, b| b.prioridad.cmp(&a.prioridad));
        v
    }

    /// Cantidad de grupos silenciados (ruido).
    pub fn silenciados(&self) -> usize {
        self.grupos.iter().filter(|g| g.silenciado).count()
    }
}

/// Reglas por defecto. Los umbrales aplican contra embeddings reales (daemon
/// `verbo`); con el MockProvider no matchean (vectores ~ortogonales), así que
/// degradan a "solo clustering" — honesto: el valor semántico aparece con el
/// daemon vivo.
pub fn reglas_por_defecto() -> Vec<Regla> {
    vec![
        Regla {
            nombre: "build".into(),
            ejemplo: "la compilación falló, build failed, error de CI, tests rotos".into(),
            umbral: 0.55,
            accion: Accion::Sugerir("revisar el log del build".into()),
        },
        Regla {
            nombre: "mensaje".into(),
            ejemplo: "nuevo mensaje de chat, te escribieron, mensaje recibido".into(),
            umbral: 0.55,
            accion: Accion::Priorizar(2),
        },
        Regla {
            nombre: "ruido".into(),
            ejemplo: "sincronización completada, actualización disponible, paquete instalado".into(),
            umbral: 0.55,
            accion: Accion::Silenciar,
        },
    ]
}

/// Carga reglas del archivo del usuario, con fallback a [`reglas_por_defecto`].
/// El archivo es JSON en `$XDG_CONFIG_HOME/pata-notify/reglas.json`.
pub fn cargar_reglas() -> Vec<Regla> {
    match leer_reglas() {
        Some(reglas) => reglas,
        None => reglas_por_defecto(),
    }
}

/// Ruta del archivo de reglas del usuario.
pub fn ruta_reglas() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("pata-notify").join("reglas.json")
}

fn leer_reglas() -> Option<Vec<Regla>> {
    let ruta = ruta_reglas();
    let txt = std::fs::read_to_string(&ruta).ok()?;
    match serde_json::from_str::<Vec<Regla>>(&txt) {
        Ok(r) => {
            tracing::info!(reglas = r.len(), ruta = %ruta.display(), "reglas cargadas de archivo");
            Some(r)
        }
        Err(e) => {
            tracing::warn!(?e, ruta = %ruta.display(), "reglas.json inválido — uso las default");
            None
        }
    }
}

/// Ejecuta las acciones `Ejecutar` autorizadas de los grupos del digest. Es la
/// auto-acción explícita: solo corre aquí (no en `triage`), solo si la regla
/// está marcada `autorizado: true`. Devuelve un log de lo ejecutado.
pub fn aplicar(d: &Digest) -> Vec<String> {
    let mut log = Vec::new();
    for g in &d.grupos {
        if let Some((comando, autorizado)) = &g.ejecutar {
            if *autorizado {
                let estado = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(comando)
                    .status();
                match estado {
                    Ok(s) => log.push(format!("✓ ejecutado [{}]: {comando} ({s})", g.titulo)),
                    Err(e) => log.push(format!("✗ falló [{}]: {comando} ({e})", g.titulo)),
                }
            } else {
                log.push(format!("· omitido (no autorizado) [{}]: {comando}", g.titulo));
            }
        }
    }
    log
}

/// Texto representativo de una notificación para embeber.
fn texto(n: &Notificacion) -> String {
    format!("{} {} {}", n.app_name, n.summary, n.body)
        .trim()
        .to_string()
}

/// Corre el triage completo. `llm` es opcional: sin él, los títulos de grupo
/// son heurísticos.
pub async fn triage(
    historial: &[Notificacion],
    reglas: &[Regla],
    provider: &dyn Provider,
    llm: Option<&dyn ChatClient>,
) -> anyhow::Result<Digest> {
    if historial.is_empty() {
        return Ok(Digest::default());
    }

    // 1. Embeber cada notificación.
    let textos: Vec<String> = historial.iter().map(texto).collect();
    let embs = provider.embed_batch(&textos).await?;

    // 2. Clustering codicioso: cada ítem entra al primer grupo cuyo
    //    representante esté lo bastante cerca; si no, abre grupo nuevo.
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    let mut reps: Vec<usize> = Vec::new();
    for i in 0..historial.len() {
        let mut destino = None;
        for (c, &rep) in reps.iter().enumerate() {
            if embs[i].cosine(&embs[rep])? >= UMBRAL_CLUSTER {
                destino = Some(c);
                break;
            }
        }
        match destino {
            Some(c) => clusters[c].push(i),
            None => {
                reps.push(i);
                clusters.push(vec![i]);
            }
        }
    }

    // 3. Embeber los ejemplos de las reglas una sola vez.
    let regla_embs = if reglas.is_empty() {
        Vec::new()
    } else {
        let ejemplos: Vec<String> = reglas.iter().map(|r| r.ejemplo.clone()).collect();
        provider.embed_batch(&ejemplos).await?
    };

    // 4. Construir cada grupo: clasificar contra reglas + título.
    let mut grupos = Vec::with_capacity(clusters.len());
    for (c, miembros) in clusters.iter().enumerate() {
        let rep = reps[c];
        let items: Vec<Notificacion> = miembros.iter().map(|&i| historial[i].clone()).collect();

        // Prioridad base = máxima urgencia del grupo.
        let mut prioridad = items.iter().map(|n| n.urgency).max().unwrap_or(1);
        let mut sugerencia = None;
        let mut ejecutar = None;
        let mut silenciado = false;
        let mut matched = Vec::new();

        for (j, regla) in reglas.iter().enumerate() {
            if embs[rep].cosine(&regla_embs[j])? >= regla.umbral {
                matched.push(regla.nombre.clone());
                match &regla.accion {
                    Accion::Priorizar(p) => prioridad = prioridad.max(*p),
                    Accion::Silenciar => silenciado = true,
                    Accion::Sugerir(s) => sugerencia = Some(s.clone()),
                    Accion::Ejecutar { comando, autorizado } => {
                        ejecutar = Some((comando.clone(), *autorizado))
                    }
                }
            }
        }

        let titulo = titulo_grupo(&items, &textos, miembros, llm).await;

        grupos.push(Grupo {
            titulo,
            items,
            prioridad,
            reglas: matched,
            sugerencia,
            ejecutar,
            silenciado,
        });
    }

    Ok(Digest { grupos })
}

/// Título de un grupo. Singleton → su propio summary. Multi → resumen del LLM,
/// o heurístico si no hay LLM o la llamada falla.
async fn titulo_grupo(
    items: &[Notificacion],
    textos: &[String],
    miembros: &[usize],
    llm: Option<&dyn ChatClient>,
) -> String {
    if items.len() == 1 {
        return primer_no_vacio(&items[0]);
    }
    if let Some(llm) = llm {
        let cuerpo: String = miembros
            .iter()
            .map(|&i| textos[i].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if let Ok(resumen) = resumir(llm, &cuerpo).await {
            if !resumen.is_empty() {
                return resumen;
            }
        }
    }
    heuristico(items)
}

/// Pide al LLM una línea que resuma el grupo.
async fn resumir(llm: &dyn ChatClient, cuerpo: &str) -> anyhow::Result<String> {
    let req = ChatRequest::una_vuelta(
        format!("Resume en UNA línea (máx 8 palabras, español) este grupo de notificaciones:\n{cuerpo}"),
        60,
    )
    .con_sistema("Eres un triador de notificaciones. Responde sólo la línea, sin comillas ni prefijos.")
    .con_temperatura(0.2);
    let resp = llm.complete(&req).await?;
    Ok(resp.content.trim().lines().next().unwrap_or("").trim().to_string())
}

/// Título heurístico para un grupo multi-ítem sin LLM: "N× <app más común>".
fn heuristico(items: &[Notificacion]) -> String {
    use std::collections::HashMap;
    let mut conteo: HashMap<&str, usize> = HashMap::new();
    for n in items {
        let app = if n.app_name.trim().is_empty() {
            "notificaciones"
        } else {
            n.app_name.as_str()
        };
        *conteo.entry(app).or_default() += 1;
    }
    let app = conteo
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(a, _)| a)
        .unwrap_or("notificaciones");
    format!("{}× {}", items.len(), app)
}

/// El primer campo no vacío de una notificación (summary, si no body, si no app).
fn primer_no_vacio(n: &Notificacion) -> String {
    if !n.summary.trim().is_empty() {
        n.summary.clone()
    } else if !n.body.trim().is_empty() {
        n.body.clone()
    } else if !n.app_name.trim().is_empty() {
        n.app_name.clone()
    } else {
        "(sin texto)".to_string()
    }
}

/// Imprime el digest como texto (para el CLI; certificable sin GUI).
pub fn imprimir(d: &Digest) {
    let visibles = d.visibles();
    if visibles.is_empty() {
        println!("(nada que mostrar)");
    }
    for g in visibles {
        let sug = g
            .sugerencia
            .as_ref()
            .map(|s| format!("  → {s}"))
            .unwrap_or_default();
        let etiquetas = if g.reglas.is_empty() {
            String::new()
        } else {
            format!(" [{}]", g.reglas.join(","))
        };
        let conteo = if g.items.len() > 1 {
            format!(" ({})", g.items.len())
        } else {
            String::new()
        };
        println!("▾ P{} {}{}{}{}", g.prioridad, g.titulo, conteo, etiquetas, sug);
        if let Some((comando, autorizado)) = &g.ejecutar {
            let marca = if *autorizado { "⚙ autorizado" } else { "⚙ requiere autorización" };
            println!("    {marca}: {comando}");
        }
        for n in &g.items {
            let app = if n.app_name.trim().is_empty() {
                "—"
            } else {
                n.app_name.as_str()
            };
            println!("    · {app}: {}", n.summary);
        }
    }
    let ruido = d.silenciados();
    if ruido > 0 {
        println!("\n({ruido} grupo(s) silenciado(s) como ruido)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimay_verbo_mock::MockProvider;

    fn noti(id: u32, app: &str, summary: &str) -> Notificacion {
        Notificacion {
            id,
            app_name: app.into(),
            summary: summary.into(),
            body: String::new(),
            urgency: 1,
            actions: Vec::new(),
            timeout_ms: -1,
            created_usec: id as u64,
        }
    }

    /// Notificaciones idénticas (mismo texto → mismo vector mock) caen en un
    /// grupo; las distintas (vectores ~ortogonales) quedan aparte.
    #[tokio::test]
    async fn agrupa_por_similitud() {
        let provider = MockProvider::new(384);
        let hist = vec![
            noti(1, "ci", "build failed"),
            noti(2, "ci", "build failed"),
            noti(3, "ci", "build failed"),
            noti(4, "mail", "nuevo correo de Ana"),
            noti(5, "mail", "nuevo correo de Beto"),
        ];
        // Sin reglas ni LLM: clustering puro.
        let d = triage(&hist, &[], &provider, None).await.unwrap();
        // 3 builds idénticos → 1 grupo; 2 correos distintos → 2 grupos.
        assert_eq!(d.grupos.len(), 3);
        let mayor = d.grupos.iter().map(|g| g.items.len()).max().unwrap();
        assert_eq!(mayor, 3, "los tres builds idénticos deben agruparse");
    }

    #[tokio::test]
    async fn digest_vacio_sin_historial() {
        let provider = MockProvider::new(384);
        let d = triage(&[], &[], &provider, None).await.unwrap();
        assert!(d.grupos.is_empty());
    }

    #[tokio::test]
    async fn titulo_singleton_es_su_summary() {
        let provider = MockProvider::new(384);
        let hist = vec![noti(1, "ci", "deploy ok")];
        let d = triage(&hist, &[], &provider, None).await.unwrap();
        assert_eq!(d.grupos[0].titulo, "deploy ok");
    }
}
