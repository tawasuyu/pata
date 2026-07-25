# pata-notify-panel

The notification history panel: a right sidebar showing the history **grouped** by
the semantic triage. A client of the `pata-notify` daemon: it queries over D-Bus and
refreshes on the `Cambio` signal (with a safety tick in case the signal is lost).

---

Part of **pata** — see [pata](../README.md).
