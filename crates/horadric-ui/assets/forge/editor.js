// The Forge's editor: the whole file worked on in the pane. Markdown and
// HTML get their source beside the page, a CSV becomes a grid. Every change
// is written as the whole file together with the text it started from, so
// the host can refuse it when Claude wrote the file meanwhile.
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

  function clampSplit(f) {
    return typeof f === "number" && f > 0.15 && f < 0.85 ? f : 0.46;
  }

  function current() {
    if (kind === "csv") return grid ? grid.text() : base;
    return src ? src.value() : base;
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
      if (kind === "md" && typeof m.html === "string") swapPreview(m.html);
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
    if (!on || typeof m.current !== "string" || base === null) return true;
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
    if (kind === "csv") grid.load(theirs, true);
    else src.replaceAll(theirs);
    if (kind === "html") reloadPreview();
    if (kind === "md") refreshMarkdown();
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
    if (kind === "csv") grid.load(theirs, true);
    else src.replaceAll(theirs);
    if (kind === "html") reloadPreview();
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
    const s = { on: true, view };
    if (text !== null && text !== base) s.draft = { base, text };
    if (src) Object.assign(s, src.where());
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
    if (!fromHost) post({ forge: "editing", on: true });
    view = (saved && saved.view) || "source";
    base = null;
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
    base = text;
    const draft = saved && saved.draft;
    let shown = text;
    if (draft && typeof draft.text === "string" && draft.text !== draft.base) {
      shown = draft.text;
      if (draft.base !== text) base = draft.base;
    }
    if (kind === "csv") grid.load(shown, false);
    else src.load(shown);
    restoring = saved || null;
    if (src) src.restore(saved);
    if (grid) grid.restore(saved && saved.cell);
    if (base !== text) showConflict(text);
    else if (shown !== base) schedule();
    else status("saved");
    keepSession();
  }

  async function leave(fromHost) {
    if (!on) return;
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
    const done = key("Done", "Stop editing (E)", () => leave(false));
    const cls = document.documentElement.classList;
    cls.add("forge-editing", `forge-editing-${kind}`);

    if (kind === "csv") {
      const tools = el("div", "forge-ed-tools");
      tools.append(
        el("span", "forge-ed-legend", "Editing"),
        key("Row below", "Add a row under the chosen cell", () => grid.addRow(false)),
        key("Row at end", "Add a row after the last", () => grid.addRow(true)),
        key("Column right", "Add a column right of the chosen cell", () => grid.addCol()),
        key("Delete row", "Delete the chosen cell's row", () => grid.deleteRow()),
        key("Delete column", "Delete the chosen cell's column", () => grid.deleteCol()),
      );
      const end = el("div", "forge-ed-end");
      end.append(ui.conflict, ui.status, done);
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
    if (kind === "md") unwireMarkdownPreview();
    for (const h of document.querySelectorAll(".forge-ed-here")) h.classList.remove("forge-ed-here");
    if (host) host.remove();
    const cls = document.documentElement.classList;
    for (const c of [...cls]) if (c.startsWith("forge-editing")) cls.remove(c);
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

    function render() {
      width = Math.max(1, ...recs.map((r) => r.cells.length));
      let h = '<thead><tr data-row="0">';
      const head = recs[0] ? recs[0].cells : [];
      for (let c = 0; c < width; c++) h += cellHtml("th", 0, c, head[c] || "", figures(c));
      h += "</tr></thead><tbody>";
      for (let r = 1; r < recs.length; r++) {
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

    function move(dr, dc, wrap) {
      if (!sel) { select(recs.length > 1 ? 1 : 0, 0); return; }
      let r = sel.r + dr;
      let c = sel.c + dc;
      if (wrap && c >= width) { c = 0; r++; }
      if (wrap && c < 0) { c = width - 1; r--; }
      r = Math.min(Math.max(r, 0), rows() - 1);
      c = Math.min(Math.max(c, 0), width - 1);
      select(r, c);
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
      editing = { td, r: sel.r, c: sel.c, old: (rec && rec.cells[sel.c]) || "" };
      td.setAttribute("contenteditable", "plaintext-only");
      td.classList.add("forge-ed-editing");
      td.addEventListener("keydown", cellKey);
      td.addEventListener("blur", cellBlur);
      if (seed != null) td.textContent = seed;
      if (document.activeElement !== td) {
        td.focus({ preventScroll: true });
        const range = document.createRange();
        range.selectNodeContents(td);
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
      td.removeEventListener("keydown", cellKey);
      td.removeEventListener("blur", cellBlur);
      td.removeAttribute("contenteditable");
      td.classList.remove("forge-ed-editing");
      let v = td.innerText.replace(/\r\n?/g, "\n");
      if (v.endsWith("\n") && !ed.old.endsWith("\n")) v = v.slice(0, -1);
      if (!keep || v === ed.old) {
        td.textContent = ed.old;
        return;
      }
      snapshot();
      setCell(ed.r, ed.c, v);
      td.textContent = v;
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
      const single = !editing.td.textContent.includes("\n");
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
          if (td) td.textContent = "";
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
    if (e.defaultPrevented || e.repeat || e.ctrlKey || e.metaKey || e.altKey) return;
    if (e.key !== "e" && e.key !== "E") return;
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
      if (m.on) enter(true, null);
      else leave(true);
      return true;
    });
    f.on("write-result", writeResult);
    f.on("changed-on-disk", changedOnDisk);
  }

  function boot() {
    hook();
    const saved = recall("session", KEY);
    if (saved && saved.on && !on) enter(false, saved);
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
    text: () => current(),
    enter: () => enter(false, null),
    leave: () => leave(false),
    save,
  });
})();
