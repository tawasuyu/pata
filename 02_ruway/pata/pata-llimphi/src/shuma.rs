//! El `shuma_input` y su despliegue **Quake**.
//!
//! La frontera del SDD §5: el marco (`pata`) provee el borde; `shuma` provee el
//! contenido. `shuma_input` es el cabezal que vive en una barra; al activarlo
//! (click o hotkey) el frontend **despliega un drawer** estilo Quake sobre el
//! escritorio, con un input que captura el teclado. Repliega al cerrar.
//!
//! La ejecución del comando es, estrictamente, trabajo de `shuma` (no de
//! `pata`). El puente del SDD §5 ya existe: [`ejecutar`] corre el comando por
//! el **ejecutor real de shuma** (`shuma-exec`) —captura acotada, eventos en
//! streaming— en vez de un `sh -c` pelado. El mecanismo del drawer no cambia.

use llimphi_motion::Tween;
use llimphi_theme::Theme;
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::View;

use llimphi_widget_scroll::{clamp_offset, scroll_y, ScrollPalette};
use pata_core::WidgetSpec;
use shuma_exec::StageSpec;
use shuma_line::{split_pipeline, tokenize, Dialect, TokenKind};

use crate::Msg;

/// Alto máximo del drawer, como fracción de la pantalla.
const DRAWER_FRAC: f32 = 0.45;

/// Una línea de salida con su naturaleza, para colorearla en la card.
#[derive(Clone, Debug)]
pub struct OutLine {
    /// `true` si vino por stderr (se pinta en color de error).
    pub err: bool,
    pub text: String,
}

/// Una *card* del drawer: un comando ejecutado con sus etapas de pipe, su
/// salida y su código de salida. Es el modelo de paridad con el shell de
/// shuma (cards + etapas clickeables), que el render del marco pinta.
#[derive(Clone, Debug)]
pub struct DrawerBlock {
    /// La línea tal cual se tecleó.
    pub cmd: String,
    /// Etiquetas de cada etapa del pipe (de `shuma-line`) — chips clickeables
    /// que revelan la salida capturada (tee) de esa etapa intermedia.
    pub stages: Vec<String>,
    /// Líneas de salida (stdout/stderr entremezcladas en orden de llegada).
    /// Es la salida de la **última** etapa del pipe.
    pub lines: Vec<OutLine>,
    /// Salida capturada de cada etapa **intermedia** del pipe (tee), indexada
    /// por etapa. La última etapa no tiene entrada propia: su stdout es
    /// `lines`. Vacío si la línea no corrió como pipe directo simple.
    pub stage_lines: Vec<Vec<OutLine>>,
    /// Etapa cuya salida capturada está revelada inline (`None` = ninguna).
    pub expanded_stage: Option<usize>,
    /// Código de salida; `None` mientras el comando sigue corriendo.
    pub exit: Option<i32>,
    /// `true` si la card está plegada (sólo se ve el encabezado).
    pub collapsed: bool,
    /// `true` si la card es una consulta a la **IA** (no un comando shell): el
    /// encabezado muestra `✦ <prompt>` sin chips de etapa y el cuerpo es la
    /// respuesta del modelo. Lo setea [`ShumaState::push_pending_ia`].
    pub is_ia: bool,
}

/// El resultado estructurado de una corrida — lo que un hilo de fondo manda de
/// vuelta para rellenar la card pendiente.
#[derive(Clone, Debug)]
pub struct RunResult {
    pub lines: Vec<OutLine>,
    pub exit: Option<i32>,
    /// Salida capturada por etapa intermedia (tee), indexada por etapa. Vacío
    /// si la línea no corrió como pipe directo simple (cayó a `sh -c`).
    pub stages: Vec<Vec<OutLine>>,
}

/// Las etiquetas de las etapas de pipe de una línea (vía `shuma-line`): el
/// `comando` de cada etapa, o el texto crudo si no se reconoció. Vacío si la
/// línea no tiene pipe (una sola etapa no amerita chips).
pub fn stage_labels(cmd: &str) -> Vec<String> {
    let pipeline = split_pipeline(&tokenize(cmd, Dialect::Bash));
    if pipeline.stages.len() < 2 {
        return Vec::new();
    }
    pipeline
        .stages
        .iter()
        .map(|s| {
            s.command.clone().unwrap_or_else(|| {
                // Sin comando reconocido: el primer argumento, o «·».
                s.args.first().cloned().unwrap_or_else(|| "·".into())
            })
        })
        .collect()
}

/// Si `cmd` es un pipe «simple» (sólo comandos/args/flags y `|`, sin comillas,
/// variables, redirecciones, globs ni `~`), lo descompone en [`StageSpec`] para
/// correrlo por `Exec::Direct` con **captura por etapa** (tee). Si no, `None`
/// (cae a `sh -c`, sin tee). Espeja `shuma-module-shell::simple_pipe_stages`:
/// cualquier sintaxis que el modo directo no absorbe va al shell.
pub fn simple_pipe_stages(cmd: &str) -> Option<Vec<StageSpec>> {
    let tokens = tokenize(cmd, Dialect::Bash);
    let simple = !tokens.is_empty()
        && tokens.iter().all(|t| {
            matches!(
                t.kind,
                TokenKind::Command
                    | TokenKind::Argument
                    | TokenKind::Flag
                    | TokenKind::Pipe
                    | TokenKind::Whitespace
            ) && !t.text.contains(['*', '?', '[', ']', '{', '}'])
                && !t.text.starts_with('~')
        });
    if !simple {
        return None;
    }
    let pipeline = split_pipeline(&tokens);
    if pipeline.stages.len() < 2 {
        return None;
    }
    let mut stages = Vec::with_capacity(pipeline.stages.len());
    for st in &pipeline.stages {
        // Una etapa sin comando (línea incompleta, p. ej. termina en `|`)
        // → al shell, que reporta el error de sintaxis como toca.
        let program = st.command.clone()?;
        stages.push(StageSpec { program, args: st.args.clone() });
    }
    Some(stages)
}

/// El estado del cabezal del shell y su drawer. Vive en el `Model` del frontend
/// —es interacción, no modelo de dominio—, no en `pata-core`.
pub struct ShumaState {
    /// `true` cuando el drawer está desplegado.
    pub open: bool,
    /// El comando que se está escribiendo.
    pub buffer: String,
    /// Historial de comandos del drawer, uno por *card* — paridad con el
    /// shell de shuma (cada `$ cmd` con sus etapas de pipe, su salida y su
    /// código). El más reciente va al final.
    pub blocks: Vec<DrawerBlock>,
    /// `true` mientras el comando corre en segundo plano.
    pub pending: bool,
    /// Desplazamiento del historial, en px desde arriba (0 = la card más
    /// vieja al tope; el máximo deja la más nueva pegada al fondo). El render
    /// lo acota; al lanzar/terminar un comando salta al fondo.
    pub scroll: f32,
    /// Alto del viewport del historial en px — lo cachea el backend en cada
    /// render para que el clamp del scroll (en `update`) sea exacto.
    pub viewport_h: f32,
    /// Hotkey que abre/cierra el drawer (de la prop `hotkey`), o `None`.
    pub hotkey: Option<String>,
    /// Prompt al frente del input (`›`, `$`, …).
    pub prompt: String,
    /// Placeholder cuando el buffer está vacío.
    pub placeholder: String,
    /// Animación de despliegue `0..1` (0 = replegado, 1 = desplegado).
    pub anim: Tween<f32>,
    /// `true` si el config declaró algún `shuma_input` (si no, no hay cabezal
    /// ni drawer).
    pub present: bool,
}

/// Métricas de layout del historial, en px — deben seguir al render
/// (`card_view`) para que el alto estimado sea fiel y el scroll acote bien.
const HEADER_H: f32 = 22.0;
const LINE_H: f32 = 18.0;
const CARD_GAP: f32 = 10.0;
const BODY_GAP: f32 = 2.0;

impl ShumaState {
    /// Tope de cards en el historial — más allá, las viejas se descartan.
    const MAX_BLOCKS: usize = 12;

    /// Alto total estimado del historial, en px — base del clamp del scroll y
    /// de la barra. Estimado desde el modelo con [`HEADER_H`]/[`LINE_H`] (el
    /// render no devuelve medidas); fiel mientras `card_view` no cambie.
    pub fn content_height(&self) -> f32 {
        if self.blocks.is_empty() {
            return LINE_H; // la pista de uso
        }
        let mut h = 0.0;
        for b in &self.blocks {
            h += HEADER_H;
            if !b.collapsed {
                let mut lines = b.lines.len();
                if b.exit.is_none() {
                    lines += 1; // la línea "…"
                }
                if matches!(b.exit, Some(c) if c != 0) {
                    lines += 1; // el footer "exit N"
                }
                // La etapa revelada (tee) agrega su salida capturada, o una
                // línea de aviso si no capturó nada.
                if let Some(si) = b.expanded_stage {
                    let n = b.stage_lines.get(si).map(|l| l.len()).unwrap_or(0);
                    lines += n.max(1);
                }
                h += lines as f32 * LINE_H + BODY_GAP;
            }
            h += CARD_GAP;
        }
        h
    }

    /// Suma `delta` px al scroll y lo acota al viewport cacheado. Lo llama el
    /// `update` al recibir la rueda o el arrastre de la barra.
    pub fn scroll_by(&mut self, delta: f32) {
        self.scroll = clamp_offset(self.scroll + delta, self.content_height(), self.viewport_h);
    }

    /// Pega el scroll al fondo (la card más nueva). Se llama al lanzar y al
    /// terminar un comando para que la salida fresca quede a la vista; el
    /// render lo acota al máximo real.
    fn pin_bottom(&mut self) {
        self.scroll = self.content_height();
    }

    /// Empuja una card nueva en estado «corriendo» para `cmd` (con sus etapas
    /// de pipe ya resueltas) y marca el drawer como pendiente. Acota el
    /// historial a [`MAX_BLOCKS`]. Lo usan ambos backends al lanzar.
    pub fn push_pending(&mut self, cmd: String) {
        let stages = stage_labels(&cmd);
        self.blocks.push(DrawerBlock {
            cmd,
            stages,
            lines: Vec::new(),
            stage_lines: Vec::new(),
            expanded_stage: None,
            exit: None,
            collapsed: false,
            is_ia: false,
        });
        self.trim_and_pin();
    }

    /// Empuja una card de consulta a la **IA** para `prompt` y marca el drawer
    /// como pendiente. Sin chips de etapa (un prompt no es un pipe). El
    /// resultado llega por [`finish_last`], igual que un comando shell.
    pub fn push_pending_ia(&mut self, prompt: String) {
        self.blocks.push(DrawerBlock {
            cmd: prompt,
            stages: Vec::new(),
            lines: Vec::new(),
            stage_lines: Vec::new(),
            expanded_stage: None,
            exit: None,
            collapsed: false,
            is_ia: true,
        });
        self.trim_and_pin();
    }

    /// Acota el historial a [`MAX_BLOCKS`] (descarta las cards más viejas),
    /// marca pendiente y pega el scroll al fondo. Compartido por los dos
    /// `push_pending*`.
    fn trim_and_pin(&mut self) {
        if self.blocks.len() > Self::MAX_BLOCKS {
            let drop = self.blocks.len() - Self::MAX_BLOCKS;
            self.blocks.drain(0..drop);
        }
        self.pending = true;
        self.pin_bottom();
    }

    /// Rellena la última card (la pendiente) con el resultado de la corrida.
    pub fn finish_last(&mut self, res: RunResult) {
        self.pending = false;
        if let Some(b) = self.blocks.last_mut() {
            b.lines = res.lines;
            b.stage_lines = res.stages;
            b.exit = res.exit;
        }
        self.pin_bottom();
    }
}

impl Default for ShumaState {
    fn default() -> Self {
        Self {
            open: false,
            buffer: String::new(),
            blocks: Vec::new(),
            pending: false,
            scroll: 0.0,
            viewport_h: 300.0,
            hotkey: None,
            prompt: "›".into(),
            placeholder: "shuma".into(),
            anim: Tween::idle(0.0),
            present: false,
        }
    }
}

impl ShumaState {
    /// Construye el estado desde la spec del `shuma_input` (prompt/placeholder/
    /// hotkey). Marca `present = true`.
    pub fn from_spec(spec: &WidgetSpec) -> Self {
        let hotkey = spec.str_prop("hotkey", "");
        Self {
            prompt: spec.str_prop("prompt", "›").to_string(),
            placeholder: spec.str_prop("placeholder", "shuma").to_string(),
            hotkey: if hotkey.is_empty() {
                None
            } else {
                Some(hotkey.to_string())
            },
            present: true,
            ..Self::default()
        }
    }

    /// `true` si el drawer debe pintarse (abierto o aún animando el cierre).
    pub fn visible(&self) -> bool {
        self.open || self.anim.value() > 0.01
    }
}

/// El cabezal clicable que va en la barra: prompt + buffer/placeholder. Un click
/// despliega el drawer.
pub fn headline_view(state: &ShumaState, theme: &Theme) -> View<Msg> {
    let texto = if state.buffer.is_empty() {
        state.placeholder.clone()
    } else {
        state.buffer.clone()
    };
    let color = if state.buffer.is_empty() {
        theme.fg_muted
    } else {
        theme.fg_text
    };

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: length(220.0_f32),
            height: length(24.0_f32),
        },
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexStart),
        gap: Size {
            width: length(6.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .on_click(Msg::ShumaToggle)
    .children(vec![
        chip_text(&state.prompt, 13.0, theme.accent),
        chip_text(&texto, 13.0, color),
    ])
}

/// El drawer desplegado: scrim que cierra al click + panel inferior con el input
/// y la salida. `None` si no hay nada que mostrar.
pub fn drawer_overlay(state: &ShumaState, screen: (i32, i32), theme: &Theme) -> Option<View<Msg>> {
    if !state.visible() {
        return None;
    }
    let t = state.anim.value().clamp(0.0, 1.0);
    let (_sw, sh) = screen;
    let alto = (sh as f32 * DRAWER_FRAC * t).max(1.0);

    // Línea del input: prompt + buffer + cursor.
    let linea = {
        let mut s = state.buffer.clone();
        s.push('▏'); // cursor
        View::new(Style {
            flex_direction: FlexDirection::Row,
            size: Size {
                width: percent(1.0_f32),
                height: length(28.0_f32),
            },
            align_items: Some(AlignItems::Center),
            gap: Size {
                width: length(8.0_f32),
                height: length(0.0_f32),
            },
            ..Default::default()
        })
        .children(vec![
            chip_text(&state.prompt, 16.0, theme.accent),
            chip_text(&s, 16.0, theme.fg_text),
        ])
    };

    // Cuerpo: las cards del historial (paridad con el shell de shuma). El
    // viewport es el panel menos la línea de input y los paddings.
    let cuerpo = blocks_view(state, theme, (alto - 76.0).max(40.0));

    let panel = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            right: length(0.0_f32),
            top: auto(),
            bottom: length(0.0_f32),
        },
        size: Size {
            width: percent(1.0_f32),
            height: length(alto),
        },
        flex_direction: FlexDirection::Column,
        padding: TaffyRect {
            left: length(20.0_f32),
            right: length(20.0_f32),
            top: length(16.0_f32),
            bottom: length(16.0_f32),
        },
        gap: Size {
            width: length(0.0_f32),
            height: length(12.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .children(vec![linea, cuerpo]);

    // Scrim a pantalla completa: oscurece el fondo y cierra al click.
    let scrim = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            top: length(0.0_f32),
            right: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_app)
    .alpha(0.55 * t)
    .on_click(Msg::ShumaToggle)
    .children(vec![panel]);

    Some(scrim)
}

/// El **cuerpo** del drawer (sin scrim ni posición absoluta), pensado para
/// llenar el contenedor que le da el backend `wlr-layer-shell`: ahí la propia
/// layer surface ya *es* el panel del Quake (la barra crece hacia arriba), así
/// que no hace falta scrim ni animación. Línea de input (prompt + buffer +
/// cursor) arriba, salida del último comando debajo.
pub fn drawer_body_view(state: &ShumaState, theme: &Theme, viewport_h: f32) -> View<Msg> {
    let mut buf = state.buffer.clone();
    buf.push('▏'); // cursor
    let linea = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: percent(1.0_f32),
            height: length(28.0_f32),
        },
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(8.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(vec![
        chip_text(&state.prompt, 16.0, theme.accent),
        chip_text(&buf, 16.0, theme.fg_text),
    ]);

    let cuerpo = blocks_view(state, theme, viewport_h);

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        padding: TaffyRect {
            left: length(20.0_f32),
            right: length(20.0_f32),
            top: length(16.0_f32),
            bottom: length(16.0_f32),
        },
        gap: Size {
            width: length(0.0_f32),
            height: length(12.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .children(vec![linea, cuerpo])
}

/// El cuerpo del drawer: las cards del historial (paridad con el shell de
/// shuma), o la pista de uso si no hay ninguna. Lo comparten los dos backends
/// (winit con scrim y layer-shell). Cada card es un `$ cmd` con sus etapas de
/// pipe clickeables, su salida coloreada por stdout/stderr y su código.
fn blocks_view(state: &ShumaState, theme: &Theme, viewport_h: f32) -> View<Msg> {
    let col = Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: auto() },
        gap: Size { width: length(0.0_f32), height: length(CARD_GAP) },
        ..Default::default()
    };
    let content = if state.blocks.is_empty() {
        View::new(col).text(
            "Enter pregunta a la IA · prefijo ! o $ corre shell · Esc cierra".to_string(),
            13.0,
            theme.fg_muted,
        )
    } else {
        let cards = state
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| card_view(i, b, theme))
            .collect();
        View::new(col).children(cards)
    };
    // Área de scroll: el contenido se desplaza y se recorta al viewport; la
    // rueda y el arrastre de la barra emiten `ShumaScroll(delta)`. El offset
    // se acota acá mismo (el modelo puede guardar uno mayor sin romper nada).
    let content_h = state.content_height();
    let offset = clamp_offset(state.scroll, content_h, viewport_h);
    scroll_y(
        offset,
        content_h,
        viewport_h,
        content,
        Msg::ShumaScroll,
        &ScrollPalette::from_theme(theme),
    )
}

/// Una card: encabezado (`$` plegable + etapas de pipe clickeables) y, si no
/// está plegada, la salida de la etapa revelada (tee), las líneas de salida
/// final y el código de salida.
fn card_view(idx: usize, b: &DrawerBlock, theme: &Theme) -> View<Msg> {
    // Encabezado: el `$` pliega/despliega; las chips de etapa **intermedia**
    // revelan su salida capturada en vivo (tee). La última etapa no se captura
    // aparte —su stdout es el cuerpo de la card— así que su chip es pasiva.
    // Card de IA: marca `✦` + el prompt, sin chips de etapa.
    if b.is_ia {
        let header = View::new(Style {
            flex_direction: FlexDirection::Row,
            size: Size { width: percent(1.0_f32), height: length(22.0_f32) },
            align_items: Some(AlignItems::Center),
            gap: Size { width: length(6.0_f32), height: length(0.0_f32) },
            ..Default::default()
        })
        .children(vec![
            chip_text("✦", 14.0, theme.accent).on_click(Msg::ShumaCollapse(idx)),
            chip_text(&b.cmd, 14.0, theme.fg_text).on_click(Msg::ShumaCollapse(idx)),
        ]);
        let mut col: Vec<View<Msg>> = vec![header];
        if !b.collapsed {
            if b.exit.is_none() {
                col.push(out_line("…pensando", theme.fg_muted));
            }
            for l in &b.lines {
                let c = if l.err { theme.fg_destructive } else { theme.fg_text };
                col.push(out_line(&l.text, c));
            }
        }
        return View::new(Style {
            flex_direction: FlexDirection::Column,
            size: Size { width: percent(1.0_f32), height: auto() },
            gap: Size { width: length(0.0_f32), height: length(2.0_f32) },
            ..Default::default()
        })
        .children(col);
    }
    let mut head: Vec<View<Msg>> = vec![chip_text("$", 14.0, theme.accent)
        .on_click(Msg::ShumaCollapse(idx))];
    if b.stages.is_empty() {
        // Sin pipe: la línea entera re-ejecuta al clickearla.
        head.push(chip_text(&b.cmd, 14.0, theme.fg_text).on_click(Msg::ShumaRunLine(b.cmd.clone())));
    } else {
        let last = b.stages.len() - 1;
        for (si, label) in b.stages.iter().enumerate() {
            if si > 0 {
                head.push(chip_text("|", 14.0, theme.fg_muted));
            }
            // La etapa revelada se resalta (más clara); clic togglea su revelado.
            let revealed = b.expanded_stage == Some(si);
            let color = if revealed { theme.fg_text } else { theme.accent };
            let chip = chip_text(label, 14.0, color);
            head.push(if si < last {
                chip.on_click(Msg::ShumaStageToggle(idx, si)).hover_fill(theme.bg_button_hover)
            } else {
                chip // la última etapa: su salida es el cuerpo de la card
            });
        }
    }
    let header = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(22.0_f32) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(6.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(head);

    let mut col: Vec<View<Msg>> = vec![header];
    if !b.collapsed {
        // Salida capturada de la etapa revelada (tee), indentada y atenuada.
        if let Some(si) = b.expanded_stage {
            match b.stage_lines.get(si) {
                Some(lines) if !lines.is_empty() => {
                    for l in lines {
                        let c = if l.err { theme.fg_destructive } else { theme.fg_muted };
                        col.push(out_line(&format!("  {}", l.text), c));
                    }
                }
                _ => col.push(out_line("  (sin salida capturada)", theme.fg_muted)),
            }
        }
        if b.exit.is_none() {
            col.push(out_line("…", theme.fg_muted));
        }
        for l in &b.lines {
            let c = if l.err { theme.fg_destructive } else { theme.fg_text };
            col.push(out_line(&l.text, c));
        }
        if let Some(code) = b.exit {
            if code != 0 {
                col.push(out_line(&format!("exit {code}"), theme.fg_destructive));
            }
        }
    }
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: auto() },
        gap: Size { width: length(0.0_f32), height: length(2.0_f32) },
        ..Default::default()
    })
    .children(col)
}

/// Una línea de salida a ancho completo.
fn out_line(t: &str, color: llimphi_theme::Color) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: auto() },
        ..Default::default()
    })
    .text(t.to_string(), 13.0, color)
}

/// Un texto suelto, centrado verticalmente.
fn chip_text(t: &str, size: f32, color: llimphi_theme::Color) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: auto(),
            height: length(size + 6.0),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text(t.to_string(), size, color)
}

/// El puente pata→shuma (SDD §5): ejecuta `cmd` por el **ejecutor real de
/// shuma** (`shuma-exec`) y devuelve su stdout, o el stderr/código como error.
/// Reúne los eventos en streaming hasta el final; la captura está acotada a
/// [`CAPTURE_CAP`] (lo que excede se marca como truncado). Bloqueante: se
/// llama desde un hilo de fondo (`Handle::spawn` o `std::thread`).
pub fn ejecutar(cmd: &str) -> RunResult {
    use shuma_exec::{run, CommandSpec, Exec, RunEvent};

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "/".to_string());

    // Pipe «simple» → ejecución directa con **captura por etapa** (tee): cada
    // etapa intermedia emite su stdout en vivo, sin re-ejecutar (paridad con el
    // shell de shuma). Cualquier otra sintaxis cae a `sh -c`, sin tee.
    let (spec, n_stages) = match simple_pipe_stages(cmd) {
        Some(stages) => {
            let n = stages.len();
            let spec = CommandSpec {
                exec: Exec::Direct { stages },
                cwd,
                capture_limit: CAPTURE_CAP,
                spill_path: None,
                stdin_data: None,
                capture_stages: true,
            };
            (spec, n)
        }
        None => (CommandSpec::shell(cmd, cwd).with_limit(CAPTURE_CAP), 0),
    };

    let mut lines: Vec<OutLine> = Vec::new();
    let mut exit: Option<i32> = None;
    let mut stages: Vec<Vec<OutLine>> = vec![Vec::new(); n_stages];
    for ev in run(&spec).wait_all() {
        match ev {
            RunEvent::Stdout(t) => lines.push(OutLine { err: false, text: t }),
            // Salida de una etapa intermedia (tee): a su balde, para revelarla
            // al clickear la chip de esa etapa. La última etapa va por Stdout.
            RunEvent::StageStdout { stage, line } => {
                if let Some(buf) = stages.get_mut(stage) {
                    buf.push(OutLine { err: false, text: line });
                }
            }
            RunEvent::Stderr(t) => lines.push(OutLine { err: true, text: t }),
            RunEvent::Exited(c) => exit = Some(c),
            RunEvent::Failed(m) => lines.push(OutLine {
                err: true,
                text: format!("no pude lanzar: {m}"),
            }),
            RunEvent::Truncated => lines.push(OutLine {
                err: true,
                text: format!("… (salida truncada a {CAPTURE_CAP} bytes)"),
            }),
            RunEvent::Spilled(p) => lines.push(OutLine {
                err: false,
                text: format!("… (salida volcada a {p})"),
            }),
            // Sólo aparece en modo PTY; el drawer no lo usa.
            RunEvent::Bytes(_) => {}
        }
    }
    RunResult { lines, exit, stages }
}

/// Tope de captura del drawer, en bytes — un comando charlatán no infla la RAM.
const CAPTURE_CAP: usize = 256 * 1024;

/// Cómo se clasifica el buffer al hacer submit en el drawer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitKind<'a> {
    /// Buffer vacío (o sólo espacios): no se hace nada.
    Empty,
    /// Comando shell — el prefijo explícito `!`/`$` lo fuerza.
    Shell(&'a str),
    /// Prompt para la IA — el default cuando no hay prefijo.
    Ia(&'a str),
}

/// Clasifica el buffer al submit (paridad con el quake de mirada-launcher):
/// `!cmd` o `$cmd` van al shell; vacío no hace nada; **todo lo demás es un
/// prompt para la IA**. Así el drawer es a la vez terminal y asistente.
pub fn classify(buffer: &str) -> SubmitKind<'_> {
    let trimmed = buffer.trim();
    if let Some(rest) = trimmed.strip_prefix('!').or_else(|| trimmed.strip_prefix('$')) {
        SubmitKind::Shell(rest.trim())
    } else if trimmed.is_empty() {
        SubmitKind::Empty
    } else {
        SubmitKind::Ia(trimmed)
    }
}

/// Consulta a la IA bloqueando — pensado para un hilo de fondo (`Handle::spawn`).
/// `pluma_llm::from_env` autodetecta el backend y cae a Mock sin credenciales
/// (eco determinista, útil para iterar sin red). Devuelve un [`RunResult`] para
/// rellenar la card pendiente igual que un comando: la respuesta va a `lines`
/// (una por renglón), un error va como línea de stderr + `exit 1`.
pub fn preguntar_ia(prompt: &str) -> RunResult {
    use pluma_llm::pluma_llm_core::ChatRequest;

    let resultado = (|| -> Result<String, String> {
        let cli = pluma_llm::from_env().map_err(|e| format!("{e}"))?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("{e}"))?;
        rt.block_on(async {
            let req = ChatRequest::una_vuelta(prompt, 256)
                .con_sistema("Sos un asistente conciso del escritorio. Responde corto.");
            cli.complete(&req)
                .await
                .map(|r| r.content)
                .map_err(|e| format!("{e}"))
        })
    })();

    match resultado {
        Ok(texto) => RunResult {
            lines: texto
                .lines()
                .map(|l| OutLine { err: false, text: l.to_string() })
                .collect(),
            exit: Some(0),
            stages: Vec::new(),
        },
        Err(e) => RunResult {
            lines: vec![OutLine { err: true, text: e }],
            exit: Some(1),
            stages: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn una_linea_sin_pipe_no_tiene_chips() {
        assert!(stage_labels("ls -la").is_empty());
        assert!(stage_labels("").is_empty());
    }

    #[test]
    fn las_etapas_de_un_pipe_son_los_comandos() {
        assert_eq!(
            stage_labels("ls -la | grep foo | sort"),
            vec!["ls".to_string(), "grep".to_string(), "sort".to_string()]
        );
    }

    #[test]
    fn classify_rutea_prefijo_a_shell_y_resto_a_ia() {
        assert_eq!(classify(""), SubmitKind::Empty);
        assert_eq!(classify("   "), SubmitKind::Empty);
        assert_eq!(classify("!firefox"), SubmitKind::Shell("firefox"));
        assert_eq!(classify("$ ls -la"), SubmitKind::Shell("ls -la"));
        assert_eq!(classify("qué hora es?"), SubmitKind::Ia("qué hora es?"));
        assert_eq!(classify("  hola  "), SubmitKind::Ia("hola"));
    }

    #[test]
    fn push_pending_ia_marca_la_card_sin_etapas() {
        let mut s = ShumaState::default();
        s.push_pending_ia("explicá el kernel".into());
        let last = s.blocks.last().unwrap();
        assert!(last.is_ia);
        assert!(last.stages.is_empty());
        assert!(last.exit.is_none() && s.pending);
    }

    #[test]
    fn un_pipe_simple_se_descompone_en_etapas() {
        let stages = simple_pipe_stages("ls -la | grep foo | sort").expect("pipe simple");
        assert_eq!(stages.len(), 3);
        assert_eq!(stages[0].program, "ls");
        assert_eq!(stages[0].args, vec!["-la".to_string()]);
        assert_eq!(stages[1].program, "grep");
        assert_eq!(stages[2].program, "sort");
    }

    #[test]
    fn sin_pipe_o_con_sintaxis_compleja_no_hay_etapas_directas() {
        // Una sola etapa: no amerita modo directo.
        assert!(simple_pipe_stages("ls -la").is_none());
        // Comillas, variables, redirecciones, globs o `~` → al shell (sin tee).
        assert!(simple_pipe_stages("echo \"hola\" | cat").is_none());
        assert!(simple_pipe_stages("cat *.rs | wc -l").is_none());
        assert!(simple_pipe_stages("echo $HOME | cat").is_none());
        // Línea incompleta (termina en `|`).
        assert!(simple_pipe_stages("ls |").is_none());
    }

    #[test]
    fn push_pending_resuelve_etapas_y_acota_el_historial() {
        let mut s = ShumaState::default();
        s.push_pending("a | b".into());
        let last = s.blocks.last().unwrap();
        assert_eq!(last.stages, vec!["a".to_string(), "b".to_string()]);
        assert!(last.exit.is_none() && s.pending);
        // Más allá del tope, las viejas se descartan.
        for _ in 0..ShumaState::MAX_BLOCKS + 5 {
            s.push_pending("x".into());
        }
        assert_eq!(s.blocks.len(), ShumaState::MAX_BLOCKS);
    }

    #[test]
    fn finish_last_rellena_la_card_pendiente() {
        let mut s = ShumaState::default();
        s.push_pending("echo hi".into());
        s.finish_last(RunResult {
            lines: vec![OutLine { err: false, text: "hi".into() }],
            exit: Some(0),
            stages: Vec::new(),
        });
        let last = s.blocks.last().unwrap();
        assert_eq!(last.exit, Some(0));
        assert_eq!(last.lines.len(), 1);
        assert!(!s.pending);
    }

    #[test]
    fn finish_last_guarda_la_salida_por_etapa() {
        let mut s = ShumaState::default();
        s.push_pending("seq 3 | sort".into());
        s.finish_last(RunResult {
            lines: vec![OutLine { err: false, text: "1".into() }],
            exit: Some(0),
            stages: vec![
                vec![OutLine { err: false, text: "3".into() }],
                Vec::new(),
            ],
        });
        let last = s.blocks.last().unwrap();
        assert_eq!(last.stage_lines.len(), 2);
        assert_eq!(last.stage_lines[0][0].text, "3");
        // Revelar la etapa 0 agranda el contenido (entra su salida capturada).
        let antes = s.content_height();
        s.blocks.last_mut().unwrap().expanded_stage = Some(0);
        assert!(s.content_height() > antes);
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::*;

    #[test]
    fn content_height_crece_con_cards_y_lineas() {
        let mut s = ShumaState::default();
        let vacio = s.content_height();
        s.push_pending("a".into());
        s.finish_last(RunResult {
            lines: vec![OutLine { err: false, text: "x".into() }; 3],
            exit: Some(0),
            stages: Vec::new(),
        });
        assert!(s.content_height() > vacio);
    }

    #[test]
    fn scroll_by_acota_a_cero_y_al_maximo() {
        let mut s = ShumaState::default();
        s.viewport_h = 50.0;
        // Historial alto: varias cards con salida.
        for _ in 0..6 {
            s.push_pending("cmd".into());
            s.finish_last(RunResult {
                lines: vec![OutLine { err: false, text: "l".into() }; 4],
                exit: Some(0),
                stages: Vec::new(),
            });
        }
        let max = (s.content_height() - s.viewport_h).max(0.0);
        s.scroll = 0.0;
        s.scroll_by(-100.0); // no baja de 0
        assert_eq!(s.scroll, 0.0);
        s.scroll_by(1e6); // no pasa del máximo
        assert!((s.scroll - max).abs() < 0.01);
    }

    #[test]
    fn lanzar_un_comando_pega_el_scroll_al_fondo() {
        let mut s = ShumaState::default();
        s.viewport_h = 40.0;
        for _ in 0..6 {
            s.push_pending("c".into());
            s.finish_last(RunResult { lines: vec![], exit: Some(0), stages: Vec::new() });
        }
        s.scroll = 0.0; // el usuario subió a ver lo viejo
        s.push_pending("nuevo".into()); // un comando nuevo
        // El scroll quedó pegado al fondo (>= el máximo tras el clamp del render).
        assert!(s.scroll >= (s.content_height() - s.viewport_h).max(0.0));
    }
}
