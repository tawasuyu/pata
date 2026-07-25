//! Implementación de los métodos de `LayerApp`: lógica de la aplicación,
//! gestión de panels, muestreo, render y manejo de mensajes.

use std::ffi::c_void;
use std::ptr::NonNull;

use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::shell::WaylandSurface;
use wayland_client::{protocol::wl_surface, Proxy, QueueHandle};

use llimphi_ui::llimphi_compositor::{
    hit_test_click, hit_test_hover, hit_test_scroll, measure_text_node, mount, paint, DragPhase,
};
use llimphi_ui::llimphi_hal::{wgpu, Hal, RawSurface, Surface as _};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_raster::{peniko::color::palette, vello, Renderer};
use llimphi_ui::llimphi_text::Typesetter;

use pata_core::SurfaceKind;

use crate::nouser::{MembersOutcome, PollOutcome};
use pata_host::HostServer;
use crate::toplevel::{Toplevel, WindowEntry};
use crate::{render, Msg};

use super::{
    diag, CardState, LayerApp, LayerDrag, MenuKind, PanelGpu, Panel, RenderCache, TaskDrag,
    DRAWER_H, MENU_H,
};

/// Cap de present por panel CON actividad (cambio real o animación discreta): ~30fps.
const PRESENT_CAP_MS: u128 = 33;
/// Cap de present por panel EN REPOSO (sólo efectos ambientales: respiración del
/// diente, pseudocava del CPU, nubecita de clima): ~7fps. Estos efectos son lentos
/// y sutiles — a 7fps no molestan — y así pata deja de presentar (y de despertar al
/// compositor a recomponer) 30 veces/s en reposo. Simétrico al fondo vivo de mirada.
const PRESENT_CAP_IDLE_MS: u128 = 150;

/// ¿El visualizador `cava` tiene AUDIO real (música sonando), y no sólo ruido de
/// piso con un stream pausado/corked? Umbral por SUMA de barras (∈[0,1] c/u): la
/// música levanta varias bandas (suma » 0), el silencio queda cerca de 0 aunque
/// alguna barra tenga ruido. Con audio real cava va a 30fps; en silencio, el panel
/// entra en reposo ambiental y se throttlea. Extraído para test.
fn cava_con_audio(frame: &[f32]) -> bool {
    frame.iter().copied().sum::<f32>() > 0.2
}

/// Cap de present en ms según si el panel está en reposo ambiental (`ambiente`) o
/// con actividad. Extraído para test.
fn present_cap_ms(ambiente: bool) -> u128 {
    if ambiente {
        PRESENT_CAP_IDLE_MS
    } else {
        PRESENT_CAP_MS
    }
}

/// Cuánto tiene que moverse una banda de `cava` para que valga repintar. Es ~1/50
/// de la altura de la barra: por debajo no se distingue del cuadro anterior.
const CAVA_EPS: f32 = 0.02;

/// ¿El cuadro nuevo de `cava` da algo que animar frente al vigente? Extraído para
/// test.
///
/// Dos condiciones, y hacen falta las dos. **Hay audio** (en el cuadro nuevo o en
/// el viejo — el viejo importa para pintar la caída a cero cuando la música para,
/// si no las barras quedarían clavadas arriba). Y **se movió algo** por encima de
/// [`CAVA_EPS`]: el daemon manda cuadros aunque el stream esté pausado, y repintar
/// por un temblor de ruido de piso es gastar un frame en algo invisible.
fn cava_ensucia(nuevo: &[f32], vigente: &[f32]) -> bool {
    if !cava_con_audio(nuevo) && !cava_con_audio(vigente) {
        return false; // silencio: nada que animar
    }
    if nuevo.len() != vigente.len() {
        return true; // cambió la cantidad de bandas: repintar sí o sí
    }
    nuevo.iter().zip(vigente).any(|(a, b)| (a - b).abs() > CAVA_EPS)
}

/// ¿Una surface está **inerte** — sin nada que mostrar? Extraído para test.
///
/// `drawer` = es el panel desplegable de un sidebar (y `mostrado`, si está
/// desplegado); `w`/`h` = su tamaño actual; `con_contenido` = tiene algo que
/// pintar (un cartel de OSD, el árbol de Alt-Tab, un texto de tooltip).
///
/// Existe porque el cap de present es **por panel** y nadie miraba el total:
/// medido en metal con dos monitores, `pata` presentaba **90 cuadros/s en
/// reposo** repartidos en 13 paneles a ~7/s cada uno — y 7 de esos 13 eran
/// surfaces de servicio a 1×1 (OSD, Alt-Tab, tooltip) y drawers de sidebar
/// **cerrados**. Unos 50 presents/s pintando lo que nadie ve, cada uno además
/// despertando al compositor a recomponer.
fn inerte(drawer: bool, mostrado: bool, w: u32, h: u32, con_contenido: bool) -> bool {
    if drawer {
        // El drawer cerrado se pintó transparente UNA vez y no cambia hasta que
        // alguien lo despliegue — y desplegarlo marca `dirty` (`reconcile_drawer`).
        return !mostrado;
    }
    if w > 1 || h > 1 {
        return false; // surface crecida: está mostrando algo
    }
    !con_contenido
}

impl LayerApp {
    /// Índice del panel cuya layer surface es `surface`.
    pub(super) fn panel_de(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.panels
            .iter()
            .position(|p| p.layer.wl_surface() == surface)
    }

    /// Marca la barra de shuma para re-pintar.
    pub(super) fn marcar_shuma_dirty(&mut self) {
        if let Some(pi) = self.shuma_panel {
            self.panels[pi].dirty = true;
        }
    }

    /// Marca todas las barras para re-pintar.
    pub(super) fn marcar_todo_dirty(&mut self) {
        // Los paneles INERTES no se ensucian: un cambio de estado global (volumen,
        // muestreo, geometría) no le da nada que pintar a un drawer cerrado ni a
        // una surface de servicio a 1×1. Sin este filtro el gate de `draw` no
        // llegaba a cortar nunca —medido en metal: los 7 paneles inertes entraban
        // con `dirty=true` en el 90% de sus vueltas— y se pagaban ~50 present/s de
        // lo que nadie ve. Ver [`inerte`].
        let vivos: Vec<usize> =
            (0..self.panels.len()).filter(|&pi| !self.panel_inerte(pi)).collect();
        diag!("pata diag · marcar_todo_dirty vivos={} de {}", vivos.len(), self.panels.len());
        for pi in vivos {
            self.panels[pi].dirty = true;
        }
    }

    /// Tras rodar la rueda sobre el volumen: refleja el valor nuevo YA (sin
    /// esperar el ciclo del sampler de fondo) re-muestreando a `self.ctx` y
    /// marcando todo para repintar — así el tooltip se actualiza en tiempo real.
    pub(super) fn refresh_volume_now(&mut self) {
        if let Some((v, _muted)) = crate::sampler::sample_volume() {
            self.ctx.volume = v;
        }
        self.marcar_todo_dirty();
    }

    /// Igual que [`Self::refresh_volume_now`] para el brillo.
    pub(super) fn refresh_brightness_now(&mut self) {
        if let Some(b) = crate::sampler::sample_backlight() {
            self.ctx.brightness = b;
        }
        self.marcar_todo_dirty();
    }

    /// Dispara el cartel OSD (volumen/brillo) y marca su surface para repintar.
    pub(super) fn flash_osd(&mut self, kind: crate::render::OsdKind, level: f32, muted: bool) {
        self.osd = Some(crate::render::Osd::flash(kind, level, muted));
        if let Some(pi) = self.osd_pi {
            self.panels[pi].dirty = true;
        }
        // El diente vivo también reacciona al volumen al instante (sin esperar al
        // muestreo de 1 Hz): dispara su transitorio con la misma señal del OSD.
        if kind == crate::render::OsdKind::Volume {
            let now = self.diente_t0.elapsed().as_secs_f64();
            self.atencion.flash(
                pata_core::atencion::Manifestacion::Volumen { frac: level, muted },
                pata_core::atencion::VOLUMEN_TTL,
                now,
            );
            let s = self.senales_diente();
            self.diente_manifest = self.atencion.resolver(s, now);
        }
    }

    /// La lista de ventanas para el render del `window_list`, en el orden propio
    /// definido por el drag-to-reorder (`task_order`). Las ventanas que no
    /// figuran en ese orden (recién abiertas) quedan al final en orden natural.
    pub(super) fn window_entries(&self) -> Vec<WindowEntry> {
        let mut entries: Vec<WindowEntry> = self
            .toplevels
            .iter()
            .map(|t| WindowEntry {
                id: t.id,
                label: t.etiqueta(),
                app_id: t.app_id.clone(),
                icon_name: t.icon_name.clone(),
                // foreign-toplevel no reporta el escritorio ni el tab (0 =
                // desconocido); esta lista alimenta la taskbar de la barra, no el
                // rail de tabs (que usa el muestreo de `mirada-ctl windows`).
                workspace: 0,
                active: t.activated,
                minimized: t.minimized,
                tab: 0,
            })
            .collect();
        if !self.task_order.is_empty() {
            // `sort_by_key` es estable: las desconocidas (clave `usize::MAX`)
            // conservan su orden natural relativo al final de la lista.
            entries.sort_by_key(|e| {
                self.task_order
                    .iter()
                    .position(|&id| id == e.id)
                    .unwrap_or(usize::MAX)
            });
        }
        entries
    }

    /// El toplevel con ese `id`, si sigue abierto.
    pub(super) fn toplevel_por_id(&self, id: u32) -> Option<&Toplevel> {
        self.toplevels.iter().find(|t| t.id == id)
    }

    /// El nombre del conector del output donde vive el panel `pi` (`"DP-1"`),
    /// o `None` si aún no se resolvió (surface sin output asignado todavía).
    pub(super) fn panel_output_name(&self, pi: usize) -> Option<String> {
        self.panels
            .get(pi)?
            .output
            .as_ref()
            .and_then(|o| self.output_state.info(o).and_then(|i| i.name))
    }

    /// Vista de escritorios **desde el monitor del panel `pi`**: `(activo,
    /// otros)`. Con varios monitores cada barra debe pintar el escritorio de
    /// SU pantalla, no el del monitor enfocado. Se resuelve del
    /// `output_workspaces` que reporta mirada (`outputs=DP-1:3,HDMI-A-1:4`):
    ///
    /// - `activo` = el escritorio del output de este panel;
    /// - `otros` = máscara de los escritorios activos en los **demás** monitores
    ///   (para marcarlos distinto en el switcher — cliquearlos salta a esa
    ///   pantalla).
    ///
    /// Si no hay dato por-output (mono-monitor o WM viejo) cae al global
    /// `self.ctx` (`active_workspace` / `workspace_others`), preservando el
    /// comportamiento previo.
    pub(super) fn panel_workspace_view(&self, pi: usize) -> (u8, u16) {
        let ows = &self.ctx.output_workspaces;
        if ows.is_empty() {
            return (self.ctx.active_workspace, self.ctx.workspace_others);
        }
        let name = self.panel_output_name(pi);
        // El activo de ESTE monitor (por nombre de conector); si no lo hallamos,
        // caemos al global para no dejar el switcher en 0 (que lo ocultaría).
        let active = name
            .as_deref()
            .and_then(|n| ows.iter().find(|(o, _)| o == n).map(|&(_, ws)| ws))
            .unwrap_or(self.ctx.active_workspace);
        // Otros = escritorios activos en el resto de los outputs.
        let mut others = 0u16;
        for (o, ws) in ows {
            if name.as_deref() != Some(o.as_str()) && (1..=16).contains(ws) {
                others |= 1 << (ws - 1);
            }
        }
        (active, others)
    }

    /// Pertenencia de escritorios vista **desde el monitor del panel `pi`**,
    /// para el switcher de su barra: hogar de cada escritorio (`homes=` de
    /// mirada), monitores conectados con su activo (`outputs=`) y el monitor
    /// con el foco del sistema (`focus=`). Con todo vacío (mono-monitor / WM
    /// viejo) el switcher pinta como siempre.
    pub(super) fn panel_ws_monitores(&self, pi: usize) -> render::WsMonitores {
        render::WsMonitores {
            panel: self.panel_output_name(pi).unwrap_or_default(),
            homes: self.ctx.workspace_homes.clone(),
            outputs: self.ctx.output_workspaces.clone(),
            foco: self.ctx.focused_output.clone(),
        }
    }

    /// ¿La barra del panel `pi` hospeda alguno de estos widgets? (por su config
    /// de superficie: `start`/`center`/`end`). Lo usan los re-apuntados
    /// multi-monitor para decidir si un panel puede recibir el menú/shuma sin
    /// depender de listas cacheadas que pueden quedar rezagadas.
    pub(super) fn panel_hospeda(&self, pi: usize, kinds: &[&str]) -> bool {
        let Some(p) = self.panels.get(pi) else { return false };
        let Some(s) = self.cfg.surfaces.get(p.idx) else { return false };
        s.start
            .iter()
            .chain(&s.center)
            .chain(&s.end)
            .any(|w| kinds.contains(&w.kind.as_str()))
    }

    /// Marca `pi` como la barra de shuma **activa** (la que expande el drawer). Con
    /// varios monitores, clickear o enfocar la barra de otra pantalla mueve el
    /// drawer a ese monitor: si había uno abierto en la barra anterior, lo repliega
    /// primero (si no, quedaría un drawer huérfano expandido en el monitor viejo).
    /// No hace nada si `pi` no es una barra de shuma o ya es la activa. Se auto-sana:
    /// si `pi` hospeda un `shuma_input` pero no estaba en `shuma_panels` (creado
    /// tarde vía reanchor, orden de arranque inesperado), lo agrega — así el clic en
    /// la barra de CUALQUIER monitor la vuelve la activa.
    pub(super) fn focus_shuma_panel(&mut self, pi: usize) {
        if self.shuma_panel == Some(pi) {
            return;
        }
        if !self.shuma_panels.contains(&pi) {
            if self.panel_hospeda(pi, &["shuma_input"]) {
                self.shuma_panels.push(pi);
            } else {
                return;
            }
        }
        diag!(
            "pata diag · focus_shuma_panel({pi}) (era {:?}, open={})",
            self.shuma_panel,
            self.shuma.open
        );
        // Saltar de monitor cierra el drawer del viejo YA (sin animar; el switch es
        // abrupto). `shrink_shuma_surface_now` opera sobre el `shuma_panel` AÚN viejo.
        if self.shuma.open || self.shuma_closing_at.is_some() {
            self.shrink_shuma_surface_now();
        }
        self.shuma_panel = Some(pi);
        self.shuma_bar_px = self.cfg.surfaces[self.panels[pi].idx].thickness.max(1.0) as u32;
    }

    /// Espeja [`Self::focus_shuma_panel`] para el **menú de inicio**: hace de `pi`
    /// el panel donde se despliega el menú, si esa barra tiene botón de inicio
    /// (`start_button`/`front_panel`). Sin esto el menú abría SIEMPRE en la primera
    /// barra (`resolve_menu_panel` cachea la de menor índice), no en el monitor que
    /// clickeaste — con dos monitores, tocar el PS1 del secundario abría el menú en
    /// el primario. Si el menú estaba abierto en otra barra, lo cierra primero (si
    /// no, quedaría desplegado huérfano en el monitor viejo).
    pub(super) fn focus_menu_panel(&mut self, pi: usize) {
        if self.menu_panel == Some(pi) || !self.panel_hospeda(pi, &["start_button", "front_panel"]) {
            return;
        }
        if self.menu_open {
            self.set_menu_open(false);
        }
        self.menu_panel = Some(pi);
    }

    /// Despliega o repliega el drawer Quake, animado.
    ///
    /// **Abrir** es inmediato: la surface crece a pantalla completa UNA vez (no se
    /// redimensiona por-frame — tóxico en Iris Xe) y el contenido se *desenrolla*
    /// hacia abajo (un clip que crece + fade), gobernado por [`Self::shuma_reveal`].
    /// **Cerrar** difiere el encogido: arranca la animación de enrollado y la
    /// surface se queda a tamaño completo hasta que [`Self::finalize_shuma_close`]
    /// la achica al vencer [`crate::layer::SHUMA_CLOSE`]. Así el cierre también se
    /// anima sin tocar la surface a cada frame.
    /// `target`: `Some(modo)` despliega el drawer registrando **cómo se abrió**
    /// (gobierna el cierre, ver [`crate::shuma::OpenMode`]); `None` lo repliega.
    /// Abrir cuando ya está abierto sólo **actualiza el modo** (p. ej. un vistazo
    /// que ejecuta Enter escala a [`OpenMode::Firme`]).
    /// Márgenes `(top, right, bottom, left)` que el drawer de shuma debe dejar
    /// libres para **no solaparse con los sidebars DOCKED** (los que reservan
    /// franja) del mismo output. Así el despliegue se restringe al área que pata
    /// efectivamente tiene disponible, incluso con el ragsidebar anclado. Un
    /// sidebar que flota (no docked) o autoesconde no reserva su franja de rail →
    /// no descuenta nada (el panel que despliega un diente se maneja aparte).
    fn shuma_drawer_insets(&self, pi: usize) -> (i32, i32, i32, i32) {
        let out_name = self.panels[pi]
            .output
            .as_ref()
            .and_then(|o| self.output_state.info(o).and_then(|i| i.name));
        let (mut top, mut right, mut bottom, mut left) = (0i32, 0i32, 0i32, 0i32);
        for s in &self.cfg.surfaces {
            if s.kind != SurfaceKind::Sidebar {
                continue;
            }
            // La franja del RAIL se reserva según el eje **Ocultar** (`!autohide`),
            // no **Espacio** (`reserve`) — mismo criterio que `layout::resolve` y
            // `aplicar_geometria_sidebar`. Con autohide el rail no reserva → no insetea.
            if s.autohide {
                continue;
            }
            // ¿Este sidebar vive en el mismo output que el drawer? (si no podemos
            // resolver el nombre del output, aplicamos por las dudas — el caso
            // común es un único monitor).
            let out = s.output.trim();
            let aplica = if out == "*" || out.eq_ignore_ascii_case("all") {
                out_name
                    .as_deref()
                    .map(|n| !s.exclude_outputs.iter().any(|e| e.eq_ignore_ascii_case(n)))
                    .unwrap_or(true)
            } else {
                out_name.as_deref().map(|n| out == n).unwrap_or(true)
            };
            if !aplica {
                continue;
            }
            let t = s.thickness.max(1.0) as i32;
            match s.anchor {
                pata_core::Anchor::Left => left += t,
                pata_core::Anchor::Right => right += t,
                pata_core::Anchor::Top => top += t,
                pata_core::Anchor::Bottom => bottom += t,
            }
        }
        (top, right, bottom, left)
    }

    /// Re-estampa el reloj del watchdog anti-atasco del drawer con un input real
    /// (tecla / botón / movimiento de puntero). Sólo hace algo mientras el drawer
    /// está abierto — cerrado no interesa y el puntero dispararía esto a cada frame.
    /// Ver [`crate::layer::LayerApp::shuma_input_reloj`] y el chequeo en `latido`.
    pub(super) fn toca_shuma_watchdog(&mut self) {
        if self.shuma.open {
            self.shuma_input_reloj = Some(std::time::Instant::now());
            // Si el idle largo nos había hecho soltar el grab (bajando a OnDemand
            // con claude a la vista), un input real significa que el usuario volvió:
            // re-reclamamos el `Exclusive` para que el teclado vuelva a ir al drawer
            // sin tener que clickearlo.
            if self.shuma_grab_released {
                self.shuma_grab_released = false;
                if let Some(pi) = self.shuma_panel {
                    diag!("pata diag · re-reclamo Exclusive del drawer (input tras idle)");
                    let layer = &self.panels[pi].layer;
                    layer.set_keyboard_interactivity(
                        smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity::Exclusive,
                    );
                    layer.commit();
                }
            }
        }
    }

    pub(super) fn set_shuma_open(&mut self, target: Option<crate::shuma::OpenMode>) {
        let Some(pi) = self.shuma_panel else {
            diag!("pata diag · set_shuma_open({target:?}) SIN shuma_panel → no-op");
            return;
        };
        if let Some(mode) = target {
            // Re-abrir cancela un cierre en curso. Si ya está abierto y asentado,
            // no reanimamos, pero SÍ actualizamos el modo (Fugaz→Firme al ejecutar).
            let reabriendo = self.shuma_closing_at.take().is_some();
            if self.shuma.open && !reabriendo {
                if self.shuma.open_mode != mode {
                    diag!("pata diag · set_shuma_open: modo {:?}→{mode:?}", self.shuma.open_mode);
                    self.shuma.open_mode = mode;
                    self.panels[pi].dirty = true;
                }
                return;
            }
            diag!("pata diag · set_shuma_open({mode:?}) pi={pi} → set_size(0,10000) + Exclusive (anim in)");
            self.shuma.open = true;
            self.shuma.open_mode = mode;
            // El canvas queda A LA VISTA: las teclas vuelven a poder ir al PTY
            // interactivo (claude/vim) si lo hay.
            self.set_shuma_canvas_visible(true);
            // Arranca el watchdog anti-atasco: si a partir de aquí no entra ningún
            // input real durante `SHUMA_WATCHDOG`, el latido cierra el drawer solo.
            self.shuma_input_reloj = Some(std::time::Instant::now());
            // Abrimos con `Exclusive` fresco (abajo): el release-de-grab por idle
            // arranca en cero.
            self.shuma_grab_released = false;
            // El drawer toma la surface completa; el completado flotante cede (el
            // popup ahora vive dentro del cuerpo del drawer).
            self.completion_open = false;
            self.completion_opened_at = None;
            self.shuma_opened_at = Some(std::time::Instant::now());
            // El desenrollado NO arranca aquí: la surface aún mide la barra fina.
            // `draw` estampa `shuma_reveal_at` cuando el `configure` la agranda, así
            // el clip nace en 0 con el buffer grande ya presente (sin tirón/sliver).
            self.shuma_reveal_at = None;
            // El despliegue descuenta la franja de los sidebars DOCKED (el ragsidebar)
            // para no taparlos — pero SOLO en el eje vertical (top/bottom) a nivel de
            // surface. El descuento LATERAL (left/right) NO va como margen de surface:
            // eso angostaba también la barra de arriba al abrir. Va como padding del
            // CUERPO del drawer (ver `shuma_open_view`), dejando la franja full-width.
            let (mt, _mr, mb, _ml) = self.shuma_drawer_insets(pi);
            let layer = &self.panels[pi].layer;
            layer.set_size(0, 10_000);
            layer.set_margin(mt, 0, mb, 0);
            // Abierto = Exclusive (el drawer agarra todo el teclado).
            layer.set_keyboard_interactivity(
                smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity::Exclusive,
            );
            layer.commit();
            self.panels[pi].cache = None;
            self.panels[pi].dirty = true;
        } else {
            // Cerrar = arrancar el enrollado; la surface se encoge recién al terminar.
            if !self.shuma.open || self.shuma_closing_at.is_some() {
                return;
            }
            diag!("pata diag · set_shuma_open(None) pi={pi} → anim out (surface full hasta terminar)");
            self.shuma_closing_at = Some(std::time::Instant::now());
            self.panels[pi].cache = None;
            self.panels[pi].dirty = true;
        }
    }

    /// Encoge YA la surface del drawer a la barra y limpia el estado de apertura.
    /// Lo llaman [`Self::finalize_shuma_close`] al vencer el enrollado y
    /// [`Self::focus_shuma_panel`] al saltar de monitor (cierre abrupto, sin animar).
    pub(super) fn shrink_shuma_surface_now(&mut self) {
        let Some(pi) = self.shuma_panel else { return };
        self.shuma.open = false;
        self.shuma_opened_at = None;
        self.shuma_reveal_at = None;
        self.shuma_closing_at = None;
        // Canvas OCULTO: un PTY interactivo vivo (claude) deja de comerse el
        // tipeo de la barra — el input vuelve a ser input. El PTY sigue de
        // fondo; re-desplegar lo retoma.
        self.set_shuma_canvas_visible(false);
        let bar = self.shuma_bar_px;
        let layer = &self.panels[pi].layer;
        layer.set_size(0, bar);
        // La barra fina vuelve a ancho completo (sin la reserva del ragsidebar).
        layer.set_margin(0, 0, 0, 0);
        // Plegado = OnDemand (no `None`): la barra sigue pudiendo reclamar el teclado,
        // así mirada se lo da en escritorio vacío (keyboard_fallback_target) sin
        // robárselo a una ventana enfocada.
        layer.set_keyboard_interactivity(
            smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity::OnDemand,
        );
        layer.commit();
        self.panels[pi].cache = None;
        self.panels[pi].dirty = true;
    }

    /// Si venció la animación de cierre, encoge la surface de verdad.
    pub(super) fn finalize_shuma_close(&mut self) {
        if self
            .shuma_closing_at
            .is_some_and(|t| t.elapsed() >= crate::layer::SHUMA_CLOSE)
        {
            self.shrink_shuma_surface_now();
        }
    }

    /// Despliega/repliega el **completado flotante** del input de shuma como
    /// surface autónoma sobre la barra fina. Crece a [`crate::layer::COMPLETION_H`]
    /// UNA vez al aparecer y encoge a la barra UNA vez al desaparecer — nunca por
    /// tecla (tóxico en Iris Xe) — y **no** toca el foco de teclado: la barra lo
    /// conserva para seguir tipeando y navegando el popup. El drawer manda sobre
    /// la surface cuando está abierto (ahí el popup vive en el cuerpo), así que
    /// con `shuma.open` sólo marcamos dirty sin redimensionar.
    pub(super) fn set_completion_open(&mut self, open: bool) {
        let open = open && !self.shuma.open;
        if open == self.completion_open {
            return;
        }
        self.completion_open = open;
        self.completion_opened_at = if open {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let Some(pi) = self.shuma_panel else { return };
        if self.shuma.open {
            self.panels[pi].cache = None;
            self.panels[pi].dirty = true;
            return;
        }
        let h = if open {
            crate::layer::COMPLETION_H
        } else {
            self.shuma_bar_px
        };
        let layer = &self.panels[pi].layer;
        layer.set_size(0, h);
        layer.commit();
        self.panels[pi].cache = None;
        self.panels[pi].dirty = true;
        self.set_shuma_input_region(pi);
    }

    /// Acota la input-region de la surface de shuma cuando está alta **sólo
    /// porque el input creció**.
    ///
    /// La surface alta nació para el completado, donde la zona transparente de
    /// arriba es a propósito un scrim de click-away. Pero ahora también se
    /// levanta al escribir una frase larga, y ahí ese scrim es un rectángulo
    /// invisible de 360 px sobre el escritorio que se come los clicks: la
    /// ventana de abajo (su titlebar, sus botones) deja de responder mientras
    /// escribís. Sin candidatos que descartar, la región se ciñe a la franja de
    /// la barra y el resto vuelve a atravesarse.
    pub(super) fn set_shuma_input_region(&mut self, pi: usize) {
        use smithay_client_toolkit::compositor::Region;
        use smithay_client_toolkit::shell::WaylandSurface;
        let hay_candidatos = match self.shuma_full.as_ref() {
            Some(full) => crate::shuma_app::active_shell_state(full)
                .is_some_and(|s| s.completion.is_some()),
            None => self.shuma.inner.completion.is_some(),
        };
        if !self.completion_open || hay_candidatos || self.shuma.open {
            // Idempotente: sin esto, cada tecla mandaba un commit de más y la
            // barra parpadeaba a cada pulsación.
            if self.shuma_region_franja.is_some() {
                let layer = &self.panels[pi].layer;
                layer.wl_surface().set_input_region(None); // toda la surface
                layer.commit();
                self.shuma_region_franja = None;
            }
            return;
        }
        let franja = (self.shuma_bar_px as f32 + self.shuma_input_alto_extra()).ceil() as i32;
        if self.shuma_region_franja == Some(franja as u32) {
            return; // la franja no cambió: nada que commitear
        }
        let Some(comp) = self.compositor.as_ref() else { return };
        let Ok(region) = Region::new(comp) else { return };
        let layer = &self.panels[pi].layer;
        let idx = self.panels[pi].idx;
        let alto = crate::layer::COMPLETION_H as i32;
        let ancho = self.panels[pi].width as i32;
        // La barra vive en el borde hacia el que crece la surface: abajo si el
        // anchor es inferior (el panel sube hacia el input), arriba si no.
        let abajo = self
            .cfg
            .surfaces
            .get(idx)
            .map(|s| s.anchor.crece_hacia_el_borde_inicial())
            .unwrap_or(true);
        let y = if abajo { (alto - franja).max(0) } else { 0 };
        region.add(0, y, ancho, franja);
        layer.wl_surface().set_input_region(Some(region.wl_region()));
        layer.commit();
        self.shuma_region_franja = Some(franja as u32);
    }

    /// Reconcilia el completado flotante con el estado del input tras un cambio
    /// (tecla / click en una fila): desplega si hay candidatos y el drawer está
    /// plegado, replega si no. Barato: sólo actúa en la transición.
    /// Filas visuales que ocupa hoy el input de shuma (ajuste blando incluido).
    /// `1` = el caso normal, la barra fina alcanza.
    pub(super) fn shuma_input_filas(&self) -> usize {
        let st = match self.shuma_full.as_ref() {
            Some(full) => crate::shuma_app::active_shell_state(full),
            None => Some(&self.shuma.inner),
        };
        st.map(shuma_module_shell::input_filas_visuales).unwrap_or(1)
    }

    /// `(avance del texto en su última fila, caracteres que entran en una fila)`
    /// del input de shuma. Sirve para anticipar el envolvimiento.
    pub(super) fn shuma_input_avance(&self) -> (usize, usize) {
        let st = match self.shuma_full.as_ref() {
            Some(full) => crate::shuma_app::active_shell_state(full),
            None => Some(&self.shuma.inner),
        };
        st.map(shuma_module_shell::input_avance_en_fila).unwrap_or((0, usize::MAX))
    }

    /// Alto extra (px) que la barra necesita para no recortar un input de
    /// varias filas. Cero cuando el input entra en una sola.
    pub(super) fn shuma_input_alto_extra(&self) -> f32 {
        let st = match self.shuma_full.as_ref() {
            Some(full) => crate::shuma_app::active_shell_state(full),
            None => Some(&self.shuma.inner),
        };
        // El MISMO alto animado que usa la caja: si el host contara filas por su
        // cuenta, la franja saltaría mientras la caja se desliza y se vería el
        // borde despegado.
        st.map(shuma_module_shell::input_alto_extra_px).unwrap_or(0.0)
    }

    pub(super) fn reconcile_completion(&mut self) {
        // La fuente del completado es la sesión ACTIVA del modelo full (el default),
        // o el inner bare si no hay full — el mismo `State` que navega el teclado. Sin
        // esto, en full el completado leía el inner bare (siempre vacío) y nunca abría.
        let hay = match self.shuma_full.as_ref() {
            Some(full) => crate::shuma_app::active_shell_state(full)
                .is_some_and(|s| s.completion.is_some()),
            None => self.shuma.inner.completion.is_some(),
        };
        // La surface alta también es la casa del input CRECIDO: en un
        // layer-shell la vista no puede desbordar la superficie, así que un
        // input de varias filas queda recortado en la barra fina. En vez de
        // redimensionar por tecla (tóxico en Iris Xe, ver `COMPLETION_H`),
        // reusamos este mismo interruptor de dos estados: una línea = barra
        // fina, más de una = surface alta.
        // Se ANTICIPA al corte: crecer justo cuando la palabra salta de fila es
        // el peor momento posible — la superficie se redimensiona en medio del
        // reflujo y se ve la «pelea por caber». Creciendo unos caracteres antes,
        // el texto envuelve dentro de una superficie que ya es alta.
        const ANTICIPO: usize = 6;
        let (avance, cols) = self.shuma_input_avance();
        // `saturating_add`: sin ancho publicado todavía, `cols` es el máximo y
        // sumarle desbordaría.
        let crecido = self.shuma_input_filas() > 1 || avance.saturating_add(ANTICIPO) >= cols;
        let quiere = !self.shuma.open && (hay || crecido);
        if quiere != self.completion_open {
            self.set_completion_open(quiere);
        } else if let Some(pi) = self.shuma_panel {
            // Sin transición pero la franja de la barra pudo cambiar de alto
            // (una fila más de input): la región tiene que seguirla o queda
            // tapando de menos — o de más, comiéndose clicks ajenos.
            self.set_shuma_input_region(pi);
        }
    }

    /// Asegurá que el módulo shell tenga el catálogo de apps lanzables (una vez):
    /// las necesita para ofrecer candidatos-app en el completado desde la primera
    /// tecla. Lo empuja pata desde su `AppRegistry`.
    pub(super) fn asegurar_apps(&mut self) {
        if self.shuma.inner.apps.is_empty() {
            self.shuma.inner.apps = crate::apps_lanzables(&self.registry);
        }
    }

    /// `true` si el shell que pinta el drawer (la sesión activa del modelo
    /// full, o el inner bare) tiene un **PTY interactivo vivo** — claude/vim/
    /// htop consumiendo teclado. Mientras sea cierto el drawer se trata como
    /// una terminal estable: el watchdog anti-atasco no lo cierra (mirar el
    /// output 45 s sin tipear es uso normal, no un wedge).
    pub(super) fn shuma_pty_vivo(&self) -> bool {
        match self.shuma_full.as_ref() {
            Some(full) => {
                crate::shuma_app::active_shell_state(full).is_some_and(|s| s.tiene_pty_vivo())
            }
            None => self.shuma.inner.tiene_pty_vivo(),
        }
    }

    /// `true` si ese PTY además entró a **pantalla completa** (alt-screen):
    /// el drawer debe darle la terminal entera (piso 0.95 de alto).
    pub(super) fn shuma_tui_fullscreen(&self) -> bool {
        match self.shuma_full.as_ref() {
            Some(full) => {
                crate::shuma_app::active_shell_state(full).is_some_and(|s| s.is_fullscreen_tui())
            }
            None => self.shuma.inner.is_fullscreen_tui(),
        }
    }

    /// `true` si el claude de la sesión activa está **trabajando** (spinner
    /// vivo en su cola). Alimenta el PS1 (pulsa vs estable) y el placeholder
    /// «pensando…» de la barra.
    pub(super) fn shuma_claude_ocupado(&self) -> bool {
        match self.shuma_full.as_ref() {
            Some(full) => {
                crate::shuma_app::active_shell_state(full).is_some_and(|s| s.claude_ocupado)
            }
            None => self.shuma.inner.claude_ocupado,
        }
    }

    /// Espejo **full** de [`Self::asegurar_apps`]: empuja el catálogo a la
    /// sesión activa del modelo full (el modo default), que es de donde el
    /// completado arma sus candidatos-app (tier 0, con ícono). Sin esto el modo
    /// full ofrecía sólo tokens/historial — el "no discrimina apps".
    pub(super) fn asegurar_apps_full(&mut self) {
        let sin_apps = self
            .shuma_full
            .as_ref()
            .and_then(crate::shuma_app::active_shell_state)
            .is_some_and(|s| s.apps.is_empty());
        if sin_apps {
            let apps = crate::apps_lanzables(&self.registry);
            if let Some(full) = self.shuma_full.as_mut() {
                crate::shuma_app::asegurar_shell_apps(full, move || apps);
            }
        }
    }

    /// #3 — launcher: spawnea (detached) la app que el input haya pedido lanzar
    /// (por Enter sobre un candidato-app, o el match sin prefijo). Devuelve `true`
    /// si lanzó algo — el caller lo usa para **no** desplegar el drawer (lanzar
    /// una app queda "caleta": la sesión igual la registra, pero no salta el
    /// Quake). No-op si no hay nada pendiente.
    pub(super) fn drenar_app_launch(&mut self) -> bool {
        if let Some(cmd) = self.shuma.inner.take_app_launch() {
            crate::spawn_cmd(&cmd);
            true
        } else {
            false
        }
    }

    /// Factor de revelado del drawer `0..1` (0 = enrollado/oculto, 1 = desplegado).
    /// Sube al abrir (ease-out cúbico), baja al cerrar (smoothstep). Gobierna el
    /// alto del clip y el fade del cuerpo del drawer.
    pub(super) fn shuma_reveal(&self) -> f32 {
        if let Some(t) = self.shuma_closing_at {
            // El enrollado visual termina en SHUMA_CLOSE_ROLL (< SHUMA_CLOSE); el
            // resto del tiempo reveal queda en 0 pintando frames vacíos antes de
            // encoger la surface (ver la nota de SHUMA_CLOSE).
            let p = (t.elapsed().as_secs_f32() / crate::layer::SHUMA_CLOSE_ROLL.as_secs_f32())
                .clamp(0.0, 1.0);
            let e = p * p * (3.0 - 2.0 * p); // smoothstep
            return (1.0 - e).clamp(0.0, 1.0);
        }
        match self.shuma_reveal_at {
            Some(t) => {
                let p = (t.elapsed().as_secs_f32() / crate::layer::SHUMA_OPEN.as_secs_f32())
                    .clamp(0.0, 1.0);
                // smootherstep (6p⁵−15p⁴+10p³): velocidad ~0 en ambos extremos, así
                // el drawer no salta al arrancar ni frena de golpe al asentarse.
                p * p * p * (p * (p * 6.0 - 15.0) + 10.0)
            }
            // Pendiente: la surface todavía crece de la barra a pantalla completa;
            // nada revelado aún (evita animar sobre un buffer chico → sin tirón).
            None => 0.0,
        }
    }

    /// ¿El drawer está animando (abriendo o cerrando)? El draw loop mantiene el
    /// panel dirty mientras dure, para no cortar la animación.
    pub(super) fn shuma_animando(&self) -> bool {
        self.shuma_closing_at.is_some()
            // Pendiente de desenrollar (surface creciendo): seguimos pintando para
            // no perder el instante en que `draw` estampa `shuma_reveal_at`.
            || (self.shuma.open && self.shuma_reveal_at.is_none())
            || self
                .shuma_reveal_at
                .is_some_and(|t| t.elapsed() < crate::layer::SHUMA_OPEN)
    }

    /// Propaga la visibilidad del canvas del drawer a AMBOS modelos (el módulo
    /// bare y la shuma completa): gobierna si las teclas van al PTY interactivo
    /// o al input de la barra.
    pub(super) fn set_shuma_canvas_visible(&mut self, visible: bool) {
        self.shuma.inner.canvas_visible = visible;
        if let Some(full) = self.shuma_full.as_mut() {
            crate::shuma_app::set_canvas_visible(full, visible);
        }
    }

    /// Drena la cola de la shuma completa (live-wire) y aplica cada `Msg`.
    /// Repinta el panel si el drawer está abierto (plegado igual avanza el
    /// modelo, sólo no fuerza repaint).
    pub(super) fn drain_shuma_full(&mut self, pi: usize) {
        let msgs: Vec<crate::shuma_app::Msg> = match self.shuma_full_rx.as_ref() {
            Some(rx) => rx.try_iter().collect(),
            None => return,
        };
        // DIAG: pulso del drain (cada ~50 tandas con contenido, para no inundar):
        // si estas líneas NO aparecen con el drawer abierto, los ticks de la
        // shuma no están llegando al loop de pata (o el draw del panel no corre).
        if crate::layer::diag_on() && !msgs.is_empty() {
            use std::sync::atomic::{AtomicU64, Ordering};
            static TANDAS: AtomicU64 = AtomicU64::new(0);
            static TOTAL: AtomicU64 = AtomicU64::new(0);
            let t = TANDAS.fetch_add(1, Ordering::Relaxed);
            let tot = TOTAL.fetch_add(msgs.len() as u64, Ordering::Relaxed);
            if t % 50 == 0 {
                eprintln!(
                    "pata·drain_shuma_full tandas={t} msgs_acum={tot} (esta tanda: {})",
                    msgs.len()
                );
            }
            // Con el drawer abierto, vuelca cada ~2 s los gates que deciden el
            // render del cuerpo — para distinguir "el modelo vivo no ve el PTY"
            // de "la vista no lo pinta" (headless ambos andan; el metal no).
            if self.shuma.open {
                static ULTIMO: AtomicU64 = AtomicU64::new(0);
                let ahora = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if ahora.saturating_sub(ULTIMO.load(Ordering::Relaxed)) >= 2 {
                    ULTIMO.store(ahora, Ordering::Relaxed);
                    let (pty, alt, ses, dims, consola) = self
                        .shuma_full
                        .as_ref()
                        .and_then(|f| {
                            crate::shuma_app::active_shell_state(f).map(|s| {
                                let dims = s
                                    .running
                                    .as_ref()
                                    .and_then(|arc| arc.try_lock().ok())
                                    .and_then(|g| g.tui.as_ref().map(|t| (t.rows, t.cols)))
                                    .unwrap_or((0, 0));
                                (
                                    s.tiene_pty_vivo(),
                                    s.is_fullscreen_tui(),
                                    f.active_session,
                                    dims,
                                    s.diag_consola(),
                                )
                            })
                        })
                        .unwrap_or((false, false, usize::MAX, (0, 0), String::new()));
                    eprintln!(
                        "pata·drawer gates: pty_vivo={pty} altscreen={alt} ses={ses} pty_dims={}x{} reveal={:.2} h={} closing={} {consola}",
                        dims.0,
                        dims.1,
                        self.shuma_reveal(),
                        self.panels[pi].height,
                        self.shuma_closing_at.is_some(),
                    );
                    // Volcado del LAYOUT COMPUTADO del último render del panel:
                    // dónde colapsa la altura (nodos h≈0) y qué cajas dominan.
                    // Es la radiografía del "cuerpo transparente/incompleto".
                    if let Some(c) = self.panels[pi].cache.as_ref() {
                        let mut total = 0usize;
                        let mut h0 = 0usize;
                        let mut cajas: Vec<(f32, f32, f32, f32, String)> = Vec::new();
                        for n in &c.mounted.nodes {
                            if let Some(r) = c.computed.get(n.id) {
                                total += 1;
                                if r.h <= 1.0 {
                                    h0 += 1;
                                }
                                // Fill del nodo (rgba) o "-" si no tiene: la
                                // radiografía distingue "caja sin fondo" de
                                // "fondo con alpha bajo del theme vivo".
                                let fill = n
                                    .fill
                                    .map(|col| {
                                        let [r, g, b, a] = col.components;
                                        format!(
                                            "#{:02x}{:02x}{:02x}a{:.2}",
                                            (r * 255.0) as u8,
                                            (g * 255.0) as u8,
                                            (b * 255.0) as u8,
                                            a
                                        )
                                    })
                                    .unwrap_or_else(|| "-".into());
                                cajas.push((r.x, r.y, r.w, r.h, fill));
                            }
                        }
                        cajas.sort_by(|a, b| {
                            (b.2 * b.3).partial_cmp(&(a.2 * a.3)).unwrap_or(std::cmp::Ordering::Equal)
                        });
                        cajas.truncate(10);
                        eprintln!(
                            "pata·drawer layout: nodos={total} h0={h0} cajas_top={cajas:?}"
                        );
                    }
                }
            }
        }
        if msgs.is_empty() {
            return;
        }
        self.apply_shuma_full(msgs);
        if self.shuma.open {
            self.panels[pi].dirty = true;
        }
    }

    /// Aplica una tanda de `Msg` a la shuma completa con el handle
    /// channel-backed (sus follow-ups vuelven a la cola).
    pub(super) fn apply_shuma_full(&mut self, msgs: Vec<crate::shuma_app::Msg>) {
        let Some(handle) = self.shuma_full_handle.clone() else {
            return;
        };
        if let Some(mut full) = self.shuma_full.take() {
            for m in msgs {
                // Enviar (Enter) → **firme** (despliega el drawer para ver la salida).
                // El `FocusInput` (clic/foco del input) YA NO abre el drawer: sólo
                // enfoca la barra fina, así el completado flotante bonito aparece al
                // tipear (gate `!shuma.open`). Espeja `accion_click_input` del path bare.
                let modo = if crate::shuma_app::msg_is_submit_raw(&m) {
                    Some(crate::shuma::OpenMode::Firme)
                } else {
                    None
                };
                full = crate::shuma_app::update(full, m, &handle, |x| x);
                if let Some(modo) = modo {
                    self.shuma_full = Some(full);
                    self.set_shuma_open(Some(modo));
                    full = self.shuma_full.take().unwrap();
                }
            }
            self.shuma_full = Some(full);
        }
    }

    /// Traduce un evento de teclado de SCTK al `llimphi_ui::KeyEvent`.
    pub(super) fn keysym_to_keyevent(
        &self,
        event: &smithay_client_toolkit::seat::keyboard::KeyEvent,
    ) -> Option<llimphi_ui::KeyEvent> {
        use llimphi_ui::{Key, NamedKey};
        use smithay_client_toolkit::seat::keyboard::Keysym as K;
        let named = match event.keysym {
            K::Return | K::KP_Enter => Some(NamedKey::Enter),
            K::BackSpace => Some(NamedKey::Backspace),
            K::Tab | K::ISO_Left_Tab => Some(NamedKey::Tab),
            K::Escape => Some(NamedKey::Escape),
            K::Up => Some(NamedKey::ArrowUp),
            K::Down => Some(NamedKey::ArrowDown),
            K::Right => Some(NamedKey::ArrowRight),
            K::Left => Some(NamedKey::ArrowLeft),
            K::Home => Some(NamedKey::Home),
            K::End => Some(NamedKey::End),
            K::Page_Up => Some(NamedKey::PageUp),
            K::Page_Down => Some(NamedKey::PageDown),
            K::Delete => Some(NamedKey::Delete),
            K::Insert => Some(NamedKey::Insert),
            K::F1 => Some(NamedKey::F1),
            K::F2 => Some(NamedKey::F2),
            K::F3 => Some(NamedKey::F3),
            K::F4 => Some(NamedKey::F4),
            K::F5 => Some(NamedKey::F5),
            K::F6 => Some(NamedKey::F6),
            K::F7 => Some(NamedKey::F7),
            K::F8 => Some(NamedKey::F8),
            K::F9 => Some(NamedKey::F9),
            K::F10 => Some(NamedKey::F10),
            K::F11 => Some(NamedKey::F11),
            K::F12 => Some(NamedKey::F12),
            _ => None,
        };
        let modifiers = llimphi_ui::Modifiers {
            shift: self.mods.shift,
            ctrl: self.mods.ctrl,
            alt: self.mods.alt,
            meta: self.mods.logo,
        };
        let (key, text) = if let Some(n) = named {
            (Key::Named(n), None)
        } else {
            let txt = match event.utf8.as_deref() {
                Some(s) if !s.is_empty() && !s.chars().all(char::is_control) => s.to_string(),
                _ => event.keysym.key_char()?.to_string(),
            };
            (Key::Character(txt.as_str().into()), Some(txt))
        };
        Some(llimphi_ui::KeyEvent {
            key,
            state: llimphi_ui::KeyState::Pressed,
            text,
            modifiers,
            repeat: false,
        })
    }

    /// Reencuentra el panel que hospeda el menú de inicio (el del `start_button`
    /// o, en CDE, el `front_panel`). Se computa una vez al arrancar, pero un
    /// hot-reload o un orden de creación inesperado lo pueden dejar en `None`;
    /// esto lo resana sobre los paneles vivos. Devuelve `None` si de verdad no
    /// hay barra con botón de inicio.
    pub(super) fn resolve_menu_panel(&mut self) -> Option<usize> {
        if self.menu_panel.is_none() {
            self.menu_panel = self.panels.iter().position(|p| {
                let s = &self.cfg.surfaces[p.idx];
                s.start
                    .iter()
                    .chain(&s.center)
                    .chain(&s.end)
                    .any(|w| w.kind == "start_button" || w.kind == "front_panel")
            });
            if self.menu_panel.is_none() && std::env::var_os("PATA_DIAG").is_some() {
                eprintln!(
                    "pata diag · menú inicio: ningún panel tiene start_button/front_panel \
                     (paneles={}); el botón no abrirá nada",
                    self.panels.len()
                );
            }
        }
        self.menu_panel
    }

    /// Estampa el **snapshot congelado** de los fugaces si no hay uno vigente
    /// (lo llaman `RevealFantasmas`/`FantasmaPin`; el gemelo winit vive en
    /// `lib.rs`). El `BarData` es **parcial**: sólo los campos que miran los
    /// candidatos fugaces — el resto en default.
    pub(super) fn estampar_fugaz_fijo(&mut self) {
        if self.fugaz_fijo.is_some() {
            return;
        }
        let now = willay_emit::ahora_usec();
        let freeze = {
            let data = render::BarData {
                weather: self.weather_now.as_ref(),
                network: self.network_now.as_ref(),
                media: self.media_now.as_ref(),
                cava: &self.cava_frame,
                anim_t: self.diente_t0.elapsed().as_secs_f32(),
                matilda: self.matilda_salud.as_ref(),
                cpu: self.ctx.cpu,
                cpu_cores: &self.ctx.cpu_cores
                    [..(self.ctx.cpu_cores_n as usize).min(self.ctx.cpu_cores.len())],
                cpu_temp: self.cpu_temp,
                bat: self.bat_now,
                bat_evento: now < self.bat_evento_hasta,
                fugaz_uso: Some(&self.fugaz_uso),
                volume: self.ctx.volume,
                muted: self.ctx.muted,
                moon_phase: self.ctx.moon_phase,
                sun_longitude: self.ctx.sun_longitude_deg,
                cielo: self.cielo_now.as_ref(),
                khipu: Some(&self.khipu_snapshot),
                tampu: self.tampu_now.as_ref(),
                usb: self.usb_now.as_ref(),
                brightness: self.ctx.brightness,
                vol_evento: (now < self.vol_evento_hasta).then_some(self.vol_subiendo),
                net_trafico: self.red_trafico,
                fugaz_idx: self.fugaz_idx,
                fugaz_pin: self.fugaz_pin,
                ..Default::default()
            };
            crate::shuma::congelar_fugaces(&data, &self.theme, now)
        };
        self.fugaz_fijo = Some(freeze);
    }

    /// El alto (px) al que crece la surface del menú al abrirse: `MENU_H` para los
    /// menús-banda de siempre, o el **alto lógico del monitor** para la pantalla de
    /// confirmación fullscreen (`MenuKind::Confirm`), que debe cubrir todo. Fallback a
    /// 2160 si el output no reporta su tamaño.
    pub(super) fn menu_surface_height(&self) -> u32 {
        if self.menu_kind == MenuKind::Confirm {
            return self
                .menu_panel
                .and_then(|pi| self.panels[pi].output.as_ref())
                .and_then(|o| self.output_state.info(o))
                .and_then(|i| i.logical_size)
                .map(|(_, h)| (h.max(1) as u32).min(16384))
                .unwrap_or(2160);
        }
        MENU_H
    }

    /// Despliega/repliega el menú de inicio.
    pub(super) fn set_menu_open(&mut self, open: bool) {
        let Some(pi) = self.resolve_menu_panel() else {
            diag!("pata diag · set_menu_open({open}) SIN panel de menú (no hay start_button) → no-op");
            return;
        };
        if self.menu_open == open {
            return;
        }
        diag!(
            "pata diag · set_menu_open({open}) kind={:?} panel={pi} h_actual={}",
            self.menu_kind,
            self.panels[pi].height
        );
        self.menu_open = open;
        self.menu_opened_at = open.then(std::time::Instant::now);
        if open {
            // El ancla del menú es la x del click que lo abrió (última posición
            // del puntero sobre una barra): el diálogo se posa debajo del icono
            // que lo invocó, no al centro de la pantalla.
            self.menu_anchor_x = self.pointer_ultimo_x;
        }
        // El desenrollado NO arranca aquí: la surface aún mide la barra fina.
        // `draw` estampa `menu_reveal_at` cuando el `configure` la agranda a MENU_H,
        // así el fade+slide nacen con el buffer grande ya presente (sin tirón/sliver).
        self.menu_reveal_at = None;
        if open {
            // Cada apertura arranca en la primera categoría, sin selección.
            self.menu_cat = None;
            self.menu_sel = 0;
        } else {
            self.menu_query.clear();
            self.menu_scroll = 0.0;
            self.menu_cat = None;
            self.menu_sel = 0;
        }
        let h = if open { self.menu_surface_height() } else { self.menu_bar_px };
        let layer = &self.panels[pi].layer;
        layer.set_size(0, h);
        let toma_teclado = open && matches!(self.menu_kind, MenuKind::Apps | MenuKind::Khipu);
        layer.set_keyboard_interactivity(if toma_teclado {
            smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity::Exclusive
        } else {
            smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity::None
        });
        layer.commit();
        self.panels[pi].cache = None;
        self.panels[pi].dirty = true;
    }

    /// Drena las solicitudes del agente polkit. La primera abre el diálogo (crece
    /// el panel del menú como `Polkit` y captura el teclado); si ya hay una en
    /// curso, la nueva se rechaza.
    pub(super) fn poll_polkit(&mut self) {
        let Some(h) = &self.polkit else { return };
        let mut nuevas = Vec::new();
        while let Some(req) = h.try_recv() {
            nuevas.push(req);
        }
        for req in nuevas {
            if self.polkit_prompt.is_none() {
                self.polkit_input.clear();
                self.polkit_prompt = Some(req);
                self.menu_kind = MenuKind::Polkit;
                self.set_menu_open(true);
                self.set_menu_keyboard(true);
            } else {
                let _ = req.reply.send(None);
            }
        }
    }

    /// Cierra el diálogo de polkit: revoca el teclado y repliega el menú.
    pub(super) fn cerrar_polkit(&mut self) {
        self.polkit_input.clear();
        self.set_menu_keyboard(false);
        self.set_menu_open(false);
    }

    /// Concede o revoca el foco de teclado al panel del menú abierto (lo usa la
    /// entrada de contraseña Wi-Fi, que necesita teclear dentro del popup como el
    /// buscador del menú de inicio).
    pub(super) fn set_menu_keyboard(&mut self, exclusive: bool) {
        let Some(pi) = self.menu_panel else { return };
        use smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity;
        let layer = &self.panels[pi].layer;
        layer.set_keyboard_interactivity(if exclusive {
            KeyboardInteractivity::Exclusive
        } else {
            KeyboardInteractivity::None
        });
        layer.commit();
    }

    /// Abre/cierra el drawer de la barra del menú mostrando el cuerpo `kind`.
    pub(super) fn toggle_menu(&mut self, kind: MenuKind) {
        diag!(
            "pata diag · toggle_menu({kind:?}) desde open={} kind_previo={:?} menu_panel={:?}",
            self.menu_open,
            self.menu_kind,
            self.menu_panel
        );
        if self.menu_open && self.menu_kind == kind {
            self.set_menu_open(false);
        } else if self.menu_open {
            self.menu_kind = kind;
            // Cambio de menú con otro click → re-ancla bajo el icono nuevo y
            // arranca su scroll desde arriba.
            self.menu_anchor_x = self.pointer_ultimo_x;
            self.menu_scroll = 0.0;
            if let Some(pi) = self.menu_panel {
                // Apps (buscador) y Khipu (borrador de nota) capturan el teclado.
                let toma = matches!(kind, MenuKind::Apps | MenuKind::Khipu);
                let layer = &self.panels[pi].layer;
                layer.set_keyboard_interactivity(if toma {
                    smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity::Exclusive
                } else {
                    smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity::None
                });
                layer.commit();
                self.panels[pi].cache = None;
                self.panels[pi].dirty = true;
            }
        } else {
            self.menu_kind = kind;
            self.set_menu_open(true);
        }
    }

    /// Actualiza el tooltip flotante para el nodo `node_idx` bajo el cursor.
    pub(super) fn update_tooltip(&mut self, pi: usize, node_idx: Option<usize>, qh: &QueueHandle<Self>) {
        let Some(tpi) = self.tooltip_pi else { return };
        if pi == tpi {
            return;
        }
        let info = node_idx.and_then(|i| {
            let c = self.panels[pi].cache.as_ref()?;
            let node = c.mounted.nodes.get(i)?;
            let text = node.tooltip.clone()?;
            let rect = c.computed.get(node.id)?;
            Some((text, rect))
        });
        match info {
            Some((text, rect)) => {
                let x = rect.x.max(0.0) as i32;
                let y = self.panels[pi].height as i32 + 4;
                let w = (text.chars().count() as u32 * 8 + 16).clamp(24, 600);
                let h = 24u32;
                self.tooltip_text = Some(text);
                {
                    let layer = &self.panels[tpi].layer;
                    layer.set_margin(y, 0, 0, x);
                    layer.commit();
                    layer.set_size(w, h);
                    layer.commit();
                }
                self.panels[tpi].width = w;
                self.panels[tpi].height = h;
                self.panels[tpi].dirty = true;
                self.draw(tpi, qh);
            }
            None => self.hide_tooltip(qh),
        }
    }

    /// Oculta el tooltip encogiendo la surface a 1×1.
    pub(super) fn hide_tooltip(&mut self, qh: &QueueHandle<Self>) {
        let Some(tpi) = self.tooltip_pi else { return };
        if self.tooltip_text.is_none() {
            return;
        }
        self.tooltip_text = None;
        {
            let layer = &self.panels[tpi].layer;
            layer.set_size(1, 1);
            layer.commit();
        }
        self.panels[tpi].width = 1;
        self.panels[tpi].height = 1;
        self.panels[tpi].dirty = true;
        self.draw(tpi, qh);
    }

    /// Lanza una app del menú por su `id` y cierra el menú.
    pub(super) fn lanzar_app(&mut self, id: String) {
        if let Some(app) = self.registry.get(&id) {
            // Vía arje si está levantado (Ente OneShot); si no, crudo.
            arje_applaunch::launch_entry(app);
        }
        self.set_menu_open(false);
    }

    /// Marca para re-pintar la barra que hospeda el menú de inicio.
    pub(super) fn marcar_menu_dirty(&mut self) {
        if let Some(pi) = self.menu_panel {
            self.panels[pi].cache = None;
            self.panels[pi].dirty = true;
        }
    }

    /// Enter en el menú de inicio: lanza el primer resultado del filtro.
    pub(super) fn lanzar_seleccionado_menu(&mut self) {
        let style = crate::MenuStyle::from_cfg(&self.cfg.general.menu_style);
        let ids =
            render::menu_nav_ids(self.registry.all(), &self.menu_query, style, self.menu_cat);
        let id = ids.get(self.menu_sel).or_else(|| ids.first()).cloned();
        if let Some(id) = id {
            self.lanzar_app(id);
        }
    }

    /// Mueve la selección de teclado del menú de inicio. `dx`/`dy` vienen de
    /// las flechas: en el reposo del estilo Classic ←/→ cambian de CATEGORÍA
    /// (con wrap, reseteando la fila) y ↑/↓ recorren las apps del panel; en la
    /// búsqueda (cualquier estilo) todo es lineal sobre las coincidencias; en
    /// el grid GNOME ↑/↓ saltan una fila entera (columnas).
    pub(super) fn mover_sel_menu(&mut self, dx: i32, dy: i32) {
        let style = crate::MenuStyle::from_cfg(&self.cfg.general.menu_style);
        let buscando = !self.menu_query.is_empty();
        if dx != 0 && !buscando && matches!(style, crate::MenuStyle::Classic) {
            let ncats = render::menu_cats_len(self.registry.all());
            if ncats > 0 {
                let cur = self.menu_cat.unwrap_or(0).min(ncats - 1) as i32;
                self.menu_cat = Some((cur + dx).rem_euclid(ncats as i32) as usize);
                self.menu_sel = 0;
                self.menu_scroll = 0.0;
                self.marcar_menu_dirty();
            }
            return;
        }
        let cols = if matches!(style, crate::MenuStyle::Gnome) {
            (self.cfg.general.menu_columns.max(1)) as i32
        } else {
            1
        };
        let len = render::menu_nav_ids(self.registry.all(), &self.menu_query, style, self.menu_cat)
            .len();
        if len == 0 {
            return;
        }
        let paso = dx + dy * cols;
        let nuevo = (self.menu_sel as i32 + paso).clamp(0, len as i32 - 1) as usize;
        if nuevo != self.menu_sel {
            self.menu_sel = nuevo;
            self.autoscroll_menu();
            self.marcar_menu_dirty();
        }
    }

    /// Ajusta `menu_scroll` para que la fila seleccionada quede visible dentro
    /// del viewport del cuerpo (mismas cuentas que `start_menu_view`: fila =
    /// `APP_ROW_H` + gap, viewport = alto del menú menos barra/search/cromo).
    fn autoscroll_menu(&mut self) {
        let row = render::APP_ROW_H + 3.0;
        let viewport = (self.menu_surface_height() as f32
            - self.menu_bar_px as f32
            - render::MENU_SEARCH_H
            - 55.0)
            .max(row);
        let y0 = self.menu_sel as f32 * row;
        if y0 < self.menu_scroll {
            self.menu_scroll = y0;
        } else if y0 + row > self.menu_scroll + viewport {
            self.menu_scroll = y0 + row - viewport;
        }
    }

    /// Sondea el plano de datos del sidebar.
    pub(super) fn poll_nav(&mut self) {
        let mut cambios = false;
        if let Some(rx) = self.nav_rx.as_ref() {
            let mut ultimo = None;
            while let Ok(o) = rx.try_recv() {
                ultimo = Some(o);
            }
            if let Some(outcome) = ultimo {
                match outcome {
                    PollOutcome::Ok { socket, resp } => {
                        self.nav.socket = Some(socket);
                        self.nav.apply_monads(*resp);
                    }
                    PollOutcome::Failed(e) => {
                        self.nav.socket = None;
                        self.nav.error = Some(e);
                    }
                }
                cambios = true;
            }
        }
        while let Ok(outcome) = self.members_rx.try_recv() {
            match outcome {
                MembersOutcome::Ok { monad, members } => self.nav.apply_members(monad, members),
                MembersOutcome::Failed(e) => self.nav.error = Some(e),
            }
            cambios = true;
        }
        // Resultados del motor RAG (respuesta/error/listo): los procesa el mismo
        // `handle_msg`, que marca los sidebars sucios al mutar el estado.
        while let Ok(m) = self.rag_rx.try_recv() {
            self.handle_msg(m);
        }
        if cambios {
            self.marcar_sidebars_dirty();
        }
    }

    /// `true` si el diente abierto del sidebar es el panel RAG (su contenido es
    /// `rag`/`search`). El teclado se rutea a su buscador sólo entonces.
    pub(super) fn rag_panel_open(&self) -> bool {
        self.nav.open.values().any(|&(si, ti)| {
            self.cfg
                .surfaces
                .get(si)
                .and_then(|s| s.tabs.get(ti))
                .map(|t| crate::rag::is_rag_kind(&t.content.kind))
                .unwrap_or(false)
        })
    }

    /// El `app_id` del toplevel que tiene foco ahora.
    pub(super) fn focused_app_id(&self) -> Option<&str> {
        self.toplevels
            .iter()
            .find(|t| t.activated)
            .map(|t| t.app_id.as_str())
    }

    /// Sondea el rail hospedado: cambios de dientes (revisión) y **comandos
    /// sueltos** (esquina caliente → togglear shuma). Corre cada frame; el rail
    /// respira siempre, así que un comando se recoge en ~un frame.
    pub(super) fn poll_host(&mut self) {
        // Tomamos revisión + comandos soltando el borrow de `self.host` antes de
        // dispatchar (handle_msg necesita `&mut self`).
        let (rev, cmds) = match &self.host {
            Some(h) => (h.revision(), h.take_commands()),
            None => return,
        };
        if rev != self.last_host_rev {
            self.last_host_rev = rev;
            self.marcar_sidebars_dirty();
        }
        for cmd in cmds {
            match cmd {
                pata_host::ShellCommand::ToggleShuma => {
                    // Debounce: la esquina caliente puede re-disparar mientras el
                    // puntero sigue en la zona (dwell) — dos Toggle seguidos abren
                    // y cierran al instante ("abre parpadeando, se cierra solo").
                    // Un segundo Toggle dentro de la ventana se ignora.
                    use std::sync::atomic::{AtomicU64, Ordering};
                    static ULTIMO_MS: AtomicU64 = AtomicU64::new(0);
                    let ahora_ms = (willay_emit::ahora_usec() / 1000) as u64;
                    let previo = ULTIMO_MS.swap(ahora_ms, Ordering::Relaxed);
                    if ahora_ms.saturating_sub(previo) < 700 {
                        diag!("pata diag · comando ToggleShuma DEBOUNCED (repetido en <700ms)");
                        continue;
                    }
                    // Anclar el drawer al monitor DONDE ESTÁ EL USUARIO: la barra
                    // de shuma del mismo output que el último panel con puntero.
                    // Sin esto abría en el monitor del `shuma_panel` viejo — "no
                    // pasa nada" porque pasaba en la otra pantalla.
                    if let Some(up) = self.ultimo_panel_puntero {
                        let out = self.panel_out_key(up);
                        if let Some(&pi) = self
                            .shuma_panels
                            .iter()
                            .find(|&&p| self.panel_out_key(p) == out)
                        {
                            self.focus_shuma_panel(pi);
                        }
                    }
                    diag!(
                        "pata diag · comando ToggleShuma (esquina caliente) → panel {:?}",
                        self.shuma_panel
                    );
                    self.handle_msg(crate::Msg::ShumaToggle);
                }
            }
        }
    }

    /// Marca todas las superficies sidebar para re-pintar.
    pub(super) fn marcar_sidebars_dirty(&mut self) {
        for p in &mut self.panels {
            if p.card.is_none() && self.cfg.surfaces[p.idx].kind == SurfaceKind::Sidebar {
                p.dirty = true;
            }
        }
    }

    /// Índice del drawer del sidebar `si` en el monitor `out` (conector). Sin
    /// match (mono-monitor, sin dato de output) cae al primero de ese `si`.
    pub(super) fn drawer_panel_en(&self, si: usize, out: Option<&str>) -> Option<usize> {
        let mut primero = None;
        for i in 0..self.panels.len() {
            let p = &self.panels[i];
            if p.idx != si || !p.drawer {
                continue;
            }
            if primero.is_none() {
                primero = Some(i);
            }
            if out.is_some() && self.panel_output_name(i).as_deref() == out {
                return Some(i);
            }
        }
        primero
    }

    /// ¿El panel `pi` es un drawer MOSTRADO? Con varios monitores el drawer de
    /// un sidebar `"*"` existe en cada salida y cada una muestra (o no) el
    /// suyo, según el diente desplegado de ESE monitor (`drawers_mostrados`).
    pub(super) fn drawer_mostrado_en(&self, pi: usize) -> bool {
        let p = &self.panels[pi];
        p.drawer && self.drawers_mostrados.contains(&(p.idx, self.panel_out_key(pi)))
    }

    /// Los paneles drawer del sidebar `si` actualmente MOSTRADOS (uno por
    /// monitor con su diente desplegado — un mismo sidebar puede estar abierto
    /// en varias pantallas a la vez).
    pub(super) fn drawers_mostrados_de(&self, si: usize) -> Vec<usize> {
        (0..self.panels.len())
            .filter(|&pi| self.panels[pi].idx == si && self.drawer_mostrado_en(pi))
            .collect()
    }

    /// Hace del monitor del panel `pi` el **dueño** del sidebar (a dónde va la
    /// próxima apertura del drawer). Se llama en cada press sobre un rail o
    /// drawer de sidebar — espeja `focus_shuma_panel`/`focus_menu_panel`: los
    /// popups y paneles se despliegan en la pantalla que estás usando.
    pub(super) fn focus_sidebar_panel(&mut self, pi: usize) {
        let idx = self.panels[pi].idx;
        let es_sidebar = self
            .cfg
            .surfaces
            .get(idx)
            .map(|s| s.kind == SurfaceKind::Sidebar)
            .unwrap_or(false);
        if !es_sidebar {
            return;
        }
        if let Some(name) = self.panel_output_name(pi) {
            if self.drawer_output.as_deref() != Some(name.as_str()) {
                diag!("pata diag · focus_sidebar_panel({pi}) → output {name}");
                self.drawer_output = Some(name);
            }
        }
    }

    /// `true` si el rail del sidebar `si` debe pintarse OCULTO ahora: tiene autohide y
    /// no está revelado. **Independiente del panel**: el autohide esconde SÓLO los
    /// dientes (el rail), aunque haya un diente desplegado — el panel es una surface
    /// aparte que sigue su propia reserva (Fijo). Sin autohide el rail siempre visible.
    pub(super) fn sidebar_oculto(&self, si: usize) -> bool {
        self.cfg.surfaces.get(si).map(|s| s.autohide).unwrap_or(false)
            && !self.revealed_sidebars.contains(&si)
    }

    /// Ajusta la input-region del RAIL `pi`: revelado = toda la surface (`None`);
    /// oculto = solo una fina franja pegada al borde de pantalla (la zona caliente de
    /// reaparición), para que el puntero pueda revelarlo sin comerse los clics del
    /// contenido de atrás (con autohide el escritorio recuperó la franja del rail, así
    /// que hay apps debajo que deben recibir el click).
    fn set_rail_input_region(&self, pi: usize, revealed: bool) {
        use smithay_client_toolkit::compositor::Region;
        use smithay_client_toolkit::shell::WaylandSurface;
        /// Ancho de la franja caliente de reaparición, px.
        const EDGE_W: i32 = 3;
        let layer = &self.panels[pi].layer;
        if revealed {
            layer.wl_surface().set_input_region(None);
        } else if let Some(comp) = self.compositor.as_ref() {
            if let Ok(region) = Region::new(comp) {
                let idx = self.panels[pi].idx;
                let sw = self.panels[pi].width as i32;
                let right = self
                    .cfg
                    .surfaces
                    .get(idx)
                    .map(|s| s.anchor == pata_core::Anchor::Right)
                    .unwrap_or(false);
                let x = if right { (sw - EDGE_W).max(0) } else { 0 };
                region.add(x, 0, EDGE_W, self.panels[pi].height.max(8192) as i32);
                layer.wl_surface().set_input_region(Some(region.wl_region()));
            }
        }
        layer.commit();
    }

    /// Revela el rail autohide del sidebar `si` (puntero en la franja caliente). Solo
    /// cambia el estado + marca dirty; la input-region la aplica la reconciliación del
    /// `draw` (único lugar que la toca).
    pub(super) fn revelar_sidebar(&mut self, si: usize) {
        if self.revealed_sidebars.insert(si) {
            self.marcar_rails_de(si);
        }
    }

    /// Invalida y marca dirty TODOS los rails del sidebar `si` (con `"*"` hay
    /// uno por monitor).
    fn marcar_rails_de(&mut self, si: usize) {
        for pi in 0..self.panels.len() {
            if self.panels[pi].idx == si && !self.panels[pi].drawer && self.panels[pi].card.is_none()
            {
                self.panels[pi].cache = None;
                self.panels[pi].dirty = true;
            }
        }
    }

    /// Re-oculta el rail autohide del sidebar `si` (el puntero se fue). Esconde SÓLO
    /// los dientes aunque su drawer esté desplegado: el panel es una surface aparte que
    /// permanece (Fijo). Para volver a ver los dientes, hover en la franja caliente.
    pub(super) fn ocultar_sidebar(&mut self, si: usize) {
        if self.revealed_sidebars.remove(&si) {
            self.marcar_rails_de(si);
        }
    }

    /// Activa/repliega el diente `(si, ti)`. Sólo toca el ESTADO (`nav.open`) y
    /// marca el rail sucio; el drawer (una surface aparte, YA creada al arranque)
    /// lo muestra/oculta [`Self::reconcile_drawer`] en el próximo `draw`.
    /// Ya NO redimensiona el rail — ese resize por-diente fallaba en Iris Xe.
    pub(super) fn set_sidebar_open(&mut self, si: usize, ti: usize) {
        let dos_pasos = self.cfg.general.diente_dos_pasos;
        let out = self.out_para_sidebar(si);
        self.nav.activate_tab(&out, si, ti, dos_pasos);
        // Los rails repintan la pastilla activa del diente (con `"*"` hay uno
        // por monitor); el drawer se reconcilia solo.
        for pi in 0..self.panels.len() {
            if self.panels[pi].idx == si && !self.panels[pi].drawer {
                self.panels[pi].cache = None;
                self.panels[pi].dirty = true;
            }
        }
    }

    /// El monitor destino de una apertura del sidebar `si`: el del último
    /// press sobre un rail/drawer (`drawer_output`); sin dato (apertura
    /// programática antes de todo press), el del primer rail de `si`.
    fn out_para_sidebar(&self, si: usize) -> String {
        self.drawer_output
            .clone()
            .or_else(|| {
                (0..self.panels.len())
                    .find(|&i| {
                        let p = &self.panels[i];
                        p.idx == si && !p.drawer && p.card.is_none()
                    })
                    .and_then(|pi| self.panel_output_name(pi))
            })
            .unwrap_or_default()
    }

    /// La clave de monitor del panel `pi` para el estado por-monitor del
    /// sidebar (`nav.open`): el conector, o `""` si no se resolvió.
    pub(super) fn panel_out_key(&self, pi: usize) -> String {
        self.panel_output_name(pi).unwrap_or_default()
    }

    /// El escritorio activo **del monitor `out`** (1-based; `0` = desconocido):
    /// el del rail de sidebar que vive en esa pantalla. Con `out` vacío (clave
    /// del backend winit / sin dato), el primer rail; sin rails, el global.
    fn workspace_activo_en(&self, out: &str) -> u8 {
        let rail = (0..self.panels.len()).find(|&i| {
            let p = &self.panels[i];
            !p.drawer
                && p.card.is_none()
                && self
                    .cfg
                    .surfaces
                    .get(p.idx)
                    .map(|s| s.kind == SurfaceKind::Sidebar)
                    .unwrap_or(false)
                && (out.is_empty() || self.panel_out_key(i) == out)
        });
        rail.map(|pi| self.panel_workspace_view(pi).0)
            .unwrap_or(self.ctx.active_workspace)
    }

    /// **Invariante del rail**: el panel visible de un diente-escritorio sigue
    /// SIEMPRE al escritorio activo del monitor del sidebar — jamás debe verse
    /// el diente activo en uno y el panel de otro. Los cambios que entran por
    /// el propio rail ya re-apuntan al clickear (`WorkspaceTooth`); esto cubre
    /// los que llegan de afuera (atajo de teclado, mirada-ctl, clic en el
    /// switcher de la barra): se aplica en cada sample, cuando `outputs=` ya
    /// refleja el cambio. Los dientes que no son de escritorio (config,
    /// terminal, host, footer) no se tocan.
    pub(super) fn sidebar_sigue_al_workspace(&mut self) {
        use crate::render::sidebar::{TERM_BASE, WS_BASE};
        let es_ws = |ti: usize| (WS_BASE..TERM_BASE).contains(&(ti as u64));
        // Por CADA monitor con estado de sidebar: su panel/selección siguen al
        // escritorio activo de ESA pantalla (independientes entre sí).
        let outs: std::collections::HashSet<String> = self
            .nav
            .active
            .keys()
            .chain(self.nav.open.keys())
            .cloned()
            .collect();
        for out in outs {
            let activo = self.workspace_activo_en(&out);
            if activo == 0 {
                continue;
            }
            let nuevo = WS_BASE as usize + activo as usize;
            // La selección (relevante con `diente_dos_pasos`): que el próximo
            // clic sobre el diente activo expanda directo, sin paso fantasma.
            if let Some(&(si, ti)) = self.nav.active.get(&out) {
                if es_ws(ti) && ti != nuevo {
                    self.nav.active.insert(out.clone(), (si, nuevo));
                }
            }
            // El panel desplegado: re-apuntar el drawer al escritorio nuevo.
            if let Some(&(si, ti)) = self.nav.open.get(&out) {
                if es_ws(ti) && ti != nuevo {
                    self.nav.open.insert(out.clone(), (si, nuevo));
                    self.nav.scroll = 0.0;
                    // Un menú contextual abierto pertenecía al panel viejo.
                    self.nav.close_menu();
                    for pi in 0..self.panels.len() {
                        if self.panels[pi].idx == si && !self.panels[pi].drawer {
                            self.panels[pi].cache = None;
                            self.panels[pi].dirty = true;
                        }
                    }
                    self.marcar_todo_dirty();
                }
            }
        }
    }

    /// Muestra/oculta el **drawer** del sidebar para que refleje `nav.open`, SIN
    /// crear ni destruir surfaces (crear/destruir surfaces wgpu en vivo pierde el
    /// `VkSurface` en Iris Xe → `ERROR_SURFACE_LOST_KHR` y muere pata). El drawer
    /// se crea UNA vez al arranque (como el tooltip/OSD) y aquí sólo se togglea:
    ///
    /// - **abierto**: `input_region = None` (todo el drawer recibe clicks) y se
    ///   repinta con el contenido del diente.
    /// - **cerrado**: `input_region` VACÍA (el puntero lo atraviesa hacia lo de
    ///   atrás) y se repinta transparente (invisible).
    ///
    /// Idempotente: si el sidebar mostrado no cambió, retorna sin tocar nada (el
    /// contenido del drawer abierto lo refresca `dirty`).
    pub(super) fn reconcile_drawer(&mut self, _qh: &QueueHandle<Self>) {
        // FAST-PATH SIN ALOCAR (corre por CADA frame-callback): si el conjunto de
        // drawers deseado ya coincide con el mostrado, salimos sin construir el
        // HashSet `want`. En reposo (nada desplegado) es len 0 == 0 → cero trabajo;
        // con algo abierto, un `contains` por entrada en vez de un HashSet nuevo.
        // El control puede colisionar con una entrada de `nav.open` (mismo (si,out)):
        // ahí `quiere_n` sobreestima y caemos al camino lento — correcto, no rompe.
        let control_extra = self.nav.control_open && self.nav.control_si.is_some();
        let quiere_n = self.nav.open.len() + control_extra as usize;
        let ya_conciliado = quiere_n == self.drawers_mostrados.len()
            && self
                .nav
                .open
                .iter()
                .all(|(out, &(si, _))| self.drawers_mostrados.contains(&(si, out.clone())))
            && (!control_extra
                || self.drawers_mostrados.contains(&(
                    self.nav.control_si.unwrap(),
                    self.drawer_output.clone().unwrap_or_default(),
                )));
        if ya_conciliado {
            return;
        }
        // Qué drawers deben verse: el diente desplegado de CADA monitor
        // (`nav.open`, por conector — cada pantalla expande lo suyo), más la
        // ventanita del control suelto (`control_si`, sin diente) en el monitor
        // dueño del último press. (Camino lento: sólo cuando algo cambió.)
        let mut want: std::collections::HashSet<(usize, String)> = self
            .nav
            .open
            .iter()
            .map(|(out, &(si, _))| (si, out.clone()))
            .collect();
        if self.nav.control_open {
            if let Some(si) = self.nav.control_si {
                want.insert((si, self.drawer_output.clone().unwrap_or_default()));
            }
        }
        if want == self.drawers_mostrados {
            return;
        }
        diag!(
            "pata diag · reconcile_drawer want={want:?} shown={:?}",
            self.drawers_mostrados
        );
        let prev = std::mem::take(&mut self.drawers_mostrados);
        // Ocultar los que sobran (cerrados o re-apuntados).
        for (si, out) in prev.iter() {
            if !want.contains(&(*si, out.clone())) {
                if let Some(pi) =
                    self.drawer_panel_en(*si, (!out.is_empty()).then_some(out.as_str()))
                {
                    self.set_drawer_clickable(pi, false);
                    self.panels[pi].cache = None;
                    self.panels[pi].dirty = true; // se repinta transparente.
                }
            }
        }
        // Mostrar los nuevos, cada uno en SU monitor.
        let mut hubo_nuevo = false;
        for (si, out) in want.iter() {
            if prev.contains(&(*si, out.clone())) {
                continue;
            }
            match self.drawer_panel_en(*si, (!out.is_empty()).then_some(out.as_str())) {
                Some(pi) => {
                    diag!(
                        "pata diag · reconcile MOSTRAR si={si} out={out} → drawer pi={pi} {}x{}",
                        self.panels[pi].width,
                        self.panels[pi].height
                    );
                    // ¿Se muestra sólo por el control (sin diente)? Entonces toda la
                    // surface (card + backdrop) debe recibir clicks; si hay diente, sólo
                    // el panel (salvo que además el control esté abierto).
                    let solo_control = self.nav.open_en(out).map(|(s, _)| s) != Some(*si);
                    if solo_control || self.nav.control_open {
                        self.set_drawer_full_input(pi);
                    } else {
                        self.set_drawer_clickable(pi, true);
                    }
                    self.panels[pi].cache = None;
                    self.panels[pi].dirty = true; // se repinta con el contenido.
                }
                None => diag!("pata diag · reconcile MOSTRAR si={si} out={out} → SIN drawer"),
            }
            hubo_nuevo = true;
        }
        if hubo_nuevo {
            // Marca de apertura para la gracia anti-churn del guardado-al-desenfocar.
            self.drawer_opened_at = Some(std::time::Instant::now());
            // Mostrar un drawer cancela cualquier cierre-por-flota diferido stale (un
            // timestamp viejo no debe cerrar el que se acaba de abrir).
            self.flota_close_at = None;
        } else if want.is_empty() {
            self.drawer_opened_at = None;
        }
        // Re-anclar la geometría de los sidebars afectados (reserva del panel en
        // Fijo, offset del rail en Adentro): el que se cerró suelta, el que se
        // abrió reserva — por monitor. `aplicar_geometria_sidebar` lee el estado
        // nuevo, por eso va DESPUÉS de actualizarlo.
        let sis: std::collections::HashSet<usize> = prev
            .iter()
            .map(|(si, _)| *si)
            .chain(want.iter().map(|(si, _)| *si))
            .collect();
        self.drawers_mostrados = want;
        for si in sis {
            self.aplicar_geometria_sidebar(si);
        }
    }

    /// Ajusta la input-region del drawer `pi`: `None` (toda la surface recibe input,
    /// drawer abierto) o VACÍA (el puntero lo atraviesa, drawer cerrado).
    ///
    /// CLAVE (verificado con el diag de mirada): hay que commitear con
    /// `LayerSurface::commit()` (el commit de sctk), NO con el `wl_surface.commit()`
    /// crudo. El crudo NO latcheaba la región (mirada seguía viendo la región VACÍA
    /// del arranque aun con el drawer abierto → `region=VACÍA(atraviesa)` en el log),
    /// probablemente porque wgpu/Mesa maneja el `wl_surface` por su cuenta. El commit
    /// de la LayerSurface es el mismo camino que usa el arranque —que SÍ latchea—.
    fn set_drawer_clickable(&self, pi: usize, clickable: bool) {
        use smithay_client_toolkit::compositor::Region;
        use smithay_client_toolkit::shell::WaylandSurface;
        let layer = &self.panels[pi].layer;
        if clickable {
            // La surface es más ancha que el panel (ancho máximo fijo, para resize sin
            // recrearla). El área clickeable = SOLO el panel al ancho actual, pegado a
            // su borde interno (izquierda si el sidebar es izquierdo, derecha si es
            // derecho). El resto de la surface (transparente) lo atraviesa el puntero.
            if let Some(comp) = self.compositor.as_ref() {
                if let Ok(region) = Region::new(comp) {
                    let idx = self.panels[pi].idx;
                    let pw = self
                        .cfg
                        .surfaces
                        .get(idx)
                        .map(|s| s.panel_width.max(1.0))
                        .unwrap_or(300.0) as i32;
                    let sw = self.panels[pi].width as i32;
                    // Alto amplio (el compositor recorta a la surface): evita que un
                    // `height` provisional (1 px, antes del primer `configure`) deje el
                    // panel incliqueable justo tras el boot.
                    let sh = self.panels[pi].height.max(8192) as i32;
                    let right = self
                        .cfg
                        .surfaces
                        .get(idx)
                        .map(|s| s.anchor == pata_core::Anchor::Right)
                        .unwrap_or(false);
                    let x = if right { (sw - pw).max(0) } else { 0 };
                    region.add(x, 0, pw, sh);
                    layer.wl_surface().set_input_region(Some(region.wl_region()));
                }
            }
        } else if let Some(comp) = self.compositor.as_ref() {
            if let Ok(region) = Region::new(comp) {
                layer.wl_surface().set_input_region(Some(region.wl_region())); // vacía = atraviesa.
            }
        }
        layer.commit(); // commit de la LayerSurface (latchea la región; el crudo no).
        diag!("pata diag · set_drawer_clickable pi={pi} clickable={clickable} → commit");
    }

    /// Declara (o retira) la **región opaca** de la `wl_surface` del panel `pi`
    /// según si la barra es hoy un rectángulo 100 % opaco. Una barra lo es cuando:
    /// es una `Bar`/`Sidebar` (no un drawer/fondo), su fondo no tiene alfa
    /// (`bg_panel_alt.a · opacity ≥ 1`), sin margen ni radio (que dejan borde/
    /// esquinas transparentes) y **replegada** (desplegar shuma/menú crece la
    /// surface con zonas translúcidas). Declararla opaca deja que mirada saltee el
    /// frost del glass y que cualquier compositor no dibuje lo que queda debajo —el
    /// grueso del CPU que se iba blureando un fondo tapado—. Si NO es opaca (o
    /// dudamos), se **retira** la región (`None`) → comportamiento de siempre.
    ///
    /// Sólo re-committea cuando el estado cambia (`region_opaca[pi]`), para no
    /// meter un commit por frame (el crudo de wgpu no latchea la región; hay que
    /// usar `LayerSurface::commit()`, igual que la input-region — ver
    /// [`Self::set_drawer_clickable`]).
    fn actualizar_region_opaca(&mut self, pi: usize) {
        use smithay_client_toolkit::compositor::Region;
        use smithay_client_toolkit::shell::WaylandSurface;
        let idx = self.panels[pi].idx;
        let (w, h) = (self.panels[pi].width, self.panels[pi].height);
        let opaca = (|| {
            if self.panels[pi].drawer || w == 0 || h == 0 {
                return false;
            }
            let Some(s) = self.cfg.surfaces.get(idx) else { return false };
            if !matches!(s.kind, SurfaceKind::Bar | SurfaceKind::Sidebar) {
                return false; // sólo barras/rails; los fondos/docks/paneles no
            }
            if s.margin > 0.0 || s.radius > 0.0 {
                return false; // margen/esquinas → borde transparente
            }
            // Fondo efectivo opaco: alfa del color × opacity de la surface ≥ 1.
            let bg_a = self.theme.bg_panel_alt.components[3];
            if bg_a * s.opacity < 0.999 {
                return false;
            }
            // Replegada: la barra crece (shuma/menú) en su eje FINO; si el eje fino
            // supera el grosor base, hay zonas translúcidas (reveal) → no opaca.
            let fino = if s.anchor.es_horizontal() { h } else { w } as f32;
            fino <= s.thickness + 6.0
        })();
        // Sólo committear si el estado cambió respecto al último declarado.
        if self.region_opaca.get(&pi).copied() == Some(opaca) {
            return;
        }
        let Some(comp) = self.compositor.as_ref() else { return };
        let layer = &self.panels[pi].layer;
        if opaca {
            if let Ok(region) = Region::new(comp) {
                region.add(0, 0, w as i32, h as i32);
                layer.wl_surface().set_opaque_region(Some(region.wl_region()));
            } else {
                return; // no pudimos crear la región: no toques el estado
            }
        } else {
            layer.wl_surface().set_opaque_region(None);
        }
        layer.commit(); // LayerSurface::commit latchea la región (el crudo no).
        self.region_opaca.insert(pi, opaca);
        diag!("pata diag · region_opaca pi={pi} opaca={opaca} {w}x{h} → commit");
    }

    /// Ensancha la input-region del drawer `pi` a TODA la surface (region = None). Se
    /// usa mientras el popover de disposición está abierto: la card + su backdrop de
    /// click-away viven en la surface del drawer (640 px), FUERA del panel angosto, así
    /// que necesitan que toda la surface reciba clicks. Al cerrarlo se restaura al panel
    /// con [`Self::set_drawer_clickable`].
    fn set_drawer_full_input(&self, pi: usize) {
        use smithay_client_toolkit::shell::WaylandSurface;
        let layer = &self.panels[pi].layer;
        layer.wl_surface().set_input_region(None); // None = toda la surface clickeable.
        layer.commit();
        diag!("pata diag · set_drawer_full_input pi={pi} → commit");
    }

    /// Restaura la input-region del drawer mostrado tras cerrar un overlay que la
    /// había ensanchado (el menú contextual del taskbar): vuelve al panel angosto,
    /// salvo que el popover de disposición siga abierto (que también necesita toda
    /// la surface). Complementa [`Self::set_drawer_full_input`].
    fn restaurar_input_drawer(&self) {
        for (si, out) in &self.drawers_mostrados {
            if let Some(pi) = self.drawer_panel_en(*si, (!out.is_empty()).then_some(out.as_str()))
            {
                if self.nav.control_open {
                    self.set_drawer_full_input(pi);
                } else {
                    self.set_drawer_clickable(pi, true);
                }
            }
        }
    }

    /// Cambia EN VIVO el eje docked de la surface `si` y re-aplica su geometría. Sin
    /// re-exec — el compositor re-tesela las ventanas al recibir el commit.
    pub(super) fn aplicar_docked_sidebar(&mut self, si: usize, docked: bool) {
        if let Some(s) = self.cfg.surfaces.get_mut(si) {
            s.reserve = Some(docked);
        }
        self.aplicar_geometria_sidebar(si);
    }

    /// Re-ancla EN VIVO las dos surfaces (rail + drawer) del sidebar `si`: posición
    /// lateral (`set_margin`) y reserva de espacio (`set_exclusive_zone`), según los ejes
    /// **Fijo/Flota** (`reserve`), **Autoesconde** (`autohide`), **Rail Adentro/Afuera**
    /// (`rail_outside`) y si el panel está DESPLEGADO. Todo con `set_margin`/`exclusive_zone`
    /// (nunca `set_size` — recrear el swapchain crashea Iris Xe [[pata-sidebar-panel-resize]]).
    ///
    /// El compositor (smithay `LayerMap::arrange`) reserva zonas en ORDEN de creación de
    /// las surfaces: el **rail** se crea antes que el **drawer**, así que el rail encoge
    /// la zona usable primero. De ahí las dos disposiciones:
    /// - **Afuera** `[borde] rail(thickness) | panel(panel_w)`: el rail reserva `thickness`
    ///   (empuja el drawer a `thickness`); el drawer reserva `panel_w`.
    /// - **Adentro** `[borde] panel(panel_w) | rail(thickness)`: el rail NO reserva y se
    ///   corre a `panel_w` por margen; el drawer va al borde y reserva `panel_w+thickness`
    ///   (el total) → las apps arrancan detrás del rail.
    ///
    /// Dos reservas INDEPENDIENTES, una por eje:
    /// - **rail (dientes)** ← eje **Ocultar** (`autohide`): reserva su `thickness` sólo
    ///   como fixture permanente, `!autohide`. Con autohide el rail se esconde y
    ///   **suelta** su franja → el escritorio se come el espacio de los dientes. NO
    ///   depende de `reserve`/Fijo.
    /// - **panel (el "sidebar")** ← eje **Espacio** (`reserve`): reserva su `panel_w`
    ///   cuando hay un diente desplegado y es Fijo (`docked`), aunque autoesconda — el
    ///   escritorio NO se come el panel.
    pub(super) fn aplicar_geometria_sidebar(&mut self, si: usize) {
        let Some(s) = self.cfg.surfaces.get(si) else {
            return;
        };
        let thickness = s.thickness.max(1.0) as i32;
        let panel_w = s.panel_width.max(1.0) as i32;
        let docked = s.reserve.unwrap_or(self.sidebar_docked);
        let autohide = s.autohide;
        let rail_outside = s.rail_outside.unwrap_or(self.dientes_outside);
        let right = s.anchor == pata_core::Anchor::Right;
        // "Abierto" para efectos de RESERVA de franja = hay un diente desplegado
        // en este sidebar EN ese monitor (se evalúa por-panel abajo). Un drawer
        // mostrado sólo por la ventanita de opciones (sin diente) NO reserva: la
        // card flota sobre el escritorio sin reacomodarlo.
        // Dos ejes independientes: el RAIL (columna de dientes) reserva su franja según
        // **Ocultar** (`!autohide` → fixture permanente; autoesconde → la suelta). El
        // PANEL reserva su ancho al desplegarse según **Espacio** (`docked`/Fijo). Así
        // "dientes fijos + panel flotante" (o viceversa) es una combinación válida.
        let rail_reserva = !autohide;
        let panel_reserva = docked;

        // (margen lateral desde el borde, exclusive_zone) para rail y drawer, en
        // función de si el drawer está desplegado EN ESE monitor. Con varias
        // pantallas el drawer se muestra en una sola: las demás quedan con la
        // geometría de cerrado (nada de reservar franja por un panel ajeno).
        let geometria = |open: bool| -> (i32, i32, i32, i32) {
            if !open {
                // Cerrado: sólo el rail-fixture al borde reserva; con autohide reserva 0
                // (el escritorio recupera la franja de los dientes).
                (0, if rail_reserva { thickness } else { 0 }, 0, 0)
            } else if rail_outside {
                // Afuera: el rail va al borde (reserva thickness sólo si es fixture); el
                // panel a continuación reserva panel_w si es Fijo. Si el rail flota
                // (autohide), el drawer se corre a thickness por margen y el rail overlaya.
                (
                    0,
                    if rail_reserva { thickness } else { 0 },
                    if rail_reserva { 0 } else { thickness },
                    if panel_reserva { panel_w } else { 0 },
                )
            } else {
                // Adentro: el panel va al borde y el rail se corre a panel_w. El panel
                // reserva panel_w (+thickness sólo si el rail también es fixture, para dejar
                // las apps detrás del rail). Con autohide reserva sólo panel_w (rail overlaya).
                (
                    panel_w,
                    0,
                    0,
                    if panel_reserva {
                        if rail_reserva { panel_w + thickness } else { panel_w }
                    } else {
                        0
                    },
                )
            }
        };

        for i in 0..self.panels.len() {
            if self.panels[i].idx != si {
                continue;
            }
            // ¿El drawer está desplegado en el monitor de ESTE panel?
            let open_aqui = self
                .nav
                .open_en(&self.panel_out_key(i))
                .map(|(s, _)| s)
                == Some(si);
            let (rail_off, rail_excl, drawer_off, drawer_excl) = geometria(open_aqui);
            let p = &self.panels[i];
            let (off, excl) = if p.drawer {
                (drawer_off, drawer_excl)
            } else {
                (rail_off, rail_excl)
            };
            if right {
                p.layer.set_margin(0, off, 0, 0); // (top, right, bottom, left)
            } else {
                p.layer.set_margin(0, 0, 0, off);
            }
            p.layer.set_exclusive_zone(excl);
            p.layer.commit();
        }
        diag!(
            "pata diag · geometria si={si} outside={rail_outside} \
             rail_reserva={rail_reserva} panel_reserva={panel_reserva}"
        );
        self.marcar_sidebars_dirty();
    }

    /// Cierra el panel del sidebar (si alguno está abierto).
    pub(super) fn cerrar_sidebar(&mut self) {
        if self.nav.open.is_empty() {
            return;
        }
        self.nav.open.clear();
        self.marcar_sidebars_dirty();
        self.marcar_todo_dirty();
    }

    /// Cancela un cierre-por-flota diferido si el puntero volvió a tocar alguna
    /// surface (rail o panel) del sidebar `si`. Lo llama el pointer-frame en cada
    /// `Enter`/`Motion` sobre un panel — así mover el puntero panel↔dientes (dos
    /// surfaces del mismo sidebar) no lo cierra.
    pub(super) fn flota_cancelar_cierre(&mut self, si: usize) {
        if matches!(&self.flota_close_at, Some((s, _, _)) if *s == si) {
            self.flota_close_at = None;
        }
    }

    /// Confirma un cierre-por-flota diferido: si venció [`FLOTA_CLOSE_GRACE`] sin que
    /// el puntero volviera al sidebar, **guarda** (cierra) su panel. Idempotente y
    /// llamado desde `draw` (que late en continuo vía `latido`), como
    /// `finalize_shuma_close`.
    pub(super) fn finalize_flota_close(&mut self) {
        if let Some((si, out, t)) = self.flota_close_at.clone() {
            if t.elapsed() >= crate::layer::FLOTA_CLOSE_GRACE {
                self.flota_close_at = None;
                // Sólo guarda el panel del MONITOR que el puntero abandonó; los
                // drawers de las demás pantallas siguen en lo suyo.
                if self.nav.open.get(&out).map(|&(s, _)| s) == Some(si) {
                    self.nav.open.remove(&out);
                    self.marcar_sidebars_dirty();
                    self.marcar_todo_dirty();
                }
            }
        }
    }

    /// Expande/colapsa un nodo del navegador.
    pub(super) fn nav_toggle(&mut self, id: u64) {
        if self.nav.expanded.contains(&id) {
            self.nav.expanded.remove(&id);
        } else {
            self.nav.expanded.insert(id);
            if let (Some(mid), Some(sock)) =
                (self.nav.needs_resolve(id), self.nav.socket.clone())
            {
                let tx = self.members_tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(crate::nouser::resolve(sock, mid));
                });
            }
        }
        self.marcar_sidebars_dirty();
    }

    /// Recoge el último snapshot del hilo de muestreo. Incluye hot-reload de config.
    pub(super) fn maybe_recargar_config(&mut self) -> bool {
        if !self.cfg_watch.changed() {
            return false;
        }
        // TOML roto a mitad de una edición a mano: CONSERVÁ el marco actual en
        // vez de pisarlo con el preset — un typo no debe volarte el escritorio.
        // Al corregir el archivo, el próximo cambio de mtime recarga normal.
        let Some(cfg) = pata_config::try_load() else {
            return false;
        };
        // Comparamos el conteo de superficies ENCENDIDAS: agregar/quitar una
        // barra O prenderla/apagarla cambia cuántas layer surfaces hay que
        // anclar → re-exec. (Editar dientes dentro de una barra: hot-reload.)
        let enc = |c: &pata_core::Config| c.surfaces.iter().filter(|s| s.enabled).count();
        if enc(&cfg) != enc(&self.cfg) {
            // Cambió la CANTIDAD de superficies (p. ej. vista mac/mirada con
            // 2 superficies vs. una vista de 1): no se pueden reanclar layer
            // surfaces en caliente sin recrearlas. La vía limpia: re-ejecutar
            // pata en el mismo proceso (`exec`), que arranca leyendo el nuevo
            // launcher.toml y ancla las superficies correctas. Sin esto, cambiar
            // a mac/mirada "no hacía nada" (el reload se descartaba).
            self.re_exec_pata("cambió la cantidad de superficies");
            return false;
        }
        self.surfaces = crate::Model::construir_surfaces(&cfg);
        let mut theme = llimphi_theme::Theme::dark();
        if let Some(c) = crate::render::parse_hex(&cfg.general.accent) {
            theme.accent = c;
        }
        self.theme = theme;
        self.cfg = cfg;
        true
    }

    /// Re-ejecuta pata en el mismo proceso (`exec`) para reanclar las layer
    /// surfaces cuando un cambio no se puede aplicar en caliente. Sólo retorna si
    /// el exec falló.
    pub(super) fn re_exec_pata(&self, motivo: &str) {
        eprintln!("pata · {motivo}; re-ejecutando para reanclar las layer surfaces.");
        if let Ok(exe) = std::env::current_exe() {
            use std::os::unix::process::CommandExt;
            let args: Vec<String> = std::env::args().skip(1).collect();
            let err = std::process::Command::new(exe).args(args).exec();
            eprintln!("pata · re-exec falló: {err}; reinicia pata a mano.");
        }
    }

    pub(super) fn maybe_sample(&mut self) {
        // Cosecha una grabación que murió sola (p. ej. wf-recorder no pudo iniciar
        // el encode): así el punto rojo no queda pegado. Corre siempre, aunque el
        // sampler todavía no tenga un cuadro nuevo.
        if self.grabacion.as_mut().is_some_and(|g| !g.vivo()) {
            self.grabacion = None;
            self.marcar_todo_dirty();
        }
        let Some((mut ctx, clipboard)) = self.sampler.latest() else {
            return;
        };
        // Sostiene el realce optimista del switcher hasta que el muestreo
        // confirme el salto (un sample viejo reportaría el escritorio anterior y
        // parpadearía). Misma lógica pura que el backend winit.
        let (pending, active) =
            crate::sampler::reconcile_optimistic(self.pending_ws, ctx.active_workspace);
        self.pending_ws = pending;
        ctx.active_workspace = active;
        self.maybe_recargar_config();
        // La disposición del sidebar vive en el TEMA/VISTA (`cfg.general`), que
        // `maybe_recargar_config` acaba de refrescar en `self.cfg`. Un cambio en
        // cualquiera de los dos ejes —`sidebar_dientes_outside` (posición del
        // rail) o `sidebar_docked` (reserva de franja / exclusive_zone)— cambia el
        // anclaje de los rails → re-exec para reanclar.
        if self.cfg.general.sidebar_dientes_outside != self.dientes_outside {
            self.dientes_outside = self.cfg.general.sidebar_dientes_outside;
            self.re_exec_pata("cambió la posición del rail (dientes adentro/afuera)");
        }
        if self.cfg.general.sidebar_docked != self.sidebar_docked {
            self.sidebar_docked = self.cfg.general.sidebar_docked;
            self.re_exec_pata("cambió el docked del sidebar (reserva de franja)");
        }
        self.ctx = ctx;
        // Con el ctx fresco (outputs= ya refleja un cambio de escritorio hecho
        // por teclado/ctl), el panel del diente-escritorio sigue al activo.
        self.sidebar_sigue_al_workspace();
        if crate::push_clip_history(&mut self.clip_history, &clipboard) {
            if let Some(t) = &clipboard {
                // Persiste el clip nuevo (Klipper, camino de producción).
                if let Some(store) = &self.clip_store {
                    let _ = store.empujar(
                        pata_portapapeles::Contenido::Texto(t.clone()),
                        willay_emit::ahora_usec(),
                    );
                }
                willay_emit::emitir_silencioso(&crate::evento_clip(t, willay_emit::ahora_usec()));
            }
        }
        self.clipboard = clipboard;
        if let Some(h) = &self.weather {
            if let Some(w) = h.latest() {
                // Ubicación automática: siembra al cielo las coords que el clima
                // resolvió por IP (misma ubicación para ambos).
                if crate::cielo_loc_inicial(&self.cfg).is_none() {
                    if let Some((lat, lon)) = w.coords {
                        if let Ok(mut g) = self.cielo_loc.lock() {
                            *g = Some((lat as f64, lon as f64));
                        }
                    }
                }
                self.weather_now = Some(w);
            }
        }
        if let Some(h) = &mut self.cielo {
            if let Some(st) = h.latest() {
                self.cielo_now = Some(st.clone());
            }
        }
        self.khipu_snapshot = self.khipu.snapshot(crate::khipu::ahora_unix());
        if let Some(h) = &mut self.tampu {
            if let Some(s) = h.latest() {
                self.tampu_now = Some(s.clone());
            }
        }
        if let Some(h) = &mut self.usb {
            if let Some(s) = h.latest() {
                self.usb_now = Some(s.clone());
            }
        }
        if let Some(h) = &mut self.agora {
            if let Some(s) = h.latest() {
                self.agora_now = Some(s.clone());
            }
        }
        if let Some(h) = &mut self.willay {
            if let Some(s) = h.latest() {
                self.willay_now = Some(s.clone());
            }
        }
        if let Some(h) = &self.network {
            if let Some(n) = h.latest() {
                self.network_now = Some(n);
            }
        }
        if let Some(h) = &self.mpris {
            if let Some(m) = h.latest() {
                self.media_now = Some(m);
            }
        }
        if let Some(h) = &self.bluetooth {
            if let Some(b) = h.latest() {
                self.bluetooth_now = Some(b);
            }
        }
        if let Some(h) = &self.unidades {
            if let Some(s) = h.latest() {
                self.unidades_now = Some(s);
            }
        }
        if let Some(h) = &self.flota_discover {
            if let Some(v) = h.latest() {
                self.flota_remoto = Some(v);
            }
        }
        if let Some(h) = &self.movil_discover {
            if let Some(v) = h.latest() {
                self.movil_obs = Some(v);
            }
        }
        if let Some(h) = &self.matilda_local {
            if let Some(rt) = h.latest() {
                self.matilda_now = Some(rt);
            }
        }
        // Salud combinada (local + remoto): recomputada cada tick.
        self.matilda_salud = crate::matilda_salud::SaludFlota::compute(
            self.matilda_now.as_ref(),
            self.flota_remoto.as_deref(),
        );
        // Mezclador: refresca mientras su popup está abierto (sliders en vivo).
        if self.menu_open && self.menu_kind == MenuKind::Volume {
            self.sink_inputs = crate::sampler::sample_sink_inputs();
            self.sinks = crate::sampler::sample_sinks();
            self.source_outputs = crate::sampler::sample_source_outputs();
            self.sources = crate::sampler::sample_sources();
        }
        // Aviso de batería baja (una vez por escalón al descargar).
        if let Some((pct, charging)) = crate::bateria::read() {
            let (nuevo, aviso) = crate::bateria::decidir(pct, charging, self.bat_avisado);
            self.bat_avisado = nuevo;
            // Enchufar/desenchufar es un EVENTO: abre la ventana en que el
            // fantasma de batería sale fijo como acuse visible del cable.
            if crate::bateria::transicion(self.bat_now.map(|(_, c)| c), charging) {
                self.bat_evento_hasta = willay_emit::ahora_usec() + crate::bateria::EVENTO_US;
                self.marcar_shuma_dirty();
            }
            self.bat_now = Some((pct as f32 / 100.0, charging));
            if let Some(a) = aviso {
                crate::bateria::avisar(a, pct);
            }
        }
        self.cpu_temp = crate::sampler::cpu_temp_celsius();
        // Acuse de cambio de volumen: si el nivel (o el mute) se movió desde la
        // última muestra, abre la ventana en que el fantasma de sonido muestra
        // la rampa creciente/decreciente.
        {
            let ahora = (self.ctx.volume, self.ctx.muted);
            if let Some((v0, m0)) = self.vol_prev {
                if (ahora.0 - v0).abs() > 0.005 || ahora.1 != m0 {
                    self.vol_subiendo = ahora.0 >= v0;
                    self.vol_evento_hasta = willay_emit::ahora_usec() + 1_800_000;
                    self.marcar_shuma_dirty();
                }
            }
            self.vol_prev = Some(ahora);
        }
        // Tráfico de red: tasa desde los acumulados de /proc/net/dev,
        // normalizada log para las microbarras del fantasma.
        if let Some((rx, tx)) = crate::network::trafico_totales() {
            let ahora_us = willay_emit::ahora_usec();
            if let Some((rx0, tx0, t0)) = self.red_trafico_prev {
                let dt = (ahora_us.saturating_sub(t0) as f64 / 1_000_000.0).max(0.1);
                self.red_trafico = (
                    crate::network::trafico_frac(rx.saturating_sub(rx0) as f64 / dt),
                    crate::network::trafico_frac(tx.saturating_sub(tx0) as f64 / dt),
                );
            }
            self.red_trafico_prev = Some((rx, tx, ahora_us));
        }
        // Pestañas verticales del rail (`window_tabs` en el slot de un sidebar) o el
        // taskbar del navegador de escritorios (`TabsSource::Workspaces`, el preset
        // nativo): ambas muestrean las ventanas del WM con su escritorio. La lista
        // foreign de este backend no sirve aquí (sin escritorio, ids locales); sólo se
        // corre el subproceso si la config las declara.
        if crate::config_tiene_widget(&self.cfg, "window_tabs")
            || crate::config_quiere_taskbar_ws(&self.cfg)
        {
            self.windows_ws = crate::sampler::sample_windows();
        }
        // Triage semántico de notificaciones (importancia por significado) →
        // marquesina del input del shell hospedado. Prefiere el aviso del triage;
        // si no hay, cae al `sys_alert` del sistema. Es la barra real (layer).
        if let Some(h) = &self.triage {
            if let Some(r) = h.latest() {
                self.triage_now = r;
            }
        }
        let fase = (willay_emit::ahora_usec() / 500_000 % 2) as u8;
        let ahora_us = willay_emit::ahora_usec();
        let sistema = crate::render::sys_alert(self.ctx.cpu, self.bat_now, self.cpu_temp);
        // Gravedad del aviso de sistema: CPU recaliente o batería crítica
        // descargando ⇒ la marquesina cambia BRUSCO (tarjeta fija, sin fundido).
        let sistema_urgente = self
            .cpu_temp
            .map(|t| t >= crate::render::CPU_TEMP_ALERTA_C)
            .unwrap_or(false)
            || matches!(self.bat_now, Some((frac, false)) if frac <= 0.05);
        let flota_aviso = self.matilda_salud.as_ref().and_then(|s| s.resumen());
        let fuentes = crate::marquesina::Fuentes {
            triage: self.triage_now.as_ref().map(|r| (r.texto.as_str(), r.urgencia)),
            sistema: sistema.as_deref().map(|s| (s, sistema_urgente)),
            flota: flota_aviso.as_deref(),
            audio: self
                .media_now
                .as_ref()
                .filter(|m| m.playing)
                .map(|m| m.title.as_str()),
            foco: Some(self.ctx.focused_title.as_str()),
            clip: self.clipboard.as_deref(),
            hora: self.ctx.clock.hour,
        };
        // «pensando…» del agente MANDA sobre la rotativa: el spinner real de
        // claude queda recortado del panel (es chrome del input box), así que
        // la barra lo narra en el placeholder; los avisos externos esperan a
        // que termine (la rotación se pausa, no se pierde — `marquesina_est`
        // retoma donde estaba).
        let marq = if self.shuma_claude_ocupado() {
            let mut m = shuma_module_shell::Marquesina::leve("pensando…");
            m.icono = Some('✻');
            m.icono_rgb = Some([0xB0, 0x7A, 0xE8]);
            Some(m)
        } else {
            crate::marquesina::rotativa(&fuentes, &mut self.marquesina_est, ahora_us)
        };
        // En live-wire (default) el input que se pinta es la sesión activa del
        // modelo full — hay que fijarla ahí, no en el inner bare (que no se
        // pinta). En bare, al inner directo.
        if let Some(full) = self.shuma_full.as_mut() {
            crate::shuma_app::set_active_marquesina(full, marq, fase);
        } else {
            self.shuma.inner.set_marquesina(marq, fase);
        }
        // El control center persistente necesita perfil de energía + luz nocturna
        // frescos (el flyout los leía sólo al abrirse).
        if crate::config_tiene_diente_vivo(&self.cfg) {
            let (pp, night) = crate::render::read_power_night();
            self.control_extras.power_profile = pp;
            self.control_extras.night = night;
        }
        // Paisaje sonoro (takiy): mientras esté encendido, se le empuja el estado
        // del escritorio (apps abiertas + foco + si hay audio real sonando) que
        // pata ya conoce por ser el shell. El hilo decide con histéresis si
        // regenera/silencia. Reflejamos su estado en el control center.
        if let Some(h) = &self.paisaje {
            if self.paisaje_on {
                let focus = self.toplevels.iter().find(|t| t.activated).map(|t| t.app_id.clone());
                let apps = self.toplevels.iter().map(|t| t.app_id.clone()).collect();
                let media = self.media_now.as_ref().map(|m| m.playing).unwrap_or(false);
                h.push_desktop(crate::paisaje::DesktopSnapshot { apps, focus, media });
            }
            self.paisaje_estado = h.estado();
            self.control_extras.paisaje = self.paisaje_estado.enabled;
        }
        // Diente vivo: refresca su manifestación con las señales nuevas.
        self.actualizar_diente();
        // `WidgetCtx` ya no es `Copy` (lleva el título de la ventana enfocada),
        // así que los widgets tickean contra `&self.ctx` (recién asignado).
        for sw in &mut self.surfaces {
            for w in sw.core_mut() {
                w.tick(&self.ctx);
            }
        }
        for p in &mut self.panels {
            if let Some(c) = p.card.as_mut() {
                for w in &mut c.widgets {
                    w.tick(&self.ctx);
                }
            }
            p.dirty = true;
        }
    }

    /// Arma la notificación de inactividad si el compositor la expone y el idle
    /// de energía está habilitado. Idempotente: no re-crea si ya hay una viva.
    pub(super) fn ensure_idle_arm(&mut self, qh: &QueueHandle<Self>) {
        if self.idle_notif.is_some() || !self.energia_cfg.habilitado {
            return;
        }
        let secs = self.energia_cfg.suspender_secs;
        if secs > 0 {
            self.armar_idle(secs, qh);
        }
    }

    /// (Re)crea la notificación de inactividad con `secs` de timeout. Necesita
    /// notifier + seat; cae al primer seat conocido si `self.seat` aún es `None`
    /// (mismo fallback que `activar_ventana`).
    fn armar_idle(&mut self, secs: u32, qh: &QueueHandle<Self>) {
        let Some(notifier) = self.idle_notifier.clone() else {
            return;
        };
        let seat = self
            .seat
            .clone()
            .or_else(|| self.seat_state.seats().next());
        let Some(seat) = seat else {
            return;
        };
        if let Some(old) = self.idle_notif.take() {
            old.destroy();
        }
        let notif = notifier.get_idle_notification(secs.saturating_mul(1000), &seat, qh, ());
        self.idle_notif = Some(notif);
    }

    /// El sistema cumplió el umbral de inactividad: consulta el veto (unidades
    /// del plano de control + carga del sistema) y suspende, **pospone** (con
    /// aviso del motivo) o no hace nada según la política.
    pub(super) fn energia_al_ociar(&mut self, qh: &QueueHandle<Self>) {
        if self.energia_disparado {
            return;
        }
        // Hay batería y NO está cargando = corriendo con batería.
        let en_bateria = matches!(self.bat_now, Some((_, false)));
        let bloqueos =
            crate::energia::reunir_bloqueos(self.unidades_now.as_ref(), &self.energia_cfg);
        let accion = crate::energia::decidir(
            &self.energia_cfg,
            crate::energia::Nivel::Suspender,
            en_bateria,
            &bloqueos,
        );
        match accion {
            crate::energia::Accion::Suspender | crate::energia::Accion::Apagar => {
                crate::energia::ejecutar(&accion, false);
                self.energia_disparado = true;
            }
            crate::energia::Accion::Posponer { .. } => {
                // Avisar el motivo una sola vez; reintentar más tarde si la
                // inactividad sigue (el trabajo puede terminar y entonces sí
                // conviene suspender).
                crate::energia::ejecutar(&accion, !self.energia_pospuesto);
                self.energia_pospuesto = true;
                self.armar_idle(super::REINTENTO_ENERGIA_SECS, qh);
            }
            crate::energia::Accion::Nada => {}
        }
    }

    /// Reconcilia el inhibidor de inactividad del compositor con el estado del
    /// café: lo crea cuando se enciende (pausa apagado-de-pantalla y bloqueo en
    /// mirada) y lo destruye al apagarlo. Idempotente.
    pub(super) fn ensure_cafe_inhibitor(&mut self, qh: &QueueHandle<Self>) {
        use smithay_client_toolkit::shell::WaylandSurface;
        let quiere = self.energia_cfg.cafe;
        if quiere == self.idle_inhibitor.is_some() {
            return;
        }
        if quiere {
            let Some(mgr) = self.idle_inhibit_mgr.clone() else {
                return;
            };
            let Some(panel) = self.panels.first() else {
                return;
            };
            let inh = mgr.create_inhibitor(panel.layer.wl_surface(), qh, ());
            self.idle_inhibitor = Some(inh);
        } else if let Some(inh) = self.idle_inhibitor.take() {
            inh.destroy();
        }
    }

    /// El usuario volvió (hubo actividad): reinicia el ciclo del idle de energía.
    pub(super) fn energia_al_volver(&mut self, qh: &QueueHandle<Self>) {
        self.energia_disparado = false;
        self.energia_pospuesto = false;
        let secs = self.energia_cfg.suspender_secs;
        if self.energia_cfg.habilitado && secs > 0 {
            self.armar_idle(secs, qh);
        }
    }

    /// Drena el último cuadro del visualizador (cava).
    pub(super) fn maybe_cava(&mut self) {
        let Some(h) = &self.cava else {
            return;
        };
        let Some(frame) = h.latest() else {
            return;
        };
        // El daemon sigue mandando cuadros en silencio (ruido de piso), y antes
        // CADA cuadro ensuciaba los 13 paneles. Medido en metal con el escritorio
        // quieto y sin música: 70 de las 120 ensuciadas de 15 s eran ésta, a ~7/s
        // — la canilla que mantenía repintando hasta a los drawers cerrados y las
        // surfaces de 1×1. Ahora sólo ensucia si hay algo que ANIMAR.
        let ensucia = cava_ensucia(&frame, &self.cava_frame);
        self.cava_frame = frame;
        if !ensucia {
            return;
        }
        // Y sólo a quien pinta: ni las cards ni los paneles inertes muestran cava.
        let vivos: Vec<usize> = (0..self.panels.len())
            .filter(|&pi| self.panels[pi].card.is_none() && !self.panel_inerte(pi))
            .collect();
        for pi in vivos {
            self.panels[pi].dirty = true;
        }
    }

    /// Arma las [`pata_core::atencion::Senales`] del diente vivo desde el estado
    /// actual: volumen/mute/CPU del `WidgetCtx`, batería de `bat_now`, música de
    /// `media_now`.
    fn senales_diente(&self) -> pata_core::atencion::Senales {
        pata_core::atencion::Senales {
            volume: self.ctx.volume,
            muted: self.ctx.muted,
            cpu: self.ctx.cpu,
            cpu_temp: self.cpu_temp,
            bateria: self.bat_now.map(|(f, _)| f),
            cargando: self.bat_now.map(|(_, c)| c).unwrap_or(false),
            musica: self.media_now.as_ref().map(|m| m.playing).unwrap_or(false),
        }
    }

    /// Refresca la manifestación del diente vivo (señales frescas → árbitro).
    pub(super) fn actualizar_diente(&mut self) {
        let s = self.senales_diente();
        let now = self.diente_t0.elapsed().as_secs_f64();
        self.diente_manifest = self.atencion.update(s, now);
    }

    /// Detecta el cambio de escritorio activo y arranca/expira la animación del
    /// switcher (el resaltado que viaja de la celda vieja a la nueva).
    pub(super) fn update_ws_anim(&mut self) {
        let cur = self.ctx.active_workspace;
        if cur != 0 && self.ws_last_active != 0 && cur != self.ws_last_active {
            // Arranca desde donde estábamos (o desde el destino de una cometa aún
            // en vuelo, si el usuario encadena cambios rápidos).
            let from = self
                .ws_anim
                .map(|a| a.to)
                .unwrap_or(self.ws_last_active);
            self.ws_anim = Some(crate::layer::WsAnimState {
                from,
                to: cur,
                start: std::time::Instant::now(),
            });
        }
        if cur != 0 {
            self.ws_last_active = cur;
        }
        if let Some(a) = self.ws_anim {
            if a.start.elapsed() >= crate::layer::WS_ANIM {
                self.ws_anim = None;
            }
        }
    }

    /// Fase de apertura del menú de inicio `0..1` (0 = recién abierto, 1 =
    /// asentado). `1.0` si no hay menú abierto. Mueve el fade + slide de entrada.
    pub(super) fn menu_open_t(&self) -> f32 {
        // El reloj es `menu_reveal_at` (estampado cuando la surface ya creció a
        // MENU_H), no `menu_opened_at`: mientras la surface todavía mide la barra
        // fina devolvemos 0 (nada revelado) para no animar sobre un buffer chico —
        // eso era el tirón/sliver parpadeante. Espeja `LayerApp::shuma_reveal`.
        match self.menu_reveal_at {
            Some(t) => (t.elapsed().as_secs_f32() / crate::layer::MENU_OPEN.as_secs_f32())
                .clamp(0.0, 1.0),
            // Menú abierto pero surface aún creciendo: reveal pendiente = 0.
            None if self.menu_open => 0.0,
            None => 1.0,
        }
    }

    /// Factor de aparición `0..1` del completado flotante (misma curva que el
    /// menú): gobierna el fade + el deslizamiento del panel al desplegarse.
    pub(super) fn completion_open_t(&self) -> f32 {
        match self.completion_opened_at {
            Some(t) => (t.elapsed().as_secs_f32() / crate::layer::MENU_OPEN.as_secs_f32())
                .clamp(0.0, 1.0),
            None => 1.0,
        }
    }

    /// La cometa del switcher para este frame (posición interpolada de la cabeza),
    /// o `None` si no hay animación en curso.
    pub(super) fn ws_comet(&self) -> Option<render::WsComet> {
        let a = self.ws_anim?;
        let dur = crate::layer::WS_ANIM.as_secs_f32();
        let t = (a.start.elapsed().as_secs_f32() / dur).clamp(0.0, 1.0);
        let e = 1.0 - (1.0 - t).powi(3); // ease-out cúbico
        let from = a.from as f32 - 1.0;
        let to = a.to as f32 - 1.0;
        Some(render::WsComet {
            head: from + (to - from) * e,
            dir: if to >= from { 1.0 } else { -1.0 },
        })
    }

    /// Crea el estado wgpu de un panel.
    pub(super) fn ensure_gpu(&mut self, pi: usize) {
        if self.panels[pi].gpu.is_some() {
            return;
        }
        let display_ptr = self.conn.backend().display_ptr() as *mut c_void;
        let surface_ptr = self.panels[pi].layer.wl_surface().id().as_ptr() as *mut c_void;
        let (w, h) = (self.panels[pi].width, self.panels[pi].height);
        let display_handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            NonNull::new(display_ptr).expect("wl_display ptr"),
        ));
        let window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            NonNull::new(surface_ptr).expect("wl_surface ptr"),
        ));
        // SAFETY: los handles apuntan a objetos Wayland que `self` mantiene vivos.
        let make_target = || wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: display_handle,
            raw_window_handle: window_handle,
        };

        let surface = if self.hal.is_none() {
            // Throttle del reintento tras un DeviceLost cuyo adapter no vuelve:
            // `new_for_raw_surface` es lento/bloqueante — hacerlo cada frame en 8
            // paneles starva el event-loop de wayland y el compositor desconecta a
            // pata (Broken pipe). Entre intentos salimos barato (el latido sigue
            // pidiendo frames), así que reintentamos con backoff hasta que el
            // adapter vuelva, sin morir ni spamear.
            if let Some(t) = self.hal_retry_after {
                if std::time::Instant::now() < t {
                    return;
                }
            }
            match pollster::block_on(unsafe { Hal::new_for_raw_surface(make_target, w, h) }) {
                Ok((hal, surface)) => {
                    if self.hal_fail_streak > 0 {
                        eprintln!(
                            "pata layer · GPU recuperada tras {} intento(s) sin adapter.",
                            self.hal_fail_streak
                        );
                    }
                    self.hal_fail_streak = 0;
                    self.hal_retry_after = None;
                    self.hal = Some(hal);
                    surface
                }
                Err(e) => {
                    // Backoff creciente: 100ms, 200ms, … hasta un tope de 2s. Sólo
                    // logueamos la primera falla de la racha (no 1388 líneas).
                    if self.hal_fail_streak == 0 {
                        eprintln!("pata layer · panel {pi} sin gpu: {e} — reintento con backoff");
                    }
                    self.hal_fail_streak = self.hal_fail_streak.saturating_add(1);
                    let backoff = std::time::Duration::from_millis(
                        (100u64 * self.hal_fail_streak as u64).min(2000),
                    );
                    self.hal_retry_after = Some(std::time::Instant::now() + backoff);
                    return;
                }
            }
        } else {
            let hal = self.hal.as_ref().expect("hal");
            let wgpu_surface = match unsafe { hal.instance.create_surface_unsafe(make_target()) } {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("pata layer · panel {pi} sin gpu: {e}");
                    return;
                }
            };
            match RawSurface::from_surface(hal, wgpu_surface, display_handle, window_handle, w, h) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("pata layer · panel {pi} sin gpu: {e}");
                    return;
                }
            }
        };
        let hal = self.hal.as_ref().expect("hal");
        diag!(
            "pata diag · panel {pi} surface creada {w}x{h} · backend={:?} format={:?}",
            hal.adapter.get_info().backend,
            surface.format(),
        );
        let renderer = match Renderer::new(hal) {
            Ok(r) => r,
            Err(e) => {
                // No paniquear si el renderer vello no inicializa durante el churn
                // de un reset: dejar el panel sin gpu y reintentar el próximo draw.
                eprintln!("pata layer · panel {pi} sin renderer: {e}");
                return;
            }
        };
        self.panels[pi].gpu = Some(PanelGpu {
            surface,
            renderer,
            typesetter: llimphi_ui::llimphi_text::Typesetter::new(),
            scene: vello::Scene::new(),
            layout: llimphi_ui::llimphi_layout::LayoutTree::new(),
        });
    }

    /// Mantiene vivo el latido de un panel: pide su siguiente frame-callback.
    pub(super) fn latido(&self, pi: usize, qh: &QueueHandle<Self>) {
        // Un solo frame-callback pendiente por panel (patrón Wayland correcto). Si
        // ya pedimos uno y no llegó, NO encolamos otro: encadenar uno por cada
        // draw es el commit-storm que le clava un core al compositor. Se limpia en
        // `frame()` al llegar el callback. El piso de 500ms del compositor garantiza
        // que el callback llegue aunque el commit sea vacío → nada queda congelado.
        if !self.frame_pending.borrow_mut().insert(pi) {
            return; // ya había uno pendiente
        }
        let surface = self.panels[pi].layer.wl_surface();
        surface.frame(qh, surface.clone());
        surface.commit();
    }

    /// Reconstruye **todo el stack GPU** tras un
    /// [`llimphi_ui::llimphi_hal::SurfaceError::DeviceLost`]: suelta el device
    /// muerto (`self.hal`) y **todas** las surfaces/renderers de los paneles
    /// (el device es compartido: si una surface lo pierde, murió para todos), y
    /// re-arma el frame-callback de cada panel que tenía GPU. El próximo `draw`
    /// de cada uno llama `ensure_gpu`, que rehace el `Hal` (el primero, vía
    /// `Hal::new_for_raw_surface`) + su `RawSurface` + `Renderer` contra el
    /// device nuevo. Las `wl_surface`/`LayerSurface` de sctk siguen vivas (sólo
    /// soltamos el estado wgpu), así que la barra se recupera SIN morir por
    /// `closed` ni depender del wrapper de respawn — y sin forzar backend GL.
    /// Sirve para cualquier GPU que pierda el device, no sólo Iris Xe.
    pub(super) fn rebuild_gpu_after_device_loss(&mut self, qh: &QueueHandle<Self>) {
        eprintln!(
            "pata layer · DeviceLost: reconstruyo el stack GPU (device + surfaces + renderers de {} panel/es)",
            self.panels.iter().filter(|p| p.gpu.is_some()).count()
        );
        let had_gpu: Vec<bool> = self.panels.iter().map(|p| p.gpu.is_some()).collect();
        for p in &mut self.panels {
            p.gpu = None;
            p.cache = None;
            p.dirty = true;
        }
        self.hal = None;
        // Reset del throttle: el primer intento de reconstrucción es inmediato; el
        // backoff sólo crece si el adapter sigue ausente (churn largo de Iris Xe).
        self.hal_retry_after = None;
        self.hal_fail_streak = 0;
        // Soltamos los frame-callbacks pendientes: tras el reset re-armamos abajo,
        // y sin limpiar el gate de `latido` los saltearía (creería que siguen en
        // vuelo) y los paneles no reconstruirían.
        self.frame_pending.borrow_mut().clear();
        // Re-armar sólo los paneles que ya tenían GPU (los lazy —tooltip/OSD sin
        // mostrar— se reconstruyen solos en su próximo draw, como siempre).
        for (i, &had) in had_gpu.iter().enumerate() {
            if had {
                self.latido(i, qh);
            }
        }
    }

    /// ¿Hay una animación DISCRETA en curso en/para el panel `pi`? — cometa del
    /// switcher de escritorios, apertura de menú/completado, drawer de shuma
    /// entrando/saliendo, tecla sostenida (auto-repeat), o el visualizador `cava`
    /// con audio. Se distingue de la respiración AMBIENTAL perpetua del diente/halo:
    /// las discretas exigen ~30fps para ir suaves; la ambiental no. Lo usa el cap de
    /// present de `draw` para bajar a ~11fps sólo cuando el único motivo de repintar
    /// es la respiración ambiente.
    fn anim_discreta_activa(&self, pi: usize) -> bool {
        self.ws_comet().is_some()
            || self.key_held.is_some()
            // Árbol Alt-Tab desplegado: mantené el ritmo (que el cursor siga a
            // los Tab crujiente), no lo bajes al ambiente de ~11fps.
            || self.altab.as_ref().is_some_and(|v| v.tree)
            || self.shuma_animando()
            || (self.menu_open
                && self.menu_panel == Some(pi)
                && (self.menu_reveal_at.is_none() || self.menu_open_t() < 1.0))
            || (self.completion_open
                && self.shuma_panel == Some(pi)
                && self.completion_open_t() < 1.0)
            || cava_con_audio(&self.cava_frame)
    }

    /// ¿El panel `pi` no tiene nada que mostrar? (ver [`inerte`]). Resuelve el
    /// «tiene contenido» según qué surface de servicio sea: el OSD por su cartel,
    /// el Alt-Tab por su árbol, el tooltip por su texto. Cualquier otra surface a
    /// 1×1 no tiene qué pintar.
    fn panel_inerte(&self, pi: usize) -> bool {
        let p = &self.panels[pi];
        let con_contenido = if self.osd_pi == Some(pi) {
            self.osd.is_some()
        } else if self.altab_pi == Some(pi) {
            self.altab.is_some()
        } else if self.tooltip_pi == Some(pi) {
            self.tooltip_text.is_some()
        } else {
            false
        };
        inerte(p.drawer, self.drawer_mostrado_en(pi), p.width, p.height, con_contenido)
    }

    /// Avanza el frame de un panel.
    pub(super) fn draw(&mut self, pi: usize, qh: &QueueHandle<Self>) {
        // Surface cerrada por el compositor (monitor desenchufado): no la toques.
        // No recibe frames propios; este guard cubre además los draws internos
        // (OSD/recursión) que pudieran apuntarla. Ver `closed` en event_handlers.
        if self.panels[pi].dead {
            return;
        }
        // Reconciliar el drawer del sidebar (crear/destruir su surface) según
        // `nav.open`. Lo hacemos SÓLO desde los paneles fijos (rails/barras, que
        // laten en continuo), nunca desde el propio drawer: así reconcile jamás
        // toca el panel que estamos por dibujar. El `pi >= len` es una red por si
        // otro camino lo destruyó.
        if !self.panels[pi].drawer {
            self.reconcile_drawer(qh);
        }
        if pi >= self.panels.len() {
            return;
        }
        // Empuje del OSD: su surface arranca 1×1 y podría no recibir frames
        // propios; las barras (que laten en continuo) sirven su draw cuando hay
        // un cartel que mostrar o que encoger. (`pi != osd_pi` evita recursión.)
        if self.osd_pi.is_some() && self.osd_pi != Some(pi) {
            let opi = self.osd_pi.unwrap();
            let needs = self.panels[opi].dirty
                || self.osd.is_some()
                || self.panels[opi].width > 1;
            if needs {
                self.draw(opi, qh);
            }
        }
        // Empuje del árbol Alt-Tab: su surface arranca 1×1 y no late sola; una
        // barra (que late en continuo, y a la que mirada le manda frame-callback
        // en cada cambio del switcher) SONDEA el archivo runtime y sirve su draw.
        // Guardado a paneles «reales» (ni OSD ni el propio Alt-Tab) para no
        // recursar entre las dos surfaces empujadas.
        if self.altab_pi.is_some() && Some(pi) != self.altab_pi && Some(pi) != self.osd_pi {
            let nuevo = crate::altab::read();
            let api = self.altab_pi.unwrap();
            if nuevo != self.altab {
                self.altab = nuevo;
                self.panels[api].dirty = true;
            }
            let needs = self.panels[api].dirty
                || self.altab.is_some()
                || self.panels[api].width > 1;
            if needs {
                self.draw(api, qh);
            }
        }
        // Cap de present ADAPTATIVO, en la CIMA de draw: en reposo saltea TODO el
        // trabajo por-frame (polls + updates de animación + present), no sólo el
        // present GPU. `cambio_real` = panel sucio al ENTRAR (un evento real lo marcó,
        // antes de las ramas de animación de abajo) → evita latencia de input.
        // `ambiente` = sin cambio real, drawer de shuma cerrado y ninguna animación
        // discreta en curso (switcher/menú/completado/tecla/cava con audio): sólo
        // animan efectos AMBIENTALES perpetuos — respiración del diente, lluvia del
        // clima y casicava del CPU (estos dos animan por tiempo en el paint, montados
        // sobre los presents; el ritmo lo fija este cap). A ~11fps no se distinguen y
        // dejan de despertar a mirada 30 veces/s en reposo. Con actividad, ~30fps.
        // Simétrico al throttle del fondo vivo de mirada.
        let cambio_real = self.panels[pi].dirty;
        let ambiente = !cambio_real && !self.shuma.open && !self.anim_discreta_activa(pi);
        if self
            .last_present
            .get(&pi)
            .is_some_and(|t| t.elapsed().as_millis() < present_cap_ms(ambiente))
        {
            self.latido(pi, qh);
            return;
        }
        // Panel INERTE (drawer cerrado, o surface de servicio a 1×1 sin cartel):
        // no hay nada que pintar y nada cambió → ni paint ni present. Conserva el
        // latido, así que en cuanto algo lo marque `dirty` —desplegar el drawer,
        // un OSD, el árbol de Alt-Tab, un tooltip— su propio draw lo pinta en el
        // callback siguiente; nada queda congelado. Ver [`inerte`] para el número
        // que lo motivó (50 de 90 presents/s eran esto).
        diag!(
            "pata diag · draw pi={pi} {}x{} dirty={cambio_real} inerte={} drawer={} mostrado={}",
            self.panels[pi].width,
            self.panels[pi].height,
            self.panel_inerte(pi),
            self.panels[pi].drawer,
            self.drawer_mostrado_en(pi)
        );
        if !cambio_real && self.panel_inerte(pi) {
            self.last_present.insert(pi, std::time::Instant::now());
            self.latido(pi, qh);
            return;
        }
        // DIAG: quién ensucia paneles. Cada poll de este tramo corre en CADA draw
        // (y hay ~13 paneles latiendo), así que uno que marque `dirty` de más
        // mantiene a todos los demás repintando para siempre. Contamos cuántos
        // paneles sucios agrega cada uno; sólo se emite si alguno agregó.
        let sucios = |s: &Self| s.panels.iter().filter(|p| p.dirty).count();
        let d0 = if crate::layer::diag_on() { sucios(self) } else { 0 };
        self.maybe_sample();
        let d1 = if crate::layer::diag_on() { sucios(self) } else { 0 };
        self.ensure_idle_arm(qh);
        let d2 = if crate::layer::diag_on() { sucios(self) } else { 0 };
        self.ensure_cafe_inhibitor(qh);
        let d3 = if crate::layer::diag_on() { sucios(self) } else { 0 };
        self.maybe_cava();
        let d4 = if crate::layer::diag_on() { sucios(self) } else { 0 };
        self.poll_nav();
        let d5 = if crate::layer::diag_on() { sucios(self) } else { 0 };
        self.poll_host();
        let d6 = if crate::layer::diag_on() { sucios(self) } else { 0 };
        self.poll_polkit();
        let d7 = if crate::layer::diag_on() { sucios(self) } else { 0 };
        self.finalize_flota_close();
        if crate::layer::diag_on() {
            let d8 = sucios(self);
            if d8 > d0 {
                diag!(
                    "pata diag · ensucia pi={pi} sample=+{} idle=+{} cafe=+{} cava=+{} nav=+{} host=+{} polkit=+{} flota=+{}",
                    d1.saturating_sub(d0), d2.saturating_sub(d1), d3.saturating_sub(d2),
                    d4.saturating_sub(d3), d5.saturating_sub(d4), d6.saturating_sub(d5),
                    d7.saturating_sub(d6), d8.saturating_sub(d7)
                );
            }
        }
        // Animación del switcher: si cambió el escritorio, el resaltado viaja.
        // Mientras dura, mantén el panel pintándose para animar suave.
        self.update_ws_anim();
        let ws_anim = self.ws_comet();
        if ws_anim.is_some() {
            self.panels[pi].dirty = true;
        }
        // Diente vivo: re-resuelve la manifestación cada frame (para que los
        // transitorios caduquen suave) y mantén latiendo el panel del sidebar con
        // un diente vivo —incluso en reposo, para la respiración ambiental del halo—.
        {
            let s = self.senales_diente();
            let now = self.diente_t0.elapsed().as_secs_f64();
            self.diente_manifest = self.atencion.resolver(s, now);
        }
        {
            let idx = self.panels[pi].idx;
            let es_sidebar_animado = self
                .cfg
                .surfaces
                .get(idx)
                .map(|s| {
                    s.kind == SurfaceKind::Sidebar
                        && s.tabs.iter().any(|t| {
                            crate::es_diente_vivo(&t.content.kind)
                                || crate::es_monitor(&t.content.kind)
                                || crate::es_unidades(&t.content.kind)
                        })
                })
                .unwrap_or(false);
            // El rail siempre respira; el drawer sólo si está MOSTRADO (el cerrado se
            // pintó transparente una vez y queda quieto — nada de spin de GPU en vacío).
            let drawer_cerrado = self.panels[pi].drawer && !self.drawer_mostrado_en(pi);
            // El drawer MOSTRADO late aunque el sidebar no tenga diente vivo: su
            // control mutable respira en idle (comunicador de estado topológico).
            let drawer_mostrado = self.drawer_mostrado_en(pi);
            let control_idle_late = drawer_mostrado && self.nav.search.is_empty();
            if (es_sidebar_animado && !drawer_cerrado) || control_idle_late {
                self.panels[pi].dirty = true;
            }
        }
        // El menú de inicio se desenrolla igual que el drawer de shuma: el reloj de
        // la animación (`menu_reveal_at`) arranca SÓLO cuando la surface ya creció a
        // MENU_H (el `configure` que sigue a `set_size(0, MENU_H)` llega a pintar por
        // aquí). Así el fade+slide nacen en 0 con el buffer grande presente, sin el
        // tirón/sliver de animar mientras la surface medía la barra fina.
        if self.menu_open && self.menu_panel == Some(pi) {
            if self.menu_reveal_at.is_none()
                && self.panels[pi].height > self.menu_bar_px + 10
            {
                self.menu_reveal_at = Some(std::time::Instant::now());
            }
            // Pendiente de desenrollar (surface creciendo) o aún animando: repinta
            // para no perder el instante del `configure` ni cortar el fade+slide.
            if self.menu_reveal_at.is_none() || self.menu_open_t() < 1.0 {
                self.panels[pi].dirty = true;
            }
        }
        // Ídem para el completado flotante mientras hace su fade de aparición.
        if self.completion_open && self.shuma_panel == Some(pi) && self.completion_open_t() < 1.0 {
            self.panels[pi].dirty = true;
        }
        self.ensure_gpu(pi);

        // AUTO-SANADO: una barra de shuma NO-activa con la surface todavía
        // CRECIDA es un huérfano — pasa cuando `focus_shuma_panel` salta de
        // monitor con el estado desincronizado (su gate `shuma.open` no ve la
        // surface grande) y deja atrás un fantasma fullscreen transparente que
        // "no se actualiza" (pinta la rama de barra, no el drawer). La barra
        // huérfana se encoge sola aquí, en su propio draw.
        if self.shuma_panel != Some(pi)
            && self.shuma_panels.contains(&pi)
            && self.panels[pi].height > self.shuma_bar_px + 10
        {
            diag!(
                "pata diag · panel {pi} huérfano crecido ({}px) sin ser el shuma activo → encojo",
                self.panels[pi].height
            );
            let bar = self.shuma_bar_px;
            let layer = &self.panels[pi].layer;
            layer.set_size(0, bar);
            layer.set_margin(0, 0, 0, 0);
            layer.set_keyboard_interactivity(
                smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity::OnDemand,
            );
            layer.commit();
            self.panels[pi].cache = None;
            self.panels[pi].dirty = true;
        }

        // Shell hospedado: avanza solo.
        if self.shuma_panel == Some(pi) {
            // La surface donde pata monta el overlay de shuma es la caja contra
            // la que sus menús contextuales se posicionan y se voltean. Sin
            // decírselo usaba el tamaño de ventana por defecto de la shuma
            // suelta, que no tiene nada que ver con esta pantalla.
            let caja = (self.panels[pi].width as f32, self.panels[pi].height as f32);
            if self.shuma_overlay_box != Some(caja) {
                self.shuma_overlay_box = Some(caja);
                if let Some(full) = self.shuma_full.as_mut() {
                    crate::shuma_app::set_overlay_box(full, caja.0, caja.1);
                }
            }
            // Arranca el desenrollado SÓLO cuando la surface ya creció a pantalla
            // completa (el `configure` que sigue a `set_size(0, 10_000)` llega a
            // pintar por aquí). Así el clip nace en reveal=0 con el buffer grande ya
            // presente — sin el tirón/sliver de animar mientras medía la barra fina.
            if self.shuma.open
                && self.shuma_reveal_at.is_none()
                && self.panels[pi].height > self.shuma_bar_px + 10
            {
                self.shuma_reveal_at = Some(std::time::Instant::now());
                self.panels[pi].dirty = true;
            }
            // Watchdog anti-atasco: si el drawer sigue abierto (y no está ya
            // cerrándose) y hace más de `SHUMA_WATCHDOG` que a pata no le llega
            // ningún input real, ciérralo solo. Recupera la sesión de un wedge —
            // drawer fullscreen + teclado Exclusive que no se pudo cerrar por las
            // vías normales (grab colgado en el compositor, respawn arrancando
            // abierto, etc.) — sin intervención del usuario. Cualquier tecla/click
            // re-estampa `shuma_input_reloj`, así que un uso activo nunca lo dispara.
            // EXCEPTO con un PTY interactivo vivo (claude/vim): mirar el output
            // largo sin tipear es uso normal, no un atasco — no cerrar.
            if self.shuma.open
                && self.shuma_closing_at.is_none()
                && !self.shuma_pty_vivo()
                && self
                    .shuma_input_reloj
                    .is_some_and(|t| t.elapsed() >= crate::layer::SHUMA_WATCHDOG)
            {
                diag!("pata diag · watchdog: drawer huérfano sin input → cierro");
                self.set_shuma_open(None);
            }
            // Release de grab DESACOPLADO del cierre: cuando el watchdog de cierre
            // está inhibido por un PTY vivo (claude/vim), un `Exclusive` colgado
            // dejaría a la sesión sin teclado indefinidamente. Tras un idle mucho
            // más largo (`SHUMA_GRAB_RELEASE`) soltamos el grab (OnDemand) SIN cerrar
            // el drawer —claude sigue a la vista— para que otras ventanas puedan
            // recibir teclado; el próximo input re-reclama el Exclusive
            // (`toca_shuma_watchdog`). Sólo con PTY vivo: sin él, el watchdog de
            // arriba ya cierra y suelta.
            if self.shuma.open
                && self.shuma_closing_at.is_none()
                && self.shuma_pty_vivo()
                && !self.shuma_grab_released
                && self
                    .shuma_input_reloj
                    .is_some_and(|t| t.elapsed() >= crate::layer::SHUMA_GRAB_RELEASE)
            {
                diag!("pata diag · release-grab: drawer idle con PTY vivo → suelto Exclusive (OnDemand), sin cerrar");
                self.shuma_grab_released = true;
                let layer = &self.panels[pi].layer;
                layer.set_keyboard_interactivity(
                    smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity::OnDemand,
                );
                layer.commit();
                self.panels[pi].dirty = true;
            }
            // Si el enrollado de cierre venció, encoge la surface de verdad; mientras
            // el drawer entra/sale (clip + fade), mantén el panel pintándose.
            self.finalize_shuma_close();
            if self.shuma_animando() {
                self.panels[pi].dirty = true;
            }
            if self.shuma_full.is_some() {
                // Live-wire: drenar los Msg que la shuma completa empujó al canal
                // (ticks/async/follow-ups) y aplicarlos. Repinta si está abierto.
                self.drain_shuma_full(pi);
            } else if self.shuma.open {
                self.shuma.inner = shuma_module_shell::update(
                    self.shuma.inner.clone(),
                    shuma_module_shell::Msg::Tick,
                );
                self.panels[pi].dirty = true;
            }
            // Bare: mientras el micrófono escucha, anima el halo del botón —
            // bumpea el reloj de voz y repinta aunque el drawer esté plegado
            // (en full mode el input pintado es el de la sesión activa, que la
            // shuma anima por su cuenta; aquí `escucha()` queda Apagado → no-op).
            if self.shuma_full.is_none() && self.shuma.inner.escucha().activo() {
                self.shuma.inner.set_voz_reloj((willay_emit::ahora_usec() / 1000) as u64);
                self.panels[pi].dirty = true;
            }
            // Fundido de los controles fantasma: avanza la opacidad hacia su
            // objetivo y, mientras anima, mantén el panel sucio para que el
            // draw-loop pida el próximo frame (aparición/esfumado suaves).
            if crate::avanzar_fantasmas(
                &mut self.fantasmas_alpha,
                self.fantasmas_hover,
                self.fantasmas_hasta,
                &mut self.fantasmas_reloj,
                willay_emit::ahora_usec(),
            ) {
                self.panels[pi].dirty = true;
            }
            // Turno rotativo de los fantasmas leves (uno a la vez); congelado
            // mientras el reveal está activo o hay un icono pinneado.
            if crate::shuma::avanzar_fugaz_idx(
                &mut self.fugaz_idx,
                &mut self.fugaz_reloj,
                willay_emit::ahora_usec(),
                self.fantasmas_hover || self.fantasmas_alpha > 0.01 || self.fugaz_pin.is_some(),
            ) {
                self.panels[pi].dirty = true;
            }
            // Zona fantasma apagada del todo → libera el orden congelado: el
            // asiento aprendido por los clicks recién rige ahora, con los
            // iconos ya invisibles (nadie los ve saltar).
            if !self.fantasmas_hover
                && self.fantasmas_alpha <= 0.01
                && self.fugaz_pin.is_none()
            {
                self.fugaz_fijo = None;
            }
        }

        // El panel del OSD crece al dispararse (volumen/brillo) y se encoge al
        // expirar; mantiene su latido para reaparecer sin recrear la surface.
        if self.osd_pi == Some(pi) {
            let visible = self.osd.map(|o| !o.expired()).unwrap_or(false);
            let target = if visible {
                (render::OSD_W, render::OSD_H)
            } else {
                (1u32, 1u32)
            };
            if (self.panels[pi].width, self.panels[pi].height) != target {
                {
                    let layer = &self.panels[pi].layer;
                    layer.set_size(target.0, target.1);
                    layer.commit();
                }
                self.panels[pi].width = target.0;
                self.panels[pi].height = target.1;
                self.panels[pi].cache = None;
                self.panels[pi].dirty = true;
            }
            if !visible {
                // Expiró (o nunca se mostró): suelta el cartel. NO retornamos sin
                // pintar —eso dejaría el último buffer 240×60 pegado a la surface
                // (bug)—: caemos al render con la vista vacía de abajo para
                // presentar un frame 1×1 limpio (como `hide_tooltip`). Si ya estaba
                // en 1×1 y no quedó sucio, el chequeo de `dirty` corta sin re-pintar.
                self.osd = None;
            }
        }

        // El panel del árbol Alt-Tab crece al desplegarse (mirada escribió el
        // archivo con tree=1) y se encoge a 1×1 al cerrarse. Tamaño FIJO en ancho
        // (esquiva el resize WSI del Iris Xe); alto por nº de ventanas, con tope.
        if self.altab_pi == Some(pi) {
            const ALTAB_MAX_H: u32 = 900;
            let target = match self.altab.as_ref().filter(|v| v.tree) {
                Some(v) => (render::ALTAB_W, render::altab_height(v).min(ALTAB_MAX_H).max(1)),
                None => (1u32, 1u32),
            };
            if (self.panels[pi].width, self.panels[pi].height) != target {
                {
                    let layer = &self.panels[pi].layer;
                    layer.set_size(target.0, target.1);
                    layer.commit();
                }
                self.panels[pi].width = target.0;
                self.panels[pi].height = target.1;
                self.panels[pi].cache = None;
                self.panels[pi].dirty = true;
            }
        }

        // AUTO-REPEAT: con una tecla sostenida, bombea la repetición (re-rutea
        // el press guardado cuando vence su `next_at`) y mantén este panel
        // dirty — así los frame-callbacks siguen a ritmo de frame y el pump
        // corre cada ~16-33 ms, no al piso de 500 ms del compositor en reposo.
        if self.key_held.is_some() {
            self.pump_key_repeat();
            self.panels[pi].dirty = true;
        }

        if !self.panels[pi].dirty {
            self.latido(pi, qh);
            return;
        }
        // El cap de present adaptativo ya corrió en la CIMA de draw (saltea todo el
        // trabajo por-frame en reposo). Aquí sólo seguimos si el panel quedó sucio.

        let idx = self.panels[pi].idx;
        let (w, h) = (self.panels[pi].width, self.panels[pi].height);
        let windows = self.window_entries();
        // Escritorios DESDE el monitor de este panel (multi-monitor): cada barra
        // pinta el escritorio de su pantalla, no el del monitor enfocado. El ctx
        // por-panel lo comparten el re-tick de los widgets de la barra y el rail
        // del sidebar (`DienteVivo`) — toda superficie ve el status de SU monitor.
        let (panel_ws_active, panel_ws_others) = self.panel_workspace_view(pi);
        let mut pctx = self.ctx.clone();
        pctx.active_workspace = panel_ws_active;
        pctx.workspace_others = panel_ws_others;
        // El SurfaceState de una barra "*" se comparte entre monitores; como el
        // `draw` es sincrónico, re-tickeamos los widgets de ESTE panel con su
        // vista de escritorio justo antes de pintarlos (el switcher-widget lee la
        // view cacheada del último tick). Sólo cuando hay dato por-output.
        if !self.ctx.output_workspaces.is_empty() {
            for wdg in self.surfaces[idx].core_mut() {
                wdg.tick(&pctx);
            }
        }
        let tray_items = self.tray.as_ref().map(|t| t.items()).unwrap_or_default();
        let notif = self.notifications.as_ref().map(|n| n.snapshot());
        // Chakana (PS1 topológico): color+titila por resultado de comandos + notifs.
        // En live-wire el resultado sale de la sesión activa del modelo full.
        let ultimo = self
            .shuma_full
            .as_ref()
            .map_or_else(|| self.shuma.inner.ultimo_resultado(), crate::shuma_app::active_ultimo_resultado);
        // MODO CONSOLA (PTY inline vivo a la vista): el PS1 se transforma y su
        // pulso narra el ESTADO del agente — pensando = violeta pulsante,
        // idle = violeta estable. Manda sobre resultado/notifs.
        let consola = self.shuma.open && self.shuma_pty_vivo() && !self.shuma_tui_fullscreen();
        let (chakana_c, chakana_t) = if consola {
            (
                llimphi_ui::llimphi_raster::peniko::Color::from_rgb8(0xB0, 0x7A, 0xE8),
                self.shuma_claude_ocupado(),
            )
        } else {
            render::chakana_vista(
                self.chakana_cfg.reactiva,
                self.chakana_cfg.titila_idle,
                ultimo,
                self.triage_now.as_ref().map(|r| r.urgencia),
                &self.theme,
            )
        };
        let data = render::BarData {
            windows: &windows,
            clipboard: self.clipboard.as_deref(),
            tray: &tray_items,
            weather: self.weather_now.as_ref(),
            network: self.network_now.as_ref(),
            media: self.media_now.as_ref(),
            bluetooth: self.bluetooth_now.as_ref(),
            notifications: notif.as_ref(),
            progreso: self.progreso.as_ref().and_then(|h| {
                let s = h.snapshot();
                s.hay().then(|| s.fraccion_determinada().unwrap_or(-1.0))
            }),
            cava: &self.cava_frame,
            apps: self.registry.all(),
            shuma_full: self.shuma_full.as_ref(),
            workspace: (
                panel_ws_active,
                self.ctx.workspace_count,
                self.ctx.workspace_occupied,
            ),
            // Pertenencia de escritorios vista desde el monitor de esta barra
            // (hogar por escritorio + conectados + foco del sistema).
            ws_monitores: self.panel_ws_monitores(pi),
            clock: (self.ctx.clock.hour, self.ctx.clock.minute),
            // En la barra real los botones de ventana se reordenan arrastrándolos.
            reorderable_tasks: true,
            ws_anim,
            anim_t: self.diente_t0.elapsed().as_secs_f32(),
            sys_alert: render::sys_alert(self.ctx.cpu, self.bat_now, self.cpu_temp),
            matilda: self.matilda_salud.as_ref(),
            cpu: self.ctx.cpu,
            cpu_cores: &self.ctx.cpu_cores[..(self.ctx.cpu_cores_n as usize).min(self.ctx.cpu_cores.len())],
            cpu_temp: self.cpu_temp,
            bat: self.bat_now,
            bat_evento: willay_emit::ahora_usec() < self.bat_evento_hasta,
            fugaz_uso: Some(&self.fugaz_uso),
            fugaz_fijo: self.fugaz_fijo.as_ref(),
            menu_scroll: self.menu_scroll,
            volume: self.ctx.volume,
            muted: self.ctx.muted,
            moon_phase: self.ctx.moon_phase,
            sun_longitude: self.ctx.sun_longitude_deg,
            cielo: self.cielo_now.as_ref(),
            khipu: Some(&self.khipu_snapshot),
            tampu: self.tampu_now.as_ref(),
            usb: self.usb_now.as_ref(),
            brightness: self.ctx.brightness,
            vol_evento: (willay_emit::ahora_usec() < self.vol_evento_hasta)
                .then_some(self.vol_subiendo),
            net_trafico: self.red_trafico,
            revelar_alpha: self.fantasmas_alpha,
            fugaz_idx: self.fugaz_idx,
            fugaz_pin: self.fugaz_pin,
            grabando: self.grabacion.as_ref().map(|g| g.segundos()),
            chakana_color: Some(chakana_c),
            chakana_titila: chakana_t,
            chakana_forma: if consola {
                crate::render::ChakanaForma::Consola
            } else {
                crate::render::ChakanaForma::Chakana
            },
        };

        let view = if self.altab_pi == Some(pi) {
            // Árbol Alt-Tab desplegado: lo pintamos; si no (cerrado o modo plano),
            // vista vacía que limpia el frame 1×1.
            match self.altab.as_ref().filter(|v| v.tree) {
                Some(v) => render::altab_surface_view(v, &self.theme),
                None => llimphi_ui::View::new(Default::default()),
            }
        } else if self.osd_pi == Some(pi) {
            // Con cartel vigente, lo pintamos; al expirar, una vista vacía limpia
            // el frame 1×1 (NO un `bar_view`, que metería la barra en la surface
            // del OSD).
            match self.osd {
                Some(osd) => render::osd_surface_view(&osd, &self.theme),
                None => llimphi_ui::View::new(Default::default()),
            }
        } else if self.tooltip_pi == Some(pi) {
            render::tooltip_view(self.tooltip_text.as_deref().unwrap_or(""), &self.theme)
        } else if let Some(c) = self.panels[pi].card.as_ref() {
            render::card_view(&c.spec, &c.widgets, &self.theme)
        } else if self.menu_panel == Some(pi) && self.menu_open {
            match self.menu_kind {
                MenuKind::Apps => render::start_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    self.registry.all(),
                    &self.menu_query,
                    self.menu_scroll,
                    h as f32,
                    // El estilo del menú lo fija la vista vía la config de pata.
                    crate::MenuStyle::from_cfg(&self.cfg.general.menu_style),
                    self.cfg.general.menu_columns,
                    self.menu_cat,
                    self.menu_sel,
                    self.menu_open_t(),
                ),
                MenuKind::Clipboard => render::clipboard_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    &render::clip_rows(&self.clip_store, &self.clip_history),
                    // Ancla bajo el widget que lo abrió (último x del puntero en
                    // esa barra), acotado al ancho de la barra.
                    self.menu_anchor_x.unwrap_or(self.panels[idx].width as f32 * 0.5),
                    self.panels[idx].width as f32,
                    self.menu_open_t(),
                ),
                MenuKind::Clock => render::clock_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    &self.clock_draft,
                    self.menu_open_t(),
                ),
                MenuKind::Control => render::control_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    self.ctx.volume,
                    self.ctx.muted,
                    self.ctx.brightness,
                    &self.control_extras,
                    self.menu_anchor_x.unwrap_or(self.panels[idx].width as f32 * 0.5),
                    self.panels[idx].width as f32,
                    self.menu_open_t(),
                ),
                MenuKind::Network => render::network_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    self.network_now.as_ref(),
                    self.net_password.as_ref().map(|(s, p)| (s.as_str(), p.as_str())),
                    self.menu_anchor_x.unwrap_or(self.panels[idx].width as f32 * 0.5),
                    self.panels[idx].width as f32,
                    self.menu_open_t(),
                ),
                MenuKind::Volume => render::volume_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    &self.ctx,
                    &self.sinks,
                    &self.sink_inputs,
                    &self.sources,
                    &self.source_outputs,
                    self.volume_tab,
                    self.menu_anchor_x.unwrap_or(self.panels[idx].width as f32 * 0.5),
                    self.panels[idx].width as f32,
                    self.menu_open_t(),
                ),
                MenuKind::Session => render::session_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    self.session_confirm,
                    self.menu_anchor_x.unwrap_or(self.panels[idx].width as f32 * 0.5),
                    self.panels[idx].width as f32,
                    self.menu_open_t(),
                ),
                MenuKind::Bluetooth => render::bluetooth_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    self.bluetooth_now.as_ref(),
                    self.menu_anchor_x.unwrap_or(self.panels[idx].width as f32 * 0.5),
                    self.panels[idx].width as f32,
                    self.menu_open_t(),
                ),
                MenuKind::Cielo => render::cielo_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    &self.cfg.general.ubicacion.localidades,
                    self.cfg.general.ubicacion.activa,
                    self.menu_anchor_x.unwrap_or(self.panels[idx].width as f32 * 0.5),
                    self.panels[idx].width as f32,
                    self.menu_open_t(),
                ),
                MenuKind::Weather => render::weather_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    self.menu_anchor_x.unwrap_or(self.panels[idx].width as f32 * 0.5),
                    self.panels[idx].width as f32,
                    self.menu_open_t(),
                ),
                MenuKind::Tampu => render::tampu_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    self.menu_anchor_x.unwrap_or(self.panels[idx].width as f32 * 0.5),
                    self.panels[idx].width as f32,
                    self.menu_open_t(),
                ),
                MenuKind::Captura => render::captura_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    self.menu_anchor_x.unwrap_or(self.panels[idx].width as f32 * 0.5),
                    self.panels[idx].width as f32,
                    self.menu_open_t(),
                ),
                MenuKind::Usb => render::usb_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    self.menu_anchor_x.unwrap_or(self.panels[idx].width as f32 * 0.5),
                    self.panels[idx].width as f32,
                    self.menu_open_t(),
                ),
                MenuKind::Agora => render::agora_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    self.agora_now.as_ref(),
                    self.menu_anchor_x.unwrap_or(self.panels[idx].width as f32 * 0.5),
                    self.panels[idx].width as f32,
                    self.menu_open_t(),
                ),
                MenuKind::Khipu => render::khipu_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    self.khipu_input.as_deref(),
                    self.menu_anchor_x.unwrap_or(self.panels[idx].width as f32 * 0.5),
                    self.panels[idx].width as f32,
                    self.menu_open_t(),
                ),
                MenuKind::Notifications => render::notifications_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    notif.as_ref(),
                    self.menu_anchor_x.unwrap_or(self.panels[idx].width as f32 * 0.5),
                    self.panels[idx].width as f32,
                    self.menu_open_t(),
                ),
                MenuKind::Polkit => render::polkit_menu_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    self.menu_bar_px as f32,
                    self.polkit_prompt.as_ref().map(|r| r.message.as_str()).unwrap_or(""),
                    &self.polkit_input,
                    self.panels[idx].width as f32,
                ),
                // Pantalla de confirmación fullscreen: scrim traslúcido sobre toda la
                // surface (crecida al alto del monitor) + tarjeta centrada. Si no hay
                // acción pendiente (carrera de cierre), una vista vacía.
                MenuKind::Confirm => match self.confirm_overlay.as_ref() {
                    Some(accion) => render::confirm_overlay_view(accion, w as f32, h as f32, &self.theme),
                    None => llimphi_ui::View::new(Default::default()),
                },
            }
        } else if self.completion_open && self.shuma_panel == Some(pi) {
            // Completado flotante autónomo sobre la barra fina (drawer plegado):
            // la surface creció a COMPLETION_H y pintamos la barra + el panel de
            // candidatos anclado al input, con sombra/animación. El teclado sigue
            // en la barra (no lo tocamos), así Tab/↑↓/Enter navegan y aceptan.
            render::completion_menu_view(
                &self.cfg.surfaces[idx],
                &self.surfaces[idx],
                &self.shuma,
                &data,
                &self.theme,
                // La barra se lleva el alto extra del input crecido: sin esto
                // la surface crece pero la barra sigue midiendo `shuma_bar_px`
                // y recorta las filas de abajo.
                self.shuma_bar_px as f32 + self.shuma_input_alto_extra(),
                self.panels[idx].cursor_x,
                self.panels[idx].width as f32,
                self.completion_open_t(),
            )
        } else if self.shuma_panel == Some(pi) && self.shuma.open {
            render::shuma_open_view(
                &self.cfg.surfaces[idx],
                &self.surfaces[idx],
                &self.shuma,
                &data,
                &self.theme,
                // El alto base va aparte del crecimiento del input: el primero
                // reserva espacio en el flujo, el segundo se pinta ENCIMA del
                // cuerpo para no correr la conversación (ver `shuma_open_view`).
                self.shuma_bar_px as f32,
                self.shuma_input_alto_extra(),
                // Alto del drawer = fracción configurable de la pantalla
                // (general.shuma_height). `h` es el alto de la superficie (=
                // pantalla, ya que al abrir crece a 10_000 y el compositor la
                // capa). Cae a DRAWER_H si la superficie aún no se configuró.
                {
                    // Maximizado (botón ▢ de la barra de título) → casi pantalla
                    // completa; si no, la fracción configurable. Un TUI en
                    // pantalla completa (claude/vim/htop) pide la terminal
                    // entera: piso 0.95 aunque el drawer estuviera a media
                    // altura — sin esto quedaba "cortado a mitad de pantalla".
                    let mut frac = self.shuma.height_frac.unwrap_or(if self.shuma.maximized {
                        0.95
                    } else {
                        self.cfg.general.shuma_height.clamp(0.1, 0.95)
                    });
                    if self.shuma_tui_fullscreen() {
                        frac = frac.max(0.95);
                    }
                    if h > self.shuma_bar_px + 10 {
                        h as f32 * frac
                    } else {
                        DRAWER_H as f32
                    }
                },
                self.shuma_reveal(),
                {
                    // Insets laterales para el CUERPO del drawer (esquiva sidebars
                    // docked); la franja de la barra NO se insetea (ya no vía margen
                    // de surface). `shuma_drawer_insets` → (top, right, bottom, left).
                    let (_t, _r, _b, l) = self.shuma_drawer_insets(pi);
                    l as f32
                },
                {
                    let (_t, r, _b, _l) = self.shuma_drawer_insets(pi);
                    r as f32
                },
            )
        } else if self.cfg.surfaces[idx].kind == SurfaceKind::Sidebar {
            let hosted = {
                let app = self.focused_app_id().map(|s| s.to_string());
                match (app, self.host.as_ref()) {
                    (Some(id), Some(h)) => {
                        h.snapshot(&id).map(|(_, teeth, active)| (id, teeth, active))
                    }
                    _ => None,
                }
            };
            let (hosted_app, hosted_teeth, hosted_active): (&str, &[pata_host::HostedTooth], Option<u32>) =
                match &hosted {
                    Some((id, teeth, active)) => (id.as_str(), teeth.as_slice(), *active),
                    None => ("", &[], None),
                };
            // Sesiones de terminal (shuma completa) → tarjetas para el rail del
            // sidebar de terminal (`</>` como workspace especial).
            let terminal_sessions = self
                .shuma_full
                .as_ref()
                .map(crate::shuma_app::sessions_overview)
                .unwrap_or_default();
            let vivo = render::DienteVivo {
                manifest: self.diente_manifest,
                cava_frame: &self.cava_frame,
                // El ctx por-panel: el rail de escritorios de este sidebar pinta
                // el activo/otros del monitor donde vive, no el del enfocado.
                ctx: &pctx,
                unidades: self.unidades_now.as_ref(),
                flota_remoto: self.flota_remoto.as_deref(),
                // Las ventanas con escritorio (mirada-ctl) — pestañas del rail.
                windows: &self.windows_ws,
                terminal_sessions: &terminal_sessions,
                t: self.diente_t0.elapsed().as_secs_f64(),
            };
            let extras = render::extras_vivos(
                self.bat_now,
                self.network_now
                    .as_ref()
                    .map(|n| n.wifi_enabled)
                    .unwrap_or(self.control_extras.wifi),
                self.bluetooth_now
                    .as_ref()
                    .map(|b| b.powered)
                    .unwrap_or(self.control_extras.bt),
                &self.control_extras,
            );
            let centro = render::CentroDatos {
                ctx: &self.ctx,
                extras: &extras,
                media: self.media_now.as_ref(),
                net: self.network_now.as_ref(),
                net_password: self
                    .net_password
                    .as_ref()
                    .map(|(s, p)| (s.as_str(), p.as_str())),
                bt: self.bluetooth_now.as_ref(),
                flota: self.flota.as_ref(),
                flota_remoto: self.flota_remoto.as_deref(),
                movil: self.movil_obs.as_deref(),
                matilda: self.matilda_salud.as_ref(),
                unidades: self.unidades_now.as_ref(),
                // Ventanas por escritorio: el taskbar de un diente-workspace las filtra.
                windows: &self.windows_ws,
                willay: self.willay_now.as_ref().map(|s| s.eventos.as_slice()).unwrap_or(&[]),
            };
            // Estado EFECTIVO de los dos ejes de esta surface, para que la barrita
            // muestre los switches en su posición actual: el override por-sidebar
            // (`reserve`/`rail_outside`) gana; si es `None`, el global.
            let s = &self.cfg.surfaces[idx];
            let docked_ef = s.reserve.unwrap_or(self.sidebar_docked);
            let rail_outside_ef = s.rail_outside.unwrap_or(self.dientes_outside);
            let autohide_ef = s.autohide;
            let dos_pasos_ef = self.cfg.general.diente_dos_pasos;
            // Reconciliar la input-region de un rail AUTOHIDE: franja fina cuando está
            // oculto (solo la zona caliente de reaparición), completa cuando revelado.
            // Se hace aquí (no por transición) para cubrir también el estado de boot.
            if !self.panels[pi].drawer {
                let oculto = self.sidebar_oculto(idx);
                let thin_now = self.rails_thin.contains(&pi);
                if oculto != thin_now {
                    self.set_rail_input_region(pi, !oculto);
                    if oculto {
                        self.rails_thin.insert(pi);
                    } else {
                        self.rails_thin.remove(&pi);
                    }
                }
            }
            if self.panels[pi].drawer && !self.drawer_mostrado_en(pi) {
                // Drawer CERRADO — o mostrado en OTRO monitor (cada pantalla expande
                // lo suyo): se pinta transparente (la surface existe siempre, pero
                // invisible + input-region vacía = el puntero la atraviesa).
                render::sidebar_drawer_hidden(w as f32, h as f32)
            } else if self.panels[pi].drawer {
                // El **drawer** ABIERTO: sólo la barrita + el contenido del diente, a
                // ancho fijo `panel_width`. El rail vive en su propia surface aparte.
                let out = self.panel_out_key(pi);
                let ti = self
                    .nav
                    .open_en(&out)
                    .filter(|(s, _)| *s == idx)
                    .map(|(_, ti)| ti)
                    .unwrap_or(0);
                render::sidebar_drawer_view(
                    &self.cfg.surfaces[idx],
                    idx,
                    &out,
                    ti,
                    w as f32,
                    h as f32,
                    &self.nav,
                    &self.shuma,
                    &self.rag,
                    &centro,
                    docked_ef,
                    rail_outside_ef,
                    autohide_ef,
                    dos_pasos_ef,
                    self.diente_t0.elapsed().as_secs_f64(),
                    &self.theme,
                )
            } else if self.sidebar_oculto(idx) {
                // Rail AUTOHIDE oculto: transparente (la surface sigue mapeada; solo la
                // franja caliente recibe input). El puntero al borde lo revela.
                render::sidebar_drawer_hidden(w as f32, h as f32)
            } else {
                // El **rail**: sólo la franja de dientes (ya no crece para alojar el
                // panel; de eso se encarga el drawer).
                render::sidebar_surface_view(
                    &self.cfg.surfaces[idx],
                    idx,
                    &self.panel_out_key(pi),
                    w as f32,
                    h as f32,
                    &self.nav,
                    hosted_teeth,
                    hosted_app,
                    hosted_active,
                    &self.shuma,
                    &vivo,
                    &self.theme,
                    self.cfg.general.rail_transparente,
                )
            }
        } else if self.cfg.surfaces[idx].kind == SurfaceKind::Dock {
            // Dock estilo macOS: apps fijadas (lanzadores) + ventanas abiertas,
            // magnificados por el puntero. Los pins se resuelven en el registro;
            // los que no existan se omiten.
            let pins: Vec<app_bus::AppEntry> = self.cfg.surfaces[idx]
                .dock_pins
                .iter()
                .filter_map(|id| self.registry.get(id).cloned())
                .collect();
            render::dock_view(
                &self.cfg.surfaces[idx],
                &pins,
                &windows,
                &self.theme,
                w as f32,
                self.panels[pi].cursor_x,
            )
        } else if self.cfg.surfaces[idx].kind == SurfaceKind::Background {
            // Fondo de escritorio (capa Background): llena la pantalla.
            render::background_view(
                &self.cfg.surfaces[idx],
                &self.surfaces[idx],
                &self.shuma,
                &data,
                &self.theme,
            )
        } else {
            // La surface puede estar TODAVÍA crecida: replegar el drawer de shuma
            // (o cerrar el menú/completado) pide `set_size(0, barra)` y sigue
            // pintando hasta que llega el `configure` con el alto nuevo. Como
            // `bar_view` es 100%×100%, esos frames estiraban la franja del input a
            // media pantalla — el parpadeo al guardar el drawer. Mientras el eje
            // fino sobre-mida, anclamos la franja a su borde con su alto real.
            let s = &self.cfg.surfaces[idx];
            let alto_barra = s.thickness.max(1.0)
                + if self.shuma_panels.contains(&pi) {
                    self.shuma_input_alto_extra()
                } else {
                    0.0
                };
            let fino = if s.anchor.es_horizontal() { h } else { w } as f32;
            if s.anchor.es_horizontal() && fino > alto_barra + 6.0 {
                render::bar_view_anclada(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                    alto_barra,
                )
            } else {
                render::bar_view(
                    &self.cfg.surfaces[idx],
                    &self.surfaces[idx],
                    &self.shuma,
                    &data,
                    &self.theme,
                )
            }
        };

        // Declará/retirá la región opaca de la surface según lo que vamos a pintar
        // (barra opaca y replegada = opaca → mirada saltea el frost tapado). Barato:
        // sólo committea al cambiar de estado. Antes del present (que no la latcha).
        self.actualizar_region_opaca(pi);

        let hover_idx = self.panels[pi].hover_idx;
        let hal = match self.hal.as_ref() {
            Some(h) => h,
            None => {
                self.latido(pi, qh);
                return;
            }
        };
        let gpu = match self.panels[pi].gpu.as_mut() {
            Some(g) => g,
            None => {
                self.latido(pi, qh);
                return;
            }
        };
        gpu.surface.resize(w, h);
        let frame = match gpu.surface.acquire() {
            Ok(f) => f,
            Err(llimphi_ui::llimphi_hal::SurfaceError::DeviceLost) => {
                // Device irrecuperable (reset de output Iris Xe, cambio de VT):
                // reconfigurar y recrear la surface contra el mismo device ya
                // fallaron adentro de `acquire`. Rehacer TODO el stack GPU en vez
                // de morir por `closed` y depender del respawn del wrapper.
                let _ = (gpu, hal);
                self.rebuild_gpu_after_device_loss(qh);
                return;
            }
            Err(_) => {
                let _ = gpu;
                self.latido(pi, qh);
                return;
            }
        };
        gpu.layout.clear();
        let mounted = mount(&mut gpu.layout, view);
        let computed = {
            let ts = &mut gpu.typesetter;
            let tmap = &mounted.text_measures;
            gpu.layout
                .compute_with_measure(mounted.root, (w as f32, h as f32), |nid, known, avail| {
                    match tmap.get(&nid) {
                        Some(tm) => measure_text_node(ts, tm, known, avail),
                        None => taffy::Size::ZERO,
                    }
                })
                .expect("layout")
        };
        gpu.scene.reset();
        paint(&mut gpu.scene, &mounted, &computed, &mut gpu.typesetter, hover_idx, None);
        // Color base del frame: SIEMPRE transparente. El cuerpo del drawer es
        // opaco por sus PROPIOS fills (canvas a1.00); la base opaca que se probó
        // aquí tapaba también el scrim → "se ocultan todas las ventanas". (Sirvió
        // como bisección del bug del painter del text-editor, que decapitaba la
        // escena: con la escena decapitada ni los fills llegaban.)
        if let Err(e) = gpu.renderer.render(hal, &gpu.scene, &frame, palette::css::TRANSPARENT) {
            eprintln!("pata layer · render: {e}");
        }
        gpu.surface.present(frame, hal);
        diag!("pata diag · present panel {pi} {w}x{h}");

        self.panels[pi].dirty = false;
        self.panels[pi].cache = Some(RenderCache { mounted, computed });
        // FROST POR SUB-REGIÓN: si el rail es transparente y está a la vista, declará
        // los DIENTES como input-region — doble función: (1) sólo los pills reciben
        // click (los gaps atraviesan al escritorio), (2) mirada usa esa región como
        // máscara de frost (glass sólo bajo los dientes; el resto del rail queda
        // transparente). Ver `over_layer_rects`/`input_frost_subrects` en mirada.
        let idx = self.panels[pi].idx;
        if !self.panels[pi].drawer
            && self.cfg.general.rail_transparente
            && self
                .cfg
                .surfaces
                .get(idx)
                .map(|s| s.kind == SurfaceKind::Sidebar)
                .unwrap_or(false)
            && !self.sidebar_oculto(idx)
        {
            self.set_rail_teeth_input_region(pi);
        }
        self.last_present.insert(pi, std::time::Instant::now()); // sella el cap de ~30fps
        self.latido(pi, qh);
    }

    /// Setea la input-region del RAIL `pi` a la unión de los rects de los DIENTES
    /// (nodos clickeables/arrastrables del layout ya computado). Sirve doble: los
    /// gaps entre dientes atraviesan el click al escritorio, y mirada la lee como
    /// máscara de frost (glass sólo bajo cada diente). Sólo en modo rail transparente
    /// y con el rail a la vista; si no hay dientes clickeables cae a `None` (todo).
    fn set_rail_teeth_input_region(&self, pi: usize) {
        use smithay_client_toolkit::compositor::Region;
        use smithay_client_toolkit::shell::WaylandSurface;
        let Some(cache) = self.panels[pi].cache.as_ref() else {
            return;
        };
        let Some(comp) = self.compositor.as_ref() else {
            return;
        };
        let Ok(region) = Region::new(comp) else {
            return;
        };
        let mut any = false;
        for n in &cache.mounted.nodes {
            let clickable = n.on_click.is_some()
                || n.on_click_at.is_some()
                || n.drag.is_some()
                || n.drag_at.is_some();
            if !clickable {
                continue;
            }
            if let Some(r) = cache.computed.get(n.id) {
                let (x, y, w, h) = (r.x as i32, r.y as i32, r.w.ceil() as i32, r.h.ceil() as i32);
                if w > 0 && h > 0 {
                    region.add(x, y, w, h);
                    any = true;
                }
            }
        }
        let layer = &self.panels[pi].layer;
        if any {
            layer.wl_surface().set_input_region(Some(region.wl_region()));
        } else {
            layer.wl_surface().set_input_region(None);
        }
        layer.commit();
    }

    /// Aplica el `Msg` que produjo un click.
    /// Arranca la **captura de voz** del micrófono (barra real). Abre el mic
    /// (cpal) vía `rimay-voz-host` con VAD por energía y el STT mock por default;
    /// cada `EventoEscucha` vuelve al loop por `rag_tx` como `Msg::VozEvento`. La
    /// guardia + el runtime viven en `self` (dropearlos apaga el mic). Para
    /// backends reales, cambiar el `VozConfig` (ver `crate::iniciar_voz`).
    fn iniciar_voz(&mut self) {
        use shuma_voz_ui::EstadoEscucha;
        let vcfg = rimay_voz::VozConfig::default();
        let opciones = rimay_voz_host::OpcionesEscucha::default();
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                self.shuma.inner.fijar_escucha(EstadoEscucha::Apagado);
                eprintln!("voz: no se pudo crear el runtime: {e}");
                return;
            }
        };
        let arranque = {
            let _g = rt.enter();
            rimay_voz_host::escuchar_cfg(vcfg, opciones)
        };
        match arranque {
            Ok((guardia, mut rx)) => {
                self.shuma.inner.fijar_escucha(EstadoEscucha::Esperando);
                let tx = self.rag_tx.clone();
                rt.spawn(async move {
                    while let Some(ev) = rx.recv().await {
                        if tx.send(Msg::VozEvento(ev)).is_err() {
                            break;
                        }
                    }
                });
                self.voz_guardia = Some(guardia);
                self.voz_rt = Some(rt);
                eprintln!("voz: 🎙 escuchando — di «shuma»");
            }
            Err(e) => {
                self.shuma.inner.fijar_escucha(EstadoEscucha::Apagado);
                eprintln!("voz: no se pudo abrir el micrófono: {e}");
            }
        }
    }

    /// Para la captura de voz: dropea la guardia (para el mic + tasks) y su
    /// runtime, y apaga el indicador de escucha del input.
    fn parar_voz(&mut self) {
        self.voz_guardia = None;
        self.voz_rt = None;
        self.shuma.inner.fijar_escucha(shuma_voz_ui::EstadoEscucha::Apagado);
    }

    pub(super) fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::ShumaToggle => {
                // Toggle a mano (borde del cabezal, chip, ─/✕ de la barra de
                // título): cierra cualquier modo; al abrir es un vistazo (Fugaz).
                if self.shuma.open {
                    self.set_shuma_open(None);
                } else {
                    self.set_shuma_open(Some(crate::shuma::OpenMode::Fugaz));
                }
            }
            Msg::TerminalSession(i) => {
                // Diente-sesión del rail: activa esa sesión en la shuma completa y
                // desplega el drawer directo en ella («abrir ese tab desde el tab»).
                self.apply_shuma_full(vec![crate::shuma_app::Msg::SelectSession(i)]);
                if !self.shuma.open {
                    self.set_shuma_open(Some(crate::shuma::OpenMode::Firme));
                }
                self.marcar_shuma_dirty();
            }
            Msg::ShumaAutoClose => {
                // Deshover (gesto liviano): repliega SÓLO el vistazo, y no el
                // evento espurio de recién abierto. El modo firme lo ignora.
                let churn = self
                    .shuma_opened_at
                    .is_some_and(|t| t.elapsed() < super::MENU_LEAVE_GRACE);
                if self.shuma.open && !churn && self.shuma.open_mode.cierra_por_gesto_liviano() {
                    self.set_shuma_open(None);
                }
            }
            Msg::ShumaMaximize => {
                self.shuma.maximized = !self.shuma.maximized;
                self.shuma.height_frac = None; // el toggle vuelve a maximized/config
                self.marcar_shuma_dirty();
            }
            Msg::ShumaResize(frac_delta) => {
                let cur = self.shuma.height_frac.unwrap_or(if self.shuma.maximized {
                    0.95
                } else {
                    self.cfg.general.shuma_height.clamp(0.1, 0.95)
                });
                self.shuma.height_frac = Some((cur + frac_delta).clamp(0.15, 0.98));
                self.shuma.maximized = false;
                self.marcar_shuma_dirty();
            }
            Msg::ShumaUndock => {
                // Desacople real ("mover de verdad"): la sesión embebida se va a
                // un shuma standalone con su scrollback (handoff), cwd e
                // historial, y el drawer queda en limpio — ya no duplica.
                crate::undock_shuma_session(&mut self.shuma.inner);
                self.set_shuma_open(None);
            }
            Msg::ShumaShell(m) => {
                // Enviar (o Enter, que lo arma `press_key`) despliega **firme**.
                // `FocusInput` por **click** (no hover) es el toggle del *vistazo*
                // (abrir fugaz / cerrar si estaba fugaz sin tipear); por **hover**
                // sólo enfoca, nunca abre (regla «no abrir con hover ni al tipear»).
                let submitting = matches!(m, shuma_module_shell::Msg::Submit);
                // El **press** sobre el texto del input (el que posa el caret)
                // cuenta, para la lógica del drawer, como el click clásico de
                // FocusInput. Nunca llega por hover. El arrastre de selección y
                // el click derecho, en cambio, NO abren el drawer: seleccionar
                // texto no es pedir que se despliegue.
                let click_input = (matches!(m, shuma_module_shell::Msg::FocusInput)
                    && !self.hover_dispatch)
                    || m.es_press_en_input();
                diag!(
                    "pata diag · handle_msg ShumaShell({m:?}) submit={submitting} click={click_input} open={}",
                    self.shuma.open
                );
                self.shuma.inner = shuma_module_shell::update(self.shuma.inner.clone(), m);
                // El botón de micrófono del input alterna `mic_intent`: arranca o
                // pará la captura de voz. Con el STT mock por default, cualquier
                // utterance real despierta y dicta —así se ven las animaciones de
                // escucha sin daemon ni nube. Los `EventoEscucha` vuelven por
                // `rag_tx` como `Msg::VozEvento`.
                match self.shuma.inner.tomar_mic_intent() {
                    Some(true) => self.iniciar_voz(),
                    Some(false) => self.parar_voz(),
                    None => {}
                }
                // #2 — aichat/semántica desde el input de la barra (`:?`, `:buscar`):
                // el módulo dejó la petición; la corremos en un thread y el resultado
                // vuelve por el canal general (`rag_tx`, un Sender<Msg> drenado cada
                // frame) como Msg::ShumaShell(...). Antes sólo corría en modo full.
                if let Some(req) = self.shuma.inner.take_llm_request() {
                    let kind = req.kind;
                    let tx = self.rag_tx.clone();
                    std::thread::spawn(move || {
                        let (ok, text) = match shuma_shell_llimphi::update::run_llm_blocking(&req) {
                            Ok(t) => (true, t),
                            Err(e) => (false, e),
                        };
                        let _ = tx.send(crate::Msg::ShumaShell(
                            shuma_module_shell::Msg::LlmResult { kind, ok, text },
                        ));
                    });
                }
                if let Some(req) = self.shuma.inner.take_semantic_request() {
                    let tx = self.rag_tx.clone();
                    std::thread::spawn(move || {
                        let (ok, hits) = match shuma_shell_llimphi::update::run_semantic_blocking(&req) {
                            Ok(h) => (true, h),
                            Err(e) => (false, vec![(e, 0.0)]),
                        };
                        let _ = tx.send(crate::Msg::ShumaShell(
                            shuma_module_shell::Msg::SemanticResult { ok, hits },
                        ));
                    });
                }
                // #3 — launcher: proveer las apps al módulo (una vez) y lanzar la
                // que el input haya pedido (spawn detached). Y reconcilia el
                // completado flotante por si el click/foco cambió el input.
                self.asegurar_apps();
                self.drenar_app_launch();
                self.reconcile_completion();
                if submitting {
                    // Firme (escala un vistazo previo a firme si ya estaba abierto).
                    self.set_shuma_open(Some(crate::shuma::OpenMode::Firme));
                } else if click_input {
                    let vacio = self.shuma.inner.input.text().trim().is_empty();
                    match crate::shuma::accion_click_input(
                        self.shuma.open,
                        self.shuma.open_mode,
                        vacio,
                    ) {
                        crate::shuma::ClickInputAccion::AbrirFugaz => {
                            self.set_shuma_open(Some(crate::shuma::OpenMode::Fugaz))
                        }
                        crate::shuma::ClickInputAccion::Cerrar => self.set_shuma_open(None),
                        crate::shuma::ClickInputAccion::SoloEnfocar => {}
                    }
                }
                self.marcar_shuma_dirty();
            }
            Msg::RevealFantasmas(si) => {
                if self.fantasmas_hover != si {
                    self.fantasmas_hover = si;
                    if si {
                        // Congela el snapshot de fugaces mientras el puntero
                        // ande cerca: nada se recoloca bajo el mouse.
                        self.estampar_fugaz_fijo();
                    }
                    if !si {
                        // Hover-out: no lo escondas ya — quédate revelado el retardo
                        // y recién después fundí (lo anima `draw`).
                        self.fantasmas_hasta = willay_emit::ahora_usec() + crate::FANT_LINGER_US;
                    }
                    self.marcar_shuma_dirty();
                }
            }
            Msg::FantasmaPin(id, entra) => {
                if entra {
                    self.fugaz_pin = Some(id);
                    self.estampar_fugaz_fijo();
                } else if self.fugaz_pin == Some(id) {
                    // Sólo despinnea el propio (los enter/leave entre iconos
                    // vecinos pueden llegar cruzados); el retardo del reveal
                    // evita que se esfume de golpe bajo el mouse.
                    self.fugaz_pin = None;
                    self.fantasmas_hasta = willay_emit::ahora_usec() + crate::FANT_LINGER_US;
                }
                self.marcar_shuma_dirty();
            }
            Msg::FugazClick(id) => {
                // Aprende el uso (asiento a la derecha, persistido) y despacha
                // la acción del icono (abrir su diálogo).
                self.fugaz_uso.bump(id);
                // El icono de sonido con un reproductor activo es un
                // «empausador»: el click izquierdo alterna play/pausa en vez de
                // abrir el panel (que sigue en el click derecho).
                if id == crate::shuma::Fugaz::Sonido
                    && self.media_now.as_ref().map(|m| m.has_player).unwrap_or(false)
                {
                    self.handle_msg(Msg::MediaPlayPause);
                } else if let Some(m) = crate::shuma::accion_fugaz(id) {
                    self.handle_msg(m);
                }
            }
            Msg::VozEvento(ev) => {
                use rimay_voz_host::EventoEscucha as E;
                use shuma_voz_ui::EstadoEscucha as Es;
                let ahora_ms = (willay_emit::ahora_usec() / 1000) as u64;
                let estado = match &ev {
                    E::Escuchando => Es::Oyendo,
                    E::Desperto => Es::Despierto,
                    E::Dictar(_) => Es::Dictando,
                    E::SeDurmio => Es::Esperando,
                };
                self.shuma.inner.fijar_escucha(estado);
                self.shuma.inner.set_voz_reloj(ahora_ms);
                if let E::Dictar(t) = ev {
                    self.shuma.inner = shuma_module_shell::update(
                        self.shuma.inner.clone(),
                        shuma_module_shell::Msg::InsertAtCursor(t),
                    );
                }
                self.marcar_shuma_dirty();
            }
            // Live-wire: click sobre la shuma completa (cuerpo o input de la
            // barra). `apply_shuma_full` ya abre el drawer ante un FocusInput.
            Msg::ShumaFull(m) => {
                self.apply_shuma_full(vec![m.0]);
                self.marcar_shuma_dirty();
            }
            Msg::Spawn(cmd) => crate::spawn_cmd(&cmd),
            Msg::VolumeWheel(dy) => {
                // Rueda arriba = subir. El stack entrega dy>0 al rodar hacia
                // abajo, así que invertimos: scroll-up (dy<0) sube el volumen.
                if dy != 0.0 {
                    crate::sampler::nudge_volume(dy < 0.0);
                    self.refresh_volume_now();
                    self.flash_osd(crate::render::OsdKind::Volume, self.ctx.volume, self.ctx.muted);
                }
            }
            Msg::VolumeMute => {
                crate::sampler::toggle_mute();
                self.flash_osd(crate::render::OsdKind::Volume, self.ctx.volume, !self.ctx.muted);
            }
            Msg::VolumeSet(f) => {
                crate::sampler::set_volume(f);
                self.flash_osd(crate::render::OsdKind::Volume, f, false);
            }
            Msg::VolumePanel => {
                // Antes lanzaba pavucontrol externo; ahora el mezclador nativo.
                if !(self.menu_open && self.menu_kind == MenuKind::Volume) {
                    self.sink_inputs = crate::sampler::sample_sink_inputs();
                    self.sinks = crate::sampler::sample_sinks();
                }
                self.toggle_menu(MenuKind::Volume);
            }
            Msg::VolumeTabSet(t) => {
                self.volume_tab = t;
                self.marcar_menu_dirty();
            }
            Msg::SourceOutputVolume(index, frac) => crate::sampler::set_source_output_volume(index, frac),
            Msg::SourceOutputMute(index) => crate::sampler::toggle_source_output_mute(index),
            Msg::SourceVolume(name, frac) => crate::sampler::set_source_volume(&name, frac),
            Msg::SourceMute(name) => crate::sampler::toggle_source_mute(&name),
            Msg::SourceSelect(name) => {
                crate::sampler::set_default_source(&name);
                for s in &mut self.sources {
                    s.is_default = s.name == name;
                }
                self.marcar_menu_dirty();
            }
            Msg::SinkVolume(name, frac) => crate::sampler::set_sink_volume(&name, frac),
            Msg::SinkMute(name) => crate::sampler::toggle_sink_mute(&name),
            Msg::SinkInputVolume(index, frac) => {
                crate::sampler::set_sink_input_volume(index, frac);
            }
            Msg::SinkInputMute(index) => crate::sampler::toggle_sink_input_mute(index),
            Msg::SinkSelect(name) => {
                crate::sampler::set_default_sink(&name);
                // Refleja al toque el nuevo default (la marca ●) sin esperar al
                // próximo refresco del panel.
                for s in &mut self.sinks {
                    s.is_default = s.name == name;
                }
            }
            Msg::SessionToggle => {
                self.session_confirm = None;
                self.toggle_menu(MenuKind::Session);
            }
            Msg::SessionConfirm(a) => {
                self.session_confirm = Some(a);
                self.marcar_menu_dirty();
            }
            Msg::SessionCancel => {
                self.session_confirm = None;
                self.marcar_menu_dirty();
            }
            Msg::SessionRun(a) => {
                crate::run_session_action(a);
                self.session_confirm = None;
                self.set_menu_open(false);
            }
            Msg::ConfirmPedir(accion) => {
                // Abre la pantalla de confirmación fullscreen: guarda la acción y crece
                // la surface del menú al alto del monitor (vía `menu_surface_height`).
                self.confirm_overlay = Some(accion);
                self.session_confirm = None;
                if self.menu_open && self.menu_kind != MenuKind::Confirm {
                    // Cierra cualquier otro menú-banda antes de abrir el fullscreen.
                    self.set_menu_open(false);
                }
                self.menu_kind = MenuKind::Confirm;
                self.set_menu_open(true);
            }
            Msg::ConfirmAceptar => {
                if let Some(accion) = self.confirm_overlay.take() {
                    accion.ejecutar();
                }
                self.set_menu_open(false);
            }
            Msg::ConfirmCancelar => {
                self.confirm_overlay = None;
                self.set_menu_open(false);
            }
            Msg::MediaPlayPause => crate::mpris::play_pause(),
            Msg::MediaNext => crate::mpris::next(),
            Msg::MediaPrev => crate::mpris::previous(),
            Msg::BluetoothToggle => self.toggle_menu(MenuKind::Bluetooth),
            Msg::BluetoothPower(on) => {
                crate::bluetooth::set_power(on);
                if let Some(b) = &mut self.bluetooth_now {
                    b.powered = on;
                }
                self.marcar_menu_dirty();
            }
            Msg::BluetoothConnect(mac) => crate::bluetooth::connect(&mac),
            Msg::BluetoothDisconnect(mac) => crate::bluetooth::disconnect(&mac),
            Msg::BluetoothScan => crate::bluetooth::scan(),
            Msg::BluetoothPair(mac) => crate::bluetooth::pair(&mac),
            Msg::NotificationsToggle => self.toggle_menu(MenuKind::Notifications),
            Msg::NotificationsDnd(on) => {
                if let Some(h) = &self.notifications {
                    h.set_dnd(on);
                }
                self.marcar_menu_dirty();
            }
            Msg::NotificationsClear => {
                if let Some(h) = &self.notifications {
                    h.clear();
                }
                self.marcar_menu_dirty();
            }
            Msg::PolkitChar(c) => {
                self.polkit_input.push(c);
                self.marcar_menu_dirty();
            }
            Msg::PolkitBackspace => {
                self.polkit_input.pop();
                self.marcar_menu_dirty();
            }
            Msg::PolkitSubmit => {
                if let Some(req) = self.polkit_prompt.take() {
                    let _ = req.reply.send(Some(std::mem::take(&mut self.polkit_input)));
                }
                self.cerrar_polkit();
            }
            Msg::PolkitCancel => {
                if let Some(req) = self.polkit_prompt.take() {
                    let _ = req.reply.send(None);
                }
                self.cerrar_polkit();
            }
            Msg::BrightnessWheel(dy) => {
                if dy != 0.0 {
                    crate::sampler::nudge_brightness(dy < 0.0);
                    self.refresh_brightness_now();
                    self.flash_osd(crate::render::OsdKind::Brightness, self.ctx.brightness, false);
                }
            }
            Msg::BrightnessSet(f) => {
                crate::sampler::set_brightness(f);
                self.flash_osd(crate::render::OsdKind::Brightness, f, false);
            }
            Msg::BrightnessPanel => {}
            Msg::CompletionDismiss => {
                // Clic afuera del panel de completado flotante: cierra el popup del
                // módulo y encoge la surface autónoma.
                self.shuma.inner.close_completion();
                self.set_completion_open(false);
                self.marcar_shuma_dirty();
            }
            Msg::ControlToggle => {
                // Antes el engranaje ⚙ no hacía nada en el DM. Ahora abre el
                // control panel (ajustes rápidos) como menú; al abrir, refresca
                // batería/wifi/bt.
                if !(self.menu_open && self.menu_kind == MenuKind::Control) {
                    self.control_extras = crate::render::ControlExtras::read();
                }
                self.toggle_menu(MenuKind::Control);
            }
            // Antes el path layer-shell no atendía estos toggles del Control panel
            // (caían al `_ => {}`): los switches de Wi-Fi/BT no hacían nada en el DM.
            Msg::ControlWifi(on) => {
                crate::render::set_radio("wlan", on);
                self.control_extras.wifi = on;
                self.marcar_menu_dirty();
            }
            Msg::ControlBt(on) => {
                crate::render::set_radio("bluetooth", on);
                self.control_extras.bt = on;
                self.marcar_menu_dirty();
            }
            Msg::ControlPowerProfile(id) => {
                crate::render::set_power_profile(&id);
                self.control_extras.power_profile = Some(id);
                self.marcar_menu_dirty();
            }
            Msg::ControlNight(on) => {
                crate::render::set_night(on);
                self.control_extras.night = on;
                self.marcar_menu_dirty();
            }
            Msg::ControlCafe(on) => {
                // «Mantener despierto»: gatea el idle de energía (vía
                // `energia_cfg.cafe`) y, además, el inhibidor del compositor se
                // crea/destruye en `ensure_cafe_inhibitor` (necesita `qh`).
                self.energia_cfg.cafe = on;
                self.control_extras.cafe = on;
                self.marcar_menu_dirty();
            }
            Msg::ControlTeclado(on) => {
                // Teclado en pantalla: lanza/mata el proceso `mirada-teclado`
                // (superficie wlr-layer-shell anclada abajo que inyecta al cliente
                // enfocado por `zwp_virtual_keyboard`). Idempotente: no relanza si
                // ya hay uno vivo, ni deja huérfanos al ocultar.
                if on {
                    if self.teclado_child.is_none() {
                        match std::process::Command::new("mirada-teclado").spawn() {
                            Ok(c) => self.teclado_child = Some(c),
                            Err(e) => eprintln!("pata: no pude lanzar mirada-teclado: {e}"),
                        }
                    }
                } else if let Some(mut c) = self.teclado_child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                self.control_extras.teclado = self.teclado_child.is_some();
                self.marcar_menu_dirty();
            }
            Msg::ControlPaisaje(on) => {
                // Paisaje sonoro (takiy): enciende/apaga la música ambiental que
                // el shell genera sin abrir apps. En el layer-shell (el path real)
                // esto **sí** gobierna audio; el snapshot del escritorio se le
                // empuja en el latido periódico mientras esté encendido.
                self.paisaje_on = on;
                if let Some(h) = &self.paisaje {
                    h.set_enabled(on);
                }
                self.control_extras.paisaje = on;
                self.marcar_menu_dirty();
            }
            Msg::Magnify(pct) => {
                // Lupa de pantalla: el compositor la aplica (sigue el puntero).
                // Guardamos el nivel para resaltar el segmento activo (best-effort:
                // los atajos de teclado lo mueven sin que pata se entere).
                crate::spawn_cmd(&format!("mirada-ctl magnify {pct}"));
                self.control_extras.magnify_pct = pct;
                self.marcar_menu_dirty();
            }
            Msg::Record(on) => {
                // Grabar pantalla: el compositor toma sus cuadros y los encodea.
                crate::spawn_cmd(if on {
                    "mirada-ctl record start"
                } else {
                    "mirada-ctl record stop"
                });
                self.control_extras.recording = on;
                self.marcar_menu_dirty();
            }
            Msg::NetworkToggle => {
                self.net_password = None;
                self.set_menu_keyboard(false);
                self.toggle_menu(MenuKind::Network);
            }
            Msg::NetworkPasswordPrompt(ssid) => {
                self.net_password = Some((ssid, String::new()));
                // El campo necesita foco de teclado (como el menú de inicio).
                self.set_menu_keyboard(true);
                self.marcar_menu_dirty();
            }
            Msg::NetworkPasswordChar(c) => {
                if let Some((_, pw)) = &mut self.net_password {
                    pw.push(c);
                    self.marcar_menu_dirty();
                }
            }
            Msg::NetworkPasswordBackspace => {
                if let Some((_, pw)) = &mut self.net_password {
                    pw.pop();
                    self.marcar_menu_dirty();
                }
            }
            Msg::NetworkPasswordSubmit => {
                if let Some((ssid, pw)) = self.net_password.take() {
                    crate::network::connect_with(&ssid, &pw);
                    self.set_menu_keyboard(false);
                    self.set_menu_open(false);
                }
            }
            Msg::NetworkPasswordCancel => {
                self.net_password = None;
                self.set_menu_keyboard(false);
                self.marcar_menu_dirty();
            }
            Msg::NetworkConnect(ssid) => {
                crate::network::connect(&ssid);
                self.set_menu_open(false);
            }
            Msg::NetworkDisconnect(ssid) => {
                crate::network::disconnect(&ssid);
                self.set_menu_open(false);
            }
            Msg::NetConnUp(name) => {
                crate::network::conn_up(&name);
                self.set_menu_open(false);
            }
            Msg::NetForget(name) => {
                crate::network::forget(&name);
                self.marcar_menu_dirty();
            }
            Msg::NetworkRadio(on) => {
                crate::network::set_wifi_radio(on);
                // Reflejo optimista: el próximo muestreo confirma. Repinta el popup.
                if let Some(n) = &mut self.network_now {
                    n.wifi_enabled = on;
                }
                self.marcar_menu_dirty();
            }
            Msg::ClipboardMenu => self.toggle_menu(MenuKind::Clipboard),
            Msg::ClipboardPick(text) => {
                crate::sampler::copiar_clipboard(&text);
                self.set_menu_open(false);
            }
            Msg::ClipboardAction(objetivo) => {
                let _ = std::process::Command::new("xdg-open").arg(&objetivo).spawn();
                self.set_menu_open(false);
            }
            Msg::ClipboardPin(id) => {
                if let Some(store) = &self.clip_store {
                    let _ = store.alternar_fijado(id);
                }
                self.clip_history = crate::clip_history_desde_store(&self.clip_store);
            }
            Msg::ClipboardDelete(id) => {
                if let Some(store) = &self.clip_store {
                    let _ = store.borrar(id);
                }
                self.clip_history = crate::clip_history_desde_store(&self.clip_store);
            }
            Msg::ClockPanel => {
                if !(self.menu_open && self.menu_kind == MenuKind::Clock) {
                    self.clock_draft = crate::ClockDraft::from_now(crate::usa_utc(&self.cfg));
                }
                self.toggle_menu(MenuKind::Clock);
            }
            Msg::CieloPanel => self.toggle_menu(MenuKind::Cielo),
            Msg::ClimaPanel => self.toggle_menu(MenuKind::Weather),
            Msg::TampuPanel => self.toggle_menu(MenuKind::Tampu),
            Msg::CapturaPanel => self.toggle_menu(MenuKind::Captura),
            Msg::Captura(m) => {
                if self.menu_open && self.menu_kind == MenuKind::Captura {
                    self.toggle_menu(MenuKind::Captura); // cierra
                }
                crate::spawn_cmd(m.comando());
            }
            Msg::GrabarIniciar(modo, audio) => {
                if self.menu_open && self.menu_kind == MenuKind::Captura {
                    self.toggle_menu(MenuKind::Captura); // cierra
                }
                if self.grabacion.is_none() {
                    match crate::grabacion::Grabacion::iniciar(modo, audio) {
                        Ok(g) => self.grabacion = Some(g),
                        Err(e) => eprintln!("pata: no se pudo grabar: {e}"),
                    }
                }
            }
            Msg::GrabarDetener => {
                if self.menu_open && self.menu_kind == MenuKind::Captura {
                    self.toggle_menu(MenuKind::Captura); // cierra
                }
                if let Some(g) = self.grabacion.take() {
                    let _ = g.detener();
                }
            }
            Msg::UsbPanel => self.toggle_menu(MenuKind::Usb),
            Msg::UsbMontar(dev) => crate::usb::montar(&dev),
            Msg::UsbDesmontar(dev) => crate::usb::desmontar(&dev),
            Msg::UsbExpulsar(disco) => {
                crate::usb::expulsar(&disco);
                if self.menu_open && self.menu_kind == MenuKind::Usb {
                    self.toggle_menu(MenuKind::Usb);
                }
            }
            Msg::UsbAbrir(punto) => crate::spawn_cmd(&crate::usb::abrir(&punto)),
            Msg::AgoraPanel => self.toggle_menu(MenuKind::Agora),
            Msg::AgoraAbrir => {
                crate::spawn_cmd("agora-app");
                if self.menu_open && self.menu_kind == MenuKind::Agora {
                    self.toggle_menu(MenuKind::Agora);
                }
            }
            Msg::KhipuPanel => {
                let abriendo = !(self.menu_open && self.menu_kind == MenuKind::Khipu);
                if abriendo {
                    self.khipu_snapshot = self.khipu.snapshot(crate::khipu::ahora_unix());
                    self.khipu_input = Some(String::new());
                } else {
                    self.khipu_input = None;
                }
                self.toggle_menu(MenuKind::Khipu);
            }
            Msg::KhipuChar(c) => {
                if let Some(d) = &mut self.khipu_input {
                    if !c.is_control() {
                        d.push(c);
                    }
                }
                self.marcar_menu_dirty();
            }
            Msg::KhipuBackspace => {
                if let Some(d) = &mut self.khipu_input {
                    d.pop();
                }
                self.marcar_menu_dirty();
            }
            Msg::KhipuSubmit => {
                let texto = self.khipu_input.clone().unwrap_or_default();
                self.khipu.jot(&texto, crate::khipu::ahora_unix());
                self.khipu_input = Some(String::new());
                self.khipu_snapshot = self.khipu.snapshot(crate::khipu::ahora_unix());
                self.marcar_menu_dirty();
            }
            Msg::KhipuReinforce(id) => {
                self.khipu.reinforce(id, crate::khipu::ahora_unix());
                self.khipu_snapshot = self.khipu.snapshot(crate::khipu::ahora_unix());
                self.marcar_menu_dirty();
            }
            Msg::CieloLocalidad(n) => {
                let locs = &self.cfg.general.ubicacion.localidades;
                if n == u32::MAX {
                    if let Ok(mut g) = self.cielo_loc.lock() {
                        *g = None;
                    }
                    self.cfg.general.ubicacion.activa = locs.len() as u32; // auto
                } else if let Some(loc) = locs.get(n as usize) {
                    let coords = (loc.lat, loc.lon);
                    if let Ok(mut g) = self.cielo_loc.lock() {
                        *g = Some(coords);
                    }
                    self.cfg.general.ubicacion.activa = n;
                }
                self.marcar_menu_dirty();
            }
            Msg::ClockAdjust(f, delta) => {
                self.clock_draft.adjust(f, delta);
                self.marcar_menu_dirty();
            }
            Msg::ClockApply => {
                crate::sampler::set_system_time(&self.clock_draft.stamp());
                self.set_menu_open(false);
            }
            Msg::ClockSyncNtp => {
                crate::sampler::sync_ntp();
                self.set_menu_open(false);
            }
            Msg::StartToggle => self.toggle_menu(MenuKind::Apps),
            Msg::MenuHoverCategory(i) => {
                self.menu_sel = 0;
                if self.menu_cat != Some(i) {
                    self.menu_cat = Some(i);
                    self.menu_scroll = 0.0;
                    self.marcar_menu_dirty();
                }
            }
            Msg::MenuScrollTo(v) => {
                self.menu_scroll = v;
                self.marcar_menu_dirty();
            }
            Msg::StartScroll(delta) => {
                let count =
                    render::menu_filtered(self.registry.all(), &self.menu_query).len();
                let content = count as f32 * 30.0;
                let viewport =
                    (MENU_H as f32 - self.menu_bar_px as f32 - 62.0).max(28.0);
                self.menu_scroll = llimphi_widget_scroll::clamp_offset(
                    self.menu_scroll + delta,
                    content,
                    viewport,
                );
                self.marcar_menu_dirty();
            }
            Msg::LaunchApp(id) => self.lanzar_app(id),
            Msg::SwitchPacha(id) => {
                diag!("pata diag · SwitchPacha({id}) → pacha switch {id}");
                crate::spawn_cmd(&format!("pacha switch {id}"));
                // Reconcilia el estado vivo (instancia activa/ciclo) para que el
                // panel del diente perfil y el control center salten al instante.
                self.control_extras = crate::render::ControlExtras::read();
            }
            Msg::PachaSelect(id) => {
                // Sólo cambia qué instancia se VE en el panel del diente perfil.
                self.nav.pacha_sel = id;
            }
            // Conmutar de escritorio: lo pide el switcher de la barra (dwm/
            // hyprland/solaris). Faltaba el arm en el path layer-shell → los
            // botones de workspace no hacían nada en el DM (sólo en winit).
            Msg::SwitchWorkspace(n) => {
                diag!("pata diag · SwitchWorkspace({n}) → mirada-ctl workspace {n}");
                crate::sampler::switch_workspace(n);
                // Feedback INSTANTÁNEO: el sampler de fondo refresca cada ~1s (y
                // cada tick corre varios subprocesos), así que el resalte tardaba
                // segundos y parecía que el click no entraba. Movemos el activo ya
                // y lo sostenemos unos samples (`pending_ws`) para que un muestreo
                // viejo no lo revierta antes de que el WM aplique el salto.
                self.ctx.active_workspace = n;
                self.pending_ws = Some((n, crate::sampler::OPTIMISTIC_TICKS));
                self.marcar_todo_dirty();
            }
            // Clic en un diente-workspace del rail (izquierdo): cambia de escritorio
            // Y despliega/repliega su taskbar según el modo de expansión del rag —
            // como cualquier diente. Faltaba el arm en el path layer-shell (caía al
            // `_ => {}`): por eso el sidebar de workspaces "no expandía" en el DM.
            Msg::WorkspaceTooth { si, ws } => {
                crate::sampler::switch_workspace(ws);
                self.ctx.active_workspace = ws;
                self.pending_ws = Some((ws, crate::sampler::OPTIMISTIC_TICKS));
                self.marcar_todo_dirty();
                // `set_sidebar_open` respeta `diente_dos_pasos` (un clic cambia sin
                // expandir / re-clic expande) y reconcilia el drawer. El id va CODIFICADO
                // (`WS_BASE + ws`): el rail unificado mezcla escritorios con tabs de
                // config, así que `nav.open` guarda el id de grupo, no el nº crudo.
                self.set_sidebar_open(si, crate::render::sidebar::WS_BASE as usize + ws as usize);
            }
            Msg::ActivateWindow(id) => {
                diag!(
                    "pata diag · ActivateWindow({id}) seat={} toplevel={}",
                    self.seat.is_some(),
                    self.toplevel_por_id(id).is_some()
                );
                self.activar_ventana(id);
                // Feedback inmediato: marca esta ventana como activa en la lista
                // (el foco real lo confirma el compositor en el próximo censo).
                for t in &mut self.toplevels {
                    t.activated = t.id == id;
                }
                self.marcar_todo_dirty();
            }
            Msg::CloseWindow(id) => self.cerrar_ventana(id),
            // Pestañas verticales del rail: los ids son de MIRADA (la lista sale
            // de `mirada-ctl windows`, no de los toplevels foreign), así que la
            // interacción va por la CLI del WM en vez de `activar_ventana`.
            Msg::RailTabActivate(id) => {
                crate::sampler::activate_window(id);
                // Feedback inmediato en la propia lista muestreada (el sample de
                // ~1 s confirma después).
                for w in &mut self.windows_ws {
                    w.active = w.id == id;
                }
                self.marcar_todo_dirty();
            }
            Msg::RailTabClose(id) => crate::sampler::close_window(id),
            // Menú contextual del taskbar de un diente-escritorio: popup flotante en
            // la surface del drawer (como el popover de disposición). Al abrirlo se
            // ensancha la input-region a toda la surface (para que backdrop + card
            // reciban clics); al cerrarlo se restaura al panel.
            Msg::WinMenuOpen { si, ws, id, title, x, y } => {
                self.nav.win_menu = Some(crate::nouser::WinMenu { si, ws, win_id: id, title, x, y });
                // Estampá la apertura para que el cierre-al-desenfocar ignore el
                // `leave` espurio del reacomodo de input-region que sigue.
                self.drawer_opened_at = Some(std::time::Instant::now());
                for pi in self.drawers_mostrados_de(si) {
                    self.set_drawer_full_input(pi);
                }
                self.marcar_sidebars_dirty();
            }
            Msg::WinMenuClose => {
                self.nav.win_menu = None;
                self.restaurar_input_drawer();
                self.marcar_sidebars_dirty();
            }
            Msg::WinMenuDo(id, act) => {
                let ws = self.nav.win_menu.as_ref().map(|m| m.ws).unwrap_or(0);
                crate::do_win_act(id, act, ws, &self.windows_ws);
                self.nav.win_menu = None;
                self.restaurar_input_drawer();
                self.marcar_sidebars_dirty();
            }
            Msg::TaskDragMove(id, dx) => self.task_drag_move(id, dx),
            Msg::TaskDragEnd(id) => self.task_drag_end(id),
            Msg::TrayActivate(key) => {
                if let Some(t) = &self.tray {
                    t.activate(key);
                }
            }
            Msg::NavTabActivate(si, ti) => self.set_sidebar_open(si, ti),
            // Barrita del sidebar: se aplica EN VIVO (sin re-exec ni parpadeo).
            // Docked = sólo cambia el `exclusive_zone` de la surface; posición del
            // rail = puramente render. Ambos persisten en el TOML.
            Msg::SidebarSetDocked(si, docked) => {
                crate::persistir_eje_sidebar(si, Some(docked), None);
                self.aplicar_docked_sidebar(si, docked);
            }
            Msg::SidebarSetRailOutside(si, outside) => {
                crate::persistir_eje_sidebar(si, None, Some(outside));
                if let Some(s) = self.cfg.surfaces.get_mut(si) {
                    s.rail_outside = Some(outside);
                }
                // Adentro/Afuera re-ancla las surfaces (el rail cambia de lado del panel).
                self.aplicar_geometria_sidebar(si);
            }
            // Autohide EN VIVO (sin re-exec): cambia la reserva de franja del rail
            // (exclusive_zone) + el margen del drawer, y la reconciliación del render
            // se encarga de ocultar/revelar. Al prender arranca oculto; al apagar,
            // visible (se limpia el revelado).
            Msg::SidebarSetAutohide(si, autohide) => {
                crate::persistir_autohide_sidebar(si, autohide);
                if let Some(s) = self.cfg.surfaces.get_mut(si) {
                    s.autohide = autohide;
                }
                let docked = self
                    .cfg
                    .surfaces
                    .get(si)
                    .map(|s| s.reserve.unwrap_or(self.sidebar_docked))
                    .unwrap_or(true);
                self.aplicar_docked_sidebar(si, docked);
                if !autohide {
                    self.revealed_sidebars.remove(&si);
                }
                self.marcar_sidebars_dirty();
            }
            // Dientes de dos pasos: sólo cambia el comportamiento del click, no el
            // anclaje → hot-reload sin re-exec.
            Msg::SidebarSetDienteDosPasos(b) => {
                crate::persistir_diente_dos_pasos(b);
                self.cfg.general.diente_dos_pasos = b;
                self.marcar_sidebars_dirty();
            }
            // Arrastre del divisor: cambia `panel_width` EN VIVO. La surface del drawer
            // se creó a ancho máximo (`DRAWER_SURFACE_W`), así que redimensionar el
            // panel es solo repintar + reajustar la input-region al nuevo ancho — sin
            // tocar el tamaño de la layer surface (que crashea Iris Xe).
            Msg::SidebarResize(si, dx) => {
                if let Some(s) = self.cfg.surfaces.get_mut(si) {
                    s.panel_width = (s.panel_width + dx).clamp(120.0, 600.0);
                }
                crate::persistir_panel_width_sidebar(
                    si,
                    self.cfg.surfaces.get(si).map(|s| s.panel_width).unwrap_or(300.0),
                );
                // Si el drawer de este sidebar está mostrado (en cualquier monitor),
                // refresca su input-region (área clickeable = panel al nuevo ancho).
                let mostrados = self.drawers_mostrados_de(si);
                if !mostrados.is_empty() {
                    for pi in mostrados {
                        self.set_drawer_clickable(pi, true);
                        self.panels[pi].cache = None;
                        self.panels[pi].dirty = true;
                    }
                    // En Fijo, el ancho reservado y el offset del rail (Adentro) siguen
                    // al panel al redimensionar.
                    self.aplicar_geometria_sidebar(si);
                }
            }
            Msg::SidebarControlToggle(si) => {
                self.nav.control_open = !self.nav.control_open;
                self.nav.control_si = self.nav.control_open.then_some(si);
                // Estampá la apertura para que el cierre-al-desenfocar de la ventanita
                // ignore el `leave` espurio del reacomodo de input-region que sigue.
                if self.nav.control_open {
                    self.drawer_opened_at = Some(std::time::Instant::now());
                }
                // El popover de disposición se pinta en la surface del drawer (640 px),
                // fuera del nodo del panel. Si YA hay un drawer mostrado (diente
                // desplegado), sólo se ensancha/restaura su input-region aquí. Si NO
                // (rail colapsado), `reconcile_drawer` mostrará el drawer de `si` en el
                // próximo draw para hostear la card como ventanita autónoma.
                self.restaurar_input_drawer();
                self.marcar_sidebars_dirty();
            }
            Msg::SearchFocus(f) => {
                self.nav.search_focused = f;
                self.marcar_sidebars_dirty();
            }
            Msg::SearchChar(c) => {
                self.nav.search.push(c);
                self.nav.apply_search();
                self.marcar_sidebars_dirty();
            }
            Msg::SearchBackspace => {
                self.nav.search.pop();
                self.nav.apply_search();
                self.marcar_sidebars_dirty();
            }
            Msg::SearchClear => {
                self.nav.search.clear();
                self.nav.search_focused = false;
                self.marcar_sidebars_dirty();
            }
            Msg::NavClosePanel => self.cerrar_sidebar(),
            Msg::NavSetMode(m) => {
                self.nav.mode = m;
                self.marcar_sidebars_dirty();
            }
            Msg::NavSelect(id) => {
                self.nav.selected = Some(id);
                self.marcar_sidebars_dirty();
            }
            Msg::NavToggle(id) => self.nav_toggle(id),
            Msg::NavContextMenu(id) => {
                if let Some(path) = self.nav.file_path(id).map(str::to_owned) {
                    let opts = crate::open::handlers_for_path(&self.registry, &path);
                    self.nav.open_menu(id, opts);
                    self.marcar_sidebars_dirty();
                }
            }
            Msg::NavOpenWith(id, app_id) => {
                if let Some(path) = self.nav.file_path(id).map(str::to_owned) {
                    match app_id {
                        Some(aid) => {
                            let _ = crate::open::open_with_id(&self.registry, &aid, &path);
                        }
                        None => {
                            let _ = crate::open::open_system(&path);
                        }
                    }
                }
                self.nav.close_menu();
                self.marcar_sidebars_dirty();
            }
            Msg::NavMenuCancel => {
                self.nav.close_menu();
                self.marcar_sidebars_dirty();
            }
            Msg::HostToothActivate(app_id, tooth) => {
                if let Some(h) = &self.host {
                    h.activate(&app_id, tooth);
                }
            }
            Msg::NavScroll(delta) => {
                self.nav.scroll = (self.nav.scroll + delta).max(0.0);
                self.marcar_sidebars_dirty();
            }
            // --- Sidebar RAG ---
            Msg::RagEngineReady { ok, corpus } => {
                self.rag.corpus_len = corpus;
                self.rag.status = if ok {
                    crate::rag::RagStatus::Idle
                } else {
                    crate::rag::RagStatus::Unavailable
                };
                self.marcar_sidebars_dirty();
            }
            Msg::RagChar(c) => {
                if !c.is_control()
                    && matches!(
                        self.rag.status,
                        crate::rag::RagStatus::Idle | crate::rag::RagStatus::Ready
                    )
                {
                    self.rag.query.push(c);
                    self.marcar_sidebars_dirty();
                }
            }
            Msg::RagBackspace => {
                self.rag.query.pop();
                self.marcar_sidebars_dirty();
            }
            Msg::RagClear => {
                self.rag.query.clear();
                self.rag.answer.clear();
                self.rag.sources.clear();
                self.rag.error = None;
                if matches!(self.rag.status, crate::rag::RagStatus::Ready) {
                    self.rag.status = crate::rag::RagStatus::Idle;
                }
                self.marcar_sidebars_dirty();
            }
            Msg::RagSubmit => {
                let q = self.rag.query.trim().to_string();
                if !q.is_empty()
                    && matches!(
                        self.rag.status,
                        crate::rag::RagStatus::Idle | crate::rag::RagStatus::Ready
                    )
                {
                    self.rag.status = crate::rag::RagStatus::Asking;
                    self.rag.answer.clear();
                    self.rag.sources.clear();
                    self.rag.error = None;
                    if let Ok(guard) = self.rag.engine.lock() {
                        if let Some(engine) = guard.as_ref() {
                            let tx = self.rag_tx.clone();
                            engine.ask(q, Box::new(move |res| {
                                let m = match res {
                                    Ok(a) => Msg::RagResult {
                                        answer: a.answer,
                                        sources: a.sources,
                                    },
                                    Err(e) => Msg::RagError(e.to_string()),
                                };
                                let _ = tx.send(m);
                            }));
                        } else {
                            self.rag.status = crate::rag::RagStatus::Unavailable;
                        }
                    }
                    self.marcar_sidebars_dirty();
                }
            }
            Msg::RagResult { answer, sources } => {
                self.rag.answer = answer;
                self.rag.sources = sources;
                self.rag.error = None;
                self.rag.status = crate::rag::RagStatus::Ready;
                self.marcar_sidebars_dirty();
            }
            Msg::RagError(e) => {
                self.rag.error = Some(e);
                self.rag.status = crate::rag::RagStatus::Ready;
                self.marcar_sidebars_dirty();
            }
            Msg::Quit => self.exit = true,
            _ => {}
        }
    }

    /// Click en una ventana del task manager: activa o minimiza.
    pub(super) fn activar_ventana(&mut self, id: u32) {
        // El `activate` del foreign-toplevel necesita un `wl_seat`. Normalmente
        // lo captura `new_seat`, pero si ese callback aún no corrió (o la barra
        // no bindeó capacidades) `self.seat` quedaba `None` y el click "no hacía
        // nada" SILENCIOSAMENTE. Caemos al primer seat conocido por `SeatState`.
        let seat = self.seat.clone().or_else(|| {
            let s = self.seat_state.seats().next();
            if s.is_some() {
                self.seat = s.clone();
            }
            s
        });
        let Some(seat) = seat else {
            diag!("pata diag · activar_ventana({id}) SIN seat — activate NO enviado");
            return;
        };
        if let Some(t) = self.toplevel_por_id(id) {
            // SIEMPRE activar (enfocar/levantar). Antes alternaba a minimizar la
            // ventana ya activa, pero mirada ignora `set_minimized` (no-op) → el
            // click sobre el taskicon de la ventana enfocada "no hacía nada".
            t.handle.unset_minimized();
            t.handle.activate(&seat);
            diag!("pata diag · activar_ventana({id}) → activate enviado");
        } else {
            diag!("pata diag · activar_ventana({id}) sin toplevel para ese id");
        }
    }

    /// Cierra la ventana `id`.
    pub(super) fn cerrar_ventana(&mut self, id: u32) {
        if let Some(t) = self.toplevel_por_id(id) {
            t.handle.close();
        }
    }

    /// Paso de un arrastre de reordenamiento del task manager: acumula el delta
    /// y reescribe `task_order` recolocando la ventana arrastrada según cuántos
    /// slots se movió el puntero. Se recalcula desde `orden_base` en cada paso
    /// para no acumular deriva.
    fn task_drag_move(&mut self, id: u32, dx: f32) {
        // Al primer `Move` (o si cambió la ventana arrastrada) capturamos el
        // orden visible actual como base del arrastre.
        if self.task_drag.as_ref().map(|d| d.id) != Some(id) {
            let orden: Vec<u32> = self.window_entries().iter().map(|e| e.id).collect();
            let idx_base = orden.iter().position(|&x| x == id).unwrap_or(0);
            self.task_drag = Some(TaskDrag {
                id,
                dx_acc: 0.0,
                movido: 0.0,
                orden_base: orden,
                idx_base,
            });
        }
        let Some(d) = self.task_drag.as_mut() else { return };
        d.dx_acc += dx;
        d.movido += dx.abs();
        // Cuántos slots (botón + gap) se desplazó respecto del inicio.
        let salto = (d.dx_acc / TASK_SLOT_W).round() as isize;
        let len = d.orden_base.len() as isize;
        let destino = (d.idx_base as isize + salto).clamp(0, (len - 1).max(0)) as usize;
        // Reconstruimos el orden desde la base, moviendo `id` a `destino`.
        let mut nuevo = d.orden_base.clone();
        if let Some(pos) = nuevo.iter().position(|&x| x == id) {
            let v = nuevo.remove(pos);
            nuevo.insert(destino.min(nuevo.len()), v);
        }
        self.task_order = nuevo;
        self.marcar_todo_dirty();
    }

    /// Fin de un arrastre del task manager. Si la ventana apenas se movió fue un
    /// click (el `draggable` reemplaza al `on_click`): activamos la ventana. Si
    /// hubo arrastre real, el nuevo `task_order` ya quedó aplicado en vivo.
    fn task_drag_end(&mut self, id: u32) {
        let arrastrado = self
            .task_drag
            .take()
            .map(|d| d.movido >= TASK_DRAG_UMBRAL)
            .unwrap_or(false);
        if !arrastrado {
            self.activar_ventana(id);
            // Feedback inmediato del foco (igual que `Msg::ActivateWindow`).
            for t in &mut self.toplevels {
                t.activated = t.id == id;
            }
        }
        self.marcar_todo_dirty();
    }
}

/// Ancho aproximado de un slot del task manager (botón fijo + gap), en px, para
/// traducir el delta del arrastre a saltos de posición. Debe seguir a `TASK_W`
/// de `render::task_manager` (170 px) + el gap chico (≤ 4 px).
const TASK_SLOT_W: f32 = 174.0;

/// Movimiento mínimo (px) para considerar un arrastre "real" y no un click.
const TASK_DRAG_UMBRAL: f32 = 6.0;

#[cfg(test)]
mod tests {
    use super::{
        cava_con_audio, cava_ensucia, inerte, present_cap_ms, PRESENT_CAP_IDLE_MS, PRESENT_CAP_MS,
    };

    /// La canilla que mantenía repintando a `pata` en reposo: el daemon de `cava`
    /// manda cuadros aunque no suene nada, y cada uno ensuciaba los 13 paneles.
    #[test]
    fn cava_solo_ensucia_si_hay_algo_que_animar() {
        let silencio = vec![0.0009_f32; 32];
        let silencio2 = vec![0.0011_f32; 32];
        assert!(!cava_ensucia(&silencio2, &silencio), "ruido de piso no repinta");
        let musica = vec![0.6_f32; 32];
        assert!(cava_ensucia(&musica, &silencio), "arranca la música: repintar");
        assert!(cava_ensucia(&silencio, &musica), "para la música: pintar la caída");
        // Con música sonando, un temblor por debajo del umbral tampoco vale un frame.
        let casi = vec![0.605_f32; 32];
        assert!(!cava_ensucia(&casi, &musica), "un temblor invisible no repinta");
        let subio: Vec<f32> = musica.iter().map(|v| v + 0.2).collect();
        assert!(cava_ensucia(&subio, &musica), "un cambio visible sí repinta");
        // Cambiar la cantidad de bandas es reconfiguración: repintar.
        assert!(cava_ensucia(&vec![0.6; 16], &musica));
    }

    /// Lo que se paga sin este gate: 13 paneles × ~7 present/s, de los cuales 7
    /// no muestran nada (4 drawers cerrados + 3 surfaces de servicio a 1×1).
    #[test]
    fn una_surface_sin_nada_que_mostrar_esta_inerte() {
        // Drawer del sidebar: manda si está desplegado, no su tamaño (el cerrado
        // conserva el tamaño grande, que es justo lo que engañaba antes).
        assert!(inerte(true, false, 640, 1040, false), "drawer cerrado no pinta");
        assert!(!inerte(true, true, 640, 1040, false), "drawer desplegado sí pinta");
        // Surfaces de servicio: viven a 1×1 hasta que tienen algo que mostrar.
        assert!(inerte(false, false, 1, 1, false), "OSD sin cartel no pinta");
        assert!(!inerte(false, false, 1, 1, true), "con cartel pinta aunque siga a 1×1");
        // Una surface crecida siempre pinta — es la barra, el rail, el tooltip abierto.
        assert!(!inerte(false, false, 1920, 40, false), "la barra pinta siempre");
        assert!(!inerte(false, false, 1, 24, false), "crecida en un eje también");
    }

    #[test]
    fn cava_distingue_musica_de_silencio_ruidoso() {
        // Silencio con ruido de piso por barra (stream pausado): NO cuenta como audio.
        let silencio: Vec<f32> = vec![0.0009; 32];
        assert!(!cava_con_audio(&silencio), "ruido de piso no es audio");
        assert!(!cava_con_audio(&[]), "sin cava no es audio");
        // Música: varias bandas levantadas → suma » umbral.
        let musica: Vec<f32> = vec![0.6, 0.8, 0.3, 0.9, 0.1];
        assert!(cava_con_audio(&musica), "música real sí es audio");
    }

    #[test]
    fn present_cap_baja_en_reposo_ambiental() {
        // En reposo (sólo respiración ambiente) el cap es MÁS grande → menos fps.
        assert!(
            present_cap_ms(true) > present_cap_ms(false),
            "la respiración ambiente debe presentar más lento que la actividad"
        );
        assert_eq!(present_cap_ms(false), PRESENT_CAP_MS);
        assert_eq!(present_cap_ms(true), PRESENT_CAP_IDLE_MS);
        // El recorte es real: idle ≥2× el activo → ~la mitad (o menos) de presents.
        assert!(PRESENT_CAP_IDLE_MS >= PRESENT_CAP_MS * 2);
    }
}

