# pata-notify-panel

*Read this in English: [README.md](README.md).*

Panel de historial de notificaciones: sidebar derecho que muestra el
historial **agrupado** por el triage semántico. Cliente del daemon
`pata-notify`: consulta por D-Bus y refresca por la señal `Cambio` (con un
tick de seguridad por si la señal se pierde).

## Uso

```sh
cargo run --release -p pata-notify-panel
```

---

Parte de **pata** — ver [pata](../LEEME.md).
