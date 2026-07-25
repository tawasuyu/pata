//! Implementaciones de los traits de smithay-client-toolkit para `LayerApp`:
//! handlers de compositor, layer shell, output, seat, teclado y puntero,
//! más los `Dispatch` de los protocolos wlr-foreign-toplevel.

use smithay_client_toolkit::{
    compositor::CompositorHandler,
    output::OutputHandler,
    registry::ProvidesRegistryState,
    registry_handlers,
    seat::{
        keyboard::{KeyboardHandler, Keysym, Modifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT},
        Capability, SeatHandler,
    },
    shell::wlr_layer::{KeyboardInteractivity, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
};
use wayland_client::{
    event_created_child,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1, EVT_TOPLEVEL_OPCODE},
};
use mirada_toplevel_icon_proto::client::mirada_toplevel_icon_manager_v1::{
    self, MiradaToplevelIconManagerV1,
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::{self, ExtIdleNotifierV1},
};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::{self, ZwpIdleInhibitManagerV1},
    zwp_idle_inhibitor_v1::{self, ZwpIdleInhibitorV1},
};

use llimphi_ui::llimphi_compositor::{
    hit_test_click, hit_test_hover, hit_test_middle_click, hit_test_right_click, hit_test_scroll,
    hit_test_tooltip, DragPhase,
};

use crate::toplevel::Toplevel;

use super::{app_impl::*, diag, LayerApp, LayerDrag, MenuKind, MENU_LEAVE_GRACE};

/// Si el puntero se aleja más que esto (px) del origen del press, el `on_click`
/// armado deja de contar como click (fue un arrastre/barrido). Espeja el umbral
/// del runtime de Llimphi para una sensación uniforme en todo el escritorio.
const CLICK_MOVE_CANCEL: f32 = 6.0;

impl CompositorHandler for LayerApp {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(pi) = self.panel_de(surface) {
            // El callback pedido por `latido` llegó: liberamos el slot para que el
            // próximo `latido` pueda pedir el siguiente (uno en vuelo por panel).
            self.frame_pending.borrow_mut().remove(&pi);
            self.draw(pi, qh);
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for LayerApp {
    fn closed(&mut self, _: &Connection, qh: &QueueHandle<Self>, layer: &LayerSurface) {
        use smithay_client_toolkit::shell::WaylandSurface;
        let pi = self.panel_de(layer.wl_surface());
        diag!("pata diag · CLOSED panel={pi:?}");
        // El compositor cerró esta surface. Marcamos SÓLO ese panel como muerto —y su
        // drawer hermano si es un rail de sidebar— soltando su GPU/caché. NO lo
        // removemos del `Vec`: rompería los índices cacheados (`shuma_panel`/`osd_pi`/…).
        if let Some(pi) = pi {
            self.panels[pi].dead = true;
            self.panels[pi].gpu = None; // suelta el swapchain wgpu de esa surface
            self.panels[pi].cache = None;
            // Si era un rail de sidebar, su drawer (mismo `idx`, `drawer==true`)
            // también murió con el output.
            let idx = self.panels[pi].idx;
            if !self.panels[pi].drawer {
                for p in self.panels.iter_mut().filter(|p| p.idx == idx && p.drawer) {
                    p.dead = true;
                    p.gpu = None;
                    p.cache = None;
                }
            }
        }
        // ¿Quedó muerto TODO panel anclado a un output? (los overlays sin output
        // —tooltip/OSD— no cuentan: nunca mueren). Antes salíamos ahí (`exit = true`),
        // pero el churn de DRM del Iris Xe manda `closed` a TODAS las barras de forma
        // TRANSITORIA con el monitor todavía presente → pata salía con código 0 y el
        // escritorio quedaba sin panel hasta el relogin (nadie lo respawneaba). Ahora:
        //  · si hay outputs vivos → RE-ANCLAMOS sobre ellos y seguimos vivos;
        //  · si no queda NINGÚN output → salimos (teardown real; igual el corte del
        //    socket en el bucle nos sacaría, y mirada nos respawnea).
        // `reanchor_output` es idempotente (saltea (idx,output) ya vivos), así que un
        // `closed` que sólo afectó a una barra no recrea las otras.
        if self
            .panels
            .iter()
            .filter(|p| p.output.is_some())
            .all(|p| p.dead)
        {
            let outs: Vec<_> = self.output_state.outputs().collect();
            if outs.is_empty() {
                self.exit = true;
            } else {
                for o in &outs {
                    self.reanchor_output(qh, o);
                }
            }
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        use smithay_client_toolkit::shell::WaylandSurface;
        let (cw, ch) = configure.new_size;
        let Some(pi) = self.panel_de(layer.wl_surface()) else {
            return;
        };
        diag!("pata diag · configure panel {pi} new_size={cw}x{ch}");
        const MAX_DIM: u32 = 16384;
        if (1..=MAX_DIM).contains(&cw) {
            self.panels[pi].width = cw;
        }
        if (1..=MAX_DIM).contains(&ch) {
            self.panels[pi].height = ch;
        }
        self.panels[pi].dirty = true;
        self.draw(pi, qh);
    }
}

impl OutputHandler for LayerApp {
    fn output_state(&mut self) -> &mut smithay_client_toolkit::output::OutputState {
        &mut self.output_state
    }
    /// Un monitor (re)aparece: re-anclamos sus barras/sidebars/docks/fondos si la
    /// config los coloca ahí y todavía no tienen panel vivo. Es el camino de vuelta
    /// del churn de DRM del Iris Xe (destruyó+recreó el output) y del enchufe real de
    /// un monitor externo. Ver [`LayerApp::reanchor_output`].
    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.reanchor_output(qh, &output);
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    /// Un monitor desaparece: marcamos muertos sus paneles (soltando GPU/caché) por si
    /// el `closed` por-surface no llegó. No los removemos del `Vec` (índices cacheados).
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        for p in self
            .panels
            .iter_mut()
            .filter(|p| p.output.as_ref() == Some(&output))
        {
            p.dead = true;
            p.gpu = None;
            p.cache = None;
        }
    }
}

impl SeatHandler for LayerApp {
    fn seat_state(&mut self) -> &mut smithay_client_toolkit::seat::SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.seat.is_none() {
            self.seat = Some(seat);
        }
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard if self.keyboard.is_none() => {
                if let Ok(kbd) = self.seat_state.get_keyboard(qh, &seat, None) {
                    self.keyboard = Some(kbd);
                }
            }
            Capability::Pointer if self.pointer.is_none() => {
                if let Ok(ptr) = self.seat_state.get_pointer(qh, &seat) {
                    self.pointer = Some(ptr);
                }
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard => {
                if let Some(k) = self.keyboard.take() {
                    k.release();
                }
            }
            Capability::Pointer => {
                if let Some(p) = self.pointer.take() {
                    p.release();
                }
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for LayerApp {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        let entered = self.panel_de(surface);
        diag!(
            "pata diag · KB ENTER panel={entered:?} (shuma_panel={:?}, shuma.open={})",
            self.shuma_panel,
            self.shuma.open
        );
        // Multi-monitor: el foco de teclado llegó a la barra de shuma de un monitor
        // (clic o fallback de escritorio vacío) → hazla la activa, para que tipear
        // y Enter expandan el drawer en esa pantalla.
        if let Some(pi) = entered {
            self.focus_shuma_panel(pi);
            // Encender el **cue de foco del input** (caret + marco) — simétrico
            // del BlurInput del `leave`, que sí existía: el teclado ya está aquí,
            // que el input lo refleje. Sin esto el compositor entregaba el
            // teclado por hover pero shuma se veía apagada ("no agarra foco").
            // Sólo con el drawer plegado: abierto, el foco interno es de shuma.
            if self.shuma_panel == Some(pi) && !self.shuma.open {
                if self.shuma_full.is_some() {
                    // El borrow de `shuma_full` termina en este `let` (produce un
                    // `Msg` propio), así `apply_shuma_full` puede tomar `&mut self`.
                    let m = self
                        .shuma_full
                        .as_ref()
                        .and_then(crate::shuma_app::focus_active_input);
                    if let Some(m) = m {
                        self.apply_shuma_full(vec![m]);
                    }
                } else {
                    self.shuma.inner = shuma_module_shell::update(
                        self.shuma.inner.clone(),
                        shuma_module_shell::Msg::FocusInput,
                    );
                }
                self.marcar_shuma_dirty();
            }
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        let pi = self.panel_de(surface);
        diag!(
            "pata diag · KB LEAVE panel={pi:?} (shuma_panel={:?}, shuma.open={})",
            self.shuma_panel,
            self.shuma.open
        );
        // El teclado se fue de la surface: no va a llegar el `release` de una
        // tecla sostenida — corta el auto-repeat aquí o quedaría repitiendo solo.
        self.key_held = None;
        if let Some(pi) = pi {
            if self.menu_panel == Some(pi)
                && self.menu_open
                && self.menu_kind == MenuKind::Apps
            {
                // Ignorá el `leave` que llega justo al abrir: es el reacomodo de
                // foco del compositor al darle el teclado al menú (Exclusive), no
                // que el usuario se haya ido. Sin esta guarda el menú se cerraba
                // al instante en escritorio vacío (regresión del foco-al-shell).
                let churn = self
                    .menu_opened_at
                    .is_some_and(|t| t.elapsed() < MENU_LEAVE_GRACE);
                if !churn {
                    self.set_menu_open(false);
                }
            }
            // Perder el foco de teclado cierra el completado flotante (ya no se
            // tipea), con la misma guarda anti-churn contra el `leave` espurio del
            // reacomodo de foco al aparecer.
            if self.shuma_panel == Some(pi) && self.completion_open {
                let churn = self
                    .completion_opened_at
                    .is_some_and(|t| t.elapsed() < MENU_LEAVE_GRACE);
                // …pero si la surface alta está sosteniendo un input de varias
                // filas, encogerla acá lo recortaría a la mitad de una frase que
                // el usuario todavía no mandó. El texto no se va con el foco.
                if !churn && self.shuma_input_filas() <= 1 {
                    self.shuma.inner.close_completion();
                    self.set_completion_open(false);
                }
            }
            // Perder el foco de teclado es un gesto liviano: repliega el
            // **vistazo** pero NO el modo firme (acaparador: sigues leyendo la
            // salida aunque el foco se vaya un instante).
            if self.shuma_panel == Some(pi)
                && self.shuma.open
                && self.shuma.open_mode.cierra_por_gesto_liviano()
            {
                self.set_shuma_open(None);
            }
            // Con la barra plegada (sólo el input en la barra), perder el teclado
            // DESENFOCA el input: apaga el cue de foco (caret + marco brillante).
            // Cubre el caso teclado-puro (Alt+Tab a otra ventana sin mover el
            // mouse), donde no llega `on_pointer_leave`. Si el drawer sigue
            // abierto (firme), respetamos su foco acaparador y no tocamos nada.
            if self.shuma_panel == Some(pi) && !self.shuma.open {
                if self.shuma_full.is_some() {
                    // El borrow de `shuma_full` termina en este `let` (produce un
                    // `Msg` propio), así `apply_shuma_full` puede tomar `&mut self`.
                    let m = self
                        .shuma_full
                        .as_ref()
                        .and_then(crate::shuma_app::blur_active_input);
                    if let Some(m) = m {
                        self.apply_shuma_full(vec![m]);
                    }
                } else {
                    self.shuma.inner = shuma_module_shell::update(
                        self.shuma.inner.clone(),
                        shuma_module_shell::Msg::BlurInput,
                    );
                }
                self.marcar_shuma_dirty();
            }
        }
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: smithay_client_toolkit::seat::keyboard::KeyEvent,
    ) {
        // Cualquier tecla es input real → re-arma el watchdog anti-atasco del drawer.
        self.toca_shuma_watchdog();
        // AUTO-REPEAT CLIENTE: en layer-shell el compositor manda UN press y UN
        // release — mantener una tecla apretada NO repite sola; repetirla es
        // trabajo del cliente (con el delay/rate del `repeat_info` del seat).
        // Estampamos la tecla sostenida; `draw` bombea (`pump_key_repeat`) y
        // `release_key`/KB-leave la sueltan. Los modificadores no repiten.
        if !es_keysym_modificador(event.keysym) {
            if let Some((delay_ms, _)) = self.repeat_params() {
                self.key_held = Some(super::HeldKey {
                    event: event.clone(),
                    next_at: std::time::Instant::now()
                        + std::time::Duration::from_millis(delay_ms),
                });
            }
        }
        self.route_key_event(event);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: smithay_client_toolkit::seat::keyboard::KeyEvent,
    ) {
        // Soltar la tecla sostenida corta el auto-repeat. Comparamos por
        // `raw_code`: si el usuario encadenó press(a) press(b) release(a),
        // la sostenida es `b` y el release de `a` no debe cortarla.
        if self
            .key_held
            .as_ref()
            .is_some_and(|h| h.event.raw_code == event.raw_code)
        {
            self.key_held = None;
        }
    }

    fn update_repeat_info(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        info: smithay_client_toolkit::seat::keyboard::RepeatInfo,
    ) {
        self.repeat_info = Some(info);
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: u32,
    ) {
        self.mods = modifiers;
    }
}

/// ¿El keysym es un modificador/lock? Ésos nunca auto-repiten (mantener Shift
/// no debe generar una ráfaga de eventos Shift).
fn es_keysym_modificador(sym: Keysym) -> bool {
    matches!(
        sym,
        Keysym::Shift_L
            | Keysym::Shift_R
            | Keysym::Control_L
            | Keysym::Control_R
            | Keysym::Alt_L
            | Keysym::Alt_R
            | Keysym::Meta_L
            | Keysym::Meta_R
            | Keysym::Super_L
            | Keysym::Super_R
            | Keysym::Hyper_L
            | Keysym::Hyper_R
            | Keysym::Caps_Lock
            | Keysym::Shift_Lock
            | Keysym::Num_Lock
            | Keysym::Scroll_Lock
            | Keysym::ISO_Level3_Shift
            | Keysym::ISO_Level5_Shift
    )
}

impl LayerApp {
    /// `(delay_ms, intervalo_ms)` efectivos del auto-repeat, del `repeat_info`
    /// del seat. `None` = el seat pidió NO repetir. Sin anuncio todavía, caemos
    /// a 600 ms / 25 Hz (lo que configura mirada).
    pub(super) fn repeat_params(&self) -> Option<(u64, u64)> {
        use smithay_client_toolkit::seat::keyboard::RepeatInfo;
        match self.repeat_info {
            Some(RepeatInfo::Disable) => None,
            Some(RepeatInfo::Repeat { rate, delay }) => {
                Some((delay as u64, (1000 / rate.get().max(1)) as u64))
            }
            None => Some((600, 40)),
        }
    }

    /// Bombea el auto-repeat: si hay tecla sostenida y ya venció `next_at`,
    /// re-rutea el evento guardado (idéntico a un press real) y agenda la
    /// próxima repetición. Lo llama `draw` — que mientras haya tecla sostenida
    /// mantiene el panel dirty, así los frame-callbacks llegan a ritmo de
    /// frame y no al piso de 500 ms del compositor. Una repetición por draw
    /// como máximo: si nos quedamos atrás (stall), re-sincroniza sin ráfaga.
    pub(super) fn pump_key_repeat(&mut self) {
        let Some((_, intervalo_ms)) = self.repeat_params() else {
            self.key_held = None;
            return;
        };
        let intervalo = std::time::Duration::from_millis(intervalo_ms.max(10));
        let now = std::time::Instant::now();
        // Se disparan TODAS las repeticiones vencidas, no una por draw.
        //
        // El pump corre dentro de `draw`, y el draw va al ritmo de los
        // frame-callbacks — que en reposo es el piso del compositor (~500 ms),
        // no los 40 ms del rate del teclado. Con una repetición por draw,
        // sostener una flecha avanzaba un par de caracteres por segundo en vez
        // de veinticinco: la melaza que se sentía al navegar el input.
        //
        // El tope evita la ráfaga si el cliente estuvo trabado un rato largo:
        // más allá de eso se re-sincroniza y sigue desde ahora.
        const MAX_POR_PUMP: u32 = 8;
        let mut disparadas = 0u32;
        while disparadas < MAX_POR_PUMP {
            let Some(h) = self.key_held.as_mut() else { return };
            if now < h.next_at {
                return;
            }
            h.next_at += intervalo;
            let ev = h.event.clone();
            disparadas += 1;
            self.route_key_event(ev);
        }
        // Quedamos atrás del reloj tras agotar el tope: re-sincronizar.
        if let Some(h) = self.key_held.as_mut() {
            if now > h.next_at {
                h.next_at = now + intervalo;
            }
        }
    }

    /// Rutea un press de teclado (real o repetido) al interceptor que
    /// corresponda (polkit / red / khipu / menú / buscador / RAG / shuma).
    /// Es el cuerpo histórico de `press_key`, factorizado para que el
    /// auto-repeat re-entre por el mismo camino.
    fn route_key_event(
        &mut self,
        event: smithay_client_toolkit::seat::keyboard::KeyEvent,
    ) {
        // DIAG: qué interceptor se va a comer la tecla (o ninguna → shuma).
        if crate::layer::diag_on() {
            eprintln!(
                "pata·press_key sym={:?} rutas: polkit={} net={} khipu={} menu={} navsearch={} rag={} full={} open={}",
                event.keysym,
                self.polkit_prompt.is_some(),
                self.net_password.is_some(),
                self.menu_open && self.menu_kind == super::MenuKind::Khipu,
                self.menu_open,
                self.nav.search_focused && !self.nav.open.is_empty(),
                self.rag_panel_open(),
                self.shuma_full.is_some(),
                self.shuma.open,
            );
        }
        // Diálogo de polkit (modal): el teclado va al campo de contraseña.
        if self.polkit_prompt.is_some() {
            match event.keysym {
                Keysym::Escape => self.handle_msg(crate::Msg::PolkitCancel),
                Keysym::BackSpace => self.handle_msg(crate::Msg::PolkitBackspace),
                Keysym::Return | Keysym::KP_Enter => self.handle_msg(crate::Msg::PolkitSubmit),
                _ => {
                    if let Some(txt) = event.utf8 {
                        for c in txt.chars().filter(|c| !c.is_control()) {
                            self.handle_msg(crate::Msg::PolkitChar(c));
                        }
                    }
                }
            }
            return;
        }
        // Entrada de contraseña Wi-Fi: el teclado va al campo (Enter conecta, Esc
        // cancela, Backspace borra). Se evalúa antes que el buscador del menú.
        if self.net_password.is_some() {
            match event.keysym {
                Keysym::Escape => self.handle_msg(crate::Msg::NetworkPasswordCancel),
                Keysym::BackSpace => self.handle_msg(crate::Msg::NetworkPasswordBackspace),
                Keysym::Return | Keysym::KP_Enter => {
                    self.handle_msg(crate::Msg::NetworkPasswordSubmit)
                }
                _ => {
                    if let Some(txt) = event.utf8 {
                        for c in txt.chars().filter(|c| !c.is_control()) {
                            self.handle_msg(crate::Msg::NetworkPasswordChar(c));
                        }
                    }
                }
            }
            return;
        }
        // Diálogo Khipu abierto: el teclado va al borrador de la nota (Enter
        // anota, Esc cierra, Backspace borra). Antes que el buscador del menú
        // —el diálogo Khipu también pone `menu_open`—.
        if self.menu_open && self.menu_kind == super::MenuKind::Khipu {
            match event.keysym {
                Keysym::Escape => self.handle_msg(crate::Msg::KhipuPanel),
                Keysym::BackSpace => self.handle_msg(crate::Msg::KhipuBackspace),
                Keysym::Return | Keysym::KP_Enter => self.handle_msg(crate::Msg::KhipuSubmit),
                _ => {
                    if let Some(txt) = event.utf8 {
                        for c in txt.chars().filter(|c| !c.is_control()) {
                            self.handle_msg(crate::Msg::KhipuChar(c));
                        }
                    }
                }
            }
            return;
        }
        // Menú de inicio abierto: el teclado va al buscador; las flechas mueven
        // la selección (Enter lanza la app resaltada — sin selección, la 1ª).
        if self.menu_open {
            match event.keysym {
                Keysym::Escape => self.set_menu_open(false),
                Keysym::BackSpace => {
                    self.menu_query.pop();
                    self.menu_sel = 0;
                    self.menu_scroll = 0.0;
                    self.marcar_menu_dirty();
                }
                Keysym::Return | Keysym::KP_Enter => self.lanzar_seleccionado_menu(),
                Keysym::Up | Keysym::KP_Up => self.mover_sel_menu(0, -1),
                Keysym::Down | Keysym::KP_Down => self.mover_sel_menu(0, 1),
                Keysym::Left | Keysym::KP_Left => self.mover_sel_menu(-1, 0),
                Keysym::Right | Keysym::KP_Right => self.mover_sel_menu(1, 0),
                _ => {
                    if let Some(txt) = event.utf8 {
                        if !txt.is_empty() && !txt.chars().any(|c| c.is_control()) {
                            self.menu_query.push_str(&txt);
                            self.menu_sel = 0;
                            self.menu_scroll = 0.0;
                            self.marcar_menu_dirty();
                        }
                    }
                }
            }
            return;
        }
        // Buscador jerárquico del sidebar con foco: el teclado va a `nav.search`
        // (localiza contenido en el panel activo). Se evalúa ANTES del panel RAG
        // para que, aun con un diente RAG abierto, tipear en el buscador del marco
        // gane sobre la consulta semántica. Esc suelta el foco y limpia.
        if self.nav.search_focused && !self.nav.open.is_empty() {
            match event.keysym {
                Keysym::Escape => self.handle_msg(crate::Msg::SearchClear),
                Keysym::BackSpace => self.handle_msg(crate::Msg::SearchBackspace),
                Keysym::Return | Keysym::KP_Enter => self.handle_msg(crate::Msg::SearchFocus(false)),
                _ => {
                    if let Some(txt) = event.utf8 {
                        for c in txt.chars().filter(|c| !c.is_control()) {
                            self.handle_msg(crate::Msg::SearchChar(c));
                        }
                    }
                }
            }
            return;
        }
        // Panel RAG abierto: el teclado va a su buscador (texto → consulta, Enter
        // pregunta, Esc cierra el panel, Backspace borra).
        if self.rag_panel_open() {
            match event.keysym {
                Keysym::Escape => self.cerrar_sidebar(),
                Keysym::BackSpace => self.handle_msg(crate::Msg::RagBackspace),
                Keysym::Return | Keysym::KP_Enter => self.handle_msg(crate::Msg::RagSubmit),
                _ => {
                    if let Some(txt) = event.utf8 {
                        for c in txt.chars().filter(|c| !c.is_control()) {
                            self.handle_msg(crate::Msg::RagChar(c));
                        }
                    }
                }
            }
            return;
        }
        // Reparto de un terminal de verdad: **Ctrl+Shift+Q cierra la "ventana"**
        // (aquí: repliega el drawer) y **Ctrl+Shift+W cierra la PESTAÑA**. Antes
        // pata se quedaba con la W y el `CloseTab` de la capa universal de shuma
        // no llegaba nunca — el atajo "no hacía nada" (reporte del 24-jul).
        // La W sigue replegando en el path BARE (una sola sesión, sin pestañas
        // que cerrar), así que ningún modo queda sin salida deliberada.
        let cierre_deliberado = self.mods.ctrl
            && self.mods.shift
            && (matches!(event.keysym, Keysym::q | Keysym::Q)
                || (self.shuma_full.is_none()
                    && matches!(event.keysym, Keysym::w | Keysym::W)));
        if self.shuma.open && cierre_deliberado {
            self.set_shuma_open(None);
            return;
        }
        // Escape repliega el drawer (acción deliberada, cierra fugaz y firme) —
        // salvo que haya un PTY interactivo vivo (claude/vim/less), que necesita
        // el Esc (claude lo usa a cada rato para cancelar); ahí se lo dejamos y
        // la tecla cae al shell más abajo. Antes el gate era sólo alt-screen
        // (`is_fullscreen_tui`) y un Esc dentro de claude —que renderiza inline—
        // te desaparecía el cuerpo entero. Cerrar con PTY vivo = Ctrl+Shift+Q.
        if self.shuma.open
            && matches!(event.keysym, Keysym::Escape)
            && self.shuma_full.is_none()
            && !self.shuma.inner.tiene_pty_vivo()
        {
            self.set_shuma_open(None);
            return;
        }
        // OJO: NO descartamos las teclas con el drawer plegado. `press_key` sólo
        // llega cuando la barra TIENE el foco de teclado (clic, o el fallback de
        // mirada en escritorio vacío); en ese caso ruteamos las teclas a shuma
        // para poder tipear en la barra sin abrir el drawer. Enter despliega el
        // drawer para ver la salida del comando.
        // Enter pide salida → despliega **firme** (y escala un vistazo previo a
        // firme si ya estaba abierto). El cierre firme es sólo deliberado.
        let abrir_por_enter = matches!(event.keysym, Keysym::Return | Keysym::KP_Enter);
        // Si el Enter lanza una app, no desplegamos el drawer: la app abre en su
        // ventana y el Quake queda plegado ("caleta"). La sesión igual registra el
        // lanzamiento como comando.
        let mut lanzo_app = false;
        if let Some(ke) = self.keysym_to_keyevent(&event) {
            if self.shuma_full.is_some() {
                // Esc repliega el drawer salvo que la shuma tenga algo propio que
                // descartar (modal/dropdown/campo), corra una TUI de pantalla
                // completa, o haya un PTY interactivo vivo (claude renderiza
                // inline y usa Esc a cada rato) — en todos esos casos el Esc es
                // de ella. Cerrar con PTY vivo = Ctrl+Shift+Q.
                if matches!(event.keysym, Keysym::Escape)
                    && !self.shuma_pty_vivo()
                    && self
                        .shuma_full
                        .as_ref()
                        .is_some_and(crate::shuma_app::escape_closes_drawer)
                {
                    self.set_shuma_open(None);
                    self.marcar_shuma_dirty();
                    return;
                }
                // Las apps deben estar en la sesión activa ANTES de tipear, para
                // que la primera tecla ya ofrezca candidatos-app (tier 0) en el
                // completado — espejo del asegurar_apps del path bare de abajo.
                self.asegurar_apps_full();
                // Live-wire: la tecla la traduce la shuma completa según su foco
                // interno (input de la sesión activa / PTY-TUI / rails).
                let m = self
                    .shuma_full
                    .as_ref()
                    .and_then(|f| crate::shuma_app::on_key(f, &ke));
                if crate::layer::diag_on() {
                    // La sonda de atajos revela el chord que shuma computa de ESTA
                    // tecla (con los modificadores que REALMENTE le llegan) y si
                    // matchea un bind — el desempate del «sólo sirve Ctrl+Shift+C»:
                    // si el chord sale sin Ctrl/Shift, el modificador no llega; si
                    // sale completo pero «SIN BIND», es el perfil; si matchea, el
                    // atajo dispara y el problema está aguas abajo.
                    let sonda = self
                        .shuma_full
                        .as_ref()
                        .map(|f| crate::shuma_app::diag_shortcut(f, &ke))
                        .unwrap_or_else(|| "atajo: (sin shuma_full)".to_string());
                    eprintln!(
                        "pata·shuma key={:?} ctrl={} shift={} alt={} → on_key={} · {sonda}",
                        ke.key,
                        self.mods.ctrl,
                        self.mods.shift,
                        self.mods.alt,
                        if m.is_some() { "Some(msg)" } else { "None" }
                    );
                }
                if let Some(m) = m {
                    self.apply_shuma_full(vec![m]);
                }
                // Igual que el path bare: tipear en la barra (drawer plegado) puede
                // abrir/cerrar el completado flotante — reconcilia su surface con la
                // sesión activa del modelo full.
                self.reconcile_completion();
            } else {
                // Las apps deben estar en el módulo ANTES de tipear, para que la
                // primera tecla ya ofrezca candidatos-app en el completado.
                self.asegurar_apps();
                self.shuma.inner = shuma_module_shell::update(
                    self.shuma.inner.clone(),
                    shuma_module_shell::Msg::Key(ke),
                );
                // Tipear en la barra fina (drawer plegado) puede abrir/cerrar el
                // completado: reconcilia la surface flotante autónoma con el
                // estado del input, y drena un posible lanzamiento de app (Enter
                // sobre un candidato-app).
                self.reconcile_completion();
                lanzo_app = self.drenar_app_launch();
            }
        }
        // Enter despliega el drawer para ver la salida — salvo que haya lanzado
        // una app (queda plegado; la app abre en su ventana), o que un PTY
        // interactivo ya esté consumiendo el teclado (claude/vim): ese Enter fue
        // PARA el programa, no un submit — re-abrir aquí re-dimensionaba la
        // surface en cada Enter dentro del TUI.
        if crate::layer::diag_on() && abrir_por_enter {
            eprintln!(
                "pata·enter lanzo_app={lanzo_app} open={} pty_vivo={}",
                self.shuma.open,
                self.shuma_pty_vivo(),
            );
        }
        if abrir_por_enter && !lanzo_app && !(self.shuma.open && self.shuma_pty_vivo()) {
            self.set_shuma_open(Some(crate::shuma::OpenMode::Firme));
        }
        self.marcar_shuma_dirty();
    }
}

impl PointerHandler for LayerApp {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        // Actividad de puntero (entrar/mover/click) es input real que le llega a
        // pata → re-arma el watchdog anti-atasco del drawer (no-op si está cerrado).
        if !events.is_empty() {
            self.toca_shuma_watchdog();
        }
        for e in events {
            // Memorizar el ÚLTIMO panel que vio el puntero: es el ancla de "dónde
            // está el usuario" para abrir el drawer en el monitor correcto (la
            // esquina caliente/ToggleShuma llega por socket, sin coordenadas).
            if matches!(
                e.kind,
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. }
            ) {
                if let Some(pi) = self.panel_de(&e.surface) {
                    self.ultimo_panel_puntero = Some(pi);
                }
            }
            // Reentrar (Enter/Motion) a CUALQUIER surface del sidebar cancela un
            // cierre-por-flota diferido: así mover el puntero panel↔dientes no lo cierra.
            if self.flota_close_at.is_some()
                && matches!(
                    e.kind,
                    PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. }
                )
            {
                if let Some(pi) = self.panel_de(&e.surface) {
                    self.flota_cancelar_cierre(self.panels[pi].idx);
                }
            }
            match e.kind {
                PointerEventKind::Motion { .. } => {
                    // `on_click` armado: si el puntero se alejó del origen del
                    // press más que el umbral, fue un arrastre/barrido → cancelar
                    // el click (no se disparará al soltar).
                    if let Some((_, _, (ox, oy))) = self.pending_click.as_ref() {
                        let (dx, dy) = (e.position.0 as f32 - ox, e.position.1 as f32 - oy);
                        if (dx * dx + dy * dy).sqrt() > CLICK_MOVE_CANCEL {
                            self.pending_click = None;
                        }
                    }
                    // Drag en curso: el delta va al handler del nodo.
                    if self.drag.is_some() {
                        let (px, py) = (e.position.0 as f32, e.position.1 as f32);
                        let (handler, last) = {
                            let d = self.drag.as_ref().unwrap();
                            (d.handler.clone(), d.last)
                        };
                        if let Some(d) = self.drag.as_mut() {
                            d.last = (px, py);
                        }
                        if let Some(msg) = (handler)(DragPhase::Move, px - last.0, py - last.1) {
                            self.handle_msg(msg);
                        }
                        continue;
                    }
                    if let Some(pi) = self.panel_de(&e.surface) {
                        let (px, py) = (e.position.0 as f32, e.position.1 as f32);
                        // Ancla candidata del próximo menú: la última x del puntero
                        // sobre una BARRA (se estampa al ABRIR, ver `toggle_menu`) —
                        // así el diálogo de un icono se posa debajo del icono, no al
                        // centro. Sólo barras horizontales (el menú cuelga de ellas;
                        // la x local de un sidebar/dock no significa nada ahí).
                        {
                            let si = self.panels[pi].idx;
                            if self.cfg.surfaces.get(si).map(|s| s.kind)
                                == Some(pata_core::config::SurfaceKind::Bar)
                            {
                                self.pointer_ultimo_x = Some(px);
                            }
                        }
                        // AUTOHIDE: el puntero tocó la surface del rail (por la franja
                        // caliente si estaba oculto) → revelarlo entero.
                        if !self.panels[pi].drawer {
                            let si = self.panels[pi].idx;
                            if self.cfg.surfaces.get(si).map(|s| s.autohide).unwrap_or(false) {
                                self.revelar_sidebar(si);
                            }
                        }
                        // Tooltip por hit-test PROPIO (nodos con `tooltip`), no el del
                        // hover de `hover_fill`: los iconos fugaces (y cualquier nodo
                        // pintado a mano) declaran tooltip sin hover_fill, y con el
                        // predicado viejo su tooltip jamás aparecía.
                        let tip = self.panels[pi]
                            .cache
                            .as_ref()
                            .and_then(|c| hit_test_tooltip(&c.mounted, &c.computed, px, py));
                        if self.tooltip_nodo != tip.map(|i| (pi, i)) {
                            self.tooltip_nodo = tip.map(|i| (pi, i));
                            self.update_tooltip(pi, tip, qh);
                        }
                        let nuevo = self.panels[pi]
                            .cache
                            .as_ref()
                            .and_then(|c| hit_test_hover(&c.mounted, &c.computed, px, py));
                        if self.panels[pi].hover_idx != nuevo {
                            let viejo = self.panels[pi].hover_idx;
                            self.panels[pi].hover_idx = nuevo;
                            self.panels[pi].dirty = true;
                            // Despachar on_pointer_leave del nodo que abandonamos y
                            // on_pointer_enter del recién hovereado — habilita los
                            // submenús al hover (categorías → apps del menú inicio).
                            let leave = viejo.and_then(|i| {
                                self.panels[pi].cache.as_ref().and_then(|c| {
                                    c.mounted.nodes.get(i).and_then(|n| n.on_pointer_leave.clone())
                                })
                            });
                            let enter = nuevo.and_then(|i| {
                                self.panels[pi].cache.as_ref().and_then(|c| {
                                    c.mounted.nodes.get(i).and_then(|n| n.on_pointer_enter.clone())
                                })
                            });
                            // Marcamos que estos despachos vienen de **hover**: el
                            // ruteo de `FocusInput` enfoca pero NO abre el drawer
                            // (sólo el click abre). Ver `LayerApp::hover_dispatch`.
                            self.hover_dispatch = true;
                            if let Some(m) = leave {
                                self.handle_msg(m);
                            }
                            if let Some(m) = enter {
                                self.handle_msg(m);
                            }
                            self.hover_dispatch = false;
                        }
                        // Dock: la lupa sigue al puntero — re-render por cada
                        // motion guardando la X local del panel.
                        let idx = self.panels[pi].idx;
                        if self.cfg.surfaces[idx].kind == pata_core::config::SurfaceKind::Dock
                            && self.panels[pi].cursor_x != Some(px)
                        {
                            self.panels[pi].cursor_x = Some(px);
                            self.panels[pi].dirty = true;
                        }
                    }
                    continue;
                }
                PointerEventKind::Leave { .. } => {
                    // El puntero salió de la superficie: cancela cualquier click armado.
                    self.pending_click = None;
                    if let Some(pi) = self.panel_de(&e.surface) {
                        // Al salir de la superficie ENTERA no hay un `Motion` de
                        // borde, así que el `on_pointer_leave` del nodo hovereado
                        // nunca se emitiría — y quien lo escuche (los controles
                        // fantasma) quedaría «pegado» visible. Emitilo aquí antes de
                        // limpiar el hover. Espeja el despacho de leave del Motion.
                        let leave = self.panels[pi].hover_idx.and_then(|i| {
                            self.panels[pi].cache.as_ref().and_then(|c| {
                                c.mounted.nodes.get(i).and_then(|n| n.on_pointer_leave.clone())
                            })
                        });
                        if self.panels[pi].hover_idx.is_some() {
                            self.panels[pi].hover_idx = None;
                            self.panels[pi].dirty = true;
                        }
                        if let Some(m) = leave {
                            self.hover_dispatch = true;
                            self.handle_msg(m);
                            self.hover_dispatch = false;
                        }
                        // El dock vuelve a reposo cuando el puntero se va.
                        if self.panels[pi].cursor_x.is_some() {
                            self.panels[pi].cursor_x = None;
                            self.panels[pi].dirty = true;
                        }
                        let si = self.panels[pi].idx;
                        let es_drawer = self.panels[pi].drawer;
                        // AUTOHIDE: el puntero abandonó el rail → re-ocultarlo (salvo que
                        // su drawer esté desplegado, que lo mantiene a la vista).
                        if !es_drawer
                            && self.cfg.surfaces.get(si).map(|s| s.autohide).unwrap_or(false)
                        {
                            self.ocultar_sidebar(si);
                        }
                        // FLOTA: al salir el puntero de CUALQUIER surface del sidebar
                        // (rail o panel), NO se cierra de una — el puntero puede estar
                        // yendo de una surface a la otra (panel↔dientes). Se arma un
                        // cierre DIFERIDO; cualquier reentrada a rail/panel lo cancela
                        // (`flota_cancelar_cierre`) y `finalize_flota_close` lo confirma
                        // si no vuelve tras la gracia. Con la misma guarda anti-churn (el
                        // `leave` espurio del reacomodo de foco al abrirlo).
                        {
                            let docked = self
                                .cfg
                                .surfaces
                                .get(si)
                                .map(|s| s.reserve.unwrap_or(self.sidebar_docked))
                                .unwrap_or(true);
                            let churn = self
                                .drawer_opened_at
                                .is_some_and(|t| t.elapsed() < MENU_LEAVE_GRACE);
                            let out = self.panel_out_key(pi);
                            if !docked
                                && self.drawers_mostrados.contains(&(si, out.clone()))
                                && !churn
                            {
                                self.flota_close_at =
                                    Some((si, out, std::time::Instant::now()));
                            }
                        }
                        if es_drawer {
                            let churn = self
                                .drawer_opened_at
                                .is_some_and(|t| t.elapsed() < MENU_LEAVE_GRACE);
                            // La VENTANITA DE OPCIONES (control de disposición) es un
                            // popover transitorio: se cierra al perder el foco como un
                            // menú, en Fijo o Flota, haya o no diente desplegado. Con la
                            // misma gracia anti-churn (ignorá el `leave` espurio del
                            // reacomodo de input-region al abrirla). Al cerrarse, si el
                            // drawer se mostraba SÓLO por ella, `reconcile_drawer` lo
                            // repliega en el próximo draw.
                            if self.nav.control_open && !churn {
                                self.nav.control_open = false;
                                self.nav.control_si = None;
                                self.marcar_sidebars_dirty();
                            }
                        }
                        // Deshover (gesto liviano): el puntero abandonó la superficie
                        // de shuma → repliega SÓLO el **vistazo** (el modo firme se
                        // queda: estás leyendo la salida). Con guarda anti-churn:
                        // ignorá el `leave` espurio del reacomodo de foco al abrir.
                        if self.shuma_panel == Some(pi)
                            && self.shuma.open
                            && self.shuma.open_mode.cierra_por_gesto_liviano()
                        {
                            let churn = self
                                .shuma_opened_at
                                .is_some_and(|t| t.elapsed() < MENU_LEAVE_GRACE);
                            if !churn {
                                self.set_shuma_open(None);
                            }
                        }
                    }
                    self.tooltip_nodo = None;
                    self.hide_tooltip(qh);
                    continue;
                }
                _ => {}
            }
            // Rueda sobre el historial del drawer.
            if let PointerEventKind::Axis { vertical, .. } = e.kind {
                let dy = if vertical.discrete != 0 {
                    vertical.discrete as f32
                } else {
                    vertical.absolute as f32 / 20.0
                };
                if dy != 0.0 {
                    let (px, py) = (e.position.0 as f32, e.position.1 as f32);
                    // **Ctrl+rueda = zoom del canvas**, como en cualquier terminal.
                    // Va ANTES del hit-test de scroll: si no, la rueda se la lleva
                    // el historial y el zoom no llega nunca (por eso funcionaba en
                    // la shuma suelta —que sí tiene `on_wheel`— y no en el drawer).
                    // `dy` ya viene en la convención de llimphi (positivo = hacia
                    // abajo = achica), la misma que usa `shuma::on_wheel`.
                    let sobre_shuma = self
                        .panel_de(&e.surface)
                        .is_some_and(|pi| self.shuma_panel == Some(pi));
                    if self.mods.ctrl && self.shuma.open && sobre_shuma {
                        let delta = llimphi_ui::WheelDelta { x: 0.0, y: dy };
                        let mods = llimphi_ui::Modifiers {
                            shift: self.mods.shift,
                            ctrl: true,
                            alt: self.mods.alt,
                            meta: self.mods.logo,
                        };
                        match self.shuma_full.as_ref() {
                            // Live-wire: shuma decide sobre su sesión activa.
                            Some(full) => {
                                if let Some(m) = crate::shuma_app::on_wheel(full, delta, (px, py), mods) {
                                    self.apply_shuma_full(vec![m]);
                                }
                            }
                            // Path bare: el mismo factor que aplica shuma.
                            None => {
                                let factor = crate::shuma_app::zoom_factor_de_rueda(dy);
                                self.shuma.inner = shuma_module_shell::update(
                                    self.shuma.inner.clone(),
                                    shuma_module_shell::Msg::ZoomBy(factor),
                                );
                                self.marcar_shuma_dirty();
                            }
                        }
                        continue;
                    }
                    if let Some(pi) = self.panel_de(&e.surface) {
                        let msg = self.panels[pi].cache.as_ref().and_then(|c| {
                            hit_test_scroll(&c.mounted, &c.computed, px, py)
                                .and_then(|i| c.mounted.nodes.get(i))
                                .and_then(|n| n.on_scroll.as_ref().and_then(|h| h(0.0, dy)))
                        });
                        if let Some(msg) = msg {
                            self.handle_msg(msg);
                        }
                    }
                }
                continue;
            }
            // Soltar el botón izquierdo termina un drag en curso.
            if let PointerEventKind::Release { button, .. } = e.kind {
                if button == BTN_LEFT {
                    if let Some(d) = self.drag.take() {
                        if let Some(msg) = (d.handler)(DragPhase::End, 0.0, 0.0) {
                            self.handle_msg(msg);
                        }
                    } else if let Some((_, msg, _)) = self.pending_click.take() {
                        // `on_click` armado en el press y no cancelado por
                        // movimiento → este es el click real, al soltar.
                        self.handle_msg(msg);
                    }
                }
                continue;
            }
            if let PointerEventKind::Press { button, .. } = e.kind {
                if button != BTN_LEFT && button != BTN_RIGHT && button != BTN_MIDDLE {
                    continue;
                }
                let Some(pi) = self.panel_de(&e.surface) else {
                    continue;
                };
                // Multi-monitor: si el press cae sobre la barra de ESTE monitor,
                // hazla la activa para el drawer de shuma Y para el menú de inicio
                // — así ambos popups se despliegan en la pantalla que estás usando,
                // no en la primera barra creada. Ídem el sidebar: un press sobre
                // su rail/drawer hace de este monitor el dueño (el drawer se
                // expande aquí, no espejado en todas las pantallas).
                self.focus_shuma_panel(pi);
                self.focus_menu_panel(pi);
                self.focus_sidebar_panel(pi);
                let (px, py) = (e.position.0 as f32, e.position.1 as f32);
                let derecho = button == BTN_RIGHT;
                let medio = button == BTN_MIDDLE;
                let izquierdo = button == BTN_LEFT;
                // Nodo arrastrable bajo el press (sólo botón IZQUIERDO): arranca un
                // drag. El clic medio/derecho nunca inician arrastre.
                if izquierdo {
                    // `drag_at` + `on_click_at` COEXISTEN (misma semántica que el
                    // runtime de llimphi-ui): el press dispara el click posicional
                    // (si está) y arranca un drag que recuerda el ancla local
                    // `(lx0, ly0)` — es el select-on-press + drag-to-extend del
                    // input de shuma. `drag_at` gana sobre `drag` si conviven.
                    let at = self.panels[pi].cache.as_ref().and_then(|c| {
                        let i = hit_test_click(&c.mounted, &c.computed, px, py)?;
                        let n = c.mounted.nodes.get(i)?;
                        let h = n.drag_at.clone()?;
                        let r = c.computed.get(n.id)?;
                        let click = n.on_click_at.as_ref().and_then(|f| {
                            f(px - r.x, py - r.y, r.w, r.h)
                        });
                        Some((h, px - r.x, py - r.y, click))
                    });
                    if let Some((h, lx0, ly0, click)) = at {
                        if let Some(msg) = click {
                            self.handle_msg(msg);
                        }
                        self.drag = Some(LayerDrag {
                            handler: std::sync::Arc::new(move |fase, dx, dy| {
                                h(fase, dx, dy, lx0, ly0)
                            }),
                            last: (px, py),
                        });
                        continue;
                    }
                    let handler = self.panels[pi].cache.as_ref().and_then(|c| {
                        let i = hit_test_click(&c.mounted, &c.computed, px, py)?;
                        c.mounted.nodes.get(i)?.drag.clone()
                    });
                    if let Some(handler) = handler {
                        self.drag = Some(LayerDrag { handler, last: (px, py) });
                        continue;
                    }
                }
                // Diagnóstico del click (PATA_DIAG=1): qué nodo cae bajo el
                // press y si tiene handler. Para depurar "no hace nada".
                if crate::layer::diag_on() {
                    let info = self.panels[pi].cache.as_ref().map(|c| {
                        match hit_test_click(&c.mounted, &c.computed, px, py) {
                            Some(i) => {
                                let n = &c.mounted.nodes[i];
                                format!(
                                    "nodo {i} on_click={} on_click_at={} drag={}",
                                    n.on_click.is_some(),
                                    n.on_click_at.is_some(),
                                    n.drag.is_some()
                                )
                            }
                            None => "ningún nodo clickeable".into(),
                        }
                    });
                    eprintln!(
                        "pata diag · PRESS panel={pi} pos=({px:.0},{py:.0}) der={derecho} → {}",
                        info.unwrap_or_else(|| "sin cache".into())
                    );
                }
                // Para el clic IZQUIERDO con `on_click` plano NO disparamos en el
                // press: armamos el click y lo disparamos al RELEASE (semántica
                // de escritorio), salvo que el puntero se aleje del origen. Lo
                // posicional (`on_click_at`) y el clic derecho/medio sí van en el
                // press (gestos de press por diseño).
                if izquierdo {
                    let armado = self.panels[pi].cache.as_ref().and_then(|c| {
                        let i = hit_test_click(&c.mounted, &c.computed, px, py)?;
                        c.mounted.nodes.get(i)?.on_click.clone()
                    });
                    if let Some(msg) = armado {
                        self.pending_click = Some((pi, msg, (px, py)));
                        continue;
                    }
                }
                let msg = self.panels[pi].cache.as_ref().and_then(|c| {
                    // Cada botón usa su hit-test dedicado: el derecho busca el nodo
                    // con `on_right_click*`, el medio el de `on_middle_click`, el
                    // izquierdo el clickeable. Antes los tres usaban el predicado
                    // izquierdo, así que un contenedor con menú contextual quedaba
                    // TAPADO por un hijo con `drag`/`on_click` (p. ej. el
                    // `block_surface` del output) y el right/middle click no hallaba
                    // su handler — el menú y el pegar por botón medio no salían.
                    let i = if derecho {
                        hit_test_right_click(&c.mounted, &c.computed, px, py)?
                    } else if medio {
                        hit_test_middle_click(&c.mounted, &c.computed, px, py)?
                    } else {
                        hit_test_click(&c.mounted, &c.computed, px, py)?
                    };
                    let n = c.mounted.nodes.get(i)?;
                    if derecho {
                        // `on_right_click_screen` (coords absolutas, para menús al
                        // puntero) gana; luego el plano; luego `on_right_click_at` local.
                        if let Some(screen) = n.on_right_click_screen.as_ref() {
                            let r = c.computed.get(n.id)?;
                            if let Some(m) = screen(px, py, r.w, r.h) {
                                return Some(m);
                            }
                        }
                        if let Some(m) = n.on_right_click.clone() {
                            return Some(m);
                        }
                        let at = n.on_right_click_at.as_ref()?;
                        let r = c.computed.get(n.id)?;
                        at(px - r.x, py - r.y, r.w, r.h)
                    } else if medio {
                        // Clic medio: sólo nodos con `on_middle_click` reaccionan
                        // (mismo modelo que el derecho).
                        n.on_middle_click.clone()
                    } else {
                        // Izquierdo sin `on_click` plano: lo posicional va en el press.
                        let at = n.on_click_at.as_ref()?;
                        let r = c.computed.get(n.id)?;
                        at(px - r.x, py - r.y, r.w, r.h)
                    }
                });
                if let Some(msg) = msg {
                    self.handle_msg(msg);
                }
            }
        }
    }
}

impl ProvidesRegistryState for LayerApp {
    fn registry(&mut self) -> &mut smithay_client_toolkit::registry::RegistryState {
        &mut self.registry_state
    }
    registry_handlers![
        smithay_client_toolkit::output::OutputState,
        smithay_client_toolkit::seat::SeatState
    ];
}

/// El manager de ventanas: anuncia un toplevel nuevo.
impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for LayerApp {
    fn event(
        state: &mut Self,
        _mgr: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_manager_v1::Event;
        match event {
            Event::Toplevel { toplevel } => {
                let id = state.next_toplevel_id;
                state.next_toplevel_id = state.next_toplevel_id.wrapping_add(1);
                // Correlación con el puente de íconos: el object-id de este handle.
                let obj_id = toplevel.id().protocol_id();
                let mut t = Toplevel::new(id, toplevel);
                // Un ícono pudo llegar antes que el alta del handle: aplícalo ya.
                if let Some(name) = state.pending_icons.remove(&obj_id) {
                    t.set_icon(name);
                }
                state.toplevels.push(t);
            }
            Event::Finished => {
                state.toplevels.clear();
                state.marcar_todo_dirty();
            }
            _ => {}
        }
    }

    event_created_child!(LayerApp, ZwlrForeignToplevelManagerV1, [
        EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

/// El puente propio `mirada_toplevel_icon`: el compositor manda el nombre de
/// ícono de cada ventana, correlacionado con el object-id de su handle
/// foreign-toplevel. Lo casamos con el `Toplevel` y lo preferimos sobre el
/// `app_id` al pintar el badge de la taskbar. Un ícono para un handle que aún no
/// conocemos se guarda y se aplica cuando el toplevel aparece.
impl Dispatch<MiradaToplevelIconManagerV1, ()> for LayerApp {
    fn event(
        state: &mut Self,
        _mgr: &MiradaToplevelIconManagerV1,
        event: mirada_toplevel_icon_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use mirada_toplevel_icon_proto::client::mirada_toplevel_icon_manager_v1 as icon_mgr;
        if let icon_mgr::Event::Icon { toplevel, name } = event {
            let cambio = if let Some(t) = state
                .toplevels
                .iter_mut()
                .find(|t| t.handle.id().protocol_id() == toplevel)
            {
                t.set_icon(name)
            } else {
                // Handle desconocido todavía: guarda (o limpia) para aplicar al alta.
                if name.is_empty() {
                    state.pending_icons.remove(&toplevel);
                } else {
                    state.pending_icons.insert(toplevel, name);
                }
                false
            };
            if cambio {
                state.marcar_todo_dirty();
            }
        }
    }
}

/// El notificador de inactividad no emite eventos propios; sólo fabrica
/// notificaciones. Impl vacía para poder bindear el global.
impl Dispatch<ExtIdleNotifierV1, ()> for LayerApp {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: ext_idle_notifier_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// La notificación de inactividad: `idled` cuando el sistema cumplió el timeout
/// sin actividad, `resumed` al volver. Es la señal que dispara el idle de
/// energía — y, vía re-armado, su reintento.
impl Dispatch<ExtIdleNotificationV1, ()> for LayerApp {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use ext_idle_notification_v1::Event;
        match event {
            Event::Idled => state.energia_al_ociar(qh),
            Event::Resumed => state.energia_al_volver(qh),
            _ => {}
        }
    }
}

/// El manager de idle-inhibit no emite eventos; sólo fabrica inhibidores.
impl Dispatch<ZwpIdleInhibitManagerV1, ()> for LayerApp {
    fn event(
        _: &mut Self,
        _: &ZwpIdleInhibitManagerV1,
        _: zwp_idle_inhibit_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// El inhibidor de inactividad tampoco emite eventos; existir es su efecto.
impl Dispatch<ZwpIdleInhibitorV1, ()> for LayerApp {
    fn event(
        _: &mut Self,
        _: &ZwpIdleInhibitorV1,
        _: zwp_idle_inhibitor_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Un handle de toplevel: el compositor le manda título / app_id / estado.
impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for LayerApp {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Event;
        let pos = state.toplevels.iter().position(|t| &t.handle == handle);
        let Some(i) = pos else { return };
        match event {
            Event::Title { title } => state.toplevels[i].set_title(title),
            Event::AppId { app_id } => state.toplevels[i].set_app_id(app_id),
            Event::State { state: estados } => state.toplevels[i].set_state(&estados),
            Event::Done => {
                let (cambio, recien_activada) = state.toplevels[i].confirmar();
                if cambio {
                    // Otra ventana tomó el foco (click/atajo) → los flyouts
                    // colgantes se esconden solos. Los MODALES no: polkit
                    // espera su contraseña y la confirmación fullscreen su
                    // veredicto, pase lo que pase con el foco.
                    if recien_activada
                        && state.menu_open
                        && !matches!(
                            state.menu_kind,
                            super::MenuKind::Polkit | super::MenuKind::Confirm
                        )
                    {
                        state.set_menu_open(false);
                    }
                    state.marcar_todo_dirty();
                }
            }
            Event::Closed => {
                let t = state.toplevels.remove(i);
                t.handle.destroy();
                state.marcar_todo_dirty();
            }
            _ => {}
        }
    }
}
