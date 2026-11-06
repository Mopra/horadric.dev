// The deck shell: shown around an HTML deck (a <section class="slide"> per
// slide), never written into the file. Scales slides to the pane, one at a
// time or as a scrolled list, with keys, a toolbar, a resizable slide
// strip, the speaker notes under the slide when there is room, and present
// mode; slides are added, moved, duplicated and deleted from the strip and
// the toolbar (written back to the deck's source by the host). Injected
// after forge.js; works without it for testing.
// The protocol is in README.md beside this file.
(() => {
  "use strict";
  if (window.__forgeDeck) return;

  const scriptSrc = document.currentScript && document.currentScript.src;
  const webview = window.chrome && window.chrome.webview;
  // The toolbar's height, the margin round the slide, and the strip's
  // gutter for slide numbers and its right padding (the thumbnail is the
  // rest of its width).
  const BAR_H = 44;
  const MARGIN = 20;
  const GUTTER = 30;
  const STRIP_PAD = 14;
  // Under this much room below the slide the notes are not worth showing.
  const NOTES_MIN = 96;
  const PATH = location.pathname;

  // Storage can be missing or refuse (a private profile, a file:// page):
  // the shell then just forgets.
  function stored(area, key) {
    try { return window[area].getItem(key); } catch (_) { return null; }
  }
  function store(area, key, value) {
    try {
      if (value === null) window[area].removeItem(key);
      else window[area].setItem(key, value);
    } catch (_) { /* forgets */ }
  }

  function post(msg) {
    if (window.__forge) window.__forge.post(msg);
    else if (webview) webview.postMessage(msg);
    else console.debug("[forge deck] post", msg);
  }

  let slides = [];
  let size = { w: 1920, h: 1080 };
  let current = 1;
  let mode = "single";
  let stripOn = true;
  let presenting = false;
  let wentFull = false;
  let modeBeforePresent = "single";
  let stripBeforePresent = true;
  let stripW = 0;
  let html, strip, ui, counter, notesBox, notesText, hint, buttons = {};

  function cssNode(name, key) {
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

  function findSlides() {
    return Array.from(document.querySelectorAll("section.slide:not(.forge-clone)"));
  }

  // The deck's own size wins when it says one; else the 16:9 that decks
  // here are made at.
  function designSize() {
    const px = (v) => {
      const n = parseFloat(v);
      return n > 0 ? n : 0;
    };
    const rootStyle = getComputedStyle(document.documentElement);
    const vw = px(rootStyle.getPropertyValue("--slide-width"));
    const vh = px(rootStyle.getPropertyValue("--slide-height"));
    if (vw && vh) return { w: vw, h: vh };
    for (const el of [document.documentElement, document.body, slides[0]]) {
      if (!el) continue;
      const w = px(el.getAttribute("data-width"));
      const h = px(el.getAttribute("data-height"));
      if (w && h) return { w, h };
    }
    // A slide laid out at a fixed size by the deck's own CSS is narrower or
    // wider than the box around it; one that just fills the page is not.
    const shown = slides.find((s) => s.offsetWidth > 0 && s.offsetHeight > 0);
    if (shown && shown.parentElement) {
      const around = shown.parentElement.clientWidth;
      // The declared width and height, not the border box: the shell makes
      // slides border-box, so padding falls inside the 16:9 as authors mean.
      const cs = getComputedStyle(shown);
      const w = px(cs.width);
      const h = px(cs.height);
      const ratio = w / h;
      if (Math.abs(shown.offsetWidth - around) > 2 && ratio > 1.2 && ratio < 2.4 && w >= 640) return { w, h };
    }
    return { w: 1920, h: 1080 };
  }

  function transparent(c) {
    return !c || c === "transparent" || /rgba\([^)]*,\s*0\)$/.test(c);
  }

  function slideLook() {
    const shown = slides.find((s) => getComputedStyle(s).display !== "none");
    const display = shown ? getComputedStyle(shown).display : "block";
    html.style.setProperty("--forge-slide-display", display === "inline" ? "block" : display);
    // A slide that leaves its background to the body would show the
    // letterbox through it once it stands alone. Asked again once the
    // deck's own stylesheet has loaded, so the class is cleared first.
    html.classList.remove("forge-nobg");
    const own = getComputedStyle(slides[0]).backgroundColor;
    if (transparent(own)) {
      let bg = getComputedStyle(document.body).backgroundColor;
      if (transparent(bg)) bg = getComputedStyle(document.documentElement).backgroundColor;
      if (transparent(bg)) bg = "#ffffff";
      html.style.setProperty("--forge-slide-bg", bg);
      html.classList.add("forge-nobg");
    }
  }

  // -------------------------------------------------------------- layout

  function stripShown() {
    return stripOn && !presenting;
  }

  // A strip a fifth of the pane, so thumbnails are big enough to read,
  // unless this deck was given another width by hand.
  function defaultStripW() {
    const kept = parseFloat(stored("localStorage", `forge-deck:strip:${PATH}`));
    return kept > 0 ? kept : Math.round(innerWidth * 0.2);
  }

  function clampStrip(w) {
    return Math.round(Math.max(150, Math.min(w, 560, innerWidth * 0.5)));
  }

  function layout() {
    const top = presenting ? 0 : BAR_H;
    const sw = clampStrip(stripW || defaultStripW());
    const left = stripShown() ? sw : 0;
    // The width inside the page's scrollbar, so the scroll view's margin
    // is even on both sides.
    const availW = Math.max(1, html.clientWidth - left);
    const availH = Math.max(1, innerHeight - top);
    const m = presenting ? 0 : MARGIN;
    let scale;
    let cy = top + availH / 2;
    let notesH = 0;
    if (mode === "single") {
      scale = Math.min((availW - m * 2) / size.w, (availH - m * 2) / size.h);
      // A pane taller than the slide needs: the slide goes to the top and
      // its notes fill what is left, instead of bands of nothing.
      const room = availH - m * 2 - size.h * scale;
      if (!presenting && room >= NOTES_MIN + 12) {
        cy = top + m + (size.h * scale) / 2;
        notesH = room - 12;
      }
    } else {
      scale = Math.min((availW - m * 2) / size.w, 4);
    }
    scale = Math.max(0.05, scale);
    html.style.setProperty("--forge-scale", String(scale));
    html.style.setProperty("--forge-cx", `${left + availW / 2}px`);
    html.style.setProperty("--forge-cy", `${cy}px`);
    html.style.setProperty("--forge-left", `${left}px`);
    html.style.setProperty("--forge-top", `${top}px`);
    html.style.setProperty("--forge-strip-w", `${sw}px`);
    html.classList.toggle("forge-single", mode === "single");
    html.classList.toggle("forge-scroll", mode === "scroll");
    html.classList.toggle("forge-strip-on", stripShown());
    html.classList.toggle("forge-presenting", presenting);
    if (strip) {
      strip.hidden = !stripShown();
      if (!strip.hidden) {
        // Measured once the strip wears its width; before the shell's
        // sheet has loaded, guessed with room for a thin scrollbar.
        const inside = strip.offsetWidth === sw ? strip.clientWidth : sw - 9;
        const thumbW = Math.max(40, inside - GUTTER - STRIP_PAD);
        html.style.setProperty("--forge-thumb-w", `${thumbW}px`);
        html.style.setProperty("--forge-thumb-scale", String(thumbW / size.w));
      }
    }
    if (ui) {
      ui.hidden = presenting;
      const sh = ui.__shell;
      sh.root.style.setProperty("--strip-w", `${left}px`);
      sh.root.classList.toggle("forge-narrow", innerWidth - left < 640);
      sh.grip.hidden = !stripShown();
      const slideW = size.w * scale;
      const showNotes = notesH > 0;
      notesBox.hidden = !showNotes;
      if (showNotes) {
        notesBox.style.left = `${left + (availW - slideW) / 2}px`;
        notesBox.style.width = `${slideW}px`;
        notesBox.style.top = `${top + m + size.h * scale + 12}px`;
        notesBox.style.maxHeight = `${notesH}px`;
      }
    }
  }

  function show(n, why) {
    n = Math.max(1, Math.min(slides.length, Math.round(n) || 1));
    const changed = n !== current;
    slides[current - 1] && slides[current - 1].classList.remove("forge-current");
    current = n;
    slides[current - 1].classList.add("forge-current");
    if (mode === "scroll" && why !== "scrolled") {
      slides[current - 1].scrollIntoView({ block: "start", behavior: why === "key" ? "smooth" : "auto" });
    }
    markStrip();
    if (counter) counter.textContent = `${current} / ${slides.length}`;
    if (buttons.prev) buttons.prev.disabled = current <= 1;
    if (buttons.next) buttons.next.disabled = current >= slides.length;
    if (notesText) {
      const said = notesOf(current);
      notesText.textContent = said || "No speaker notes on this slide.";
      notesText.classList.toggle("forge-none", !said);
    }
    // Kept for this tab, so a reload as the file changes stays put.
    store("sessionStorage", `forge-deck:at:${PATH}`, String(current));
    if (changed || why === "init") {
      post({ forge: "slide", n: current, of: slides.length, notes: notesOf(current) });
    }
  }

  function notesOf(n) {
    const s = slides[n - 1];
    const notes = s && s.querySelector("aside.notes");
    return notes ? notes.textContent.replace(/[ \t]+/g, " ").replace(/\n\s*\n\s*/g, "\n\n").trim() : "";
  }

  function setMode(m, quiet) {
    if (m !== "single" && m !== "scroll") return;
    const keep = current;
    mode = m;
    layout();
    show(keep, "mode");
    if (!quiet) announce();
  }

  function setStrip(onNow, quiet) {
    stripOn = !!onNow;
    layout();
    if (stripOn) markStrip(true);
    if (!quiet) announce();
  }

  function announce() {
    if (buttons.one) buttons.one.classList.toggle("forge-on", mode === "single");
    if (buttons.all) buttons.all.classList.toggle("forge-on", mode === "scroll");
    if (buttons.strip) buttons.strip.classList.toggle("forge-on", stripOn);
    store("sessionStorage", `forge-deck:view:${PATH}`, JSON.stringify({ mode, strip: stripOn }));
    post({ forge: "deck", slides: slides.length, w: size.w, h: size.h, mode, strip: stripOn });
  }

  // In the scrolled list the slide in view is the current one.
  let scrollPending = false;
  addEventListener("scroll", () => {
    if (mode !== "scroll" || scrollPending || !slides.length) return;
    scrollPending = true;
    requestAnimationFrame(() => {
      scrollPending = false;
      // A line a third down: a slide just brought to the top counts as in view.
      const mid = innerHeight / 3;
      let best = current;
      let dist = Infinity;
      slides.forEach((s, i) => {
        const r = s.getBoundingClientRect();
        const d = r.top <= mid && r.bottom >= mid ? 0 : Math.min(Math.abs(r.top - mid), Math.abs(r.bottom - mid));
        if (d < dist) { dist = d; best = i + 1; }
      });
      if (best !== current) show(best, "scrolled");
    });
  }, { passive: true });

  // --------------------------------------------------------------- strip

  function buildStrip() {
    if (strip) strip.remove();
    strip = document.createElement("div");
    strip.className = "forge-strip";
    strip.setAttribute("aria-label", "Slides");
    html.style.setProperty("--forge-ratio", `${size.w} / ${size.h}`);
    slides.forEach((s, i) => {
      const thumb = document.createElement("div");
      thumb.className = "forge-thumb";
      const clone = s.cloneNode(true);
      clone.classList.add("forge-clone");
      clone.classList.remove("forge-current");
      clone.removeAttribute("id");
      clone.setAttribute("aria-hidden", "true");
      clone.setAttribute("inert", "");
      // Thumbnails are pictures: no duplicate ids for the page's scripts
      // to find, nothing that runs or plays.
      for (const el of clone.querySelectorAll("[id]")) el.removeAttribute("id");
      for (const el of clone.querySelectorAll("script, iframe, video, audio, object, embed")) el.remove();
      const num = document.createElement("span");
      num.className = "forge-thumb-n";
      num.textContent = String(i + 1);
      thumb.append(clone, num);
      thumb.addEventListener("click", () => show(i + 1, "strip"));
      thumbEdits(thumb, i);
      strip.appendChild(thumb);
    });
    const add = document.createElement("button");
    add.type = "button";
    add.className = "forge-strip-add";
    add.textContent = "+ Add slide";
    add.title = "A new empty slide in the layout of the current one, after it";
    add.addEventListener("click", () => addSlide(current - 1));
    strip.appendChild(add);
    document.body.appendChild(strip);
    strip.hidden = !stripShown();
  }

  function markStrip(reveal) {
    if (!strip) return;
    const thumbs = strip.querySelectorAll(":scope > .forge-thumb");
    for (let i = 0; i < thumbs.length; i++) thumbs[i].classList.toggle("forge-on", i === current - 1);
    const on = thumbs[current - 1];
    if (on && !strip.hidden) {
      const top = on.offsetTop - 12;
      const bottom = on.offsetTop + on.offsetHeight + 12;
      if (reveal) strip.scrollTop = top - (strip.clientHeight - on.offsetHeight) / 2;
      else if (top < strip.scrollTop) strip.scrollTop = top;
      else if (bottom > strip.scrollTop + strip.clientHeight) strip.scrollTop = bottom - strip.clientHeight;
    }
  }


  // --------------------------------------------------------- slide edits

  // Each is sent as the new list of the current slides by number; the host
  // moves whole <section class="slide"> elements in the source and the
  // page loads again. Nothing moves here first, so a refusal leaves the
  // strip true to the file.
  let editSeq = 0;
  let dragFrom = -1;

  // `then`: the slide to show once the page loads again with the change.
  function sendOrder(order, then, blank) {
    saving(true);
    if (then) store("sessionStorage", `forge-deck:then:${PATH}`, String(then));
    const msg = { forge: "reorder", id: `deck-${++editSeq}`, file: PATH, count: slides.length, order };
    if (blank) msg.blank = true;
    post(msg);
  }

  function saving(on) {
    if (strip) strip.classList.toggle("forge-saving", on);
    if (ui) ui.__shell.root.classList.toggle("forge-saving", on);
  }

  const asIs = () => slides.map((_, i) => i + 1);

  function moveSlide(from, to) {
    const order = asIs();
    const [moved] = order.splice(from, 1);
    order.splice(to > from ? to - 1 : to, 0, moved);
    if (order.some((k, i) => k !== i + 1)) sendOrder(order, order.indexOf(current) + 1);
  }

  function duplicateSlide(i) {
    const order = asIs();
    order.splice(i + 1, 0, i + 1);
    sendOrder(order, i + 2);
  }

  // A duplicate whose copy the host empties of words: the new slide keeps
  // the layout of the one it follows.
  function addSlide(i) {
    const order = asIs();
    order.splice(i + 1, 0, i + 1);
    sendOrder(order, i + 2, true);
  }

  function deleteSlide(i) {
    if (slides.length < 2) return;
    const then = current > i + 1 ? current - 1 : Math.min(current, slides.length - 1);
    sendOrder(asIs().filter((k) => k !== i + 1), then);
  }

  function clearDrop() {
    if (!strip) return;
    for (const t of strip.querySelectorAll(".forge-thumb")) t.classList.remove("forge-drop-before", "forge-drop-after");
  }

  function thumbEdits(thumb, i) {
    thumb.draggable = true;
    const below = (e) => {
      const r = thumb.getBoundingClientRect();
      return e.clientY > r.top + r.height / 2;
    };
    thumb.addEventListener("dragstart", (e) => {
      dragFrom = i;
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/plain", String(i + 1));
      thumb.classList.add("forge-dragging");
    });
    thumb.addEventListener("dragend", () => {
      dragFrom = -1;
      clearDrop();
      thumb.classList.remove("forge-dragging");
    });
    thumb.addEventListener("dragover", (e) => {
      if (dragFrom < 0) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      clearDrop();
      thumb.classList.add(below(e) ? "forge-drop-after" : "forge-drop-before");
    });
    thumb.addEventListener("drop", (e) => {
      if (dragFrom < 0) return;
      e.preventDefault();
      const from = dragFrom;
      dragFrom = -1;
      clearDrop();
      moveSlide(from, below(e) ? i + 1 : i);
    });

    const tools = document.createElement("span");
    tools.className = "forge-thumb-tools";
    const tool = (icon, title, fn) => {
      const b = document.createElement("button");
      b.type = "button";
      b.innerHTML = icon;
      b.title = title;
      b.draggable = false;
      b.addEventListener("click", (e) => { e.stopPropagation(); fn(b); });
      tools.appendChild(b);
    };
    // Drawn, not typed: a font may show these marks as colour emoji.
    const copyIcon = `<svg viewBox="0 0 16 16"><rect x="5.5" y="5.5" width="8" height="8" rx="1"/><path d="M10.5 3.5v-1h-8v8h1"/></svg>`;
    const binIcon = `<svg viewBox="0 0 16 16"><path d="M4 4l8 8M12 4l-8 8"/></svg>`;
    tool(copyIcon, "Duplicate this slide", () => duplicateSlide(i));
    // Two clicks to delete: the first only asks.
    tool(binIcon, "Delete this slide (click twice)", (b) => {
      if (b.classList.contains("forge-sure")) { deleteSlide(i); return; }
      b.classList.add("forge-sure");
      b.textContent = "Delete?";
      setTimeout(() => { b.classList.remove("forge-sure"); b.innerHTML = binIcon; }, 2500);
    });
    thumb.appendChild(tools);
  }

  // ------------------------------------------------------------------ UI

  const ICONS = {
    strip: `<svg viewBox="0 0 16 16"><rect x="1.5" y="2" width="4" height="3" rx=".5"/><rect x="1.5" y="6.5" width="4" height="3" rx=".5"/><rect x="1.5" y="11" width="4" height="3" rx=".5"/><rect x="7.5" y="2" width="7" height="12" rx=".8"/></svg>`,
    prev: `<svg viewBox="0 0 16 16"><path d="M10 3L5 8l5 5"/></svg>`,
    next: `<svg viewBox="0 0 16 16"><path d="M6 3l5 5-5 5"/></svg>`,
    one: `<svg viewBox="0 0 16 16"><rect x="1.5" y="3.5" width="13" height="9" rx=".8"/></svg>`,
    all: `<svg viewBox="0 0 16 16"><rect x="2" y="1.5" width="12" height="5.5" rx=".8"/><rect x="2" y="9" width="12" height="5.5" rx=".8"/></svg>`,
    add: `<svg viewBox="0 0 16 16"><path d="M8 3v10M3 8h10"/></svg>`,
    present: `<svg viewBox="0 0 16 16"><path d="M2 5V2h3M11 2h3v3M14 11v3h-3M5 14H2v-3"/></svg>`,
    help: `<svg viewBox="0 0 16 16"><path d="M6 6a2 2 0 1 1 2.6 1.9c-.4.2-.6.5-.6 1V10"/><path d="M8 12.6v.1"/></svg>`,
  };

  // Three of Horadric's runes (Thurisaz, Raidho, Kenaz), kindling in turn
  // while a slide edit is being written.
  const RUNES = ["M-0.15 0L-0.15 1M-0.15 0.25L0.22 0.5L-0.15 0.75",
    "M-0.18 1L-0.18 0L0.2 0.22L-0.18 0.45L0.2 1", "M0.15 0.1L-0.15 0.5L0.15 0.9"];

  const HINT_KEY = "forge-deck:hinted";

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text) e.textContent = text;
    return e;
  }

  function buildUI() {
    if (ui) ui.remove();
    buttons = {};
    ui = document.createElement("forge-deck-ui");
    // Covers the pane but lets the pointer through: only its own parts
    // take clicks.
    ui.setAttribute("style",
      "all:initial !important; position:fixed !important; inset:0 !important;" +
      "pointer-events:none !important; z-index:2147483646 !important; display:block !important;");
    const shadow = ui.attachShadow({ mode: "closed" });
    const css = cssNode("deck.css", "deckCss");
    if (css) shadow.appendChild(css);
    const root = el("div", "forge-ui");
    const button = (name, label, title, fn) => {
      const b = el("button", "forge-key");
      b.type = "button";
      b.title = title;
      b.innerHTML = ICONS[name];
      if (label) {
        b.classList.add("forge-labelled");
        b.append(el("span", "forge-label", label));
      }
      b.addEventListener("click", (e) => { e.currentTarget.blur(); fn(); });
      buttons[name] = b;
      return b;
    };
    const group = (...kids) => {
      const g = el("div", "forge-group");
      g.append(...kids);
      return g;
    };

    const bar = el("div", "forge-bar");
    counter = el("span", "forge-count");
    const runes = el("span", "forge-runes");
    runes.title = "Writing the change to the deck";
    runes.innerHTML = RUNES.map((d) =>
      `<svg viewBox="-0.35 -0.08 0.7 1.16"><path vector-effect="non-scaling-stroke" d="${d}"/></svg>`).join("");
    bar.append(
      group(button("strip", "Slides", "Show or hide the slide strip (S)", () => setStrip(!stripOn))),
      group(
        button("prev", "", "Previous slide (Left)", () => show(current - 1, "key")),
        counter,
        button("next", "", "Next slide (Right)", () => show(current + 1, "key")),
      ),
      group(
        button("one", "One", "One slide at a time (V)", () => setMode("single")),
        button("all", "All", "Every slide in a column (V)", () => setMode("scroll")),
      ),
      el("span", "forge-fill"),
      runes,
      button("help", "", "How to work on the deck", () => showHint(hint.hidden)),
      button("add", "Add slide", "A new empty slide in the layout of this one, after it", () => addSlide(current - 1)),
      button("present", "Present", "Present full screen (F), Esc ends it", () => present(true)),
    );

    // The strip's edge, dragged to make the thumbnails bigger or smaller.
    const grip = el("div", "forge-grip");
    grip.title = "Drag to resize the slide strip, double click for its usual width";
    grip.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      grip.setPointerCapture(e.pointerId);
      root.classList.add("forge-resizing");
      const move = (ev) => {
        stripW = clampStrip(ev.clientX);
        layout();
      };
      const up = () => {
        grip.removeEventListener("pointermove", move);
        root.classList.remove("forge-resizing");
        if (stripW) store("localStorage", `forge-deck:strip:${PATH}`, String(stripW));
        markStrip();
      };
      grip.addEventListener("pointermove", move);
      grip.addEventListener("pointerup", up, { once: true });
      grip.addEventListener("pointercancel", up, { once: true });
    });
    grip.addEventListener("dblclick", () => {
      store("localStorage", `forge-deck:strip:${PATH}`, null);
      stripW = 0;
      layout();
    });

    notesBox = el("div", "forge-notes");
    notesBox.append(el("div", "forge-legend", "Speaker notes"));
    notesText = el("div", "forge-notes-text");
    notesBox.append(notesText);

    hint = el("div", "forge-hint");
    const tips = el("ul");
    for (const [k, t] of [
      ["Double click", "any text on the slide to change it"],
      ["P", "then click or drag on the slide to point Claude at it"],
      ["Drag", "a thumbnail in the strip to move the slide"],
      ["← →", "go through the slides, F presents"],
    ]) {
      const li = el("li");
      li.append(el("kbd", "", k), el("span", "", t));
      tips.append(li);
    }
    const ok = el("button", "forge-key forge-ok", "Got it");
    ok.type = "button";
    ok.addEventListener("click", () => showHint(false));
    hint.append(el("div", "forge-legend", "Working on a deck"), tips, ok);
    hint.hidden = true;

    root.append(bar, grip, notesBox, hint);
    shadow.appendChild(root);
    ui.__shell = { root, grip };
    document.documentElement.appendChild(ui);
  }

  function showHint(on) {
    if (!hint) return;
    hint.hidden = !on;
    if (buttons.help) buttons.help.classList.toggle("forge-on", on);
    if (!on) store("localStorage", HINT_KEY, "1");
  }

  // ------------------------------------------------------------- present

  async function present(onNow) {
    onNow = !!onNow;
    if (onNow === presenting) return;
    if (onNow) {
      modeBeforePresent = mode;
      stripBeforePresent = stripOn;
      presenting = true;
      mode = "single";
      layout();
      show(current, "mode");
      wentFull = false;
      if (!document.fullscreenElement && document.documentElement.requestFullscreen) {
        try {
          await document.documentElement.requestFullscreen({ navigationUI: "hide" });
          wentFull = true;
        } catch (_) {
          // No user gesture (asked by the host): present in the pane and
          // let the host make its window full screen.
        }
      }
      post({ forge: "present", on: true, slide: current, of: slides.length, notes: notesOf(current), fullscreen: wentFull });
    } else {
      presenting = false;
      if (document.fullscreenElement && document.exitFullscreen) {
        try { await document.exitFullscreen(); } catch (_) { /* already out */ }
      }
      wentFull = false;
      mode = modeBeforePresent;
      stripOn = stripBeforePresent;
      layout();
      show(current, "mode");
      post({ forge: "present", on: false, slide: current, of: slides.length });
    }
  }

  document.addEventListener("fullscreenchange", () => {
    if (!document.fullscreenElement && presenting && wentFull) present(false);
    layout();
  });

  // ------------------------------------------------------------------ keys

  function typing(e) {
    const t = e.target;
    if (!t || t.nodeType !== 1) return false;
    // Keys inside the Forge's own overlay (the pin words) are not for us.
    if (t.localName === "forge-overlay" || t.localName === "forge-deck-ui") return true;
    if (t.isContentEditable) return true;
    return t.localName === "input" || t.localName === "textarea" || t.localName === "select";
  }

  function onKey(e) {
    if (!slides.length || e.ctrlKey || e.metaKey || e.altKey || typing(e)) return;
    const k = e.key;
    let act = null;
    const nativeScroll = mode === "scroll" && (k === "ArrowUp" || k === "ArrowDown");
    if (nativeScroll) return;
    if (k === "ArrowRight" || k === "ArrowDown" || k === "PageDown" || (k === " " && !e.shiftKey)) {
      act = () => show(current + 1, "key");
    } else if (k === "ArrowLeft" || k === "ArrowUp" || k === "PageUp" || (k === " " && e.shiftKey)) {
      act = () => show(current - 1, "key");
    } else if (k === "Home") {
      act = () => show(1, "key");
    } else if (k === "End") {
      act = () => show(slides.length, "key");
    } else if (e.repeat) {
      return;
    } else if (k === "s" || k === "S") {
      act = () => setStrip(!stripOn);
    } else if (k === "v" || k === "V") {
      act = () => setMode(mode === "single" ? "scroll" : "single");
    } else if (k === "f" || k === "F" || k === "F5") {
      act = () => present(!presenting);
    } else if (k === "Escape" && presenting) {
      act = () => present(false);
    }
    if (!act) return;
    // The shell owns navigation: a deck with keys of its own would
    // otherwise move two slides per press.
    e.preventDefault();
    e.stopImmediatePropagation();
    act();
  }

  // ---------------------------------------------------------------- init

  function init() {
    if (window.__forgeDeck) return;
    slides = findSlides();
    if (!slides.length) return;
    html = document.documentElement;
    const css = cssNode("deck.css", "deckCss");
    if (css) (document.head || html).appendChild(css);
    size = designSize();
    html.style.setProperty("--forge-w", `${size.w}px`);
    html.style.setProperty("--forge-h", `${size.h}px`);
    // Print pages are 13.333 by 7.5 inches, PowerPoint's widescreen, which
    // is 1280 by 720 CSS pixels.
    html.style.setProperty("--forge-print-zoom", String(1280 / size.w));
    slideLook();
    html.classList.add("forge-deck");
    if (innerWidth < 720) stripOn = false;
    let view = null;
    try { view = JSON.parse(stored("sessionStorage", `forge-deck:view:${PATH}`)); } catch (_) { /* none */ }
    if (view && (view.mode === "single" || view.mode === "scroll")) mode = view.mode;
    if (view && typeof view.strip === "boolean") stripOn = view.strip;
    buildStrip();
    buildUI();
    layout();
    // Where the last slide edit said to go, else where this tab was.
    const then = +stored("sessionStorage", `forge-deck:then:${PATH}`);
    store("sessionStorage", `forge-deck:then:${PATH}`, null);
    const at = then || +stored("sessionStorage", `forge-deck:at:${PATH}`) || 1;
    current = 0;
    show(at, "init");
    announce();
    if (!stored("localStorage", HINT_KEY)) showHint(true);
    addEventListener("resize", layout);
    // Runs at DOMContentLoaded, before a linked stylesheet may have
    // arrived: measured then, a dark deck looked bare and was painted
    // white. Measured again once everything has loaded.
    // The size is not asked again: the shell's own rules now pin it.
    const settle = () => {
      slideLook();
      layout();
      markStrip(true);
      if (mode === "scroll") show(current, "mode");
    };
    if (document.readyState === "complete") settle();
    else addEventListener("load", settle, { once: true });
    if (css && css.localName === "link") css.addEventListener("load", settle, { once: true });
    addEventListener("keydown", onKey, true);
    // A click on the slide moves on while presenting, as a clicker would.
    document.addEventListener("click", (e) => {
      if (presenting && !e.defaultPrevented && !(e.target.closest && e.target.closest("a, button, input, textarea, select, [onclick]"))) {
        show(current + 1, "key");
      }
    });

    const onGoto = (m) => {
      if (!m.slide) return false;
      show(+m.slide, "host");
      return true;
    };
    const onPresent = (m) => { present(m.on); return true; };
    const onView = (m) => {
      if (m.mode) setMode(m.mode, true);
      if (typeof m.strip === "boolean") setStrip(m.strip, true);
      announce();
      return true;
    };
    // The host's answer to a slide edit; forge.js says saved or why not.
    const onEdited = (m) => {
      if (m.what === "reorder") {
        saving(false);
        // Refused: no reload comes, so nowhere to go after one.
        if (!m.ok) store("sessionStorage", `forge-deck:then:${PATH}`, null);
      }
      return false;
    };
    if (window.__forge) {
      window.__forge.on("edit-result", onEdited);
      window.__forge.on("goto", onGoto);
      window.__forge.on("present", onPresent);
      window.__forge.on("deck-view", onView);
    } else if (webview) {
      webview.addEventListener("message", (e) => {
        let m = e.data;
        if (typeof m === "string") { try { m = JSON.parse(m); } catch (_) { return; } }
        if (!m) return;
        if (m.forge === "goto") onGoto(m);
        else if (m.forge === "present") onPresent(m);
        else if (m.forge === "deck-view") onView(m);
      });
    }

    window.__forgeDeck = Object.freeze({
      goto: (n) => show(n, "host"),
      next: () => show(current + 1, "key"),
      prev: () => show(current - 1, "key"),
      present,
      setMode,
      setStrip,
      get current() { return current; },
      get count() { return slides.length; },
      get mode() { return mode; },
      get size() { return { ...size }; },
      rebuildStrip: buildStrip,
      moveSlide: (n, before) => moveSlide(n - 1, before - 1),
      duplicateSlide: (n) => duplicateSlide(n - 1),
      deleteSlide: (n) => deleteSlide(n - 1),
      addSlide: (after) => addSlide((after || current) - 1),
    });
  }

  if (document.readyState === "loading") addEventListener("DOMContentLoaded", init, { once: true });
  else init();
})();
