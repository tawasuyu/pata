# pata-llimphi

*Read this in English: [README.md](README.md).*

El frontend Linux del marco.

Monta el modelo agnóstico de `pata_core` sobre Llimphi. El reparto de
responsabilidades es la regla dura del repo (UIs intercambiables sobre un
`*-core` agnóstico):

- **`pata-core`** decide *qué* mostrar: resuelve la geometría
  (`pata_core::layout::resolve`) y, por cada `WidgetSpec`, materializa un
  `Widget` que emite un view-model (`WidgetView`) en cada `tick`.
- **este crate** decide *cómo*: muestrea el sistema en un
  `WidgetCtx` (ver `sampler`) y traduce el
  view-model a `View<Msg>` de Llimphi (ver `render`).

El `shuma_input` es la excepción: es **interacción**, no modelo de dominio,
así que lo intercepta el frontend (ver `shuma`) en lugar de pasar por el
`build` agnóstico —igual que `mirada-launcher` trata su shuma_bar—.

Hoy todas las superficies se pintan en una sola ventana, en los rects que el
layout resolvió. Cuando el compositor `mirada` reconozca superficies `pata`
(Fase 8), cada una será su propia ventana acoplada.

## Uso

```sh
cargo run --release -p pata-llimphi
```

---

Parte de **pata** — ver [pata](../LEEME.md).
