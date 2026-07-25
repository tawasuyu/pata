# pata-host

The **hosted rail**: the protocol by which an app lends its "teeth" (its sidebar) to
the `pata` frame while it has focus, and gets back which tooth the user activated.

The idea: an app such as `cosmos` can stop painting its own rail and become **pure
canvas**; its tools appear in pata's global rail when the app is focused. Clicking a
tooth in pata sends the command back to the app, which shows that panel over its own
canvas.

---

Part of **pata** — see [pata](../README.md).
