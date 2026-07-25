# pata-llimphi

The frame's Linux frontend.

It mounts `pata_core`'s agnostic model on Llimphi. The split of responsibilities is
the repo's hard rule (interchangeable UIs over an agnostic `*-core`): `pata-core`
decides *what* to show — it resolves the geometry and materializes each
`WidgetSpec` — and this crate paints it, sampling the live sources (clock, CPU,
RAM, volume, brightness).

---

Part of **pata** — see [pata](../README.md).
