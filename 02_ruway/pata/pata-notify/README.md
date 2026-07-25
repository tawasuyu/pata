# pata-notify

tawasuyu's desktop notification daemon.

Three faces, deliberately decoupled:

- **Frontend** (`dbus`): it registers `org.freedesktop.Notifications` on the session
  bus. Both foreign apps (any freedesktop client) and native ones (with a helper
  speaking the same interface) enter here.
- **Render** (`app`): a Llimphi `App` painting itself as a **wlr-layer-shell box**
  anchored to the corner (through `llimphi-layer`).
- **Store**: an append-only sled history.

---

Part of **pata** — see [pata](../README.md).
