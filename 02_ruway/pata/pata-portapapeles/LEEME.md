# pata-portapapeles

*Read this in English: [README.md](README.md).*

El **gestor de portapapeles** (Klipper) de la suite.

Sube el ring de sólo-texto en memoria de pata a un **historial persistente**
(sled) que además:
* guarda **texto e imágenes** (mime + bytes),
* **deduplica** moviendo al frente el clip repetido,
* deja **fijar** (pin) clips para que sobrevivan a la limpieza y al tope,
* **busca** por subcadena en los clips de texto,
* detecta **acciones** sobre un clip (URL/email/ruta → sugerencia).

Núcleo puro y testeable; el widget de pata lo consume (muestrea `wl-paste`,
empuja aquí, y pinta el historial + las acciones).

---

Parte de **pata** — ver [pata](../LEEME.md).
