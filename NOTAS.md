# Notas

## A · Documento, previews y diff

- [ ] Editor simeple agregar  footter con icon de format y btn cancel save, como en legacy

## D · Chrome de la app

Toca: `native/src/menus.rs`, `native/src/titlebar.rs`, `native/src/sidebar.rs`.

- [ ] menu de config como en legacy
      Hay que ver qué panel tenía Legacy antes de decidir qué se porta.
- [ ] integracion iconos de opencode y pi
      Hoy `navigation.rs:366` detecta `opencode` para el título y `sidebar.rs:923`
      solo lo usa para texto; `file_icons` cubre extensiones, no apps.

  Ojo: hay otra sesión editando este mismo worktree y ya tocó `sidebar.rs`,
  `native/Cargo.toml` y `scripts/bundle-macos.sh`. Repartir antes de tocar.

## E · Release e higiene del repo

No tocan el producto; van al final.

- [ ] preparar un buen script para build, release
      Hoy hay 6 scripts sueltos: `audit-rust`, `bundle-macos`, `perf-native`,
      `patched-alacritty`, `generate-native-file-icons` y el test del audit.
- [ ] limpiar repo de contenido, obosoleto, para pruebas a/b, ejemplos, cosas que
      solo fueron para ayudar en el proceso de desarrollo y ya no son utiles,
      analizar bien esos casos
      Necesita una decisión antes: ¿`legacy/` entra en la limpieza? AGENTS.md lo
      declara material de referencia congelado, y "obsoleto" lo alcanza.

## G · Notas de review sobre el diff

Toca: `native/src/diff.rs`, `core/src/files.rs`, y una sesión con agente que hoy no
existe. Legacy: `src/domain/review.ts`, `src/components/{FileDiff,ReviewComposer,
ReviewNoteList}.vue`, `src-tauri/src/{domain,services,commands}/review*.rs`.

- [ ] notas ancladas a las líneas del diff, envío como primera interacción, y el .md
      El destino es una elección recordada por checkout: `markdown` (escribe el `.md`)
      o `opencode` (entrega el review como mensaje de la sesión). El que interesa acá es
      el segundo, y es el que no se puede hacer todavía: `native` no tiene agentes, solo
      shells. O sea que **el export se puede portar primero y el envío depende de que
      haya sesiones de agente**, que no está anotado en ninguna lista todavía (el único
      rastro de opencode en `native` es el título y un texto de accesibilidad).
      Conviene dejarlo anotado así y no meterlo en el mismo cambio.
      - La nota es del diff, no del archivo: lleva `side` (`old` para borradas, `new`
        para agregadas y de contexto), `lineStart`/`lineEnd`, el **código de esas líneas
        capturado al crearla** y una huella de la línea para verificar el ancla después.
        Por eso el export nunca relee el diff.
      - La huella da un veredicto de cuatro: `unchanged`, `changed`, `missing` o
        `unknown` (`unknown` es real: un diff grande solo tiene cargado el tramo que
        se está dibujando). `outdated` es pegajoso: lo pone la deriva y lo saca el
        usuario, no el tiempo.
      - El envío es una **ronda**: se graba antes de llamar al agente, con su marcador
        `muster-review:<id>` embebido en el prompt, para que un reconectarse pueda
        preguntar si el mensaje realmente entró en vez de mandarlo dos veces. Las notas
        pasan a `sent` antes del envío, por lo mismo.
      - Una nota `outdated` no sale salvo que se pida: apunta a una línea que ya cambió,
        y la regla se dice en la propia tarjeta.
      - Prompt = prefijo fijo (`domain/review.rs:65`) + `[marcador]` + el markdown.
        Límites: 8 KiB por nota, 8 KiB de código, 1000 notas por ronda, 512 KiB de
        prompt.
      - El destino se elige una vez y se recuerda por checkout (`App.vue:228`).

- [ ] exportar el review como `.md` en los mismos lugares y con el mismo formato
      Esto sí es portable entero, y es lo que hay que usar tal cual:
      - Dónde: dos modos, y el default es el primero. `default` →
        `~/.muster/tmp/reviews`; `workdir` → `<checkout>/.muster/reviews`. Es una
        preferencia (`reviews.storage` en `config.yml`), no un nombre de carpeta, porque
        la única pregunta que uno se hace es cuál de los dos lugares usa.
      - Nombre: `<yyyy-mm-dd-HHMM>.md` en hora local, con `-2`, `-3`… si ya existe. La
        primera línea es siempre el marcador `<!-- muster-review-export -->`.
      - Ese marcador es también la prueba de propiedad: "borrar la carpeta de reviews"
        borra solo archivos que lo llevan, y en modo `workdir` se niega de plano —
        esas notas son del proyecto, no una caché de la app
        (`services/files.rs:82-90`, `176-190`).
      - Formato: el de CodeReview.nvim por defecto (`domain/review.ts:147-176`).
        `# Code Review <fecha>`, blockquote con `` `main..branch` — N files, M notes ``,
        una sección `## <path>` por archivo y, en cada nota, el bloque cercado con la
        línea como etiqueta (```rust{L10-L20}) y la nota citada con `>`. Las notas de
        un archivo van separadas por `---`.
      - El fence usa el lenguaje de la extensión del archivo, que ya sabemos resolver
        (`syntax_language`).
      - Ojo con la frontera de escritura: esto **no** es un `write_preview`, porque crea
        un archivo que no existía. Legacy lo hacía con su propia regla de contención
        (todo ancestro existente resuelve dentro del checkout, y un symlink hacia
        afuera se rechaza). AGENTS.md dice que ninguna escritura de un archivo del
        checkout puede esquivar `write_preview`, así que esto necesita una función
        propia en `core::files` con esa regla adentro, o una excepción explícita
        escrita. Decidir antes de escribir la primera línea.
      - Test: dos exports con el mismo timestamp producen `<stem>.md` y `<stem>-2.md`, y
        un archivo sin el marcador sobrevive a "borrar la carpeta".

## Otors

- [ ] Carpetas vacias, no mostrar arrow
