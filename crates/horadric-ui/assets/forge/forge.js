// The Forge's page side: scroll keeping, pins, pin runes, goto and errors.
// Injected into every file shown in the browser pane. The page is the
// human's content, so everything here hides in one closed shadow root and
// one global, window.__forge, and touches the page's own DOM only to read it.
// The protocol is in README.md beside this file; keep the two in step.
(() => {
  "use strict";
  if (window.__forge) return;

  const webview = window.chrome && window.chrome.webview;
  const handlers = new Map();
  const sent = [];

  // Remembered before any await: currentScript is only set while this runs.
  const scriptSrc = document.currentScript && document.currentScript.src;

  function post(msg) {
    sent.push(msg);
    if (sent.length > 200) sent.shift();
    if (webview) {
      try { webview.postMessage(msg); } catch (_) { /* the host went away */ }
    } else {
      // Opened in a plain browser for testing: show what would go out.
      console.debug("[forge] post", msg);
    }
  }

  function on(type, fn) {
    if (!handlers.has(type)) handlers.set(type, []);
    handlers.get(type).push(fn);
  }

  function receive(data) {
    let msg = data;
    if (typeof msg === "string") {
      try { msg = JSON.parse(msg); } catch (_) { return; }
    }
    if (!msg || typeof msg !== "object" || typeof msg.forge !== "string") return;
    // A later script (the deck shell) may claim a message by returning true.
    for (const fn of handlers.get(msg.forge) || []) {
      try { if (fn(msg) === true) return; } catch (e) { report(e); }
    }
    const own = builtin[msg.forge];
    if (own) {
      try { own(msg); } catch (e) { report(e); }
    }
  }

  if (webview) webview.addEventListener("message", (e) => receive(e.data));

  // ---------------------------------------------------------------- assets

  function cssFor(name, key) {
    const given = window.__forgeAssets && window.__forgeAssets[key];
    if (typeof given === "string") {
      const style = document.createElement("style");
      style.textContent = given;
      return style;
    }
    if (!scriptSrc) return null;
    const link = document.createElement("link");
    link.rel = "stylesheet";
    link.href = new URL(name, scriptSrc).href;
    return link;
  }

  // --------------------------------------------------------------- overlay

  let host = null;
  let root = null;
  let layer = {};

  function ensureOverlay() {
    if (root) return root;
    if (!document.documentElement) return null;
    host = document.createElement("forge-overlay");
    host.setAttribute("style",
      "all:initial !important; position:fixed !important; inset:0 !important;" +
      "z-index:2147483647 !important; pointer-events:none !important;" +
      "display:block !important; contain:strict !important;");
    root = host.attachShadow({ mode: "closed" });
    const css = cssFor("forge.css", "forgeCss");
    if (css) root.appendChild(css);
    const make = (cls) => {
      const div = document.createElement("div");
      div.className = cls;
      root.appendChild(div);
      return div;
    };
    layer.pins = make("pins");
    layer.flash = make("flash");
    layer.catcher = make("catcher");
    layer.hover = make("hover");
    layer.hoverTag = make("hover-tag");
    layer.drag = make("drag");
    layer.ask = make("ask");
    layer.errors = make("errors");
    // Under <html>, not <body>, so the page's body styles and its
    // `body > *` selectors never reach it.
    document.documentElement.appendChild(host);
    wireCatcher();
    return root;
  }

  function isOurs(el) {
    return el === host || (el && el.closest && el.closest(".forge-strip, forge-deck-ui"));
  }

  function elementAt(x, y) {
    for (const el of document.elementsFromPoint(x, y)) {
      if (!isOurs(el)) return el;
    }
    return null;
  }

  // ---------------------------------------------------------- scroll keeping

  let scrollTimer = 0;
  let lastScrollPost = 0;
  let userScrolled = false;

  function postScroll() {
    scrollTimer = 0;
    lastScrollPost = performance.now();
    post({ forge: "scroll", x: Math.round(scrollX), y: Math.round(scrollY) });
  }

  addEventListener("scroll", () => {
    userScrolled = true;
    if (scrollTimer) return;
    const wait = Math.max(0, 150 - (performance.now() - lastScrollPost));
    scrollTimer = setTimeout(postScroll, wait);
  }, { passive: true });

  function settled() {
    const loaded = document.readyState === "complete"
      ? Promise.resolve()
      : new Promise((ok) => addEventListener("load", ok, { once: true }));
    return loaded.then(() => {
      const waits = [];
      if (document.fonts && document.fonts.ready) waits.push(document.fonts.ready);
      for (const img of document.images) {
        if (!img.complete) {
          waits.push(new Promise((ok) => {
            img.addEventListener("load", ok, { once: true });
            img.addEventListener("error", ok, { once: true });
          }));
        }
      }
      // A broken image or font must not hold the page at the top forever.
      return Promise.race([
        Promise.all(waits),
        new Promise((ok) => setTimeout(ok, 1500)),
      ]);
    }).then(() => new Promise((ok) => requestAnimationFrame(() => requestAnimationFrame(ok))));
  }

  function restoreScroll(x, y) {
    settled().then(() => {
      userScrolled = false;
      scrollTo(x, y);
      // Late layout (a chart drawn by script, a lazy image) can still grow
      // the page; keep putting it back until the human scrolls themselves.
      let tries = 0;
      const again = () => {
        if (userScrolled && Math.abs(scrollY - y) > 2) return;
        if (Math.abs(scrollY - y) > 2 || Math.abs(scrollX - x) > 2) scrollTo(x, y);
        userScrolled = false;
        if (++tries < 8) setTimeout(again, 120);
      };
      setTimeout(again, 120);
    });
  }

  // ------------------------------------------------------------------- pins

  let pinning = false;
  let asking = null;
  let pressed = null;

  function setPinning(onNow, fromPage) {
    onNow = !!onNow;
    if (onNow === pinning) return;
    if (!ensureOverlay()) return;
    pinning = onNow;
    layer.catcher.classList.toggle("on", pinning);
    if (!pinning) {
      cancelAsk();
      hideHover();
      layer.drag.classList.remove("on");
      pressed = null;
    }
    if (fromPage) post({ forge: "pinning", on: pinning });
  }

  function hideHover() {
    layer.hover.classList.remove("on");
    layer.hoverTag.classList.remove("on");
  }

  function place(div, r) {
    div.style.transform = `translate(${r.x}px, ${r.y}px)`;
    div.style.width = `${Math.max(0, r.w)}px`;
    div.style.height = `${Math.max(0, r.h)}px`;
  }

  function rectOf(el) {
    const r = el.getBoundingClientRect();
    return { x: r.left, y: r.top, w: r.width, h: r.height };
  }

  function describe(el) {
    const slide = slideOf(el);
    const lines = linesOf(el);
    let tag = el.localName;
    if (el.id) tag += "#" + el.id;
    if (slide) return `slide ${slide.index} · ${tag}`;
    if (lines) {
      const where = lines[0] === lines[1] ? `line ${lines[0]}` : `lines ${lines[0]} to ${lines[1]}`;
      return `${where} · ${tag}`;
    }
    return tag;
  }

  function wireCatcher() {
    const c = layer.catcher;
    c.addEventListener("pointermove", (e) => {
      if (pressed) {
        const dx = e.clientX - pressed.x;
        const dy = e.clientY - pressed.y;
        if (!pressed.drag && Math.hypot(dx, dy) > 6) {
          pressed.drag = true;
          hideHover();
          layer.drag.classList.add("on");
        }
        if (pressed.drag) place(layer.drag, boxFrom(pressed, e));
        return;
      }
      if (asking) return;
      const el = elementAt(e.clientX, e.clientY);
      if (!el) { hideHover(); return; }
      const r = rectOf(el);
      place(layer.hover, r);
      layer.hover.classList.add("on");
      layer.hoverTag.textContent = describe(el);
      layer.hoverTag.style.transform =
        `translate(${Math.max(0, r.x)}px, ${Math.max(0, r.y - 20)}px)`;
      layer.hoverTag.classList.add("on");
    });
    c.addEventListener("pointerleave", () => { if (!pressed) hideHover(); });
    c.addEventListener("pointerdown", (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      cancelAsk();
      c.setPointerCapture(e.pointerId);
      pressed = { x: e.clientX, y: e.clientY, drag: false };
    });
    c.addEventListener("pointerup", (e) => {
      if (!pressed) return;
      const was = pressed;
      pressed = null;
      layer.drag.classList.remove("on");
      if (was.drag) {
        const box = boxFrom(was, e);
        if (box.w >= 4 && box.h >= 4) startAsk(pinForBox(box));
      } else {
        const el = elementAt(e.clientX, e.clientY);
        if (el) startAsk(pinForElement(el));
      }
    });
    c.addEventListener("pointercancel", () => {
      pressed = null;
      layer.drag.classList.remove("on");
    });
  }

  function boxFrom(start, e) {
    const x = Math.min(start.x, e.clientX);
    const y = Math.min(start.y, e.clientY);
    return { x, y, w: Math.abs(e.clientX - start.x), h: Math.abs(e.clientY - start.y) };
  }

  function trim(s, n) {
    s = (s || "").replace(/\s+/g, " ").trim();
    return s.length > n ? s.slice(0, n - 1) + "…" : s;
  }

  function slides() {
    return document.querySelectorAll("section.slide:not(.forge-clone)");
  }

  function slideOf(el) {
    const s = el.closest && el.closest("section.slide");
    if (!s || s.classList.contains("forge-clone")) return null;
    const index = Array.prototype.indexOf.call(slides(), s) + 1;
    return index > 0 ? { el: s, index } : null;
  }

  // data-lines is written by the Forge's markdown renderer as "a-b" or "a".
  function parseLines(v) {
    const m = /^\s*(\d+)\s*(?:[-,:]\s*(\d+))?\s*$/.exec(v || "");
    if (!m) return null;
    const a = +m[1];
    const b = m[2] ? +m[2] : a;
    return [Math.min(a, b), Math.max(a, b)];
  }

  function linesOf(el) {
    const block = el.closest && el.closest("[data-lines]");
    return block ? parseLines(block.getAttribute("data-lines")) : null;
  }

  function pageOf(el) {
    // PDF.js style pages, or anything the host renders with data-page.
    const p = el.closest && el.closest("[data-page], [data-page-number]");
    if (!p) return null;
    const n = +(p.getAttribute("data-page") || p.getAttribute("data-page-number"));
    return n > 0 ? n : null;
  }

  function selectorFor(el) {
    if (!el || el.nodeType !== 1) return null;
    const parts = [];
    let node = el;
    while (node && node.nodeType === 1 && node !== document.documentElement) {
      if (node.id && document.querySelectorAll("#" + CSS.escape(node.id)).length === 1) {
        parts.unshift("#" + CSS.escape(node.id));
        break;
      }
      const tag = node.localName;
      let part = /^[a-z][a-z0-9-]*$/i.test(tag) ? tag : "*";
      const parent = node.parentElement;
      if (parent) {
        const same = Array.prototype.filter.call(parent.children, (c) => c.localName === tag);
        if (same.length > 1) part += `:nth-of-type(${same.indexOf(node) + 1})`;
      }
      parts.unshift(part);
      if (node === document.body) break;
      node = parent;
    }
    return parts.join(" > ");
  }

  // The part of an image a box covers, in the image's own pixels, so a pin
  // on a photo or a rendered slide means the same spot at any zoom.
  function imageBox(img, box) {
    const nw = img.naturalWidth;
    const nh = img.naturalHeight;
    if (!nw || !nh) return null;
    const r = img.getBoundingClientRect();
    let cx = r.left, cy = r.top, cw = r.width, ch = r.height;
    const fit = getComputedStyle(img).objectFit;
    if (fit === "contain" || fit === "scale-down") {
      let s = Math.min(cw / nw, ch / nh);
      if (fit === "scale-down") s = Math.min(s, 1);
      cx += (cw - nw * s) / 2;
      cy += (ch - nh * s) / 2;
      cw = nw * s;
      ch = nh * s;
    }
    if (cw <= 0 || ch <= 0) return null;
    const clamp = (v, hi) => Math.max(0, Math.min(hi, v));
    const x0 = clamp((box.x - cx) / cw * nw, nw);
    const y0 = clamp((box.y - cy) / ch * nh, nh);
    const x1 = clamp((box.x + box.w - cx) / cw * nw, nw);
    const y1 = clamp((box.y + box.h - cy) / ch * nh, nh);
    if (x1 - x0 < 1 || y1 - y0 < 1) return null;
    return {
      x: Math.round(x0), y: Math.round(y0),
      w: Math.round(x1 - x0), h: Math.round(y1 - y0),
      naturalWidth: nw, naturalHeight: nh,
      src: img.currentSrc || img.src,
    };
  }

  function atFor(el, box, kind) {
    const at = {};
    const slide = slideOf(el);
    if (slide) at.slide = slide.index;
    const page = pageOf(el);
    if (page) at.page = page;
    const lines = linesOf(el);
    if (lines) at.lines = lines;
    at.selector = selectorFor(el);
    const text = trim(el.innerText || el.textContent, 300);
    if (text) at.text = text;
    if (kind === "element") at.html = trim(el.outerHTML, 600);
    const img = el.localName === "img" ? el : null;
    if (img) {
      const ib = imageBox(img, box);
      if (ib) at.image = ib;
    }
    return at;
  }

  // Where the rune goes back when the host sends the pin again later.
  function anchorFor(el, box, kind, at) {
    const anchor = {};
    if (kind === "element" && at.selector) anchor.selector = at.selector;
    anchor.rect = {
      x: Math.round(box.x + scrollX), y: Math.round(box.y + scrollY),
      w: Math.round(box.w), h: Math.round(box.h),
    };
    if (at.slide) {
      anchor.slide = at.slide;
      if (kind === "box") {
        const s = slides()[at.slide - 1].getBoundingClientRect();
        if (s.width > 0 && s.height > 0) {
          const f = (v) => Math.round(v * 10000) / 10000;
          anchor.slideRect = {
            x: f((box.x - s.left) / s.width), y: f((box.y - s.top) / s.height),
            w: f(box.w / s.width), h: f(box.h / s.height),
          };
        }
      }
    }
    return anchor;
  }

  function pinForElement(el) {
    const box = rectOf(el);
    const at = atFor(el, box, "element");
    return { kind: "element", box, at, anchor: anchorFor(el, box, "element", at) };
  }

  function pinForBox(box) {
    const el = elementAt(box.x + box.w / 2, box.y + box.h / 2) || document.body;
    const at = atFor(el, box, "box");
    return { kind: "box", box, at, anchor: anchorFor(el, box, "box", at) };
  }

  function startAsk(pin) {
    cancelAsk();
    hideHover();
    asking = pin;
    const mark = document.createElement("div");
    mark.className = "asking-box";
    place(mark, pin.box);
    const form = document.createElement("form");
    form.className = "ask-form";
    const input = document.createElement("input");
    input.type = "text";
    input.placeholder = "What should change here?";
    input.spellcheck = true;
    input.setAttribute("aria-label", "Pin");
    const hint = document.createElement("span");
    hint.className = "ask-hint";
    hint.textContent = "Enter sends, Esc cancels";
    form.append(input, hint);
    layer.ask.replaceChildren(mark, form);
    layer.ask.classList.add("on");

    // Beside the spot: to the right if there is room, else below, else above.
    const w = 300;
    const b = pin.box;
    let x = b.x + b.w + 10;
    let y = b.y;
    if (x + w > innerWidth - 8) {
      x = Math.max(8, Math.min(b.x, innerWidth - w - 8));
      y = b.y + b.h + 10;
      if (y + 60 > innerHeight) y = Math.max(8, b.y - 66);
    }
    y = Math.max(8, Math.min(y, innerHeight - 66));
    form.style.transform = `translate(${x}px, ${y}px)`;

    form.addEventListener("submit", (e) => {
      e.preventDefault();
      const words = input.value.trim();
      if (!words) return;
      sendPin(pin, words);
    });
    input.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") { e.preventDefault(); cancelAsk(); }
    });
    // Keys typed here are the human's words, not the deck's or page's shortcuts.
    input.addEventListener("keyup", (e) => e.stopPropagation());
    input.addEventListener("keypress", (e) => e.stopPropagation());
    requestAnimationFrame(() => input.focus({ preventScroll: true }));
  }

  function cancelAsk() {
    if (!asking) return;
    asking = null;
    layer.ask.classList.remove("on");
    layer.ask.replaceChildren();
  }

  function sendPin(pin, words) {
    cancelAsk();
    const r = pin.box;
    post({
      forge: "pin",
      file: location.pathname,
      words,
      kind: pin.kind,
      at: pin.at,
      rect: { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.w), h: Math.round(r.h) },
      dpr: devicePixelRatio,
      anchor: pin.anchor,
    });
  }

  // ----------------------------------------------------------- pin runes

  let drawn = [];
  let runeFrame = 0;

  const RUNE = "M12 1.5 22.5 12 12 22.5 1.5 12Z";

  function drawPins(list) {
    if (!ensureOverlay()) return;
    layer.pins.replaceChildren();
    drawn = [];
    for (const p of list || []) {
      const box = document.createElement("div");
      box.className = "pin-box";
      const rune = document.createElement("div");
      rune.className = "rune";
      rune.innerHTML =
        `<svg viewBox="0 0 24 24" aria-hidden="true">` +
        `<path class="stone" d="${RUNE}"/>` +
        `<path class="trace" d="${RUNE}" pathLength="100"/></svg><b></b>`;
      rune.querySelector("b").textContent = String(p.n);
      rune.style.animationDelay = `${(drawn.length % 6) * -0.7}s`;
      rune.firstChild.lastChild.style.animationDelay = `${(drawn.length % 6) * -0.5}s`;
      layer.pins.append(box, rune);
      drawn.push({ p, box, rune });
    }
    if (drawn.length && !runeFrame) runeFrame = requestAnimationFrame(followPins);
  }

  function anchorRect(p) {
    if (p.selector) {
      let el = null;
      try { el = document.querySelector(p.selector); } catch (_) { el = null; }
      if (el) {
        const r = el.getBoundingClientRect();
        return r.width || r.height ? { x: r.left, y: r.top, w: r.width, h: r.height } : null;
      }
    }
    if (p.slide) {
      const s = slides()[p.slide - 1];
      if (!s) return null;
      const r = s.getBoundingClientRect();
      if (!r.width || !r.height) return null;
      if (p.slideRect) {
        const f = p.slideRect;
        return { x: r.left + f.x * r.width, y: r.top + f.y * r.height, w: f.w * r.width, h: f.h * r.height };
      }
      if (!p.rect) return { x: r.left, y: r.top, w: r.width, h: r.height };
    }
    if (p.rect) {
      return { x: p.rect.x - scrollX, y: p.rect.y - scrollY, w: p.rect.w, h: p.rect.h };
    }
    return null;
  }

  // Every frame while runes are up: decks transform slides and pages move
  // things by script, and no single event covers all of it.
  function followPins() {
    runeFrame = 0;
    if (!drawn.length) return;
    for (const d of drawn) {
      const r = anchorRect(d.p);
      const shown = !!r && r.y + r.h > -24 && r.y < innerHeight + 24;
      d.box.classList.toggle("on", shown);
      d.rune.classList.toggle("on", shown);
      if (!shown) continue;
      place(d.box, r);
      const x = Math.max(2, Math.min(innerWidth - 30, r.x - 13));
      const y = Math.max(2, Math.min(innerHeight - 30, r.y - 13));
      d.rune.style.transform = `translate(${x}px, ${y}px)`;
    }
    runeFrame = requestAnimationFrame(followPins);
  }

  function clearPins() {
    if (!root || !drawn.length) return;
    const going = drawn;
    drawn = [];
    for (const d of going) {
      d.box.classList.add("fading");
      d.rune.classList.add("fading");
    }
    setTimeout(() => { for (const d of going) { d.box.remove(); d.rune.remove(); } }, 700);
  }

  // ------------------------------------------------------------------- goto

  function flash(el) {
    if (!ensureOverlay() || !el) return;
    const r = rectOf(el);
    const f = document.createElement("div");
    f.className = "flash-box";
    place(f, r);
    layer.flash.appendChild(f);
    // Follows the smooth scroll that brought it here, then goes.
    const start = performance.now();
    const tick = () => {
      place(f, rectOf(el));
      if (performance.now() - start < 1600) requestAnimationFrame(tick);
      else f.remove();
    };
    requestAnimationFrame(tick);
  }

  function gotoLine(line) {
    let best = null;
    let span = Infinity;
    for (const el of document.querySelectorAll("[data-lines]")) {
      const l = parseLines(el.getAttribute("data-lines"));
      if (l && l[0] <= line && line <= l[1] && l[1] - l[0] < span) {
        best = el;
        span = l[1] - l[0];
      }
    }
    if (!best) {
      // The line is between blocks (a blank line): the next block after it.
      for (const el of document.querySelectorAll("[data-lines]")) {
        const l = parseLines(el.getAttribute("data-lines"));
        if (l && l[0] > line) { best = el; break; }
      }
    }
    if (!best) return;
    best.scrollIntoView({ block: "center", behavior: "smooth" });
    flash(best);
  }

  function gotoPage(n) {
    const el = document.querySelector(
      `[data-page="${n}"], [data-page-number="${n}"], #page-${n}, #page${n}`);
    if (el) el.scrollIntoView({ block: "start", behavior: "smooth" });
  }

  function gotoSlide(n) {
    const s = slides()[n - 1];
    if (s) s.scrollIntoView({ block: "start", behavior: "smooth" });
  }

  // ----------------------------------------------------------------- errors

  const seenErrors = new Set();

  function report(err, message) {
    const msg = message || (err && err.message) || String(err);
    const stack = (err && err.stack) || "";
    const key = msg + "\n" + stack;
    if (seenErrors.has(key)) return;
    seenErrors.add(key);
    post({ forge: "error", message: msg, stack });
    showError(msg, stack);
  }

  function showError(message, stack) {
    const show = () => {
      if (!ensureOverlay()) return;
      if (layer.errors.childElementCount >= 3) layer.errors.firstChild.remove();
      const card = document.createElement("div");
      card.className = "error-card";
      const head = document.createElement("div");
      head.className = "error-head";
      head.textContent = "This page hit an error";
      const close = document.createElement("button");
      close.className = "error-close";
      close.title = "Dismiss";
      close.textContent = "×";
      close.addEventListener("click", () => card.remove());
      head.appendChild(close);
      const text = document.createElement("div");
      text.className = "error-text";
      text.textContent = trim(message, 240);
      const send = document.createElement("button");
      send.className = "error-send";
      send.textContent = "Send to session";
      send.addEventListener("click", () => {
        post({ forge: "send-error", file: location.pathname, message, stack });
        send.disabled = true;
        send.textContent = "Sent";
      });
      card.append(head, text, send);
      layer.errors.appendChild(card);
    };
    if (document.documentElement) show();
    else addEventListener("DOMContentLoaded", show, { once: true });
  }

  addEventListener("error", (e) => {
    if (e.target && e.target !== window && e.target.nodeType === 1) {
      // A resource that failed to load: the event has no message of its own.
      const t = e.target;
      const what = t.currentSrc || t.src || t.href || selectorFor(t);
      report(null, `Could not load ${t.localName} ${what}`);
      return;
    }
    report(e.error, e.message);
  }, true);

  addEventListener("unhandledrejection", (e) => {
    const r = e.reason;
    report(r instanceof Error ? r : null, r instanceof Error ? r.message : `Unhandled rejection: ${String(r)}`);
  });

  // ------------------------------------------------------------------- keys

  function typing(e) {
    for (const el of e.composedPath()) {
      if (el === host) return true;
      if (el.nodeType !== 1) continue;
      if (el.isContentEditable) return true;
      const tag = el.localName;
      if (tag === "input" || tag === "textarea" || tag === "select") return true;
    }
    return false;
  }

  addEventListener("keydown", (e) => {
    if (e.defaultPrevented || e.repeat || e.ctrlKey || e.metaKey || e.altKey) return;
    if (typing(e)) return;
    if (e.key === "p" || e.key === "P") {
      e.preventDefault();
      setPinning(!pinning, true);
    } else if (e.key === "Escape" && pinning) {
      e.preventDefault();
      setPinning(false, true);
    }
  });

  // --------------------------------------------------------------- messages

  const builtin = {
    "restore-scroll": (m) => restoreScroll(+m.x || 0, +m.y || 0),
    "pinning": (m) => setPinning(m.on, false),
    "pins": (m) => drawPins(m.pins),
    "clear-pins": () => clearPins(),
    "goto": (m) => {
      if (m.slide) gotoSlide(+m.slide);
      else if (m.line) gotoLine(+m.line);
      else if (m.page) gotoPage(+m.page);
    },
  };

  function ready() {
    ensureOverlay();
    post({
      forge: "ready",
      file: location.pathname,
      title: document.title,
      slides: slides().length,
      x: Math.round(scrollX),
      y: Math.round(scrollY),
    });
  }

  if (document.readyState === "complete") ready();
  else addEventListener("load", ready, { once: true });
  if (document.readyState === "loading") {
    addEventListener("DOMContentLoaded", ensureOverlay, { once: true });
  } else {
    ensureOverlay();
  }

  window.__forge = Object.freeze({
    post,
    on,
    receive,
    sent,
    get pinning() { return pinning; },
    setPinning: (v) => setPinning(v, true),
    selectorFor,
    report,
  });
})();
