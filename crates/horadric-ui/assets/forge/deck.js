// The deck shell: shown around an HTML deck (a <section class="slide"> per
// slide), never written into the file. Scales slides to the pane, one at a
// time or as a scrolled list, with keys, a slide strip, a counter and
// present mode. Injected after forge.js; works without it for testing.
// The protocol is in README.md beside this file.
(() => {
  "use strict";
  if (window.__forgeDeck) return;

  const scriptSrc = document.currentScript && document.currentScript.src;
  const webview = window.chrome && window.chrome.webview;
  const STRIP_W = 196;
  const THUMB_W = 164;

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
  let html, strip, ui, counter, buttons = {};

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
    // letterbox through it once it stands alone.
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

  function layout() {
    const left = stripShown() ? STRIP_W : 0;
    const availW = Math.max(1, innerWidth - left);
    const availH = Math.max(1, innerHeight);
    let scale;
    if (mode === "single") {
      const pad = presenting ? 0 : 16;
      scale = Math.min((availW - pad * 2) / size.w, (availH - pad * 2) / size.h);
    } else {
      scale = Math.min((availW - 48) / size.w, 4);
    }
    html.style.setProperty("--forge-scale", String(Math.max(0.05, scale)));
    html.style.setProperty("--forge-cx", `${left + availW / 2}px`);
    html.style.setProperty("--forge-left", `${left}px`);
    html.classList.toggle("forge-single", mode === "single");
    html.classList.toggle("forge-scroll", mode === "scroll");
    html.classList.toggle("forge-strip-on", stripShown());
    html.classList.toggle("forge-presenting", presenting);
    if (strip) strip.hidden = !stripShown();
    if (ui) ui.hidden = presenting;
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
    if (buttons.view) buttons.view.classList.toggle("on", mode === "scroll");
    if (buttons.strip) buttons.strip.classList.toggle("on", stripOn);
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
    const scale = THUMB_W / size.w;
    html.style.setProperty("--forge-thumb-scale", String(scale));
    slides.forEach((s, i) => {
      const thumb = document.createElement("div");
      thumb.className = "forge-thumb";
      thumb.style.height = `${Math.round(size.h * scale)}px`;
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
      strip.appendChild(thumb);
    });
    document.body.appendChild(strip);
    strip.hidden = !stripShown();
  }

  function markStrip(reveal) {
    if (!strip) return;
    const thumbs = strip.children;
    for (let i = 0; i < thumbs.length; i++) thumbs[i].classList.toggle("forge-on", i === current - 1);
    const on = thumbs[current - 1];
    if (on && !strip.hidden) {
      const top = on.offsetTop;
      const bottom = top + on.offsetHeight;
      if (reveal || top < strip.scrollTop || bottom > strip.scrollTop + strip.clientHeight) {
        strip.scrollTop = top - (strip.clientHeight - on.offsetHeight) / 2;
      }
    }
  }

  // ------------------------------------------------------------------ UI

  const ICONS = {
    strip: `<svg viewBox="0 0 16 16"><rect x="1.5" y="2" width="4" height="3" rx=".5"/><rect x="1.5" y="6.5" width="4" height="3" rx=".5"/><rect x="1.5" y="11" width="4" height="3" rx=".5"/><rect x="7.5" y="2" width="7" height="12" rx=".8"/></svg>`,
    view: `<svg viewBox="0 0 16 16"><rect x="2" y="1.5" width="12" height="5.5" rx=".8"/><rect x="2" y="9" width="12" height="5.5" rx=".8"/></svg>`,
    present: `<svg viewBox="0 0 16 16"><path d="M2 5V2h3M11 2h3v3M14 11v3h-3M5 14H2v-3"/></svg>`,
  };

  function buildUI() {
    if (ui) ui.remove();
    ui = document.createElement("forge-deck-ui");
    ui.setAttribute("style",
      "all:initial !important; position:fixed !important; right:12px !important;" +
      "bottom:12px !important; z-index:2147483646 !important; display:block !important;");
    const shadow = ui.attachShadow({ mode: "closed" });
    const css = cssNode("deck.css", "deckCss");
    if (css) shadow.appendChild(css);
    const bar = document.createElement("div");
    bar.className = "bar";
    const button = (name, title, fn) => {
      const b = document.createElement("button");
      b.className = "btn";
      b.title = title;
      b.innerHTML = ICONS[name];
      b.addEventListener("click", fn);
      buttons[name] = b;
      return b;
    };
    counter = document.createElement("span");
    counter.className = "count";
    bar.append(
      button("strip", "Slide strip (S)", () => setStrip(!stripOn)),
      button("view", "One slide or all (V)", () => setMode(mode === "single" ? "scroll" : "single")),
      button("present", "Present (F)", () => present(true)),
      counter,
    );
    shadow.appendChild(bar);
    document.documentElement.appendChild(ui);
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
    buildStrip();
    buildUI();
    layout();
    current = 0;
    show(1, "init");
    announce();
    addEventListener("resize", layout);
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
    if (window.__forge) {
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
    });
  }

  if (document.readyState === "loading") addEventListener("DOMContentLoaded", init, { once: true });
  else init();
})();
