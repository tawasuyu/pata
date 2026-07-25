# pata-notify

*Read this in English: [README.md](README.md).*

El daemon de notificaciones de escritorio de tawasuyu.

Tres caras, deliberadamente desacopladas:
- **Frontend** `dbus`: registra `org.freedesktop.Notifications` en el bus
  de sesión. Tanto apps ajenas (cualquier cliente freedesktop) como nativas
  (con un helper que hable la misma interfaz) entran por aquí.
- **Render** `app`: una `App` de Llimphi que se pinta a sí misma como una
  **caja wlr-layer-shell** anclada a la esquina (vía `llimphi-layer`), usando
  el widget render-only `llimphi-widget-toast`. Agnóstico del compositor.
- **Historial** `store`: cada notificación se persiste en `sled`. Es el
  sustrato que luego leerán el panel de historial y la capa de triage/IA —
  el daemon en sí se mantiene tonto y fiable.

El puente entre el frontend (runtime tokio en su propio hilo) y el loop Elm
de Llimphi es un `Handle<Msg>` clonado: el handler D-Bus reentra al `update`
con `Handle::dispatch(Msg::Entrante(..))`, sin sockets extra.

## Uso

```sh
cargo run --release -p pata-notify
```

---

Parte de **pata** — ver [pata](../LEEME.md).
