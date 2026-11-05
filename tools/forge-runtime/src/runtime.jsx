// The Forge's artefact runtime: shows a claude.ai style React artefact offline.
// Built into crates/horadric-ui/assets/forge/runtime/runtime.js by build.mjs.

import "@unocss/reset/tailwind.css";
import "./theme.css";

import * as React from "react";
import * as ReactDOM from "react-dom";
import * as ReactDOMClient from "react-dom/client";
import * as jsxRuntime from "react/jsx-runtime";
import { transform } from "sucrase";
import * as lucide from "lucide-react";
import * as recharts from "recharts";
import * as clsx from "clsx";
import * as tailwindMerge from "tailwind-merge";
import * as cva from "class-variance-authority";
import initUno from "@unocss/runtime";
import { unoConfig } from "./uno.js";

import * as utils from "./lib/utils.js";
import * as accordion from "./ui/accordion.jsx";
import * as alert from "./ui/alert.jsx";
import * as alertDialog from "./ui/alert-dialog.jsx";
import * as avatar from "./ui/avatar.jsx";
import * as badge from "./ui/badge.jsx";
import * as button from "./ui/button.jsx";
import * as card from "./ui/card.jsx";
import * as checkbox from "./ui/checkbox.jsx";
import * as collapsible from "./ui/collapsible.jsx";
import * as dialog from "./ui/dialog.jsx";
import * as dropdownMenu from "./ui/dropdown-menu.jsx";
import * as input from "./ui/input.jsx";
import * as label from "./ui/label.jsx";
import * as popover from "./ui/popover.jsx";
import * as progress from "./ui/progress.jsx";
import * as radioGroup from "./ui/radio-group.jsx";
import * as scrollArea from "./ui/scroll-area.jsx";
import * as select from "./ui/select.jsx";
import * as separator from "./ui/separator.jsx";
import * as skeleton from "./ui/skeleton.jsx";
import * as slider from "./ui/slider.jsx";
import * as switchUi from "./ui/switch.jsx";
import * as table from "./ui/table.jsx";
import * as tabs from "./ui/tabs.jsx";
import * as textarea from "./ui/textarea.jsx";
import * as toggle from "./ui/toggle.jsx";
import * as tooltip from "./ui/tooltip.jsx";

// Captured now: document.currentScript is only set while this script first runs,
// and mermaid.js is fetched from beside it later.
const BASE = document.currentScript ? new URL(".", document.currentScript.src).href : location.href;

initUno(unoConfig);

const ui = {
  accordion, alert, "alert-dialog": alertDialog, avatar, badge, button, card, checkbox, collapsible,
  dialog, "dropdown-menu": dropdownMenu, input, label, popover, progress, "radio-group": radioGroup,
  "scroll-area": scrollArea, select, separator, skeleton, slider, switch: switchUi, table, tabs,
  textarea, toggle, tooltip,
};

const PROBES = new Set(["__esModule", "default", "then", "toJSON", "$$typeof", "constructor", "prototype"]);

// An unknown named import otherwise turns into `undefined` and React's
// "Element type is invalid", which says nothing about which name was wrong.
function strict(name, ns) {
  const target = { ...ns, __esModule: true, default: ns.default ?? ns };
  return new Proxy(target, {
    get(t, key) {
      if (key in t || typeof key === "symbol" || PROBES.has(key)) return t[key];
      throw new ForgeError("import", `"${name}" has no export named "${key}".`);
    },
  });
}

const modules = new Map();
function provide(name, ns) {
  modules.set(name, strict(name, ns));
}
provide("react", React);
provide("react-dom", ReactDOM);
provide("react-dom/client", ReactDOMClient);
provide("react/jsx-runtime", jsxRuntime);
provide("lucide-react", lucide);
provide("recharts", recharts);
provide("clsx", clsx);
provide("tailwind-merge", tailwindMerge);
provide("class-variance-authority", cva);
provide("@/lib/utils", utils);
for (const [k, v] of Object.entries(ui)) provide(`@/components/ui/${k}`, v);

function normalise(spec) {
  let s = spec.replace(/\.(jsx?|tsx?)$/, "");
  s = s.replace(/^(?:[@~]\/|\/)?(components\/ui\/|lib\/)/, "@/$1");
  if (s === "lucide-react/dist/esm/lucide-react" || s.startsWith("lucide-react/icons")) s = "lucide-react";
  return s;
}

class ForgeError extends Error {
  constructor(kind, message, extra = {}) {
    super(message);
    this.name = "ForgeError";
    this.kind = kind;
    Object.assign(this, extra);
  }
}

function requireFor(file) {
  return function require(spec) {
    const m = modules.get(normalise(spec));
    if (m) return m;
    const known = [...modules.keys()].filter((k) => !k.startsWith("@/components/ui/")).join(", ");
    throw new ForgeError(
      "import",
      `${file} imports "${spec}", which the Forge's offline runtime does not include. ` +
        `It has ${known} and @/components/ui/{${Object.keys(ui).join(",")}}.`
    );
  };
}

const listeners = new Set();
function report(err, file) {
  const detail = describe(err, file);
  for (const cb of listeners) {
    try {
      cb(detail);
    } catch (e) {
      console.error(e);
    }
  }
  window.dispatchEvent(new CustomEvent("forge:artefact-error", { detail }));
  console.error(`[forge] ${detail.kind}: ${detail.message}`);
  return detail;
}

function sourceUrl(file) {
  return `forge-artefact:///${encodeURI(file)}`;
}

function describe(err, file) {
  const d = {
    kind: err?.kind ?? "runtime",
    message: String(err?.message ?? err),
    file,
    stack: err?.stack ? String(err.stack) : undefined,
  };
  if (err?.componentStack) d.componentStack = err.componentStack;
  if (err?.line) {
    d.line = err.line;
    d.column = err.column;
  } else if (d.stack && file) {
    // The artefact is evaluated with a sourceURL and no wrapper lines, so the
    // first frame inside it is a line in the user's own file.
    const m = d.stack.match(new RegExp(escapeRe(sourceUrl(file)) + ":(\\d+):(\\d+)"));
    if (m) {
      d.line = Number(m[1]);
      d.column = Number(m[2]);
    }
  }
  return d;
}

function escapeRe(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function compile(source, file) {
  const ts = /\.tsx?$/i.test(file);
  let code;
  try {
    code = transform(source, {
      transforms: ts ? ["jsx", "typescript", "imports"] : ["jsx", "imports"],
      jsxRuntime: "automatic",
      production: true,
      filePath: file,
    }).code;
  } catch (e) {
    const m = /\((\d+):(\d+)\)/.exec(e.message);
    throw new ForgeError("transform", e.message, m ? { line: Number(m[1]), column: Number(m[2]) } : {});
  }
  // Indirect eval with the body on the wrapper's first line keeps sucrase's
  // line numbers equal to the file's, so a stack frame points at the source.
  const factory = (0, eval)(
    `(function (require, module, exports, React) {${code}\n})\n//# sourceURL=${sourceUrl(file)}`
  );
  const module = { exports: {} };
  factory(requireFor(file), module, module.exports, React);
  return module.exports;
}

function pickComponent(exports, file) {
  const d = exports.default;
  if (typeof d === "function" || (d && typeof d === "object" && d.$$typeof)) return d;
  const fns = Object.entries(exports).filter(([k, v]) => k !== "default" && typeof v === "function");
  const named = fns.find(([k]) => /^[A-Z]/.test(k));
  if (named) return named[1];
  throw new ForgeError("export", `${file} has no default export to show. An artefact is one component, exported with "export default".`);
}

class Boundary extends React.Component {
  constructor(props) {
    super(props);
    this.state = { error: null };
  }
  static getDerivedStateFromError(error) {
    return { error };
  }
  componentDidCatch(error, info) {
    const e = error instanceof Error ? error : new Error(String(error));
    if (!e.kind) e.kind = "render";
    e.componentStack = info?.componentStack;
    const d = report(e, this.props.file);
    this.props.onSettle?.();
    if (this.props.showError) this.setState({ detail: d });
  }
  render() {
    if (this.state.error) {
      return this.props.showError && this.state.detail ? <ErrorCard detail={this.state.detail} /> : null;
    }
    return this.props.children;
  }
}

// Inline styles, not classes: the card must look right even when the class
// compiler is what broke.
function ErrorCard({ detail }) {
  const where = detail.line ? ` at line ${detail.line}${detail.column ? `:${detail.column}` : ""}` : "";
  return (
    <div
      data-forge-error=""
      style={{
        margin: "24px",
        padding: "16px 20px",
        border: "1px solid #e5484d",
        borderRadius: "8px",
        background: "#fff5f5",
        color: "#3b0d0c",
        font: "13px/1.5 ui-sans-serif, system-ui, sans-serif",
      }}
    >
      <div style={{ fontWeight: 600, marginBottom: "6px" }}>
        {LABELS[detail.kind] ?? "This artefact failed"}
        {detail.file ? ` in ${detail.file}` : ""}
        {where}
      </div>
      <pre style={{ margin: 0, whiteSpace: "pre-wrap", font: "12px/1.5 ui-monospace, Consolas, monospace" }}>{detail.message}</pre>
    </div>
  );
}

const LABELS = {
  transform: "Could not read the source",
  import: "Import not available offline",
  export: "Nothing to show",
  render: "The component threw while drawing",
  runtime: "The component threw",
  load: "Could not load the file",
  mermaid: "Could not draw the diagram",
};

function showStatic(el, detail) {
  const root = ReactDOMClient.createRoot(el);
  root.render(<ErrorCard detail={detail} />);
  return root;
}

const roots = new WeakMap();

function unmount(el) {
  const r = roots.get(el);
  if (r) {
    r.unmount();
    roots.delete(el);
  }
}

let current = null;
window.addEventListener("error", (ev) => {
  if (current && ev.error) report(ev.error, current);
});
window.addEventListener("unhandledrejection", (ev) => {
  if (current) report(ev.reason, current);
});

function mount(source, filename = "artefact.jsx", el = document.getElementById("root"), opts = {}) {
  const showError = opts.showError !== false;
  unmount(el);
  current = filename;
  let Component;
  try {
    Component = pickComponent(compile(source, filename), filename);
  } catch (e) {
    const d = report(e, filename);
    if (showError) roots.set(el, showStatic(el, d));
    throw e;
  }
  const root = ReactDOMClient.createRoot(el);
  roots.set(el, root);
  // Hidden until UnoCSS has styled the first render: otherwise every
  // transition-colors element visibly fades in from unstyled.
  el.style.visibility = "hidden";
  const safety = setTimeout(() => (el.style.visibility = ""), 1500);
  root.render(
    <Boundary file={filename} showError={showError} onSettle={() => reveal(el, safety)}>
      <Committed onCommit={() => reveal(el, safety)} />
      <Component />
    </Boundary>
  );
  return { unmount: () => unmount(el) };
}

function Committed({ onCommit }) {
  React.useEffect(() => {
    onCommit();
  }, []);
  return null;
}

async function reveal(el, safety) {
  try {
    const uno = window.__unocss_runtime;
    if (uno) {
      await uno.extractAll(el);
      await uno.update();
    }
  } finally {
    clearTimeout(safety);
    el.style.visibility = "";
  }
}

let mermaidLoad = null;
function loadMermaid() {
  if (window.__forgeMermaid) return Promise.resolve(window.__forgeMermaid);
  mermaidLoad ??= new Promise((resolve, reject) => {
    const s = document.createElement("script");
    s.src = new URL("mermaid.js", BASE).href;
    s.onload = () => resolve(window.__forgeMermaid);
    s.onerror = () => {
      mermaidLoad = null;
      reject(new ForgeError("mermaid", "mermaid.js could not be loaded from the runtime folder."));
    };
    document.head.appendChild(s);
  });
  return mermaidLoad;
}

async function mermaid(root = document.body) {
  const m = await loadMermaid();
  return m.render(root, (e) => report(e, current ?? undefined));
}

async function load(url, el = document.getElementById("root"), opts = {}) {
  const file = decodeURIComponent(new URL(url, location.href).pathname.split("/").pop() || url);
  let text;
  try {
    const res = await fetch(url, { cache: "no-store" });
    if (!res.ok) throw new Error(`${res.status} ${res.statusText} for ${url}`);
    text = await res.text();
  } catch (e) {
    const d = report(new ForgeError("load", e.message), file);
    if (opts.showError !== false) roots.set(el, showStatic(el, d));
    throw e;
  }
  if (/\.mmd$|\.mermaid$/i.test(file)) {
    unmount(el);
    current = file;
    el.textContent = "";
    const pre = document.createElement("pre");
    pre.className = "mermaid";
    pre.textContent = text;
    el.appendChild(pre);
    return mermaid(el);
  }
  return mount(text, file, el, opts);
}

window.__forgeRuntime = {
  version: __FORGE_VERSIONS__,
  mount,
  load,
  unmount,
  mermaid,
  modules: () => [...modules.keys()],
  onError(cb) {
    listeners.add(cb);
    return () => listeners.delete(cb);
  },
};
