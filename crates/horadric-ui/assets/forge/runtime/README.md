# The Forge's artefact runtime

Built files, committed so building Horadric needs no npm. The source and the
build are in `tools/forge-runtime/`. Do not edit these files by hand.

| File | Size | gzip | What |
|---|---|---|---|
| `runtime.js` | 2207 KB | 585 KB | React, sucrase, UnoCSS, lucide, recharts, shadcn/ui, the loader |
| `runtime.css` | 3 KB | 1 KB | Tailwind 3's preflight and shadcn's theme variables |
| `mermaid.js` | 3741 KB | 1044 KB | Mermaid, fetched by `runtime.js` the first time a diagram is drawn |
| `artefact.html` | 1 KB | 1 KB | The page for a `.jsx`, `.tsx` or `.mmd` file |
| total | 5952 KB | 1630 KB | |

Nothing here fetches from the network. An import the runtime does not have
is an error drawn in the tab, never a CDN fetch.

## The page contract

Serve this folder at any path on the Forge's virtual host, with the files
side by side (`artefact.html` loads `runtime.css` and `runtime.js` by
relative URL, and `runtime.js` loads `mermaid.js` from beside itself). Open

```
https://<x>.forge.horadric/<runtime folder>/artefact.html?src=<url of the file>
```

`src` is the artefact's URL on the same host, absolute path or relative to
the page, for example `?src=/reports/dashboard.jsx`. The page fetches it
with `cache: "no-store"`, so reloading the page reloads the artefact. A
file ending in `.mmd` or `.mermaid` is drawn as a diagram; anything else is
compiled as a React component. `.tsx` and `.ts` get the TypeScript
transform too.

## `window.__forgeRuntime`

- `mount(sourceText, filename, el, { showError = true })`: compiles the
  source and renders its default export into `el` (`#root` by default),
  replacing whatever was mounted there. If there is no default export, the
  first exported function with a capitalised name is used. Throws on a
  transform, import or export error after reporting it. Returns
  `{ unmount }`.
- `load(url, el, opts)`: fetches `url` and mounts it, or draws it as Mermaid
  for `.mmd`. A promise; rejects after reporting.
- `mermaid(root = document.body)`: draws every `pre.mermaid`, `div.mermaid`
  and `pre > code.language-mermaid` under `root`, loading `mermaid.js` on
  first use. Resolves to `{ drawn, failed }`. A diagram that fails gets an
  error card in its place.
- `onError(cb)`: `cb(detail)` for every error; returns a function that
  removes it. The same `detail` is also dispatched on `window` as the
  `forge:artefact-error` CustomEvent.
- `unmount(el)`, `modules()` (the import names available), `version` (the
  library versions it was built from).

`detail` is `{ kind, message, file, line?, column?, stack?, componentStack? }`.
`kind` is `transform` (syntax), `import` (a module or a named export the
runtime lacks), `export` (nothing to show), `render` (threw while drawing,
caught by an error boundary), `runtime` (an uncaught error or rejection
later, an event handler say), `load` (the fetch failed) or `mermaid`.
`line` is a line in the artefact's own file: sucrase keeps line numbers and
the code is evaluated with no wrapper lines and a `forge-artefact:///<file>`
source URL.

With `showError` on (the default) the runtime draws a plain error card in
`el` for every kind but `runtime`, styled inline so it shows even if the
class compiler is what broke. A page script that draws its own can pass
`{ showError: false }`. Every card has `data-forge-error`.

A page that only has diagrams (rendered markdown) can skip `runtime.js`: load
`mermaid.js` alone and call `window.__forgeMermaid.render(root, onError)`.

## What an artefact can import

`react`, `react-dom`, `react-dom/client`, `react/jsx-runtime`,
`lucide-react`, `recharts`, `clsx`, `tailwind-merge`,
`class-variance-authority`, `@/lib/utils` (`cn`) and
`@/components/ui/<name>` for accordion, alert, alert-dialog, avatar, badge,
button, card, checkbox, collapsible, dialog, dropdown-menu, input, label,
popover, progress, radio-group, scroll-area, select, separator, skeleton,
slider, switch, table, tabs, textarea, toggle and tooltip. An extension on
the path and `components/ui/...` without the `@/` are accepted.

Tailwind classes are compiled in the page as the DOM changes, with Tailwind
3's meanings (what Claude writes), arbitrary values included. The root is
hidden until the first render is styled, so nothing flashes unstyled.

Mermaid draws with dagre. ELK is left out (a megabyte and a half), so a
diagram that asks for `layout: elk` gets the error card.

## Rebuilding

```
cd tools/forge-runtime
npm ci
npm run build
```

The build prints the sizes; update the table above and
`docs/dependencies.md` when they move. Versions are pinned exactly in
`package.json` and the lockfile.

If the binary grows too much, the plan's fallback is to load this folder
from beside the binary on first use instead of embedding it. Serving the
files gzipped would cut what is embedded to about 1.6 MB.
