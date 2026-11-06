// The Forge's editor: the whole file worked on in the pane. Markdown is
// written in the page itself (each block shows its markdown where the caret
// is), a CSV is a grid; both are editable as they open, with no mode to go
// into. HTML gets its source beside the page in edit mode, as markdown does
// with its Source key. Every change is written as the whole file together
// with the text it started from, so the host can refuse it when Claude
// wrote the file meanwhile.
// Injected after forge.js into every Forge page. The protocol is in
// README.md beside this file; keep the two in step.
(() => {
  "use strict";
  if (window.__forgeEditor) return;
  // An HTML page's preview is the same page in a frame, given the same
  // scripts; there it is only a view.
  if (window.top !== window) return;

  const scriptSrc = document.currentScript && document.currentScript.src;
  const FILE = location.pathname;
  const EXT = (FILE.split(".").pop() || "").toLowerCase();
  const KEY = "forge-editor:" + FILE;
  const SPLIT_KEY = "forge-editor-split";
  const WAIT = 600;
  const NARROW = 720;
  const HEAD = 44;
  // A version from the history is shown on a host of its own, read only.
  const VERSION = /\.v\.forge\.horadric$/i.test(location.hostname);

  // ------------------------------------------------------------- plumbing

  const forge = () => window.__forge;

  function post(msg) {
    const f = forge();
    if (f) f.post(msg);
    else console.debug("[forge-editor] post", msg);
  }

  function note(text, bad) {
    const f = forge();
    if (f && f.note) f.note(text, bad);
  }

  // Storage can be missing or throw (a private window, blocked site data);
  // everything kept there is a convenience, so a failure is only forgotten.
  function store(kind) {
    try { return kind === "local" ? window.localStorage : window.sessionStorage; } catch (_) { return null; }
  }
  function recall(kind, key) {
    try { const s = store(kind); const v = s && s.getItem(key); return v ? JSON.parse(v) : null; } catch (_) { return null; }
  }
  function remember(kind, key, value) {
    try {
      const s = store(kind);
      if (!s) return;
      if (value == null) s.removeItem(key);
      else s.setItem(key, JSON.stringify(value));
    } catch (_) { /* full or blocked: the next save tries again */ }
  }

  function cssFor() {
    const given = window.__forgeAssets && window.__forgeAssets.editorCss;
    if (typeof given === "string") {
      const style = document.createElement("style");
      style.textContent = given;
      return style;
    }
    if (!scriptSrc) return null;
    const link = document.createElement("link");
    link.rel = "stylesheet";
    link.href = new URL("editor.css", scriptSrc).href;
    return link;
  }

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }

  function key(label, title, fn) {
    const b = el("button", "forge-ed-key", label);
    b.type = "button";
    if (title) b.title = title;
    b.addEventListener("click", (e) => {
      e.stopPropagation();
      fn();
      // A key keeps no focus, so Enter and Space go back to the editor.
      b.blur();
    });
    return b;
  }

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  // ---------------------------------------------------------------- state

  let on = false;
  let kind = null;
  let base = null;
  let seq = 0;
  let inflight = null;
  let timer = 0;
  let wrote = false;
  let conflict = null;
  let host = null;
  let ui = null;
  let pageCss = null;
  let view = "source";
  let split = clampSplit((recall("local", SPLIT_KEY) || {}).f);
  let restoring = null;
  // Markdown and CSV are always the editor; HTML has edit mode.
  let always = false;
  // Markdown shown as the page written in ("page") or as its source.
  let mdView = "page";
  let ty = null;
  // Claude's text that came while the file was still being read.
  let lateTheirs = null;

  function clampSplit(f) {
    return typeof f === "number" && f > 0.15 && f < 0.85 ? f : 0.46;
  }

  function current() {
    if (kind === "csv") return grid ? grid.text() : base;
    if (ty) return ty.value();
    return src ? src.value() : base;
  }

  // The whole text replaced, through the editor's own undo.
  function setAll(text) {
    if (kind === "csv") grid.load(text, true);
    else if (ty) ty.replaceAll(text);
    else src.replaceAll(text);
    if (kind === "html") reloadPreview();
    if (kind === "md" && !ty) refreshMarkdown();
  }

  function detect() {
    if (document.querySelector("main.forge-md")) return "md";
    if (document.querySelector("main.forge-csv")) return "csv";
    if (EXT === "html" || EXT === "htm") return "html";
    return null;
  }

  // The bytes as they are on disk: a BOM kept, so writing back keeps it.
  async function fetchSource() {
    const url = kind === "html" ? FILE : FILE + "?raw";
    const r = await fetch(url, { cache: "no-store" });
    if (!r.ok) throw new Error(`the file answered ${r.status}`);
    const bytes = await r.arrayBuffer();
    return new TextDecoder("utf-8", { ignoreBOM: true }).decode(bytes);
  }

  // ----------------------------------------------------------------- save

  function schedule() {
    clearTimeout(timer);
    timer = setTimeout(save, WAIT);
    if (!conflict) status("saving");
    keepSession();
  }

  function save() {
    clearTimeout(timer);
    timer = 0;
    if (!on || conflict || inflight || base === null) return;
    const text = current();
    if (text === base) {
      status("saved");
      return;
    }
    const id = ++seq;
    inflight = { id, text };
    status("saving");
    post({ forge: "write", id, file: FILE, base, text });
  }

  function writeResult(m) {
    if (!inflight || m.id !== inflight.id) return true;
    const sent = inflight;
    inflight = null;
    if (!on) return true;
    if (m.ok) {
      base = sent.text;
      wrote = true;
      if (kind === "md" && !ty && typeof m.html === "string") swapPreview(m.html);
      if (kind === "html") reloadPreview();
      if (current() !== base) schedule();
      else status("saved");
      keepSession();
    } else if (m.why === "changed" && typeof m.current === "string") {
      showConflict(m.current);
    } else {
      status("failed", m.why ? `Not saved: ${m.why}` : "Not saved");
    }
    return true;
  }

  // Someone else wrote the file while it is open here; the host sends the
  // new text instead of reloading the tab.
  function changedOnDisk(m) {
    if (!on || typeof m.current !== "string") return true;
    if (base === null) { lateTheirs = m.current; return true; }
    const theirs = m.current;
    // A cell being typed in is a change too, and the grid is drawn again.
    if (grid) grid.commit();
    if (conflict !== null) {
      conflict = theirs;
      return true;
    }
    if (theirs === base) return true;
    // A write on its way, or changes not yet sent, are the human's: they
    // choose. A write in flight is refused as changed and lands here too.
    if (inflight || timer || current() !== base) {
      if (!inflight) showConflict(theirs);
      return true;
    }
    base = theirs;
    setAll(theirs);
    status("saved");
    keepSession();
    return true;
  }

  // The rendered page is what a plain fetch of a markdown file answers;
  // only its reading screen is taken.
  async function refreshMarkdown() {
    try {
      const r = await fetch(FILE, { cache: "no-store" });
      if (!r.ok) return;
      const doc = new DOMParser().parseFromString(await r.text(), "text/html");
      const main = doc.querySelector("main.forge-md");
      if (main && main.childElementCount && on && kind === "md") swapPreview(main.innerHTML);
    } catch (_) { /* the preview stays as it was until the next save */ }
  }

  function showConflict(theirs) {
    conflict = theirs;
    clearTimeout(timer);
    timer = 0;
    status("changed");
    if (ui) ui.conflict.hidden = false;
    layoutSoon();
  }

  function loadTheirs() {
    if (conflict === null) return;
    const theirs = conflict;
    conflict = null;
    base = theirs;
    ui.conflict.hidden = true;
    setAll(theirs);
    status("saved");
    keepSession();
    layoutSoon();
  }

  function keepMine() {
    if (conflict === null) return;
    base = conflict;
    conflict = null;
    ui.conflict.hidden = true;
    layoutSoon();
    save();
  }

  const LABEL = {
    loading: "Opening",
    saving: "Saving",
    saved: "Saved",
    changed: "Changed by Claude",
    failed: "Not saved",
  };

  function status(state, text) {
    if (!ui) return;
    ui.status.dataset.state = state;
    ui.statusText.textContent = text || LABEL[state];
  }

  // What survives the page loading again: the host reloads the tab when
  // Claude writes the file, and the edit must come back as it was.
  let sessionTimer = 0;
  function keepSession() {
    clearTimeout(sessionTimer);
    sessionTimer = setTimeout(writeSession, 250);
  }
  function writeSession() {
    clearTimeout(sessionTimer);
    if (!on) return;
    const text = base === null ? null : current();
    const s = { on: true, view, mdView };
    if (text !== null && text !== base) s.draft = { base, text };
    if (src) Object.assign(s, src.where());
    if (ty) Object.assign(s, ty.where());
    if (grid) s.cell = grid.where();
    const fs = frameState();
    if (fs) s.frame = fs;
    remember("session", KEY, s);
  }
  addEventListener("pagehide", writeSession);

  // ---------------------------------------------------------- enter, leave

  async function enter(fromHost, saved) {
    if (on) return;
    kind = detect();
    if (!kind) {
      if (fromHost) post({ forge: "editing", on: false });
      note("This file cannot be edited here", true);
      return;
    }
    on = true;
    always = kind !== "html";
    if (!fromHost && !always) post({ forge: "editing", on: true });
    view = (saved && saved.view) || "source";
    if (kind === "md") mdView = saved && saved.mdView === "source" ? "source" : "page";
    base = null;
    lateTheirs = null;
    conflict = null;
    wrote = false;
    build();
    status("loading");
    let text;
    try {
      text = await fetchSource();
    } catch (e) {
      note(`Could not read the file to edit it: ${e.message}`, true);
      leave(false);
      return;
    }
    if (!on) return;
    if (lateTheirs !== null) text = lateTheirs;
    lateTheirs = null;
    base = text;
    const draft = saved && saved.draft;
    let shown = text;
    if (draft && typeof draft.text === "string" && draft.text !== draft.base) {
      shown = draft.text;
      if (draft.base !== text) base = draft.base;
    }
    restoring = saved || null;
    if (kind === "csv") grid.load(shown, false);
    else if (ty) await ty.load(shown, false);
    else src.load(shown);
    if (!on) return;
    if (src) src.restore(saved);
    if (ty) ty.restore(saved);
    if (grid) grid.restore(saved && saved.cell);
    if (base !== text) showConflict(text);
    else if (shown !== base) schedule();
    else status("saved");
    keepSession();
  }

  async function leave(fromHost) {
    if (!on || always) return;
    if (grid) grid.commit();
    if (!conflict && base !== null && current() !== base) save();
    for (let i = 0; inflight && i < 60; i++) await sleep(50);
    if (!on) return;
    on = false;
    clearTimeout(timer);
    timer = 0;
    remember("session", KEY, null);
    if (!fromHost) post({ forge: "editing", on: false });
    const reload = wrote && kind !== "md";
    teardown();
    // The page under the editor is the file as it was; the host does not
    // reload a tab for its own writes.
    if (reload) location.reload();
  }

  function toggle() {
    if (on) leave(false);
    else enter(false, null);
  }

  // Markdown between the page written in and its source beside the page
  // (Ctrl+/ or the Source key), the text carried over as it is.
  function setMdView(v) {
    if (!on || kind !== "md" || v === mdView || base === null) return;
    const text = current();
    teardown();
    mdView = v;
    build();
    if (ty) {
      ty.load(text, true);
    } else {
      src.load(text);
      src.restore(null);
      renderWhole(text);
    }
    if (conflict !== null) ui.conflict.hidden = false;
    status(conflict !== null ? "changed" : text === base ? "saved" : "saving");
    keepSession();
    layoutSoon();
  }

  // The page beside the source drawn from the text as it is now, written
  // or not.
  async function renderWhole(text) {
    const html = await renderMd(text);
    if (html != null && on && kind === "md" && !ty) swapPreview(html);
  }

  // ------------------------------------------------------------------- UI

  let src = null;
  let grid = null;
  let frame = null;

  function build() {
    if (!pageCss) {
      pageCss = cssFor();
      if (pageCss) (document.head || document.documentElement).appendChild(pageCss);
    }
    // Named as forge.js's overlay is: the deck shell leaves keys typed in a
    // forge-overlay alone, and exports and print hide every one.
    host = document.createElement("forge-overlay");
    const root = host.attachShadow({ mode: "open" });
    const css = cssFor();
    if (css) root.appendChild(css);
    const box = el("div", `forge-ed forge-ed-${kind}`);
    root.appendChild(box);
    ui = { box };
    ui.status = el("span", "forge-ed-status");
    ui.status.innerHTML =
      '<svg viewBox="0 0 14 14" aria-hidden="true"><path class="stone" d="M7 1.5 12.5 7 7 12.5 1.5 7Z"/>' +
      '<path class="trace" pathLength="100" d="M7 1.5 12.5 7 7 12.5 1.5 7Z"/></svg>';
    ui.statusText = el("span", "forge-ed-status-text");
    ui.status.appendChild(ui.statusText);
    ui.conflict = el("div", "forge-ed-conflict");
    ui.conflict.hidden = true;
    ui.conflict.append(
      el("span", "forge-ed-conflict-text", "Claude changed this file"),
      key("Load theirs", "Take Claude's version and drop your changes", loadTheirs),
      key("Keep mine", "Write your version over Claude's", keepMine),
    );
    const done = always
      ? key("Rendered", "Write in the page itself (Ctrl+/)", () => setMdView("page"))
      : key("Done", "Stop editing (E)", () => leave(false));
    const cls = document.documentElement.classList;
    cls.add("forge-editing", `forge-editing-${kind}`);

    if (kind === "md" && mdView === "page") {
      buildTypora(box);
      return;
    }

    if (kind === "csv") {
      const tools = el("div", "forge-ed-tools");
      tools.append(
        key("Row below", "Add a row under the chosen cell", () => grid.addRow(false)),
        key("Row at end", "Add a row after the last", () => grid.addRow(true)),
        key("Column right", "Add a column right of the chosen cell", () => grid.addCol()),
        key("Delete row", "Delete the chosen cell's row", () => grid.deleteRow()),
        key("Delete column", "Delete the chosen cell's column", () => grid.deleteCol()),
      );
      const end = el("div", "forge-ed-end");
      end.append(ui.conflict, ui.status);
      box.append(tools, end);
      document.documentElement.appendChild(host);
      // The bar's height is known only once its sheet is in, and it wraps
      // on a narrow pane; the table's screen ends where it starts.
      new ResizeObserver(layoutSoon).observe(box);
      grid = makeGrid();
    } else {
      const head = el("div", "forge-ed-head");
      const sw = el("div", "forge-ed-switch");
      ui.toSource = key("Source", "Show the source", () => setView("source"));
      ui.toPage = key("Page", "Show the page", () => setView("page"));
      sw.append(ui.toSource, ui.toPage);
      head.append(
        el("span", "forge-ed-legend", "Source"),
        el("span", "forge-ed-name", decodeURIComponent(FILE.split("/").pop() || "")),
        sw, ui.status, done,
      );
      const body = el("div", "forge-ed-body");
      const holder = el("div", "forge-ed-pane");
      body.appendChild(holder);
      ui.handle = el("div", "forge-ed-handle");
      ui.handle.title = "Drag to resize";
      body.appendChild(ui.handle);
      if (kind === "html") {
        const fwrap = el("div", "forge-ed-frame");
        frame = document.createElement("iframe");
        frame.title = "Page";
        frame.addEventListener("load", frameLoaded);
        frame.src = FILE + "?preview";
        fwrap.appendChild(frame);
        body.appendChild(fwrap);
      }
      box.append(head, ui.conflict, body);
      document.documentElement.appendChild(host);
      src = makeSource(holder, kind === "md");
      wireHandle(ui.handle);
      if (kind === "md") wireMarkdownPreview();
    }
    addEventListener("resize", layout);
    layout();
  }

  function teardown() {
    removeEventListener("resize", layout);
    if (grid) grid.destroy();
    if (ty) ty.destroy();
    ty = null;
    if (kind === "md") unwireMarkdownPreview();
    for (const h of document.querySelectorAll(".forge-ed-here")) h.classList.remove("forge-ed-here");
    if (host) host.remove();
    const cls = document.documentElement.classList;
    for (const c of [...cls]) if (c.startsWith("forge-editing") || c === "forge-typora") cls.remove(c);
    document.documentElement.style.removeProperty("--forge-ed-w");
    document.documentElement.style.removeProperty("--forge-ed-bar");
    host = ui = src = grid = frame = null;
  }

  function setView(v) {
    view = v;
    layout();
    if (v === "source" && src) src.focus();
    keepSession();
  }

  let layoutQueued = false;
  function layoutSoon() {
    if (layoutQueued) return;
    layoutQueued = true;
    requestAnimationFrame(() => { layoutQueued = false; layout(); });
  }

  function place(style) {
    host.setAttribute("style",
      "all:initial !important; position:fixed !important; z-index:2147483646 !important;" +
      "display:block !important;" + style);
  }

  function layout() {
    if (!host) return;
    const root = document.documentElement;
    if (kind === "csv") {
      place("left:0 !important; right:0 !important; bottom:0 !important;");
      root.style.setProperty("--forge-ed-bar", `${ui.box.offsetHeight}px`);
      return;
    }
    if (ty) {
      place("inset:0 !important; pointer-events:none !important;");
      return;
    }
    const w = innerWidth;
    const mode = w < NARROW ? view : "split";
    ui.box.dataset.mode = mode;
    ui.toSource.classList.toggle("down", mode === "source");
    ui.toPage.classList.toggle("down", mode === "page");
    for (const m of ["split", "source", "page"]) root.classList.toggle(`forge-editing-${m}`, m === mode);
    const W = Math.round(Math.min(Math.max(w * split, 280), w - 280));
    if (kind === "md") {
      if (mode === "split") {
        root.style.setProperty("--forge-ed-w", `${W}px`);
        place(`left:0 !important; top:0 !important; bottom:0 !important; width:${W}px !important;`);
      } else if (mode === "source") {
        place("inset:0 !important;");
      } else {
        place(`left:0 !important; top:0 !important; right:0 !important; height:${HEAD}px !important;`);
      }
    } else {
      place("inset:0 !important;");
      const cols = mode === "split" ? `${W}px 10px 1fr` : mode === "source" ? "1fr 0 0" : "0 0 1fr";
      ui.box.style.setProperty("--forge-ed-cols", cols);
    }
    if (src) src.measure();
  }

  function wireHandle(h) {
    h.addEventListener("pointerdown", (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      h.setPointerCapture(e.pointerId);
      ui.box.classList.add("dragging");
      const move = (ev) => {
        split = Math.min(0.84, Math.max(0.16, ev.clientX / innerWidth));
        layout();
      };
      const up = () => {
        h.removeEventListener("pointermove", move);
        h.removeEventListener("pointerup", up);
        h.removeEventListener("pointercancel", up);
        ui.box.classList.remove("dragging");
        remember("local", SPLIT_KEY, { f: split });
      };
      h.addEventListener("pointermove", move);
      h.addEventListener("pointerup", up);
      h.addEventListener("pointercancel", up);
    });
  }

  // The page written in: the editor's own UI is only what floats over it,
  // a small plate with the status and the Source key, the slash menu, a
  // table's keys and a code block's language.
  function buildTypora(box) {
    box.classList.add("forge-ed-ty");
    document.documentElement.classList.add("forge-typora");
    const bar = el("div", "forge-ty-bar");
    bar.append(ui.status, key("Source", "The whole file as markdown (Ctrl+/)", () => setMdView("source")));
    ui.menu = el("div", "forge-ty-menu");
    ui.menu.hidden = true;
    ui.tools = el("div", "forge-ty-tools");
    ui.tools.hidden = true;
    ui.tools.append(
      key("Row", "Add a row under this one (Ctrl+Enter)", () => ty && ty.addRow()),
      key("Column", "Add a column right of this one", () => ty && ty.addCol()),
      key("Delete row", "Delete this row", () => ty && ty.delRow()),
      key("Delete column", "Delete this column", () => ty && ty.delCol()),
    );
    // A key keeps the cell's caret: pressing it must not take the focus.
    ui.tools.addEventListener("mousedown", (e) => e.preventDefault());
    ui.lang = document.createElement("input");
    ui.lang.className = "forge-ty-lang";
    ui.lang.placeholder = "language";
    ui.lang.spellcheck = false;
    ui.lang.hidden = true;
    box.append(bar, ui.conflict, ui.menu, ui.tools, ui.lang);
    document.documentElement.appendChild(host);
    ty = makeTypora();
    addEventListener("resize", layout);
    layout();
  }

  // ---------------------------------------------------------- source pane

  function makeSource(parent, md) {
    const wrap = el("div", "forge-ed-src");
    const gutter = el("div", "forge-ed-gutter");
    const nums = el("div", "forge-ed-nums");
    gutter.appendChild(nums);
    const ta = el("textarea", "forge-ed-text");
    ta.spellcheck = false;
    ta.setAttribute("autocomplete", "off");
    ta.setAttribute("autocapitalize", "off");
    ta.setAttribute("aria-label", "Source");
    const mirror = el("div", "forge-ed-mirror");
    mirror.setAttribute("aria-hidden", "true");
    wrap.append(gutter, ta, mirror);
    parent.appendChild(wrap);

    let unit = "  ";
    let heights = [];
    let drawn = "";
    let curLine = -1;
    let measureQueued = false;

    function lineOf(pos) {
      let n = 0;
      const v = ta.value;
      for (let i = v.indexOf("\n"); i !== -1 && i < pos; i = v.indexOf("\n", i + 1)) n++;
      return n;
    }

    function lineStart(n) {
      const v = ta.value;
      let pos = 0;
      for (let i = 0; i < n; i++) {
        const j = v.indexOf("\n", pos);
        if (j === -1) return v.length;
        pos = j + 1;
      }
      return pos;
    }

    function padTop() { return parseFloat(getComputedStyle(ta).paddingTop) || 0; }

    function lineTop(n) {
      let y = padTop();
      for (let i = 0; i < n && i < heights.length; i++) y += heights[i];
      return y;
    }

    function measureSoon() {
      if (measureQueued) return;
      measureQueued = true;
      requestAnimationFrame(() => { measureQueued = false; measure(); });
    }

    // Line numbers follow wrapped lines: a hidden copy of the text, one
    // block per line at the textarea's width, gives each number its height.
    function measure() {
      if (!ta.isConnected) return;
      const lines = ta.value.split("\n");
      const cs = getComputedStyle(ta);
      const lh = parseFloat(cs.lineHeight) || 21;
      const long = lines.length > 4000;
      const wrapAttr = long ? "off" : "soft";
      if (ta.getAttribute("wrap") !== wrapAttr) ta.setAttribute("wrap", wrapAttr);
      if (long) {
        heights = lines.map(() => lh);
      } else {
        const w = ta.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight);
        mirror.style.width = `${Math.max(w, 10)}px`;
        while (mirror.childElementCount < lines.length) mirror.appendChild(document.createElement("div"));
        while (mirror.childElementCount > lines.length) mirror.lastChild.remove();
        for (let i = 0; i < lines.length; i++) {
          const d = mirror.children[i];
          const t = lines[i] || "​";
          if (d.textContent !== t) d.textContent = t;
        }
        heights = Array.from(mirror.children, (d) => d.offsetHeight || lh);
      }
      const sig = heights.join(",");
      if (sig !== drawn) {
        drawn = sig;
        let html = "";
        for (let i = 0; i < heights.length; i++) html += `<div style="height:${heights[i]}px">${i + 1}</div>`;
        nums.innerHTML = html;
        curLine = -1;
      }
      gutter.style.width = `${String(lines.length).length + 2.4}ch`;
      cursorMoved(false);
      scrolled();
    }

    function scrolled() {
      nums.style.transform = `translateY(${-ta.scrollTop}px)`;
    }

    let cursorQueued = false;
    let followPreview = true;
    function cursorMoved(follow) {
      followPreview = follow !== false;
      if (cursorQueued) return;
      cursorQueued = true;
      requestAnimationFrame(() => {
        cursorQueued = false;
        const n = lineOf(ta.selectionStart);
        if (n !== curLine) {
          const old = nums.children[curLine];
          if (old) old.classList.remove("on");
          const now = nums.children[n];
          if (now) now.classList.add("on");
          curLine = n;
        }
        if (md) highlightMarkdown(n + 1, followPreview);
        else highlightFrame(ta.selectionStart, followPreview);
        keepSession();
      });
    }

    function guessUnit(text) {
      let tabs = 0;
      let least = 0;
      for (const l of text.split("\n")) {
        if (l[0] === "\t") tabs++;
        const m = /^ +(?=\S)/.exec(l);
        if (m && (!least || m[0].length < least)) least = m[0].length;
      }
      if (tabs > 2) return "\t";
      return " ".repeat(least >= 2 && least <= 4 ? least : 2);
    }

    // Edits go through execCommand so the textarea's own undo keeps them.
    function replace(a, b, text, s2, e2) {
      ta.focus();
      ta.setSelectionRange(a, b);
      let ok = false;
      try {
        ok = text ? document.execCommand("insertText", false, text)
          : a === b || document.execCommand("delete");
      } catch (_) { ok = false; }
      if (!ok) {
        ta.setRangeText(text, a, b, "end");
        ta.dispatchEvent(new Event("input", { bubbles: true }));
      }
      ta.setSelectionRange(s2, e2);
      cursorMoved();
    }

    function indent(out) {
      const v = ta.value;
      const s = ta.selectionStart;
      const e = ta.selectionEnd;
      const multi = v.slice(s, e).includes("\n");
      // Tab in a list item nests the item, wherever the cursor is in it.
      const item = md && /^\s*([-*+]|\d+[.)])\s/.test(v.slice(v.lastIndexOf("\n", s - 1) + 1));
      if (!out && !multi && !item) {
        replace(s, e, unit, s + unit.length, s + unit.length);
        return;
      }
      const ls = v.lastIndexOf("\n", s - 1) + 1;
      let le = e > s && v[e - 1] === "\n" ? e - 1 : e;
      le = v.indexOf("\n", le);
      if (le < 0) le = v.length;
      const width = unit === "\t" ? 4 : unit.length;
      let first = 0;
      let total = 0;
      const done = v.slice(ls, le).split("\n").map((l, i) => {
        let d = 0;
        if (out) {
          if (l[0] === "\t") d = -1;
          else d = -(/^ */.exec(l)[0].length > width ? width : /^ */.exec(l)[0].length);
          l = l.slice(-d);
        } else if (l.trim() !== "") {
          d = unit.length;
          l = unit + l;
        }
        if (i === 0) first = d;
        total += d;
        return l;
      });
      const ns = Math.max(ls, s + first);
      replace(ls, le, done.join("\n"), ns, Math.max(ns, e + total));
    }

    function wrapMark(m) {
      const v = ta.value;
      const s = ta.selectionStart;
      const e = ta.selectionEnd;
      const L = m.length;
      if (s >= L && v.slice(s - L, s) === m && v.slice(e, e + L) === m) {
        replace(s - L, e + L, v.slice(s, e), s - L, e - L);
      } else {
        replace(s, e, m + v.slice(s, e) + m, s + L, e + L);
      }
    }

    function link() {
      const v = ta.value;
      const s = ta.selectionStart;
      const e = ta.selectionEnd;
      const sel = v.slice(s, e);
      if (/^https?:\/\/\S+$/.test(sel)) {
        replace(s, e, `[](${sel})`, s + 1, s + 1);
      } else {
        const at = s + sel.length + 3;
        replace(s, e, `[${sel}](url)`, sel ? at : s + 1, sel ? at + 3 : s + 1);
      }
    }

    // Enter in a list item starts the next item; in an empty one it ends
    // the list, as every editor of markdown does.
    function continueList() {
      const v = ta.value;
      const s = ta.selectionStart;
      if (s !== ta.selectionEnd) return false;
      const ls = v.lastIndexOf("\n", s - 1) + 1;
      let le = v.indexOf("\n", s);
      if (le < 0) le = v.length;
      const line = v.slice(ls, le);
      let m = /^(\s*)([-*+]|\d+[.)])(\s+)(\[[ xX]\]\s+)?/.exec(line);
      let next;
      if (m) {
        const n = /^\d+/.exec(m[2]);
        const marker = n ? `${+n[0] + 1}${m[2].slice(n[0].length)}` : m[2];
        next = m[1] + marker + m[3] + (m[4] ? "[ ] " : "");
      } else if ((m = /^\s*(>\s?)+/.exec(line))) {
        next = m[0];
      } else {
        return false;
      }
      if (s < ls + m[0].length) return false;
      if (line.slice(m[0].length).trim() === "") {
        replace(ls, le, "", ls, ls);
        return true;
      }
      replace(s, s, "\n" + next, s + 1 + next.length, s + 1 + next.length);
      return true;
    }

    ta.addEventListener("keydown", (e) => {
      if (e.isComposing) return;
      const mod = (e.ctrlKey || e.metaKey) && !e.altKey;
      const k = e.key.toLowerCase();
      if (mod && k === "s") { e.preventDefault(); save(); return; }
      if (e.key === "Tab" && !mod && !e.altKey) { e.preventDefault(); indent(e.shiftKey); return; }
      if (e.key === "Escape") { ta.blur(); return; }
      if (!md) return;
      if (mod && !e.shiftKey && k === "b") { e.preventDefault(); wrapMark("**"); }
      else if (mod && !e.shiftKey && k === "i") { e.preventDefault(); wrapMark("*"); }
      else if (mod && !e.shiftKey && k === "k") { e.preventDefault(); link(); }
      else if (e.key === "Enter" && !e.shiftKey && !mod && !e.altKey && continueList()) e.preventDefault();
    });
    ta.addEventListener("input", () => {
      measureSoon();
      cursorMoved();
      if (!conflict) schedule();
      else keepSession();
    });
    ta.addEventListener("scroll", scrolled, { passive: true });
    for (const t of ["keyup", "mouseup", "focus"]) ta.addEventListener(t, () => cursorMoved());
    ta.addEventListener("select", () => cursorMoved());
    new ResizeObserver(measureSoon).observe(ta);
    if (document.fonts) document.fonts.ready.then(measureSoon);

    return {
      value: () => ta.value,
      load(text) {
        unit = guessUnit(text);
        ta.value = text;
        measure();
      },
      // Through the edit history, so Ctrl+Z brings back what was replaced.
      replaceAll(text) {
        const s = ta.selectionStart;
        const top = ta.scrollTop;
        ta.focus();
        ta.select();
        let ok = false;
        try { ok = document.execCommand("insertText", false, text); } catch (_) { ok = false; }
        if (!ok) ta.value = text;
        const at = Math.min(s, text.length);
        ta.setSelectionRange(at, at);
        ta.scrollTop = top;
        measure();
      },
      restore(saved) {
        const sel = saved && saved.sel;
        if (Array.isArray(sel)) {
          const n = ta.value.length;
          ta.setSelectionRange(Math.min(sel[0], n), Math.min(sel[1], n));
        } else {
          ta.setSelectionRange(0, 0);
        }
        ta.focus({ preventScroll: true });
        measure();
        if (saved && typeof saved.top === "number") ta.scrollTop = saved.top;
        scrolled();
      },
      where: () => ({ sel: [ta.selectionStart, ta.selectionEnd], top: ta.scrollTop }),
      focus: () => ta.focus({ preventScroll: true }),
      measure: measureSoon,
      gotoLine(n) {
        const pos = lineStart(n - 1);
        ta.focus({ preventScroll: true });
        ta.setSelectionRange(pos, pos);
        ta.scrollTop = Math.max(0, lineTop(n - 1) - ta.clientHeight / 3);
        scrolled();
        cursorMoved(false);
      },
      gotoOffset(pos) {
        ta.focus({ preventScroll: true });
        ta.setSelectionRange(pos, pos);
        ta.scrollTop = Math.max(0, lineTop(lineOf(pos)) - ta.clientHeight / 3);
        scrolled();
        cursorMoved(false);
      },
      selection: () => ta.selectionStart,
      textarea: ta,
    };
  }

  // ---------------------------------------------------- markdown preview

  function parseLines(s) {
    const m = /^(\d+)(?:-(\d+))?$/.exec(s || "");
    return m ? [+m[1], +(m[2] || m[1])] : null;
  }

  function blockFor(line) {
    const main = document.querySelector("main.forge-md");
    if (!main) return null;
    let best = null;
    let span = Infinity;
    let next = null;
    for (const b of main.querySelectorAll("[data-lines]")) {
      const l = parseLines(b.getAttribute("data-lines"));
      if (!l) continue;
      if (l[0] <= line && line <= l[1] && l[1] - l[0] < span) { best = b; span = l[1] - l[0]; }
      if (!next && l[0] > line) next = b;
    }
    return best || next;
  }

  let lit = null;
  function light(elm) {
    if (lit === elm) return;
    if (lit) lit.classList.remove("forge-ed-here");
    lit = elm;
    if (lit) lit.classList.add("forge-ed-here");
  }

  function highlightMarkdown(line, follow) {
    const b = blockFor(line);
    light(b);
    if (!b || !follow) return;
    const r = b.getBoundingClientRect();
    if (r.bottom < 0 || r.top > innerHeight) b.scrollIntoView({ block: "center" });
  }

  function previewClick(e) {
    if (!on || kind !== "md" || e.button !== 0) return;
    const f = forge();
    if (f && f.pinning) return;
    const t = e.target;
    if (!t || !t.closest || !t.closest("main.forge-md")) return;
    // In the editor a link in the page is a place in the source, not a way
    // off it.
    if (t.closest("a[href]")) e.preventDefault();
    if (getSelection().toString()) return;
    const b = t.closest("[data-lines]");
    const l = b && parseLines(b.getAttribute("data-lines"));
    if (l && src) src.gotoLine(l[0]);
  }

  // A double click while the source is open would start forge.js's edit in
  // place, a second writer with an older base: it is put back at once.
  function previewDblclick() {
    if (!on || kind !== "md") return;
    const a = document.activeElement;
    if (a && a.isContentEditable && a.closest("main.forge-md")) {
      a.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    }
  }

  function wireMarkdownPreview() {
    document.addEventListener("click", previewClick, true);
    addEventListener("dblclick", previewDblclick);
  }

  function unwireMarkdownPreview() {
    document.removeEventListener("click", previewClick, true);
    removeEventListener("dblclick", previewDblclick);
    lit = null;
  }

  function swapPreview(html) {
    const main = document.querySelector("main.forge-md");
    if (!main) return;
    const x = scrollX;
    const y = scrollY;
    lit = null;
    main.innerHTML = html;
    scrollTo(x, y);
    if (src) highlightMarkdown(lineOfSel() + 1, false);
  }

  function lineOfSel() {
    const v = src.value();
    const s = src.selection();
    let n = 0;
    for (let i = v.indexOf("\n"); i !== -1 && i < s; i = v.indexOf("\n", i + 1)) n++;
    return n;
  }

  // ------------------------------------------------- markdown, written in place

  // The page itself is the editor, as in Typora: every block shows
  // rendered, and the block the caret is in shows its markdown, styled, so
  // the markers stand faded beside what they do. The source of truth is
  // the file's text, one entry a line; a block is the lines its element's
  // data-lines name, and only the block written in changes its lines. A
  // block left is drawn again by the host's own renderer, so the page
  // looks exactly as the saved file does.

  const FENCE = /^(\s{0,3})(`{3,}|~{3,})(\s*)([^\s`]*)(.*)$/;
  const LIST = /^(\s*)([-*+]|\d+[.)])(\s+)(\[[ xX]\]\s+)?/;
  const QUOTE = /^\s{0,3}(?:>\s?)+/;
  const HEADING = /^(\s{0,3})(#{1,6})(\s+|$)(.*)$/;
  const RULE = /^\s{0,3}([-*_])(\s*\1){2,}\s*$/;
  const TASK = /^(\s*(?:>\s?)*\s*(?:[-*+]|\d+[.)])\s+)\[([ xX])\]/;
  const DELIM = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;

  const SLASH = [
    { key: "h1", label: "Heading 1", hint: "#" },
    { key: "h2", label: "Heading 2", hint: "##" },
    { key: "h3", label: "Heading 3", hint: "###" },
    { key: "bullet", label: "Bullet list", hint: "-" },
    { key: "numbered", label: "Numbered list", hint: "1." },
    { key: "task", label: "Task list", hint: "[ ]" },
    { key: "quote", label: "Quote", hint: ">" },
    { key: "code", label: "Code", hint: "```" },
    { key: "table", label: "Table", hint: "|" },
    { key: "image", label: "Image", hint: "![]" },
    { key: "divider", label: "Divider", hint: "---" },
    { key: "mermaid", label: "Mermaid diagram", hint: "graph" },
  ];

  // Questions to the host, answered by id.
  const asks = new Map();
  let askSeq = 0;
  function ask(msg, wait = 15000) {
    const id = `ty-${++askSeq}`;
    return new Promise((resolve) => {
      const t = setTimeout(() => { asks.delete(id); resolve(null); }, wait);
      asks.set(id, (m) => { clearTimeout(t); resolve(m); });
      post({ ...msg, id });
    });
  }
  function answered(m) {
    const r = asks.get(m.id);
    if (!r) return false;
    asks.delete(m.id);
    r(m);
    return true;
  }

  let mermaidAt = null;
  let mermaidLoading = null;
  async function renderMd(text) {
    const m = await ask({ forge: "render", text });
    if (!m || m.ok === false || typeof m.html !== "string") return null;
    if (m.mermaid && typeof m.mermaid.src === "string") mermaidAt = m.mermaid;
    return m.html;
  }

  // Mermaid is loaded by the page only when the file had a diagram as it
  // opened; a diagram written since loads it here, dressed as the page's.
  function ensureMermaid() {
    if (window.__forgeMermaid) return Promise.resolve(true);
    if (!mermaidAt) return Promise.resolve(false);
    if (!mermaidLoading) {
      mermaidLoading = new Promise((resolve) => {
        const s = document.createElement("script");
        s.src = mermaidAt.src;
        s.onload = () => {
          try {
            const look = document.createElement("script");
            look.textContent = mermaidAt.look || "";
            document.head.appendChild(look);
          } catch (_) { /* drawn in Mermaid's own colours */ }
          resolve(!!window.__forgeMermaid);
        };
        s.onerror = () => resolve(false);
        document.head.appendChild(s);
      });
    }
    return mermaidLoading;
  }

  function drawMermaid(root) {
    if (!root || !root.querySelector("pre.mermaid:not([data-forge-drawn])")) return;
    ensureMermaid().then((ok) => {
      if (!ok || !window.__forgeMermaid) return;
      window.__forgeMermaid.render(root, (e) => { const f = forge(); if (f && f.report) f.report(e); });
    });
  }

  // ---- the source of a block, styled

  const mk = (t) => (t ? `<span class="ty-mk">${escHtml(t)}</span>` : "");

  function inl(s) {
    const re = /(`+)([^`]|[^`][\s\S]*?[^`])\1(?!`)|(!?)\[([^\]\n]*)\]\(([^)\n]*)\)|(\*\*|__)(?=\S)([\s\S]*?\S)\6|(\*|_)(?=[^\s*_])([\s\S]*?[^\s\\])\8(?![*_])|(~~)(?=\S)([\s\S]*?\S)\10|\\([\\`*_{}[\]()#+\-.!|~<>])|<(https?:[^>\s]+)>/g;
    let out = "";
    let i = 0;
    let m;
    while ((m = re.exec(s))) {
      out += escHtml(s.slice(i, m.index));
      i = re.lastIndex;
      if (m[1]) out += mk(m[1]) + `<span class="ty-code">${escHtml(m[2])}</span>` + mk(m[1]);
      else if (m[4] !== undefined) {
        out += mk(`${m[3]}[`) + `<span class="${m[3] ? "ty-alt" : "ty-link"}">${inl(m[4])}</span>` +
          mk("](") + `<span class="ty-url">${escHtml(m[5])}</span>` + mk(")");
      } else if (m[6]) out += mk(m[6]) + `<strong>${inl(m[7])}</strong>` + mk(m[6]);
      else if (m[8]) out += mk(m[8]) + `<em>${inl(m[9])}</em>` + mk(m[8]);
      else if (m[10]) out += mk(m[10]) + `<del>${inl(m[11])}</del>` + mk(m[10]);
      else if (m[12]) out += mk("\\") + escHtml(m[12]);
      else if (m[13]) out += mk("<") + `<span class="ty-link">${escHtml(m[13])}</span>` + mk(">");
    }
    return out + escHtml(s.slice(i));
  }

  function cellsHi(l) {
    if (DELIM.test(l)) return mk(l);
    return l.split(/(?<!\\)(\|)/).map((p) => (p === "|" ? mk(p) : inl(p))).join("");
  }

  function lineHi(l) {
    let out = "";
    let m = QUOTE.exec(l);
    if (m) { out += mk(m[0]); l = l.slice(m[0].length); }
    if (RULE.test(l)) return out + `<span class="ty-mk ty-rule">${escHtml(l)}</span>`;
    if ((m = HEADING.exec(l))) {
      const n = m[2].length;
      return out + `<span class="ty-h ty-h${n}">${mk(m[1] + m[2] + m[3])}${inl(m[4])}</span>`;
    }
    if ((m = LIST.exec(l))) {
      out += escHtml(m[1]) + `<span class="ty-mk ty-li">${escHtml(m[2] + m[3])}</span>`;
      if (m[4]) out += `<span class="ty-mk ty-task">${escHtml(m[4])}</span>`;
      l = l.slice(m[0].length);
    }
    if (/^\s*\|/.test(l)) return out + cellsHi(l);
    return out + inl(l);
  }

  function hi(src) {
    let fence = null;
    const out = src.split("\n").map((l) => {
      const m = FENCE.exec(l);
      if (fence) {
        if (m && m[2][0] === fence[0] && m[2].length >= fence.length && !m[4] && !m[5].trim()) {
          fence = null;
          return mk(l);
        }
        return `<span class="ty-codeline">${escHtml(l)}</span>`;
      }
      if (m && !(m[2][0] === "`" && m[5].includes("`"))) {
        fence = m[2];
        return mk(m[1] + m[2] + m[3]) + `<span class="ty-lang">${escHtml(m[4])}</span>` + mk(m[5]);
      }
      return lineHi(l);
    });
    return out.join("\n") + (src === "" || src.endsWith("\n") ? '<br class="ty-tail">' : "");
  }

  // ---- text and caret in an element written in

  function readText(root) {
    if (root.nodeType === 1 && !root.textContent && root.querySelectorAll("br").length <= 1) return "";
    let out = "";
    const walk = (n) => {
      for (let c = n.firstChild; c; c = c.nextSibling) {
        if (c.nodeType === 3) out += c.data;
        else if (c.nodeType === 1) {
          if (c.localName === "br") {
            if (!c.classList.contains("ty-tail")) out += "\n";
          } else {
            if ((c.localName === "div" || c.localName === "p") && out && !out.endsWith("\n")) out += "\n";
            walk(c);
          }
        }
      }
    };
    walk(root);
    return out.replace(/ /g, " ");
  }

  function offsetIn(root, node, off) {
    const r = document.createRange();
    r.selectNodeContents(root);
    try { r.setEnd(node, off); } catch (_) { return 0; }
    let out = "";
    const walk = (n) => {
      for (let c = n.firstChild; c; c = c.nextSibling) {
        if (c.nodeType === 3) out += c.data;
        else if (c.nodeType === 1) {
          if (c.localName === "br") { if (!c.classList.contains("ty-tail")) out += "\n"; }
          else {
            if ((c.localName === "div" || c.localName === "p") && out && !out.endsWith("\n")) out += "\n";
            walk(c);
          }
        }
      }
    };
    walk(r.cloneContents());
    return out.length;
  }

  function pointAt(root, off) {
    let left = off;
    let found = null;
    const walk = (n) => {
      for (let c = n.firstChild; c && !found; c = c.nextSibling) {
        if (c.nodeType === 3) {
          if (left <= c.data.length) { found = [c, left]; return; }
          left -= c.data.length;
        } else if (c.nodeType === 1) {
          if (c.localName === "br") {
            if (c.classList.contains("ty-tail")) continue;
            if (left === 0) { found = [n, [...n.childNodes].indexOf(c)]; return; }
            left -= 1;
          } else walk(c);
        }
      }
    };
    walk(root);
    if (found) return found;
    const tail = root.querySelector(":scope > br.ty-tail");
    return [root, tail ? [...root.childNodes].indexOf(tail) : root.childNodes.length];
  }

  function getSel(root) {
    const s = getSelection();
    if (!s || !s.rangeCount) return null;
    const r = s.getRangeAt(0);
    if (!root.contains(r.startContainer)) return null;
    const a = offsetIn(root, r.startContainer, r.startOffset);
    return [a, r.collapsed ? a : offsetIn(root, r.endContainer, r.endOffset)];
  }

  function setSel(root, a, b = a) {
    const s = getSelection();
    const r = document.createRange();
    const p = pointAt(root, a);
    r.setStart(p[0], p[1]);
    if (b !== a) {
      const q = pointAt(root, b);
      r.setEnd(q[0], q[1]);
    } else r.collapse(true);
    s.removeAllRanges();
    s.addRange(r);
  }

  // Where the caret is drawn. A collapsed range at the start of a line can
  // have no box, so the character beside it stands in.
  function caretRect() {
    const s = getSelection();
    if (!s || !s.rangeCount) return null;
    const r = s.getRangeAt(0).cloneRange();
    r.collapse(s.focusNode === r.endContainer && s.focusOffset === r.endOffset ? false : true);
    let rect = r.getClientRects()[0];
    if (rect) return rect;
    const n = r.startContainer;
    if (n.nodeType === 3) {
      const o = r.startOffset;
      const x = document.createRange();
      if (o < n.data.length) { x.setStart(n, o); x.setEnd(n, o + 1); } else if (o > 0) { x.setStart(n, o - 1); x.setEnd(n, o); }
      rect = x.getClientRects()[0];
      if (rect) return rect;
    }
    const el = n.nodeType === 1 ? n : n.parentElement;
    return el ? el.getBoundingClientRect() : null;
  }

  // The source offset of a point in a rendered block: its words walked
  // against the source, the markup the reader does not see stepped over.
  function mapShown(src, shown) {
    let i = 0;
    let j = 0;
    const ws = (c) => /\s/.test(c);
    while (j < shown.length && i < src.length) {
      const s = src[i];
      const c = shown[j];
      if (s === c || (ws(s) && ws(c))) { i++; j++; }
      else if (ws(c) || src.indexOf(c, i) === -1) j++;
      else i++;
    }
    return i;
  }

  // ---- converting what is pasted

  const SKIP = new Set(["script", "style", "head", "title", "meta", "link", "template", "noscript", "input", "svg"]);

  function htmlToMd(html) {
    const doc = new DOMParser().parseFromString(html, "text/html");
    const body = doc.body;
    if (!body || !body.querySelector(
      "h1,h2,h3,h4,h5,h6,b,strong,i,em,a[href],ul,ol,table,img,pre,blockquote,code,s,del,strike,hr,[style*='font-weight'],[style*='italic']",
    )) return null;
    const wrapIn = (m, t) => {
      const x = /^(\s*)([\s\S]*?)(\s*)$/.exec(t);
      return x[2] ? x[1] + m + x[2] + m + x[3] : t;
    };
    const block = (s) => (s.trim() ? `\n\n${s}\n\n` : "");
    const tidy = (s) => s.split("\n").map((l) => l.replace(/\s+$/, "")).join("\n").replace(/\n{3,}/g, "\n\n").replace(/^\n+|\n+$/g, "");
    const cellText = (c) => tidy(kids(c, {})).replace(/\s*\n\s*/g, " ").trim().replace(/(^|[^\\])\|/g, "$1\\|");
    const rowOf = (cells) => `| ${cells.join(" | ")} |`;
    function kids(n, cx) {
      let out = "";
      for (const c of n.childNodes) out += one(c, cx);
      return out;
    }
    function list(n, cx) {
      let i = +(n.getAttribute("start") || 1);
      const lines = [];
      for (const li of n.children) {
        if (li.localName !== "li") continue;
        const marker = n.localName === "ol" ? `${i++}. ` : "- ";
        const box = li.querySelector(":scope > input[type=checkbox], :scope > p > input[type=checkbox]");
        const task = box ? (box.checked ? "[x] " : "[ ] ") : "";
        const inner = tidy(kids(li, cx)).replace(/\n{2,}/g, "\n").split("\n");
        const pad = " ".repeat(marker.length);
        lines.push(marker + task + inner[0], ...inner.slice(1).map((l) => (l ? pad + l : l)));
      }
      return lines.join("\n");
    }
    function table(n) {
      const rows = [...n.querySelectorAll("tr")].map((tr) => [...tr.children].map(cellText));
      if (!rows.length) return "";
      const w = Math.max(...rows.map((r) => r.length));
      const pad = (r) => r.concat(Array(w - r.length).fill(""));
      return [rowOf(pad(rows[0])), rowOf(Array(w).fill("---")), ...rows.slice(1).map((r) => rowOf(pad(r)))].join("\n");
    }
    function one(n, cx) {
      if (n.nodeType === 3) return cx.pre ? n.data : n.data.replace(/\s+/g, " ");
      if (n.nodeType !== 1) return "";
      const tag = n.localName;
      if (SKIP.has(tag) || tag.includes(":")) return "";
      const st = (n.getAttribute("style") || "").toLowerCase().replace(/\s/g, "");
      if (/mso-list:ignore|display:none/.test(st)) return "";
      switch (tag) {
        case "br": return "\n";
        case "h1": case "h2": case "h3": case "h4": case "h5": case "h6":
          return block(`${"#".repeat(+tag[1])} ${tidy(kids(n, cx)).replace(/\n+/g, " ")}`);
        case "p": case "div": case "section": case "article": case "header": case "footer": case "main":
          if (/MsoListParagraph/i.test(n.className || "")) return `\n- ${tidy(kids(n, cx)).replace(/\n+/g, " ")}\n`;
          return block(tidy(kids(n, cx)));
        case "blockquote":
          return block(tidy(kids(n, cx)).split("\n").map((l) => (l ? `> ${l}` : ">")).join("\n"));
        case "pre": {
          const code = n.querySelector("code");
          const lang = ((code && code.className) || n.className || "").match(/language-([\w+#-]+)/);
          return block("```" + (lang ? lang[1] : "") + "\n" + n.textContent.replace(/\n$/, "") + "\n```");
        }
        case "ul": case "ol": return block(list(n, cx));
        case "table": return block(table(n));
        case "hr": return block("---");
        case "img": {
          const src = n.getAttribute("src") || "";
          if (!/^https?:/i.test(src)) return "";
          return `![${(n.getAttribute("alt") || "").replace(/[[\]]/g, "")}](${src})`;
        }
        case "a": {
          const href = n.getAttribute("href") || "";
          const t = kids(n, cx).trim();
          if (!href || /^(javascript|data):/i.test(href) || href.startsWith("#")) return t;
          return t ? `[${t}](${href.replace(/ /g, "%20")})` : "";
        }
        case "strong": case "b":
          return /font-weight:(normal|400)/.test(st) ? kids(n, cx) : wrapIn("**", kids(n, cx));
        case "em": case "i": return wrapIn("*", kids(n, cx));
        case "s": case "del": case "strike": return wrapIn("~~", kids(n, cx));
        case "code": case "kbd": case "samp":
          return cx.pre ? n.textContent : "`" + n.textContent.replace(/`/g, "'") + "`";
        default: {
          let t = kids(n, cx);
          if (/font-weight:(bold|[6-9]00)/.test(st)) t = wrapIn("**", t);
          if (/font-style:italic/.test(st)) t = wrapIn("*", t);
          if (/line-through/.test(st)) t = wrapIn("~~", t);
          return t;
        }
      }
    }
    const md = tidy(kids(body, {}));
    return md || null;
  }

  // ---- the editor

  function makeTypora() {
    const main = document.querySelector("main.forge-md");
    let L = [""];
    let crlf = false;
    let bom = "";
    let cur = null;
    let tbl = null;
    let busy = false;
    let composing = false;
    let lastSel = [0, 0];
    const undo = [];
    const redo = [];
    let lastKind = "";
    let lastWhere = "";
    let lastAt = 0;
    let renderSeq = 0;
    let menu = null;
    let dead = false;

    // ---- lines

    const blocks = () => [...main.children].filter((e) => e.hasAttribute("data-lines"));
    const range = (e) => parseLines(e.getAttribute("data-lines"));
    const setRange = (e, a, b) => e.setAttribute("data-lines", `${a}-${b}`);
    const srcOf = (a, b) => L.slice(a - 1, b).join("\n");

    function setText(text) {
      bom = text.startsWith("﻿") ? "﻿" : "";
      const t = text.slice(bom.length);
      crlf = t.includes("\r\n");
      L = t.replace(/\r\n/g, "\n").split("\n");
    }

    function value() {
      return bom + L.join(crlf ? "\r\n" : "\n");
    }

    // Lines a..b become `lines`; every block below moves with them.
    function splice(a, b, lines) {
      const delta = lines.length - (b - a + 1);
      L.splice(a - 1, b - a + 1, ...lines);
      if (!L.length) L = [""];
      if (!delta) return;
      for (const e of blocks()) {
        const r = range(e);
        if (r && r[0] > b) setRange(e, r[0] + delta, r[1] + delta);
      }
    }

    // `lines` put after line `at` (0 puts them first).
    function insertLines(at, lines) {
      for (const e of blocks()) {
        const r = range(e);
        if (r && r[0] > at) setRange(e, r[0] + lines.length, r[1] + lines.length);
      }
      L.splice(at, 0, ...lines);
    }

    // ---- history: the whole source, with where the caret was

    function snapshot() {
      const f = cur && cur.el.isConnected ? { a: range(cur.el)[0], n: cur.src.split("\n").length, sel: lastSel.slice() } : null;
      return { L: L.slice(), f };
    }

    function record(kind, where) {
      const now = Date.now();
      const w = where || (cur ? `b${range(cur.el)[0]}` : "");
      if (kind === "type" && lastKind === "type" && w === lastWhere && now - lastAt < 1200) {
        lastAt = now;
        return;
      }
      undo.push(snapshot());
      if (undo.length > 400) undo.shift();
      redo.length = 0;
      lastKind = kind;
      lastWhere = w;
      lastAt = now;
    }

    const same = (x, y) => x.length === y.length && x.every((v, i) => v === y[i]);

    function step(from, to) {
      const s = from.pop();
      if (!s) return;
      to.push(snapshot());
      lastKind = "";
      if (cur && s.f && s.f.a === range(cur.el)[0]) {
        const a = s.f.a;
        const n = cur.src.split("\n").length;
        if (same(L.slice(0, a - 1), s.L.slice(0, a - 1)) && same(L.slice(a - 1 + n), s.L.slice(a - 1 + s.f.n))) {
          const src = s.L.slice(a - 1, a - 1 + s.f.n).join("\n");
          splice(a, a + n - 1, src.split("\n"));
          L = s.L.slice();
          setRange(cur.el, a, a + s.f.n - 1);
          cur.src = src;
          paint(s.f.sel);
          changed();
          return;
        }
      }
      L = s.L.slice();
      redraw().then(() => {
        if (!s.f) return;
        const el = blockAt(s.f.a);
        if (el) edit(el, s.f.sel);
      });
      changed();
    }

    function changed() {
      if (!conflict) schedule();
      else keepSession();
    }

    // ---- drawing

    function prepare(root) {
      for (const box of root.querySelectorAll("li.task > input[type=checkbox]")) {
        box.removeAttribute("disabled");
        box.tabIndex = -1;
      }
    }

    // The page as the host renders it, when what is shown is not already
    // that: the file may have moved since the page was made.
    function matches(html) {
      const t = document.createElement("template");
      t.innerHTML = html;
      const want = [...t.content.children];
      const have = blocks();
      if (want.length !== have.length) return false;
      return want.every((w, i) => {
        const h = have[i];
        if (w.localName !== h.localName || w.getAttribute("data-lines") !== h.getAttribute("data-lines")) return false;
        if (w.matches("pre.mermaid")) return (h.dataset.forgeSource ?? h.textContent) === w.textContent;
        return w.outerHTML === h.outerHTML;
      });
    }

    async function redraw(fresh = true) {
      busy = true;
      const html = await renderMd(L.join("\n"));
      busy = false;
      if (dead || html == null) return false;
      closeTable(false);
      if (cur) { unwire(cur.el); cur = null; }
      hideFloaters();
      if (fresh || !matches(html)) main.innerHTML = html;
      prepare(main);
      drawMermaid(main);
      return true;
    }

    function blockAt(line) {
      let best = null;
      for (const e of blocks()) {
        const r = range(e);
        if (r[0] <= line && line <= r[1]) return e;
        if (r[0] > line) return best || e;
        best = e;
      }
      return best;
    }

    // Draws `el`'s lines again from the host, when they are still `src`.
    function renderInto(el, src, then) {
      const id = String(++renderSeq);
      el.dataset.tyRender = id;
      el.classList.add("forge-ty-pending");
      renderMd(src).then((html) => {
        if (dead || !el.isConnected || el.dataset.tyRender !== id || (cur && cur.el === el)) return;
        const r = range(el);
        if (!r || srcOf(r[0], r[1]) !== src) return;
        if (html == null) {
          // No host to draw it: the source stays, as text.
          el.classList.remove("forge-ty-pending");
          if (then) then([el]);
          return;
        }
        const t = document.createElement("template");
        t.innerHTML = html;
        const nodes = [...t.content.children];
        for (const n of nodes) {
          const nr = range(n);
          if (nr) setRange(n, nr[0] + r[0] - 1, nr[1] + r[0] - 1);
          else setRange(n, r[0], r[1]);
        }
        prepare(t.content);
        el.replaceWith(...nodes);
        drawMermaid(main);
        if (then) then(nodes);
      });
    }

    // ---- the block written in

    function wire(div) {
      div.addEventListener("keydown", onKey);
      div.addEventListener("input", onInput);
      div.addEventListener("beforeinput", onBeforeInput);
      div.addEventListener("paste", onPaste);
      div.addEventListener("compositionstart", onCompStart);
      div.addEventListener("compositionend", onCompEnd);
    }
    function unwire(div) {
      div.removeEventListener("keydown", onKey);
      div.removeEventListener("input", onInput);
      div.removeEventListener("beforeinput", onBeforeInput);
      div.removeEventListener("paste", onPaste);
      div.removeEventListener("compositionstart", onCompStart);
      div.removeEventListener("compositionend", onCompEnd);
      div.removeAttribute("contenteditable");
      div.classList.remove("forge-ty-focus");
    }

    function sourceDiv(a, b, src) {
      const d = el("div", "forge-ty-src");
      setRange(d, a, b);
      d.innerHTML = hi(src);
      return d;
    }

    function caretOf(c, src) {
      if (c === "end") return [src.length, src.length];
      if (Array.isArray(c)) return [Math.min(c[0], src.length), Math.min(c[1], src.length)];
      const n = Math.min(Math.max(+c || 0, 0), src.length);
      return [n, n];
    }

    // Starts writing in `target`: a block as rendered, or a source block
    // still waiting to be drawn.
    function edit(target, caret) {
      if (busy || dead || !target || !target.isConnected) return;
      if (cur && cur.el === target) {
        const c = caretOf(caret, cur.src);
        target.focus({ preventScroll: true });
        setSel(target, c[0], c[1]);
        return;
      }
      if (target.localName === "table") {
        commit();
        openTable(target, caret === "end" ? "end" : 0);
        return;
      }
      closeTable(true);
      commit();
      if (!target.isConnected) return;
      const [a, b] = range(target);
      const src = srcOf(a, b);
      let div = target;
      let orig = null;
      if (target.classList.contains("forge-ty-src")) {
        delete target.dataset.tyRender;
        target.classList.remove("forge-ty-pending");
      } else {
        div = sourceDiv(a, b, src);
        orig = target;
        target.replaceWith(div);
      }
      take(div, src, orig, caret);
    }

    function take(div, src, orig, caret) {
      cur = { el: div, src, orig, origSrc: src };
      div.setAttribute("contenteditable", "plaintext-only");
      div.spellcheck = false;
      div.classList.add("forge-ty-focus");
      wire(div);
      paint(caretOf(caret, src));
      div.focus({ preventScroll: true });
      const c = caretOf(caret, src);
      setSel(div, c[0], c[1]);
      lastSel = c;
      const r = div.getBoundingClientRect();
      if (r.top < 0 || r.bottom > innerHeight) {
        const cr = caretRect();
        if (cr && (cr.top < 0 || cr.bottom > innerHeight)) div.scrollIntoView({ block: "nearest" });
      }
      updateMenu();
      placeFloaters();
    }

    function paint(caret, keepFocus) {
      const c = caret || getSel(cur.el);
      cur.el.innerHTML = hi(cur.src);
      cur.el.classList.toggle("ty-fenced", FENCE.test(cur.src.split("\n")[0]));
      if (c && !keepFocus) setSel(cur.el, c[0], c[1]);
      if (c) lastSel = c;
      placeFloaters();
    }

    // The block's source is now `src`.
    function apply(src, caret, kind, keepFocus) {
      if (!cur) return;
      if (src === cur.src) {
        if (caret && !keepFocus) setSel(cur.el, caret[0], caret[1]);
        return;
      }
      record(kind || "edit");
      const [a, b] = range(cur.el);
      const lines = src.split("\n");
      splice(a, b, lines);
      setRange(cur.el, a, a + lines.length - 1);
      cur.src = src;
      paint(caret, keepFocus);
      updateMenu();
      changed();
    }

    // Leaves the block written in: drawn again when it changed, put back
    // as it was when not, gone with a blank line beside it when emptied.
    function commit(then) {
      const c = cur;
      if (!c) { if (then) then([]); return; }
      cur = null;
      hideMenu();
      hideLang();
      const div = c.el;
      unwire(div);
      if (!div.isConnected) { if (then) then([]); return; }
      const [a, b] = range(div);
      if (!c.src.trim()) {
        let from = a;
        let to = b;
        if (L[to] !== undefined && L[to].trim() === "" && !(from === 1 && to + 1 === L.length)) to++;
        else if (from > 1 && L[from - 2].trim() === "") from--;
        div.remove();
        if (L.length > 1 || L[0] !== "") splice(from, to, []);
        if (then) then([]);
        return;
      }
      if (c.orig && c.src === c.origSrc) {
        setRange(c.orig, a, b);
        div.replaceWith(c.orig);
        if (then) then([c.orig]);
        return;
      }
      renderInto(div, c.src, then);
    }

    // The block written in becomes `parts`, one blank line between each;
    // the caret goes into part `focus`.
    function split(parts, focus, caret) {
      const [a, b] = range(cur.el);
      record("struct");
      const lines = [];
      const spans = [];
      parts.forEach((p, i) => {
        if (i) lines.push("");
        const ls = p.split("\n");
        spans.push([a + lines.length, a + lines.length + ls.length - 1]);
        lines.push(...ls);
      });
      splice(a, b, lines);
      const old = cur;
      cur = null;
      hideMenu();
      hideLang();
      unwire(old.el);
      const els = parts.map((p, i) => sourceDiv(spans[i][0], spans[i][1], p));
      old.el.replaceWith(...els);
      els.forEach((d, i) => { if (i !== focus) renderInto(d, parts[i]); });
      take(els[focus], parts[focus], null, caret);
      changed();
    }

    // A new block of `src` after line `at`, kept apart by blank lines.
    function insertBlock(at, src, focus) {
      record("struct");
      const lines = src.split("\n");
      const before = blocks().find((e) => range(e)[0] > at) || null;
      if (L.every((l) => l.trim() === "")) {
        const keep = L.length;
        L = lines.concat(keep > 1 || src === "" ? [""] : []);
        if (src === "" && L.length > 1) L = [""];
        const d = sourceDiv(1, lines.length, src);
        main.insertBefore(d, before);
        if (focus) take(d, src, null, "end"); else renderInto(d, src);
        changed();
        return d;
      }
      const ins = [];
      if (at > 0 && L[at - 1] !== undefined && L[at - 1].trim() !== "") ins.push("");
      const start = at + ins.length + 1;
      ins.push(...lines);
      if (L[at] !== undefined && L[at].trim() !== "") ins.push("");
      insertLines(at, ins);
      const d = sourceDiv(start, start + lines.length - 1, src);
      main.insertBefore(d, before);
      if (focus) take(d, src, null, "end"); else renderInto(d, src);
      changed();
      return d;
    }

    function lastLine() {
      const bs = blocks();
      if (bs.length) return range(bs[bs.length - 1])[1];
      let n = L.length;
      while (n > 0 && !L[n - 1].trim()) n--;
      return n;
    }

    function siblingBlock(e, dir) {
      let n = dir < 0 ? e.previousElementSibling : e.nextElementSibling;
      while (n && !n.hasAttribute("data-lines")) n = dir < 0 ? n.previousElementSibling : n.nextElementSibling;
      return n;
    }

    const textBlock = (e) => e && (e.matches("p, h1, h2, h3, h4, h5, h6, ul, ol, blockquote") ||
      (e.classList.contains("forge-ty-src") && !FENCE.test(srcOf(...range(e)).split("\n")[0])));

    // `second`'s source joined onto the end of `first`'s; one of them is
    // the block written in, which takes the whole.
    function merge(first, second) {
      const [pa, pb] = range(first);
      const [a, b] = range(second);
      const ps = srcOf(pa, pb);
      let cs = srcOf(a, b);
      if (/^(p|div)$/.test(first.localName)) cs = cs.replace(/^\s{0,3}(#{1,6}\s+|>\s?)/, "");
      const src = ps + cs;
      record("struct");
      const keep = cur.el;
      const gone = keep === first ? second : first;
      splice(pa, b, src.split("\n"));
      gone.remove();
      setRange(keep, pa, pa + src.split("\n").length - 1);
      cur.src = src;
      cur.orig = null;
      paint([ps.length, ps.length]);
      changed();
    }

    function dropBlock(e) {
      const [a, b] = range(e);
      let to = b;
      if (L[to] !== undefined && L[to].trim() === "") to++;
      record("struct");
      e.remove();
      splice(a, to, []);
      changed();
    }

    // ---- keys in the block written in

    function selOr() {
      return getSel(cur.el) || lastSel;
    }

    function lineAround(src, c) {
      const ls = src.lastIndexOf("\n", c - 1) + 1;
      let le = src.indexOf("\n", c);
      if (le < 0) le = src.length;
      return [ls, le, src.slice(ls, le)];
    }

    function closes(line, fence) {
      const m = FENCE.exec(line);
      return !!m && m[2][0] === fence[0] && m[2].length >= fence.length && !m[4] && !m[5].trim();
    }

    function enter(shift) {
      let [s, e] = selOr();
      let src = cur.src;
      if (s !== e) { src = src.slice(0, s) + src.slice(e); e = s; }
      const c = s;
      const [ls, le, line] = lineAround(src, c);
      const lines = src.split("\n");
      const fm = FENCE.exec(lines[0]);
      if (fm && !(fm[2][0] === "`" && fm[5].includes("`"))) {
        const closed = lines.length > 1 && closes(lines[lines.length - 1], fm[2]);
        if (closed && c === src.length && !shift) { split([src, ""], 1, 0); return; }
        if (lines.length === 1 && c === src.length) {
          apply(`${src}\n\n${fm[1]}${fm[2]}`, [c + 1, c + 1], "struct");
          return;
        }
        const ind = /^\s*/.exec(line)[0];
        apply(src.slice(0, c) + "\n" + ind + src.slice(c), [c + 1 + ind.length, c + 1 + ind.length], "struct");
        return;
      }
      if (!shift && lines.length === 1 && /^\s*\|.*\|\s*$/.test(src) && c === src.length) {
        const head = splitRow(src);
        const n = Math.max(head.length, 1);
        const t = [rowLine(head), rowLine(Array(n).fill("---")), rowLine(Array(n).fill(""))].join("\n");
        apply(t, [t.length, t.length], "struct");
        commit((nodes) => {
          const table = nodes.find((x) => x.localName === "table");
          if (table) openTable(table, [1, 0]);
        });
        return;
      }
      let m = LIST.exec(line);
      const q = !m && QUOTE.exec(line);
      if (shift) {
        const pad = m ? " ".repeat(m[0].length) : q ? q[0] : "";
        apply(src.slice(0, c) + "\n" + pad + src.slice(c), [c + 1 + pad.length, c + 1 + pad.length], "struct");
        return;
      }
      if ((m || q) && c >= ls + (m || q)[0].length) {
        const lead = (m || q)[0];
        if (line.slice(lead.length).trim() === "") {
          // An empty item or quote line: the list or quote ends here and a
          // paragraph starts under it.
          const before = src.slice(0, ls).replace(/\n$/, "");
          const after = src.slice(le + 1);
          const parts = [];
          if (before.trim()) parts.push(before);
          const at = parts.length;
          parts.push("");
          if (after.trim()) parts.push(after);
          split(parts, at, 0);
          return;
        }
        let next = lead;
        if (m) {
          const n = /^\d+/.exec(m[2]);
          const marker = n ? `${+n[0] + 1}${m[2].slice(n[0].length)}` : m[2];
          next = m[1] + marker + m[3] + (m[4] ? "[ ] " : "");
        }
        apply(src.slice(0, c) + "\n" + next + src.slice(c), [c + 1 + next.length, c + 1 + next.length], "struct");
        return;
      }
      if (c === 0 && src) { split(["", src], 0, 0); return; }
      split([src.slice(0, c).replace(/[ \t]+$/, ""), src.slice(c)], 1, 0);
    }

    function wrap(m) {
      const [s, e] = selOr();
      const v = cur.src;
      const n = m.length;
      if (s >= n && v.slice(s - n, s) === m && v.slice(e, e + n) === m) {
        apply(v.slice(0, s - n) + v.slice(s, e) + v.slice(e + n), [s - n, e - n]);
      } else {
        apply(v.slice(0, s) + m + v.slice(s, e) + m + v.slice(e), [s + n, e + n]);
      }
    }

    function link() {
      const [s, e] = selOr();
      const v = cur.src;
      const sel = v.slice(s, e);
      if (/^https?:\/\/\S+$/.test(sel)) {
        apply(`${v.slice(0, s)}[](${sel})${v.slice(e)}`, [s + 1, s + 1]);
      } else {
        const at = s + sel.length + 3;
        apply(`${v.slice(0, s)}[${sel}](url)${v.slice(e)}`, sel ? [at, at + 3] : [s + 1, s + 1]);
      }
    }

    function heading(n) {
      const [s, e] = selOr();
      const v = cur.src;
      const [ls, le, line] = lineAround(v, s);
      const m = /^(\s{0,3})(#{1,6}\s+)?/.exec(line);
      const old = (m[2] || "").length;
      const add = n ? `${"#".repeat(n)} ` : "";
      const nl = m[1] + add + line.slice(m[1].length + old);
      const d = add.length - old;
      apply(v.slice(0, ls) + nl + v.slice(le), [Math.max(ls, s + d), Math.max(ls, e + d)]);
    }

    function caretOnEdge(up) {
      const s = getSelection();
      if (!s || !s.rangeCount || !s.modify) return true;
      const saved = s.getRangeAt(0).cloneRange();
      const before = caretRect();
      s.modify("move", up ? "backward" : "forward", "line");
      const after = caretRect();
      const inside = s.rangeCount && cur.el.contains(s.focusNode);
      const moved = inside && before && after && (up ? after.top < before.top - 2 : after.top > before.top + 2);
      if (!moved) {
        s.removeAllRanges();
        s.addRange(saved);
      }
      return !moved;
    }

    function onKey(e) {
      if (e.isComposing || composing || !cur) return;
      const mod = (e.ctrlKey || e.metaKey) && !e.altKey;
      const k = e.key;
      if (menu && !menu.hidden) {
        if (k === "ArrowDown" || k === "ArrowUp") {
          e.preventDefault();
          const n = menu.items.length;
          menu.i = (menu.i + (k === "ArrowDown" ? 1 : n - 1)) % n;
          drawMenu();
          return;
        }
        if ((k === "Enter" || k === "Tab") && !mod) { e.preventDefault(); pick(menu.items[menu.i]); return; }
        if (k === "Escape") { e.preventDefault(); menu.off = cur.src; hideMenu(); return; }
      }
      if (mod && !e.shiftKey) {
        const l = k.toLowerCase();
        if (l === "b") { e.preventDefault(); wrap("**"); return; }
        if (l === "i") { e.preventDefault(); wrap("*"); return; }
        if (l === "e") { e.preventDefault(); wrap("`"); return; }
        if (l === "k") { e.preventDefault(); link(); return; }
        if (/^[0-6]$/.test(k)) { e.preventDefault(); heading(+k); return; }
      }
      if (mod || e.altKey) return;
      const [s, t] = selOr();
      const v = cur.src;
      if (k === "Enter") { e.preventDefault(); enter(e.shiftKey); return; }
      if (k === "Escape") { e.preventDefault(); commit(); getSelection().removeAllRanges(); return; }
      if (k === "Tab") {
        e.preventDefault();
        const [ls, , line] = lineAround(v, s);
        if (e.shiftKey) {
          const n = /^( {1,4}|\t)?/.exec(line)[0].length;
          if (n) apply(v.slice(0, ls) + v.slice(ls + n), [Math.max(ls, s - n), Math.max(ls, t - n)]);
        } else if (LIST.test(line) || s !== t) {
          apply(v.slice(0, ls) + "  " + v.slice(ls), [s + 2, t + 2]);
        } else {
          apply(v.slice(0, s) + "  " + v.slice(t), [s + 2, s + 2]);
        }
        return;
      }
      if (e.shiftKey) return;
      if (k === "Backspace" && s === 0 && t === 0) {
        e.preventDefault();
        const prev = siblingBlock(cur.el, -1);
        if (!prev) return;
        if (prev.localName === "hr") dropBlock(prev);
        else if (textBlock(prev)) merge(prev, cur.el);
        else edit(prev, "end");
        return;
      }
      if (k === "Delete" && s === v.length && t === s) {
        e.preventDefault();
        const next = siblingBlock(cur.el, 1);
        if (!next) return;
        if (next.localName === "hr") dropBlock(next);
        else if (textBlock(next) && textBlock(cur.el)) merge(cur.el, next);
        return;
      }
      if (k === "ArrowLeft" && s === 0 && t === 0) {
        const prev = siblingBlock(cur.el, -1);
        if (prev) { e.preventDefault(); edit(prev, "end"); }
        return;
      }
      if (k === "ArrowRight" && s === v.length && t === s) {
        const next = siblingBlock(cur.el, 1);
        if (next) { e.preventDefault(); edit(next, 0); }
        return;
      }
      if (k === "ArrowUp" || k === "ArrowDown") {
        const up = k === "ArrowUp";
        e.preventDefault();
        if (!caretOnEdge(up)) return;
        const to = siblingBlock(cur.el, up ? -1 : 1);
        if (to) edit(to, up ? "end" : 0);
      }
    }

    function onBeforeInput(e) {
      if (busy) { e.preventDefault(); return; }
      const t = e.inputType;
      if (t === "historyUndo" || t === "historyRedo") {
        e.preventDefault();
        if (t === "historyUndo") step(undo, redo); else step(redo, undo);
      } else if (t === "insertParagraph" || t === "insertLineBreak") {
        e.preventDefault();
        if (cur) enter(t === "insertLineBreak");
      } else if (t.startsWith("format")) {
        e.preventDefault();
      }
    }

    function onInput() {
      if (composing || !cur) return;
      const src = readText(cur.el);
      if (src === cur.src) return;
      apply(src, getSel(cur.el), "type");
    }

    function onCompStart() { composing = true; }
    function onCompEnd() { composing = false; setTimeout(onInput, 0); }

    // ---- pasting and dropping

    function insertText(t) {
      const [s, e] = selOr();
      const v = cur.src;
      apply(v.slice(0, s) + t + v.slice(e), [s + t.length, s + t.length], "struct");
    }

    function inCode() {
      return !!cur && FENCE.test(cur.src.split("\n")[0]);
    }

    function imageOf(dt) {
      for (const f of dt.files || []) if (/^image\//.test(f.type)) return f;
      for (const i of dt.items || []) if (i.kind === "file" && /^image\//.test(i.type)) return i.getAsFile();
      return null;
    }

    function onPaste(e) {
      const dt = e.clipboardData;
      if (!dt || !cur) return;
      e.preventDefault();
      const img = imageOf(dt);
      if (img) { putImage(img, cur.el); return; }
      const plain = dt.getData("text/plain");
      const html = dt.getData("text/html");
      let text = plain;
      if (html && !inCode()) {
        const md = htmlToMd(html);
        if (md) text = md;
      }
      if (text) insertText(text.replace(/\r\n?/g, "\n"));
    }

    async function saveImage(file) {
      if (file.size > 20 << 20) { note("The picture is larger than 20 MB", true); return null; }
      const data = await new Promise((resolve) => {
        const r = new FileReader();
        r.onload = () => resolve(String(r.result));
        r.onerror = () => resolve(null);
        r.readAsDataURL(file);
      });
      if (!data) return null;
      const m = await ask({ forge: "save-image", file: FILE, name: file.name || "image.png", data: data.slice(data.indexOf(",") + 1) }, 60000);
      if (!m || !m.ok || typeof m.path !== "string") {
        note(`Picture not saved${m && m.why ? `: ${m.why}` : ""}`, true);
        return null;
      }
      return m.path.split("/").map(encodeURIComponent).join("/");
    }

    // A picture goes in where the caret was when it is the same block,
    // else as a block of its own after `near`.
    async function putImage(file, near) {
      const path = await saveImage(file);
      if (!path || dead) return;
      const md = `![](${path})`;
      if (cur && cur.el === near) { insertText(md); return; }
      const at = near && near.isConnected && range(near) ? range(near)[1] : lastLine();
      insertBlock(at, md, false);
    }

    function onDragOver(e) {
      if (e.dataTransfer && [...e.dataTransfer.types].includes("Files")) e.preventDefault();
    }

    function onDrop(e) {
      const dt = e.dataTransfer;
      if (!dt || !dt.files.length) return;
      const imgs = [...dt.files].filter((f) => /^image\//.test(f.type));
      if (!imgs.length) return;
      e.preventDefault();
      let near = topBlock(e.target);
      if (cur && cur.el === near) {
        const r = document.caretRangeFromPoint(e.clientX, e.clientY);
        if (r && cur.el.contains(r.startContainer)) {
          const o = offsetIn(cur.el, r.startContainer, r.startOffset);
          setSel(cur.el, o);
          lastSel = [o, o];
        }
      } else if (!near) near = blocks().pop() || null;
      (async () => { for (const f of imgs) await putImage(f, near); })();
    }

    // ---- the slash menu

    function updateMenu() {
      const m = cur && /^\/([^\n/]*)$/.exec(cur.src);
      if (!m || (menu && menu.off === cur.src)) { hideMenu(); return; }
      const q = m[1].trim().toLowerCase();
      const items = SLASH.filter((it) => !q || it.label.toLowerCase().includes(q) || it.key.startsWith(q));
      if (!items.length) { hideMenu(); return; }
      if (!menu) menu = { i: 0 };
      menu.items = items;
      menu.hidden = false;
      menu.i = Math.min(menu.i, items.length - 1);
      drawMenu();
    }

    function drawMenu() {
      if (!ui || !ui.menu) return;
      const box = ui.menu;
      box.textContent = "";
      menu.items.forEach((it, i) => {
        const row = el("div", `forge-ty-item${i === menu.i ? " on" : ""}`);
        row.append(el("span", "forge-ty-item-label", it.label), el("kbd", null, it.hint));
        row.addEventListener("mousedown", (e) => { e.preventDefault(); pick(it); });
        box.appendChild(row);
      });
      box.hidden = false;
      placeFloaters();
    }

    function hideMenu() {
      if (menu) menu.hidden = true;
      if (ui && ui.menu) ui.menu.hidden = true;
    }

    function pickFile() {
      return new Promise((resolve) => {
        const input = document.createElement("input");
        input.type = "file";
        input.accept = "image/png,image/jpeg,image/gif,image/webp";
        input.addEventListener("change", () => resolve(input.files[0] || null), { once: true });
        input.click();
      });
    }

    function pick(it) {
      if (!cur || !it) return;
      hideMenu();
      if (menu) menu.off = null;
      const set = (src, at) => apply(src, [at == null ? src.length : at, at == null ? src.length : at], "struct");
      switch (it.key) {
        case "h1": set("# "); break;
        case "h2": set("## "); break;
        case "h3": set("### "); break;
        case "bullet": set("- "); break;
        case "numbered": set("1. "); break;
        case "task": set("- [ ] "); break;
        case "quote": set("> "); break;
        case "code":
          set("```\n\n```", 4);
          placeFloaters();
          if (ui && ui.lang && !ui.lang.hidden) ui.lang.focus();
          break;
        case "mermaid": {
          const src = "```mermaid\nflowchart TD\n  A[Start] --> B[Finish]\n```";
          set(src, src.indexOf("\n  ") + 1 + "  A[Start] --> B[Finish]".length);
          break;
        }
        case "divider":
          set("---");
          split(["---", ""], 1, 0);
          break;
        case "table": {
          const t = "| Column 1 | Column 2 |\n| --- | --- |\n|  |  |";
          set(t);
          commit((nodes) => {
            const table = nodes.find((x) => x.localName === "table");
            if (table) openTable(table, [0, 0], true);
          });
          break;
        }
        case "image": {
          set("![]()", 4);
          const div = cur.el;
          pickFile().then(async (file) => {
            if (!file) return;
            const path = await saveImage(file);
            if (!path || !cur || cur.el !== div) return;
            apply(`![](${path})`, null, "struct");
            const n = cur.src.length;
            setSel(div, n);
          });
          break;
        }
        default: break;
      }
    }

    // ---- a code block's language

    function hideLang() {
      if (ui && ui.lang) ui.lang.hidden = true;
    }

    function onLangInput() {
      if (!cur) return;
      const lines = cur.src.split("\n");
      const m = FENCE.exec(lines[0]);
      if (!m) return;
      const lang = ui.lang.value.replace(/[\s`]/g, "");
      const first = m[1] + m[2] + (lang ? m[3] : "") + lang + m[5];
      const d = first.length - lines[0].length;
      lines[0] = first;
      const c = lastSel;
      apply(lines.join("\n"), [Math.max(0, c[0] + d), Math.max(0, c[1] + d)], "type", true);
    }

    function onLangKey(e) {
      if (e.key === "Enter" || e.key === "Escape" || e.key === "ArrowDown") {
        e.preventDefault();
        if (!cur) return;
        const at = cur.src.indexOf("\n") + 1 || cur.src.length;
        edit(cur.el, at);
      }
    }

    // ---- tables, written in cell by cell

    function splitRow(line) {
      let t = line.trim();
      if (t.startsWith("|")) t = t.slice(1);
      if (t.endsWith("|") && !t.endsWith("\\|")) t = t.slice(0, -1);
      return t.split(/(?<!\\)\|/).map((c) => c.trim());
    }

    function rowLine(cells) {
      return `| ${cells.join(" | ")} |`;
    }

    function cellAt(t, r, c) {
      const row = r === 0 ? t.el.tHead && t.el.tHead.rows[0] : t.el.tBodies[0] && t.el.tBodies[0].rows[r - 1];
      return row ? row.cells[c] : null;
    }

    function posOf(t, cell) {
      const tr = cell.parentElement;
      const r = tr.parentElement.localName === "thead" ? 0 : [...t.el.tBodies[0].rows].indexOf(tr) + 1;
      return [r, [...tr.cells].indexOf(cell)];
    }

    function openTable(table, at, selectAll) {
      closeTable(true);
      const [a, b] = range(table);
      const lines = L.slice(a - 1, b);
      if (lines.length < 2) { edit(table, 0); return; }
      const rows = [splitRow(lines[0]), ...lines.slice(2).map(splitRow)];
      const align = splitRow(lines[1]);
      const w = Math.max(align.length, ...rows.map((r) => r.length));
      for (const r of rows) while (r.length < w) r.push("");
      while (align.length < w) align.push("---");
      tbl = { el: table, rows, align, w, cell: null, changed: false };
      table.classList.add("forge-ty-focus", "forge-ty-table");
      let r = 0;
      let c = 0;
      if (at === "end") { r = rows.length - 1; c = w - 1; } else if (Array.isArray(at)) { [r, c] = at; }
      focusCell(r, c, selectAll);
    }

    function tableLine(r) {
      const a = range(tbl.el)[0];
      return r === 0 ? a : a + 1 + r;
    }

    function focusCell(r, c, selectAll) {
      if (!tbl) return;
      blurCell();
      r = Math.min(Math.max(r, 0), tbl.rows.length - 1);
      c = Math.min(Math.max(c, 0), tbl.w - 1);
      let td = cellAt(tbl, r, c);
      if (!td) {
        // The renderer leaves a short row short: the cell is made.
        const tr = r === 0 ? tbl.el.tHead.rows[0] : tbl.el.tBodies[0].rows[r - 1];
        if (!tr) return;
        while (tr.cells.length <= c) tr.appendChild(document.createElement(r === 0 ? "th" : "td"));
        td = tr.cells[c];
      }
      tbl.cell = { td, r, c, html: td.innerHTML, src: tbl.rows[r][c] };
      td.textContent = tbl.rows[r][c];
      td.setAttribute("contenteditable", "plaintext-only");
      td.classList.add("forge-ty-cell");
      td.addEventListener("keydown", onCellKey);
      td.addEventListener("input", onCellInput);
      td.addEventListener("paste", onCellPaste);
      td.focus({ preventScroll: true });
      const n = td.textContent.length;
      if (selectAll && n) setSel(td, 0, n); else setSel(td, n);
      td.scrollIntoView({ block: "nearest", inline: "nearest" });
      placeFloaters();
    }

    function blurCell() {
      const cell = tbl && tbl.cell;
      if (!cell) return;
      tbl.cell = null;
      const td = cell.td;
      td.removeEventListener("keydown", onCellKey);
      td.removeEventListener("input", onCellInput);
      td.removeEventListener("paste", onCellPaste);
      td.removeAttribute("contenteditable");
      td.classList.remove("forge-ty-cell");
      const src = tbl.rows[cell.r] ? tbl.rows[cell.r][cell.c] : "";
      if (src === cell.src) { td.innerHTML = cell.html; return; }
      td.textContent = src;
      if (!src.trim()) return;
      renderMd(src).then((html) => {
        if (html == null || !td.isConnected || td.isContentEditable || td.textContent !== src) return;
        const t = document.createElement("template");
        t.innerHTML = html;
        const p = t.content.firstElementChild;
        td.innerHTML = p && p.localName === "p" ? p.innerHTML : html;
      });
    }

    function writeRow(r) {
      const n = tableLine(r);
      splice(n, n, [rowLine(tbl.rows[r])]);
      tbl.changed = true;
      changed();
    }

    function writeTable() {
      const [a, b] = range(tbl.el);
      const lines = [rowLine(tbl.rows[0]), rowLine(tbl.align), ...tbl.rows.slice(1).map(rowLine)];
      splice(a, b, lines);
      setRange(tbl.el, a, a + lines.length - 1);
      tbl.changed = true;
      changed();
    }

    function onCellInput() {
      const cell = tbl && tbl.cell;
      if (!cell) return;
      const v = readText(cell.td).replace(/\n/g, " ").replace(/(^|[^\\])\|/g, "$1\\|").replace(/(^|[^\\])\|/g, "$1\\|");
      if (v === tbl.rows[cell.r][cell.c]) return;
      record("type", `t${range(tbl.el)[0]}:${cell.r}:${cell.c}`);
      tbl.rows[cell.r][cell.c] = v;
      writeRow(cell.r);
    }

    function onCellPaste(e) {
      const t = e.clipboardData && e.clipboardData.getData("text/plain");
      e.preventDefault();
      if (t) document.execCommand("insertText", false, t.replace(/\s*\r?\n\s*/g, " "));
    }

    function addRow(after) {
      if (!tbl) return;
      blurCell();
      record("struct");
      const row = Array(tbl.w).fill("");
      tbl.rows.splice(after + 1, 0, row);
      const [a, b] = range(tbl.el);
      insertLines(a + 1 + after, [rowLine(row)]);
      setRange(tbl.el, a, b + 1);
      let body = tbl.el.tBodies[0];
      if (!body) body = tbl.el.appendChild(document.createElement("tbody"));
      const tr = document.createElement("tr");
      for (let c = 0; c < tbl.w; c++) tr.appendChild(document.createElement("td"));
      body.insertBefore(tr, body.rows[after] || null);
      tbl.changed = true;
      changed();
      return after + 1;
    }

    function addCol(after) {
      if (!tbl) return;
      blurCell();
      record("struct");
      for (const r of tbl.rows) r.splice(after + 1, 0, "");
      tbl.align.splice(after + 1, 0, "---");
      tbl.w++;
      for (const tr of tbl.el.rows) {
        const cellEl = document.createElement(tr.parentElement.localName === "thead" ? "th" : "td");
        tr.insertBefore(cellEl, tr.cells[after + 1] || null);
      }
      writeTable();
    }

    function delRow(r) {
      if (!tbl || r < 1) return;
      blurCell();
      record("struct");
      tbl.rows.splice(r, 1);
      const tr = tbl.el.tBodies[0] && tbl.el.tBodies[0].rows[r - 1];
      if (tr) tr.remove();
      writeTable();
    }

    function delCol(c) {
      if (!tbl || tbl.w < 2) return;
      blurCell();
      record("struct");
      for (const r of tbl.rows) r.splice(c, 1);
      tbl.align.splice(c, 1);
      tbl.w--;
      for (const tr of tbl.el.rows) if (tr.cells[c]) tr.cells[c].remove();
      writeTable();
    }

    function leaveTable(dir) {
      const t = tbl && tbl.el;
      if (!t) return;
      const to = siblingBlock(t, dir);
      closeTable(true);
      if (to) edit(to, dir < 0 ? "end" : 0);
      else if (dir > 0) insertBlock(range(blocks().pop())[1], "", true);
    }

    function onCellKey(e) {
      if (e.isComposing || !tbl || !tbl.cell) return;
      const { r, c } = tbl.cell;
      const mod = (e.ctrlKey || e.metaKey) && !e.altKey;
      const k = e.key;
      const last = r === tbl.rows.length - 1;
      // A cell reached by a key has its words chosen, so typing replaces
      // them, as in a spreadsheet.
      const go = (rr, cc) => focusCell(rr, cc, true);
      if (k === "Tab") {
        e.preventDefault();
        if (e.shiftKey) {
          if (c > 0) go(r, c - 1); else if (r > 0) go(r - 1, tbl.w - 1);
        } else if (c < tbl.w - 1) go(r, c + 1);
        else if (!last) go(r + 1, 0);
        else go(addRow(r), 0);
        return;
      }
      if (k === "Enter") {
        e.preventDefault();
        if (mod) go(addRow(r), c);
        else if (e.shiftKey) { if (r > 0) go(r - 1, c); }
        else if (!last) go(r + 1, c);
        else go(addRow(r), c);
        return;
      }
      if (k === "ArrowUp" && !e.shiftKey && !mod) {
        e.preventDefault();
        if (r > 0) go(r - 1, c); else leaveTable(-1);
        return;
      }
      if (k === "ArrowDown" && !e.shiftKey && !mod) {
        e.preventDefault();
        if (!last) go(r + 1, c); else leaveTable(1);
        return;
      }
      if (k === "Escape") { e.preventDefault(); closeTable(true); getSelection().removeAllRanges(); }
    }

    function closeTable(draw) {
      if (!tbl) return;
      const t = tbl;
      blurCell();
      tbl = null;
      t.el.classList.remove("forge-ty-focus", "forge-ty-table");
      if (ui && ui.tools) ui.tools.hidden = true;
      if (draw && t.changed && t.el.isConnected) {
        const [a, b] = range(t.el);
        renderInto(t.el, srcOf(a, b));
      }
    }

    // ---- tasks

    function toggleTask(box) {
      const block = topBlock(box);
      if (!block) return;
      const [a, b] = range(block);
      const k = [...block.querySelectorAll("li.task > input[type=checkbox]")].indexOf(box);
      let n = -1;
      for (let i = a; i <= b; i++) {
        const m = TASK.exec(L[i - 1]);
        if (!m || ++n !== k) continue;
        record("struct");
        const on = m[2] === " ";
        L[i - 1] = `${L[i - 1].slice(0, m[1].length)}[${on ? "x" : " "}]${L[i - 1].slice(m[1].length + 3)}`;
        setTimeout(() => { box.checked = on; });
        changed();
        return;
      }
    }

    // ---- the pointer

    function topBlock(t) {
      let n = t && t.nodeType === 1 ? t : t && t.parentElement;
      while (n && n.parentElement !== main) n = n.parentElement;
      return n && n.hasAttribute("data-lines") ? n : null;
    }

    function onDown(e) {
      if (e.button !== 0 || busy || dead) return;
      const f = forge();
      if (f && f.pinning) return;
      const t = e.target;
      if (!t || !t.closest) return;
      if (!main.contains(t)) {
        // A press on the plate round the page leaves the block.
        if (t.closest("forge-overlay")) return;
        if (cur || tbl) { commit(); closeTable(true); }
        return;
      }
      if (cur && cur.el.contains(t)) return;
      if (e.ctrlKey && t.closest("a[href]")) return;
      if (t.matches("li.task > input[type=checkbox]")) { e.preventDefault(); return; }
      if (tbl && tbl.el.contains(t)) {
        const cell = t.closest("th, td");
        if (cell && tbl.cell && cell === tbl.cell.td) return;
        e.preventDefault();
        if (cell) { const [r, c] = posOf(tbl, cell); focusCell(r, c); }
        return;
      }
      const block = topBlock(t);
      e.preventDefault();
      if (!block) { pressBeside(e); return; }
      if (block.localName === "table") {
        const cell = t.closest("th, td");
        commit();
        openTable(block, cell ? posOf({ el: block }, cell) : 0);
        return;
      }
      let at = 0;
      try {
        const r = document.caretRangeFromPoint(e.clientX, e.clientY);
        if (r && block.contains(r.startContainer)) {
          const pre = document.createRange();
          pre.selectNodeContents(block);
          pre.setEnd(r.startContainer, r.startOffset);
          const [a, b] = range(block);
          at = block.matches("pre.mermaid, hr") ? 0 : mapShown(srcOf(a, b), pre.toString());
        }
      } catch (_) { at = 0; }
      edit(block, at);
    }

    // A press on the page but beside every block: under the last one
    // writes on at the end, as a page of paper does.
    function pressBeside(e) {
      const bs = blocks();
      if (!bs.length) { insertBlock(lastLine(), "", true); return; }
      const lastEl = bs[bs.length - 1];
      if (e.clientY > lastEl.getBoundingClientRect().bottom) {
        if (lastEl.localName === "p" || (cur && cur.el === lastEl && !cur.src.trim())) edit(lastEl, "end");
        else { commit(); insertBlock(lastLine(), "", true); }
        return;
      }
      let best = bs[0];
      let gap = Infinity;
      for (const b of bs) {
        const r = b.getBoundingClientRect();
        const d = e.clientY < r.top ? r.top - e.clientY : e.clientY > r.bottom ? e.clientY - r.bottom : 0;
        if (d < gap) { gap = d; best = b; }
      }
      edit(best, e.clientY < best.getBoundingClientRect().top ? 0 : "end");
    }

    function onClick(e) {
      const t = e.target;
      if (!t || !t.closest || !main.contains(t)) return;
      if (t.matches("li.task > input[type=checkbox]")) {
        e.preventDefault();
        toggleTask(t);
        return;
      }
      // A link is a place to write, not a way off the page; Ctrl+click
      // follows it.
      if (t.closest("a[href]") && !e.ctrlKey) e.preventDefault();
    }

    function onSelection() {
      if (!cur || composing) return;
      const s = getSel(cur.el);
      if (s) {
        lastSel = s;
        keepSession();
      }
    }

    // ---- what floats over the page

    function placeFloaters() {
      if (!ui) return;
      const W = innerWidth;
      if (ui.menu && !ui.menu.hidden && cur) {
        const r = cur.el.getBoundingClientRect();
        const h = ui.menu.offsetHeight;
        const top = r.bottom + 6 + h > innerHeight && r.top - 6 - h > 0 ? r.top - 6 - h : r.bottom + 6;
        ui.menu.style.left = `${Math.max(8, Math.min(r.left, W - ui.menu.offsetWidth - 8))}px`;
        ui.menu.style.top = `${top}px`;
      }
      if (ui.lang) {
        const fenced = cur && FENCE.exec(cur.src.split("\n")[0]);
        if (fenced && !(fenced[2][0] === "`" && fenced[5].includes("`"))) {
          const r = cur.el.getBoundingClientRect();
          ui.lang.hidden = false;
          if (ui.box.getRootNode().activeElement !== ui.lang) ui.lang.value = fenced[4];
          ui.lang.style.left = `${Math.max(8, r.right - ui.lang.offsetWidth - 8)}px`;
          ui.lang.style.top = `${r.top + 7}px`;
        } else ui.lang.hidden = true;
      }
      if (ui.tools) {
        if (tbl && tbl.el.isConnected) {
          ui.tools.hidden = false;
          const r = tbl.el.getBoundingClientRect();
          const h = ui.tools.offsetHeight;
          const top = r.top - h - 8 > 4 ? r.top - h - 8 : r.bottom + 8;
          ui.tools.style.left = `${Math.max(8, Math.min(r.right - ui.tools.offsetWidth, W - ui.tools.offsetWidth - 8))}px`;
          ui.tools.style.top = `${top}px`;
        } else ui.tools.hidden = true;
      }
    }

    function hideFloaters() {
      hideMenu();
      hideLang();
      if (ui && ui.tools) ui.tools.hidden = true;
    }

    function onPageKey(e) {
      if (dead) return;
      const mod = (e.ctrlKey || e.metaKey) && !e.altKey;
      if (!mod) return;
      const path = e.composedPath();
      if (path.some((n) => n.localName === "input" || n.localName === "textarea")) return;
      const l = e.key.toLowerCase();
      if (l === "z" && !e.shiftKey) { e.preventDefault(); if (tbl) blurCell(); step(undo, redo); }
      else if (l === "y" || (l === "z" && e.shiftKey)) { e.preventDefault(); if (tbl) blurCell(); step(redo, undo); }
    }

    const onScroll = () => placeFloaters();

    // ---- life

    document.addEventListener("mousedown", onDown, true);
    document.addEventListener("click", onClick, true);
    document.addEventListener("selectionchange", onSelection);
    main.addEventListener("dragover", onDragOver);
    main.addEventListener("drop", onDrop);
    addEventListener("keydown", onPageKey, true);
    addEventListener("scroll", onScroll, { passive: true });
    addEventListener("resize", onScroll);
    if (ui && ui.lang) {
      ui.lang.addEventListener("input", onLangInput);
      ui.lang.addEventListener("keydown", onLangKey);
    }

    return {
      value,
      async load(text, fresh) {
        setText(text);
        await redraw(!!fresh);
        if (dead) return;
        if (!blocks().length && L.every((l) => !l.trim())) insertBlock(0, "", true);
      },
      // Someone else's text taken whole, through the history.
      replaceAll(text) {
        record("struct");
        setText(text);
        redraw(true);
      },
      where() {
        if (cur && cur.el.isConnected) return { ty: { a: range(cur.el)[0], sel: lastSel.slice() } };
        return {};
      },
      restore(saved) {
        const t = saved && saved.ty;
        if (!t || typeof t.a !== "number") return;
        const b = blockAt(t.a);
        if (b) edit(b, Array.isArray(t.sel) ? t.sel : 0);
      },
      addRow: () => { if (tbl && tbl.cell) { const { r, c } = tbl.cell; focusCell(addRow(r), c); } },
      addCol: () => { if (tbl && tbl.cell) { const { r, c } = tbl.cell; addCol(c); focusCell(r, c + 1); } },
      delRow: () => { if (tbl && tbl.cell) { const { r, c } = tbl.cell; delRow(r); focusCell(Math.max(1, r - 1), c); } },
      delCol: () => { if (tbl && tbl.cell) { const { r, c } = tbl.cell; delCol(c); focusCell(r, Math.max(0, c - 1)); } },
      // Leaves the page as the file is, every block drawn as it was or as
      // the source says, before the page is given up.
      destroy() {
        dead = true;
        if (cur) {
          const c = cur;
          cur = null;
          unwire(c.el);
          if (c.orig && c.src === c.origSrc && c.el.isConnected) c.el.replaceWith(c.orig);
        }
        if (tbl) { blurCell(); tbl.el.classList.remove("forge-ty-focus", "forge-ty-table"); tbl = null; }
        document.removeEventListener("mousedown", onDown, true);
        document.removeEventListener("click", onClick, true);
        document.removeEventListener("selectionchange", onSelection);
        main.removeEventListener("dragover", onDragOver);
        main.removeEventListener("drop", onDrop);
        removeEventListener("keydown", onPageKey, true);
        removeEventListener("scroll", onScroll);
        removeEventListener("resize", onScroll);
      },
      get focused() { return cur ? cur.el : null; },
      get lines() { return L.slice(); },
    };
  }

  // ------------------------------------------------------- HTML preview

  // The host marks each element of a served HTML page with the byte range
  // of its content in the file (data-forge-at), which ties the preview to
  // the source in both directions.
  const enc = new TextEncoder();
  const byteOf = (text, i) => enc.encode(text.slice(0, i)).length;
  const charOf = (text, b) => new TextDecoder().decode(enc.encode(text).subarray(0, b)).length;

  function frameDoc() {
    try { return frame && frame.contentDocument; } catch (_) { return null; }
  }

  function frameState() {
    if (!frame) return null;
    try {
      const w = frame.contentWindow;
      const deck = w.__forgeDeck;
      return { x: w.scrollX, y: w.scrollY, slide: deck ? deck.current : 0 };
    } catch (_) {
      return null;
    }
  }

  let frameKeep = null;
  function reloadPreview() {
    if (!frame) return;
    frameKeep = frameState();
    try { frame.contentWindow.location.reload(); } catch (_) { frame.src = FILE + "?preview"; }
  }

  async function frameLoaded() {
    const doc = frameDoc();
    if (!doc) return;
    const w = frame.contentWindow;
    const keep = frameKeep || (restoring && restoring.frame) || null;
    frameKeep = null;
    restoring = null;
    const style = doc.createElement("style");
    const accent = host ? getComputedStyle(host).getPropertyValue("--hd-accent").trim() || "#a78bfa" : "#a78bfa";
    style.textContent = `.forge-ed-here { outline: 1px solid ${accent} !important; outline-offset: 3px !important; }`;
    (doc.head || doc.documentElement).appendChild(style);
    doc.addEventListener("click", (e) => {
      if (!on || !src || e.button !== 0) return;
      const t = e.target;
      if (!t || !t.closest) return;
      if (t.closest("a[href]")) e.preventDefault();
      const at = t.closest("[data-forge-at]");
      const m = at && /^(\d+)-(\d+)$/.exec(at.getAttribute("data-forge-at"));
      if (m) src.gotoOffset(charOf(src.value(), +m[1]));
    }, true);
    if (keep) {
      if (keep.slide) {
        for (let i = 0; !w.__forgeDeck && i < 20; i++) await sleep(50);
        if (w.__forgeDeck) w.__forgeDeck.goto(keep.slide);
      }
      w.scrollTo(keep.x || 0, keep.y || 0);
    }
    if (src) highlightFrame(src.selection(), false);
  }

  let frameLit = null;
  function highlightFrame(pos, follow) {
    const doc = frameDoc();
    if (!doc || !src) return;
    const b = byteOf(src.value(), pos);
    let best = null;
    let span = Infinity;
    for (const e of doc.querySelectorAll("[data-forge-at]")) {
      const m = /^(\d+)-(\d+)$/.exec(e.getAttribute("data-forge-at"));
      if (!m) continue;
      const a = +m[1];
      const z = +m[2];
      if (a <= b && b <= z && z - a < span) { best = e; span = z - a; }
    }
    if (frameLit && frameLit !== best) frameLit.classList.remove("forge-ed-here");
    frameLit = best;
    if (!best) return;
    best.classList.add("forge-ed-here");
    if (!follow) return;
    const w = frame.contentWindow;
    const slide = best.closest("section.slide");
    if (slide && w.__forgeDeck) {
      const n = [...doc.querySelectorAll("section.slide:not(.forge-clone)")].indexOf(slide) + 1;
      if (n > 0 && n !== w.__forgeDeck.current) w.__forgeDeck.goto(n);
    } else {
      const r = best.getBoundingClientRect();
      if (r.bottom < 0 || r.top > w.innerHeight) best.scrollIntoView({ block: "center" });
    }
  }

  // ------------------------------------------------------------ CSV grid

  // The table's delimiter and quoting are the core's (forge/csv.rs): the
  // delimiter is whichever of comma, semicolon or tab the first record has
  // most of. Each record keeps the text it was read from, so a record not
  // touched is written back byte for byte.
  function delimiter(s) {
    const counts = { ",": 0, ";": 0, "\t": 0 };
    let quoted = false;
    for (const c of s) {
      if (c === '"') quoted = !quoted;
      else if (!quoted && (c === "\n" || c === "\r")) break;
      else if (!quoted && c in counts) counts[c]++;
    }
    let best = ",";
    for (const d of [",", ";", "\t"]) if (counts[d] > counts[best]) best = d;
    return best;
  }

  function parseCsv(s, delim) {
    const recs = [];
    let cells = [];
    let field = "";
    let quoted = false;
    let start = 0;
    let started = false;
    for (let i = 0; i < s.length; i++) {
      const c = s[i];
      started = true;
      if (quoted) {
        if (c === '"') {
          if (s[i + 1] === '"') { field += '"'; i++; } else quoted = false;
        } else field += c;
      } else if (c === '"' && field === "") {
        quoted = true;
      } else if (c === delim) {
        cells.push(field);
        field = "";
      } else if (c === "\n" || c === "\r") {
        const end = i;
        if (c === "\r" && s[i + 1] === "\n") i++;
        cells.push(field);
        recs.push({ cells, raw: s.slice(start, end), eol: s.slice(end, i + 1) });
        cells = [];
        field = "";
        start = i + 1;
        started = false;
      } else field += c;
    }
    if (started) {
      cells.push(field);
      recs.push({ cells, raw: s.slice(start), eol: "" });
    }
    return recs;
  }

  function isNumber(s) {
    const t = s.replace(/\s/g, "").replace(",", ".");
    return t !== "" && isFinite(t);
  }

  function escHtml(s) {
    return s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  }

  function makeGrid() {
    const main = document.querySelector("main.forge-csv");
    const table = el("table", "forge-ed-grid");
    main.appendChild(table);
    let bom = "";
    let delim = ",";
    let eol = "\n";
    let recs = [];
    let width = 1;
    let sel = null;
    let editing = null;
    const undo = [];
    const redo = [];
    // A sort is only a view: the rows are shown in its order, the file
    // keeps its own. `order` is the records under the header as shown.
    let sort = null;
    let order = [];

    function quote(f) {
      return f.includes(delim) || /["\r\n]/.test(f) ? `"${f.replace(/"/g, '""')}"` : f;
    }

    function text() {
      return bom + recs.map((r) => (r.raw !== null ? r.raw : r.cells.map(quote).join(delim)) + r.eol).join("");
    }

    function load(s, keepUndo) {
      if (keepUndo) snapshot();
      bom = s[0] === "﻿" ? "﻿" : "";
      const body = bom ? s.slice(1) : s;
      delim = delimiter(body);
      eol = body.includes("\r\n") ? "\r\n" : "\n";
      recs = parseCsv(body, delim);
      render();
    }

    function snapshot() {
      undo.push({ recs: recs.map((r) => ({ cells: r.cells.slice(), raw: r.raw, eol: r.eol })), sel: sel && { ...sel } });
      if (undo.length > 200) undo.shift();
      redo.length = 0;
    }

    function restoreFrom(from, to) {
      const s = from.pop();
      if (!s) return;
      to.push({ recs, sel: sel && { ...sel } });
      recs = s.recs;
      render();
      if (s.sel) select(s.sel.r, s.sel.c);
      changed();
    }

    function figures(col) {
      let any = false;
      for (let r = 1; r < recs.length; r++) {
        const c = recs[r].cells[col];
        if (c == null || c.trim() === "") continue;
        if (!isNumber(c)) return false;
        any = true;
      }
      return any;
    }

    function cellHtml(tag, r, c, v, num) {
      return `<${tag} data-row="${r}" data-col="${c}"${num ? ' class="num"' : ""}>${escHtml(v)}</${tag}>`;
    }

    // A header cell: its words to edit, and a small key beside them that
    // sorts the view by the column.
    function headHtml(c, v, num) {
      const dir = sort && sort.c === c ? sort.dir : "";
      return `<th data-row="0" data-col="${c}"${num ? ' class="num"' : ""}${dir ? ` data-sort="${dir}"` : ""}>` +
        `<span class="forge-ed-th">${escHtml(v)}</span>` +
        `<button type="button" class="forge-ed-sort" tabindex="-1" contenteditable="false" title="Sort the view by this column (the file keeps its order)">` +
        '<svg viewBox="0 0 8 10" aria-hidden="true"><path class="up" d="M4 1 7 4H1Z"/><path class="down" d="M4 9 1 6h6Z"/></svg></button></th>';
    }

    function sortValue(s) {
      const t = s.replace(/\s/g, "").replace(",", ".");
      return t !== "" && isFinite(t) ? +t : NaN;
    }

    function ordered() {
      const rows = [];
      for (let r = 1; r < recs.length; r++) rows.push(r);
      if (!sort) return rows;
      const key = (r) => recs[r].cells[sort.c] || "";
      return rows.sort((a, b) => {
        const x = key(a);
        const y = key(b);
        const p = sortValue(x);
        const q = sortValue(y);
        const c = !isNaN(p) && !isNaN(q) ? p - q : x.localeCompare(y, undefined, { numeric: true });
        return (sort.dir === "asc" ? c : -c) || a - b;
      });
    }

    function sortBy(c) {
      commit(true);
      if (!sort || sort.c !== c) sort = { c, dir: "asc" };
      else if (sort.dir === "asc") sort.dir = "desc";
      else sort = null;
      render();
    }

    function render() {
      width = Math.max(1, ...recs.map((r) => r.cells.length));
      let h = '<thead><tr data-row="0">';
      const head = recs[0] ? recs[0].cells : [];
      for (let c = 0; c < width; c++) h += headHtml(c, head[c] || "", figures(c));
      h += "</tr></thead><tbody>";
      order = ordered();
      for (const r of order) {
        h += `<tr data-row="${r}">`;
        for (let c = 0; c < width; c++) {
          const v = recs[r].cells[c] || "";
          h += cellHtml("td", r, c, v, isNumber(v));
        }
        h += "</tr>";
      }
      table.innerHTML = h + "</tbody>";
      if (sel) select(Math.min(sel.r, Math.max(recs.length - 1, 0)), Math.min(sel.c, width - 1), true);
    }

    function cellAt(r, c) {
      return table.querySelector(`[data-row="${r}"][data-col="${c}"]`);
    }

    function select(r, c, quiet) {
      const old = sel && cellAt(sel.r, sel.c);
      if (old) old.classList.remove("forge-ed-sel");
      sel = { r, c };
      const now = cellAt(r, c);
      if (!now) return;
      now.classList.add("forge-ed-sel");
      if (!quiet) now.scrollIntoView({ block: "nearest", inline: "nearest" });
      keepSession();
    }

    function deselect() {
      const old = sel && cellAt(sel.r, sel.c);
      if (old) old.classList.remove("forge-ed-sel");
      sel = null;
    }

    function rows() { return Math.max(recs.length, 1); }

    // Moves go by the rows as shown, which a sort may have reordered.
    function move(dr, dc, wrap) {
      if (!sel) { select(recs.length > 1 ? (order[0] || 1) : 0, 0); return; }
      const shown = [0, ...order];
      let i = Math.max(shown.indexOf(sel.r), 0) + dr;
      let c = sel.c + dc;
      if (wrap && c >= width) { c = 0; i++; }
      if (wrap && c < 0) { c = width - 1; i--; }
      i = Math.min(Math.max(i, 0), Math.min(shown.length, rows()) - 1);
      c = Math.min(Math.max(c, 0), width - 1);
      select(shown[i], c);
    }

    function setCell(r, c, v) {
      while (recs.length <= r) recs.push({ cells: [], raw: null, eol: "" });
      fixEols();
      const rec = recs[r];
      while (rec.cells.length <= c) rec.cells.push("");
      rec.cells[c] = v;
      rec.raw = null;
    }

    // Every record but the last ends in a line break; the last keeps
    // whatever the file's last record had.
    function fixEols() {
      for (let i = 0; i < recs.length - 1; i++) if (!recs[i].eol) recs[i].eol = eol;
    }

    function changed() {
      if (!conflict) schedule();
      else keepSession();
    }

    function startEdit(seed) {
      if (!sel || editing) return;
      const td = cellAt(sel.r, sel.c);
      if (!td) return;
      const rec = recs[sel.r];
      // A header cell's words are in a box of their own, beside its key.
      const box = td.querySelector(".forge-ed-th") || td;
      editing = { td, box, r: sel.r, c: sel.c, old: (rec && rec.cells[sel.c]) || "" };
      box.setAttribute("contenteditable", "plaintext-only");
      td.classList.add("forge-ed-editing");
      box.addEventListener("keydown", cellKey);
      box.addEventListener("blur", cellBlur);
      if (seed != null) box.textContent = seed;
      if (document.activeElement !== box) {
        box.focus({ preventScroll: true });
        const range = document.createRange();
        range.selectNodeContents(box);
        range.collapse(false);
        const s = getSelection();
        s.removeAllRanges();
        s.addRange(range);
      }
    }

    function commit(keep = true) {
      const ed = editing;
      if (!ed) return;
      editing = null;
      const td = ed.td;
      const box = ed.box;
      box.removeEventListener("keydown", cellKey);
      box.removeEventListener("blur", cellBlur);
      box.removeAttribute("contenteditable");
      td.classList.remove("forge-ed-editing");
      let v = box.innerText.replace(/\r\n?/g, "\n");
      if (v.endsWith("\n") && !ed.old.endsWith("\n")) v = v.slice(0, -1);
      if (!keep || v === ed.old) {
        box.textContent = ed.old;
        return;
      }
      snapshot();
      setCell(ed.r, ed.c, v);
      box.textContent = v;
      if (ed.r > 0) td.classList.toggle("num", isNumber(v));
      changed();
    }

    function cellBlur() { commit(true); }

    function cellKey(e) {
      if (e.isComposing) return;
      const mod = (e.ctrlKey || e.metaKey) && !e.altKey;
      if (mod && e.key.toLowerCase() === "s") { e.preventDefault(); commit(true); save(); return; }
      if (e.key === "Enter" && e.altKey) {
        e.preventDefault();
        document.execCommand("insertText", false, "\n");
        return;
      }
      const single = !editing.box.textContent.includes("\n");
      let go = null;
      if (e.key === "Enter") go = [e.shiftKey ? -1 : 1, 0, false];
      else if (e.key === "Tab") go = [0, e.shiftKey ? -1 : 1, true];
      else if (e.key === "ArrowUp" && single) go = [-1, 0, false];
      else if (e.key === "ArrowDown" && single) go = [1, 0, false];
      else if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        commit(false);
        return;
      }
      if (!go) return;
      e.preventDefault();
      e.stopPropagation();
      commit(true);
      move(go[0], go[1], go[2]);
    }

    function onKey(e) {
      if (!on || editing || e.isComposing) return;
      const path = e.composedPath();
      if (path.some((n) => n.localName === "input" || n.localName === "textarea" || n.isContentEditable)) return;
      const mod = (e.ctrlKey || e.metaKey) && !e.altKey;
      const k = e.key;
      if (mod) {
        const l = k.toLowerCase();
        if (l === "z" && !e.shiftKey) { e.preventDefault(); restoreFrom(undo, redo); }
        else if (l === "y" || (l === "z" && e.shiftKey)) { e.preventDefault(); restoreFrom(redo, undo); }
        return;
      }
      if (!sel) {
        if (k.startsWith("Arrow")) { e.preventDefault(); move(0, 0); }
        return;
      }
      const go = {
        ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1],
      }[k];
      if (go) { e.preventDefault(); move(go[0], go[1]); return; }
      if (k === "Tab") { e.preventDefault(); move(0, e.shiftKey ? -1 : 1, true); return; }
      if (k === "Enter" || k === "F2") {
        e.preventDefault();
        if (e.shiftKey) move(-1, 0);
        else startEdit(null);
        return;
      }
      if (k === "Home") { e.preventDefault(); select(sel.r, 0); return; }
      if (k === "End") { e.preventDefault(); select(sel.r, width - 1); return; }
      if (k === "Escape") { e.preventDefault(); deselect(); return; }
      if (k === "Delete" || k === "Backspace") {
        e.preventDefault();
        const rec = recs[sel.r];
        if (rec && rec.cells[sel.c]) {
          snapshot();
          setCell(sel.r, sel.c, "");
          const td = cellAt(sel.r, sel.c);
          if (td) (td.querySelector(".forge-ed-th") || td).textContent = "";
          changed();
        }
        return;
      }
      if (k.length === 1 && !e.altKey) {
        e.preventDefault();
        startEdit(k);
      }
    }

    // Making the cell editable before the press lands lets the browser put
    // the caret where the click was.
    function onDown(e) {
      if (!on || e.button !== 0) return;
      const t = e.target;
      const key = t && t.closest && t.closest("table.forge-ed-grid .forge-ed-sort");
      if (key) {
        e.preventDefault();
        sortBy(+key.parentElement.dataset.col);
        return;
      }
      const cell = t && t.closest && t.closest("table.forge-ed-grid th, table.forge-ed-grid td");
      if (!cell) return;
      const r = +cell.dataset.row;
      const c = +cell.dataset.col;
      if (editing && editing.td === cell) return;
      if (editing) commit(true);
      select(r, c, true);
      startEdit(null);
    }

    // Excel copies a block as tab separated rows, a cell with a line break
    // in quotes.
    function onPaste(e) {
      if (!on || editing || !sel) return;
      const t = e.clipboardData && e.clipboardData.getData("text/plain");
      if (!t) return;
      e.preventDefault();
      let block = parseCsv(t, "\t").map((r) => r.cells);
      if (block.length > 1 && block[block.length - 1].every((c) => c === "")) block.pop();
      if (!block.length) return;
      snapshot();
      block.forEach((cells, i) => cells.forEach((v, j) => setCell(sel.r + i, sel.c + j, v)));
      render();
      changed();
    }

    function onCopy(e) {
      if (!on || editing || !sel) return;
      if (getSelection().toString()) return;
      const rec = recs[sel.r];
      e.preventDefault();
      e.clipboardData.setData("text/plain", (rec && rec.cells[sel.c]) || "");
    }

    function addRow(atEnd) {
      commit(true);
      snapshot();
      const at = atEnd || !sel ? recs.length : Math.max(sel.r, 0) + 1;
      const rec = { cells: Array(width).fill(""), raw: null, eol: "" };
      if (at >= recs.length) {
        const last = recs[recs.length - 1];
        if (last) { rec.eol = last.eol; if (!last.eol) last.eol = eol; }
        recs.push(rec);
      } else {
        rec.eol = eol;
        recs.splice(at, 0, rec);
      }
      render();
      select(at, sel ? sel.c : 0);
      changed();
    }

    function addCol() {
      commit(true);
      if (!recs.length) return;
      snapshot();
      const at = (sel ? sel.c : width - 1) + 1;
      for (const rec of recs) {
        while (rec.cells.length < width) rec.cells.push("");
        rec.cells.splice(at, 0, "");
        rec.raw = null;
      }
      render();
      select(sel ? sel.r : 0, at);
      changed();
    }

    function deleteRow() {
      commit(true);
      if (!sel || !recs[sel.r]) return;
      snapshot();
      const gone = recs.splice(sel.r, 1)[0];
      if (sel.r === recs.length && recs.length) recs[recs.length - 1].eol = gone.eol;
      render();
      if (recs.length) select(Math.min(sel.r, recs.length - 1), sel.c);
      else deselect();
      changed();
    }

    function deleteCol() {
      commit(true);
      if (!sel || width <= 1) return;
      snapshot();
      for (const rec of recs) {
        if (rec.cells.length > sel.c) {
          rec.cells.splice(sel.c, 1);
          rec.raw = null;
        }
      }
      render();
      select(sel.r, Math.min(sel.c, width - 1));
      changed();
    }

    addEventListener("keydown", onKey, true);
    document.addEventListener("mousedown", onDown, true);
    document.addEventListener("paste", onPaste);
    document.addEventListener("copy", onCopy);

    return {
      text,
      load,
      commit: () => commit(true),
      addRow, addCol, deleteRow, deleteCol,
      where: () => sel,
      restore(cell) {
        if (cell && typeof cell.r === "number") select(cell.r, cell.c);
        else select(recs.length > 1 ? 1 : 0, 0);
      },
      destroy() {
        commit(true);
        removeEventListener("keydown", onKey, true);
        document.removeEventListener("mousedown", onDown, true);
        document.removeEventListener("paste", onPaste);
        document.removeEventListener("copy", onCopy);
        table.remove();
      },
    };
  }

  // ------------------------------------------------------------------ keys

  function typing(e) {
    for (const n of e.composedPath()) {
      if (n.nodeType !== 1) continue;
      if (n.isContentEditable) return true;
      const tag = n.localName;
      if (tag === "input" || tag === "textarea" || tag === "select") return true;
    }
    return false;
  }

  addEventListener("keydown", (e) => {
    if (on && (e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === "s") {
      e.preventDefault();
      if (grid) grid.commit();
      save();
      return;
    }
    if (on && kind === "md" && (e.ctrlKey || e.metaKey) && !e.altKey && e.key === "/") {
      e.preventDefault();
      setMdView(mdView === "page" ? "source" : "page");
      return;
    }
    if (e.defaultPrevented || e.repeat || e.ctrlKey || e.metaKey || e.altKey) return;
    if (e.key !== "e" && e.key !== "E") return;
    if (always || VERSION) return;
    if (typing(e)) return;
    const f = forge();
    if (f && f.pinning) return;
    if (document.documentElement.classList.contains("forge-presenting")) return;
    e.preventDefault();
    toggle();
  });

  // ----------------------------------------------------------------- boot

  let hooked = false;
  function hook() {
    const f = forge();
    if (hooked || !f) return;
    hooked = true;
    f.on("editing", (m) => {
      // Markdown and CSV have no mode to turn on or off.
      if (always) return true;
      if (m.on) enter(true, null);
      else leave(true);
      return true;
    });
    f.on("write-result", writeResult);
    f.on("changed-on-disk", changedOnDisk);
    f.on("rendered", answered);
    f.on("image-saved", answered);
  }

  function boot() {
    hook();
    if (VERSION) return;
    const saved = recall("session", KEY);
    const k = detect();
    if ((k === "md" || k === "csv") && !on) enter(false, saved);
    else if (saved && saved.on && !on) enter(false, saved);
  }

  hook();
  if (document.readyState === "loading") addEventListener("DOMContentLoaded", boot, { once: true });
  else boot();
  // forge.js may come after this script; its hooks are taken once it is in.
  addEventListener("load", hook, { once: true });

  window.__forgeEditor = Object.freeze({
    get on() { return on; },
    get kind() { return kind; },
    get base() { return base; },
    get view() { return kind === "md" ? mdView : null; },
    setView: (v) => setMdView(v),
    get typora() { return ty; },
    text: () => current(),
    enter: () => enter(false, null),
    leave: () => leave(false),
    save,
  });
})();
