# pata-host

*Read this in English: [README.md](README.md).*

El **rail hospedado**: el protocolo por el que una app le presta sus "dientes" (su sidebar) al marco `pata` mientras tiene foco, y recibe de vuelta qué diente activó el usuario.

La idea (visión del autor): una app como `cosmos` puede dejar de pintar su
propio rail y quedar como **puro lienzo**; sus herramientas aparecen en el rail
global de pata cuando la app está enfocada. Al clickear un diente en pata, el
comando vuelve a la app, que muestra ese panel sobre su propio canvas. pata
sólo hospeda el **rail** (los dientes) — no los paneles ricos de la app.

## Transporte

Un socket Unix dedicado (`socket_path`, default
`$XDG_RUNTIME_DIR/pata-sidebar.sock`). pata escucha (`HostServer`); las apps
se conectan (`HostClient`). Cada conexión es un stream con marco
**prefijo-de-longitud + postcard** (igual que `mirada-link`):
```text
app  → shell:  [u32 LE len][postcard AppMsg]…   (Register, Update, Bye)
shell → app:   [u32 LE len][postcard ShellMsg]… (Activate)
```
pata correlaciona el `app_id` que la app declara en `Register` con el `app_id`
del toplevel enfocado (que ya conoce vía wlr-foreign-toplevel). Cuando coinciden,
pinta los dientes de esa app; al clickear uno, le manda `Activate{tooth}`.

---

Parte de **pata** — ver [pata](../LEEME.md).
