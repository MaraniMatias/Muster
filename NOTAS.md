# Notas

## A · Documento, previews y diff

- [x] imagees previews, svg
      Ya andaban; faltaba comprobarlo. PNG, SVG (rasterizado, se decide por el
      nombre porque no tiene header) y el resto de formatos se dibujan.
      Tests: `a_rendered_document_draws_an_svg_beside_every_other_picture`.
- [ ] archivos con changes, tab/btn para ver los cambios directamtne
      El botón está dibujado (`native/src/document.rs:728-770`, con `+n/-n` y
      `open_changes(checkout, Some(path))`), pero en la app no sale casi nunca, y el
      test que lo cubre pasa porque se lee el status a mano antes de abrir el archivo.
      La razón: el botón se dibuja solo si `git_status` ya tiene el archivo, y **nadie
      pide ese status al abrir un documento**. `request_git_status` se llama desde el
      tick de mantenimiento de 2 s (`app.rs:906-914`), y solo para el checkout activo,
      solo cada 5 ticks (10 s), y solo si `panels_visible || open_diff.is_some()`.
      Con la barra lateral cerrada no aparece nunca; con ella abierta, recién a los 10 s.
      - Arreglo: pedir el status del checkout del documento al abrirlo, como ya se hace
        después de un save (`document.rs:385`). El tick se queda como está: es el que
        refresca, este es el que responde la pregunta del header.
      - Test: el mismo `a_document_offers_the_changes_of_a_file_that_has_them`, sin el
        `read_status` previo. Hoy pasa por el atajo; la pregunta es si el botón
        aparece sin que nadie haya leído el status antes.
- [x] editoar simple funcionar, con save/cancel, foramte como en legacy
      Barra al pie con Cancel/Save (Esc y ⌘S). El save compara contra el texto leído
      y se niega a pisar lo que otro escribió (`core::files::write_preview`).
      Test: `an_edited_document_saves_or_throws_itself_away`.
  - [ ] Falta el botón de formatear (Prettier) que tenía Legacy. No hay Node acá.
- [x] espacion entre texto y imagenes en privews de md, video o archivo no prosedsa
      mostrar el alias, y entodo caso [image: file:...]
      El aire va dentro de la caja (el toolkit mide un inline como root: un margin
      ahí se mide a cero) y video/audio/archivo se dibujan en su propia caja, con el
      mismo aire que una imagen.
      Test: `what_a_document_cannot_draw_is_said_in_a_box_of_its_own`.
      El rótulo queda como está: con alt, el alt tal cual (`The demo`); sin alt,
      `Video: assets/demo.mp4` / `File: docs/notes.pdf`.
- [ ] selector de gramática en la barra del documento: Auto o forzar una
      Lo que en Legacy decía qué gramática llevaba el archivo: en **Auto** manda la
      extensión — que es lo que hace hoy `native` — y el lector podía forzar cualquier
      gramática de la lista o bajar a **Plain text**
      (`legacy/src/components/DocumentPane.vue:225-308` y el popover en
      `1013-1072`). Ojo: es el *highlight*, no el botón de formatear con Prettier de
      más abajo, que es otra cosa.
      - El botón dice la verdad: en Auto muestra la gramática detectada, y si la
        extensión no nombra ninguna, `Auto (no highlighting)` en vez de un lenguaje
        que no se está aplicando (`DocumentPane.vue:253-260`).
      - La elección es del archivo y no del panel, y muere con la ventana: un mapa en
        memoria por `checkout\0origin\0path` (`DocumentPane.vue:212-224`), ni la base
        ni `~/.muster`.
      - La búsqueda filtra gramáticas, nunca Auto ni Plain text: son las dos formas
        de salir de una forzada (`DocumentPane.vue:298-308`).
      - Solo aparece cuando hay lectura de código en el pane y no toca la lectura
        renderizada de un Markdown. Tampoco el diff, que se resalta por path igual
        que en Legacy.
      - Toca: `native/src/document.rs`. Hoy el idioma es `syntax_language(path)`
        (línea 161) y se pasa una sola vez al crear el editor (línea 64): hace falta el
        override en el estado del documento y un popover en la cabecera. No hace falta
        recrear el editor — `EditorState::set_highlighter(name, cx)` ya existe en el
        toolkit y conserva undo, selección y scroll, que es lo que Legacy perdía al
        releer el archivo.
      - La lista son las gramáticas que compila `native/Cargo.toml`, más Vue que
        registra `theme::install`. Ofrecer una que el toolkit no tiene abre el archivo
        sin colorear, que es lo mismo que Auto.
      - Test: `a_reader_can_force_the_grammar_a_file_wears` — un archivo sin extensión
        conocida, se fuerza YAML, los tokens cambian; y de vuelta a Auto.

## B · Mensajes → toast

Toca: `native/src/toast.rs` (nuevo). No toca nada más, así que va solo.

- [ ] menssaje, aletars -> deberian ser toast
      `native/` no tiene toast todavía. Legacy tiene `src/presentation/toasts.ts` y
      `ToastStack.vue` como referencia: niveles `error`/`info`, 3 s y 6 s, y la pila
      es click-through para que un toast no se coma un click.

## C · Color por estado del proceso

Toca: `native/src/sidebar.rs` (y `theme.rs` solo si falta un rol).

- [ ] colores segun se ejecuta un proceso o fallo
      `sidebar.rs:920-930` ya calcula `busy` y `failed`, pero hoy solo los usa para
      el texto de accesibilidad: nunca se pintan. Es usar `roles.status.*`.
      El más barato de la lista.

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

## F · Highlight de Vue — hecho

- [x] mejoraar highline en vue
      El problema no era el color: era que el atributo simple y el tag se pintaban
      igual. Legacy leía un `.vue` con la gramática TextMate de Shiki, donde el
      atributo y la directiva son dos escrituras de lo mismo y ambos llegan al
      token de función; la gramática que usamos mandaba el atributo a `attribute`,
      que es el color del tag, así que `class` salía idéntico a `<p>` y solo un
      atributo con `:` o `@` parecía algo. Una línea de query lo arregla:
      `(attribute_name) @function`, antes de la query de la gramática porque el
      highlighter toma el primer patrón que matchea.
      Test: `theme::tests::a_vue_attribute_is_not_painted_like_a_tag`, que compara
      el color real del archivo contra `legacy/src/muster.css`.
  - [ ] Inyectar JavaScript en los bindings (`{{ }}`, `:prop="…"`). **Decidido que
        no**: es más grande que Legacy y Legacy tampoco lo hacía — ahí el valor
        queda en texto normal. Si algún día se quiere, es un `VUE_INJECTIONS` más.

---

## Preguntas abiertas

Ninguna.

Resuelto y cerrado:

- El rótulo de lo que un documento no puede dibujar. El punto pedía «mostrar el
  alias, y en todo caso `[image: file:...]`»; lo que ya hace es lo pedido: con alt
  el alt tal cual, sin alt el tipo y el nombre (`Video: assets/demo.mp4`).
- El highlight de Vue (grupo F): era el editor nativo, no la app de `legacy/`.

## Diferencias que quedan contra Legacy (a propósito)

Verificadas corriendo las dos gramáticas sobre el mismo `.vue`:

- El valor de un binding (`:title="name"` → `name`) lo pintamos en ámbar; Legacy
  lo dejaba en texto normal.
- Los delimitadores de `{{ }}` los pintamos en gris; Legacy en texto normal.

Las dos son Legacy pintando *menos*. Se dejan así: la paridad es el criterio del
repo y nadie las mencionó al mirar el archivo.

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
