//! El sidebar **RAG** sobre el correo: un diente del rail que pregunta a tu
//! correo y muestra una respuesta citada.
//!
//! La frontera es la misma de shuma (SDD §5): el marco (`pata`) provee el borde
//! del panel; el contenido —recuperar mails + redactar con un LLM— lo provee
//! [`paloma_rag::RagEngine`]. pata **no reimplementa** nada del RAG: arma el
//! motor en un hilo aparte (lee la caché de paloma de sólo-lectura), le rutea el
//! texto de la consulta y pinta el resultado que el motor devuelve por callback.
//!
//! El estado vive aquí ([`RagState`]) —es interacción, no modelo de dominio, así
//! que no va a `pata-core`—. El motor es pesado y opcional: se construye fuera
//! del hilo de UI y se guarda tras un `Mutex` compartido; mientras tanto el panel
//! muestra «armando…», y si falta una pieza (sin daemon, sin LLM, correo sin
//! indexar) queda en «no disponible» con la razón.

use std::sync::{Arc, Mutex};

use llimphi_theme::Theme;
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::llimphi_text::Alignment;
use llimphi_ui::View;

use rag_motor::{RagMotor, RagSource};

use crate::Msg;

/// El `kind` de contenido de un [`pata_core::SidebarTab`] que monta este panel.
/// Acepta también `"search"` por compatibilidad con la config vieja (el diente
/// «Buscar» que antes caía al navegador vacío).
pub fn is_rag_kind(kind: &str) -> bool {
    matches!(kind, "rag" | "search" | "ask" | "ai")
}

/// En qué punto del ciclo está el panel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RagStatus {
    /// El motor se está armando en un hilo de fondo.
    Building,
    /// No se pudo armar (sin daemon de embeddings, sin LLM, o correo sin indexar).
    Unavailable,
    /// Listo y a la espera de una consulta.
    Idle,
    /// Consulta en vuelo (embeber → recuperar → redactar).
    Asking,
    /// Hay una respuesta para mostrar.
    Ready,
}

/// El estado del sidebar RAG. El motor vive tras un `Arc<Mutex<…>>` porque se
/// construye en otro hilo y, una vez listo, lo usa el `update` para lanzar
/// consultas sin moverlo (`ask` sólo necesita `&self`).
pub struct RagState {
    /// `true` si la config declara algún diente RAG (si no, ni se arma el motor).
    pub present: bool,
    /// El motor, una vez armado. `None` mientras se arma o si no se pudo. Es un
    /// `Box<dyn RagMotor>`: la fuente (paloma sobre el correo, willay sobre los
    /// eventos) se elige por el prop `source` del diente — el panel es agnóstico.
    pub engine: Arc<Mutex<Option<Box<dyn RagMotor>>>>,
    /// Punto del ciclo (gobierna qué pinta el panel).
    pub status: RagStatus,
    /// Texto de la consulta que se está tipeando.
    pub query: String,
    /// La respuesta redactada (cuando `status == Ready`).
    pub answer: String,
    /// Las fuentes citadas, en el orden de los `[n]` de la respuesta.
    pub sources: Vec<RagSource>,
    /// Mensaje de error a mostrar (consulta fallida o motor no disponible).
    pub error: Option<String>,
    /// Cuántos mensajes hay en el corpus leído (para el subtítulo).
    pub corpus_len: usize,
}

impl Default for RagState {
    fn default() -> Self {
        Self {
            present: false,
            engine: Arc::new(Mutex::new(None)),
            status: RagStatus::Unavailable,
            query: String::new(),
            answer: String::new(),
            sources: Vec::new(),
            error: None,
            corpus_len: 0,
        }
    }
}

impl RagState {
    /// Estado inicial cuando la config declara un diente RAG: el motor todavía no
    /// está, así que arranca en «armando…».
    pub fn presente() -> Self {
        Self {
            present: true,
            status: RagStatus::Building,
            ..Self::default()
        }
    }
}

/// El **cuerpo** del diente RAG: buscador semántico + estado/respuesta +
/// fuentes. El cabezal de título uniforme lo antepone el marco (`panel_inner`);
/// aquí va sólo lo propio del panel. `panel_h` es el alto disponible del cuerpo.
pub fn body_view(state: &RagState, panel_h: f32, theme: &Theme) -> View<Msg> {
    let mut hijos = vec![input_box(state, theme)];

    // Cuerpo según el estado.
    match state.status {
        RagStatus::Building => hijos.push(nota("Armando el asistente…", theme.fg_muted, theme)),
        RagStatus::Unavailable => {
            let msg = state.error.clone().unwrap_or_else(|| {
                "Asistente no disponible. Abrí paloma para indexar tu correo y \
                 levanta el daemon de embeddings + un backend LLM."
                    .to_string()
            });
            hijos.push(nota(&msg, theme.fg_muted, theme));
        }
        RagStatus::Idle => {
            let sub = format!(
                "{} mensajes a mano. Pregunta lo que quieras sobre tu correo.",
                state.corpus_len
            );
            hijos.push(nota(&sub, theme.fg_muted, theme));
        }
        RagStatus::Asking => hijos.push(nota("Buscando y redactando…", theme.accent, theme)),
        RagStatus::Ready => {
            if let Some(e) = &state.error {
                hijos.push(nota(e, theme.fg_muted, theme));
            } else {
                hijos.push(answer_box(state, theme));
                if !state.sources.is_empty() {
                    hijos.push(sources_view(state, theme));
                }
            }
        }
    }

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: length(panel_h),
        },
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(10.0_f32),
            bottom: length(10.0_f32),
        },
        gap: Size {
            width: length(0.0_f32),
            height: length(8.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .children(hijos)
}

/// El buscador: una caja con la consulta y un cursor. El teclado lo rutea
/// `on_key` (cuando el panel está abierto) a `Msg::RagChar/RagBackspace/RagSubmit`.
fn input_box(state: &RagState, theme: &Theme) -> View<Msg> {
    let activo = matches!(state.status, RagStatus::Idle | RagStatus::Asking | RagStatus::Ready);
    let texto = if state.query.is_empty() {
        if activo {
            "Escribe tu pregunta… (Enter)".to_string()
        } else {
            "…".to_string()
        }
    } else {
        format!("{}▌", state.query)
    };
    let color = if state.query.is_empty() {
        theme.fg_muted
    } else {
        theme.fg_text
    };
    View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(40.0_f32),
        },
        flex_shrink: 0.0,
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel_alt)
    .radius(8.0)
    .text_aligned(texto, 13.0, color, Alignment::Start)
    // Click en la caja: si hay respuesta, la limpia para arrancar otra; si no,
    // no-op visual (el foco de teclado es de la app cuando el panel está abierto).
    .on_click(Msg::RagClear)
}

/// La caja de la respuesta redactada.
fn answer_box(state: &RagState, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: auto(),
        },
        flex_grow: 1.0,
        flex_basis: length(0.0_f32),
        min_size: Size {
            width: auto(),
            height: length(40.0_f32),
        },
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(8.0_f32),
            bottom: length(8.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel_alt)
    .radius(8.0)
    .text_aligned(state.answer.clone(), 13.0, theme.fg_text, Alignment::Start)
}

/// La lista de fuentes citadas: «[n] Asunto — Remitente».
fn sources_view(state: &RagState, theme: &Theme) -> View<Msg> {
    let mut filas: Vec<View<Msg>> = vec![View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(20.0_f32),
        },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text_aligned("Fuentes".to_string(), 11.0, theme.fg_muted, Alignment::Start)];

    for (i, src) in state.sources.iter().enumerate() {
        let remitente = src.from.split('<').next().unwrap_or(&src.from).trim().to_string();
        let etiqueta = format!("[{}] {} — {}", i + 1, src.subject, remitente);
        filas.push(
            View::new(Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: length(24.0_f32),
                },
                align_items: Some(AlignItems::Center),
                padding: TaffyRect {
                    left: length(8.0_f32),
                    right: length(8.0_f32),
                    top: length(0.0_f32),
                    bottom: length(0.0_f32),
                },
                ..Default::default()
            })
            .fill(theme.bg_panel_alt)
            .hover_fill(theme.bg_button_hover)
            .radius(6.0)
            .text_aligned(etiqueta, 12.0, theme.fg_text, Alignment::Start),
        );
    }

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: auto(),
        },
        flex_shrink: 0.0,
        gap: Size {
            width: length(0.0_f32),
            height: length(3.0_f32),
        },
        ..Default::default()
    })
    .children(filas)
}

/// Una nota centrada (estado/aviso) que llena el cuerpo del panel.
fn nota(texto: &str, color: llimphi_ui::llimphi_raster::peniko::Color, _theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: auto(),
        },
        flex_grow: 1.0,
        flex_basis: length(0.0_f32),
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        padding: TaffyRect {
            left: length(6.0_f32),
            right: length(6.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .text_aligned(texto.to_string(), 12.0, color, Alignment::Start)
}
