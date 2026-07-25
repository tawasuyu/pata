# pata-core

The desktop frame's model.

`pata` (Quechua: edge, ledge, platform) is the layer that draws the desktop's
*frame*: the **bars**, the **panels** and the **dock** surrounding the windows, and
the **widgets** living inside them. It is not the compositor (that is `mirada`) nor
the shell (that is `shuma`): it is the configurable chrome both of them leave room
for.

`no_std + alloc`, so the same frame model runs on Linux and from the wawa kernel's
launcher.

---

Part of **pata** — see [pata](../README.md).
