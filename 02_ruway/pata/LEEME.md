# pata

> El marco del escritorio: barras, paneles y dock declarativos — widgets que
> colocás donde quieras, desde un archivo de config. El mismo modelo en Linux y
> en Wawa.

`pata` (quechua: *borde, repisa, andén*) es la capa de chrome del escritorio
gioser. No es el compositor (`mirada`) ni el shell (`shuma`): es el marco
configurable que rodea a las ventanas. Desde un archivo desplegás **barras**,
**paneles** y un **dock**, y dentro acomodás widgets — botón inicio, lista de
ventanas abiertas, clipboard / volumen / brillo, tray, reloj, un widget
**astro** (posición zodiacal del sol + ciclo lunar) y el input del shell que
despliega `shuma` estilo Quake.

El modelo vive en `pata-core`, agnóstico y `no_std`, así que el mismo marco
corre como frontend Llimphi en Linux (sobre el compositor `mirada`) y desde el
kernel launcher de Wawa.

Definición canónica y plan por fases: [`SDD.md`](SDD.md).
