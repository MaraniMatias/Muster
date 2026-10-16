# Notas

- [ ] menssaje, aletars -> deberian ser toast — NO HECHO. Es el grupo B, nunca se
      empezó: `native/` no tiene toast todavia
- [x] imagees previews, svg — el preview ya existia; faltaba comprobarlo. PNG, SVG
      (rasterizado, se decide por el nombre porque no tiene header) y el resto de
      formatos ya se dibujan
- [x] archivos con changes, tab/btn para ver los cambios directamtne — boton
      **Changes** en el header del documento, con `+n/-n`, solo cuando Git dijo que
      ese archivo esta en el change set
- [x] editoar simple funcionar, con save/cancel, foramte como en legacy — barra al
      pie con Cancel/Save (Esc y ⌘S), el save compara contra el texto leido y se
      niega a pisar lo que otro escribio. Falta el boton de Prettier que tenia legacy
- [ ] mejoraar highline en vue — NO HECHO. Es el grupo F y sigue con la pregunta de
      si es `legacy/` (congelado) o el highlight del editor nativo
- [x] espacion entre texto y imagenes en privews de md, video o archivo no prosedsa
      mostrar el alias, y entodo caso [image: file:...] — ver la pregunta abierta abajo
- [ ] menu de config como en legacy
- [ ] integracion iconos de opencode y pi
- [ ] colores segun se ejecuta un proceso o fallo — NO HECHO. Es el grupo C:
      `sidebar.rs:920-930` ya calcula `busy` y `failed` pero solo los usa para el
      texto de accesibilidad; falta pintar con `roles.status.*`

---

- [ ] preparar un buen script para build, release
- [ ] limpiar repo de contenido, obosoleto, para pruebas a/b, ejemplos, cosas que
      solo fueron para ayudar en el proceso de desarrollo y ya no son utiles,
      analizar bien esos casos

---

## Pregunta abierta

El punto de los previews pedia, para un video o un archivo que no se puede dibujar,
"mostrar el alias, y en todo caso `[image: file:...]`". Hoy lo que se muestra es:

- con alt: el alt tal cual (`The demo`)
- sin alt: `Video: assets/demo.mp4` / `File: docs/notes.pdf`

¿`[image: file:...]` es el formato que querias para el caso sin alt, o es otra cosa
(marca de un transcript de opencode/pi)? Con una linea se ajusta; es el unico
detalle del grupo A que quedo sin decidir.