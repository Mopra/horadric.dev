// Pins on an ordinary web page, run by forge/page_pins.rs in a world of
// its own (Page.createIsolatedWorld): the page's scripts share the DOM
// with it but never its objects, and nothing here can post to the host.
// Each evaluation waits for the next pin and answers with it as JSON, or
// with null when the human stops. The host gives `accepted` as whether it
// took the last answer, so its rune is drawn only then.
//
// Only trusted events count: a page can dispatch clicks and keys at its
// own elements, but not ones the browser marks as the human's.
(function (css, accepted) {
  const w = window;
  if (!w.__hdPins) w.__hdPins = make();
  return w.__hdPins.next(accepted);

  function make() {
    let host = null;
    let root = null;
    const layer = {};
    let waiting = null;
    let asking = null;
    let last = null;
    let drawn = [];
    let frame = 0;
    const RUNE = "M12 1.5 22.5 12 12 22.5 1.5 12Z";

    function overlay() {
      if (root && host.isConnected) return root;
      if (!document.documentElement) return null;
      host = document.createElement("horadric-pins");
      host.setAttribute("style",
        "all:initial !important; position:fixed !important; inset:0 !important;" +
        "z-index:2147483647 !important; pointer-events:none !important;" +
        "display:block !important; contain:strict !important;");
      root = host.attachShadow({ mode: "closed" });
      const style = document.createElement("style");
      style.textContent = css;
      root.appendChild(style);
      for (const name of ["pins", "catcher", "hover", "hover-tag", "ask"]) {
        const div = document.createElement("div");
        div.className = name;
        root.appendChild(div);
        layer[name] = div;
      }
      wire();
      document.documentElement.appendChild(host);
      // Runes drawn before the page took the overlay out come back with it.
      if (drawn.length) redraw();
      return root;
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

    function elementAt(x, y) {
      for (const el of document.elementsFromPoint(x, y)) {
        if (el !== host) return el;
      }
      return null;
    }

    function selector(el) {
      const unique = (e) => e.id && document.querySelectorAll("#" + CSS.escape(e.id)).length === 1;
      if (unique(el)) return "#" + CSS.escape(el.id);
      const parts = [];
      for (let e = el; e && e.nodeType === 1 && e !== document.documentElement; e = e.parentElement) {
        if (unique(e)) { parts.unshift("#" + CSS.escape(e.id)); break; }
        let part = e.localName;
        const same = e.parentElement
          ? [...e.parentElement.children].filter((c) => c.localName === e.localName)
          : [];
        if (same.length > 1) part += `:nth-of-type(${same.indexOf(e) + 1})`;
        parts.unshift(part);
      }
      return parts.join(" > ");
    }

    // Cut by characters, not UTF-16 units: half an emoji is a lone
    // surrogate, which the JSON reader on the other side refuses.
    const cut = (t, n) => t.length <= n ? t : Array.from(t.slice(0, n + 1)).slice(0, -1).join("");

    // The markup without what is typed in its fields: a login the Vault
    // filled must not reach a session through a pin.
    function markup(el) {
      const copy = el.cloneNode(true);
      const fields = [copy, ...copy.querySelectorAll("input, textarea, select")];
      for (const f of fields) {
        if (f.localName === "input") f.removeAttribute("value");
        if (f.localName === "textarea") f.textContent = "";
      }
      return cut(copy.outerHTML, 4000);
    }

    function hideHover() {
      layer.hover.classList.remove("on");
      layer["hover-tag"].classList.remove("on");
    }

    function wire() {
      const c = layer.catcher;
      c.addEventListener("pointermove", (e) => {
        if (asking || !e.isTrusted) return;
        const el = elementAt(e.clientX, e.clientY);
        if (!el) { hideHover(); return; }
        const r = rectOf(el);
        place(layer.hover, r);
        layer.hover.classList.add("on");
        layer["hover-tag"].textContent = el.localName + (el.id ? "#" + el.id : "");
        layer["hover-tag"].style.transform =
          `translate(${Math.max(0, r.x)}px, ${Math.max(0, r.y - 20)}px)`;
        layer["hover-tag"].classList.add("on");
      });
      c.addEventListener("pointerleave", hideHover);
      c.addEventListener("pointerdown", (e) => {
        e.preventDefault();
        if (!e.isTrusted || e.button !== 0) return;
        const el = elementAt(e.clientX, e.clientY);
        if (el) ask(el);
      });
      c.addEventListener("contextmenu", (e) => e.preventDefault());
    }

    function ask(el) {
      cancelAsk();
      hideHover();
      const box = rectOf(el);
      asking = { el, box };
      const mark = document.createElement("div");
      mark.className = "asking-box";
      place(mark, box);
      const form = document.createElement("div");
      form.className = "ask-form";
      const input = document.createElement("input");
      input.type = "text";
      input.placeholder = "What should change here?";
      input.setAttribute("aria-label", "Pin");
      const hint = document.createElement("span");
      hint.className = "ask-hint";
      hint.textContent = "Enter sends, Esc cancels";
      form.append(input, hint);
      layer.ask.replaceChildren(mark, form);
      layer.ask.classList.add("on");

      const fw = 300;
      let x = box.x + box.w + 10;
      let y = box.y;
      if (x + fw > innerWidth - 8) {
        x = Math.max(8, Math.min(box.x, innerWidth - fw - 8));
        y = box.y + box.h + 10;
        if (y + 60 > innerHeight) y = Math.max(8, box.y - 66);
      }
      y = Math.max(8, Math.min(y, innerHeight - 66));
      form.style.transform = `translate(${x}px, ${y}px)`;

      input.addEventListener("keydown", (e) => {
        e.stopPropagation();
        if (!e.isTrusted) return;
        if (e.key === "Escape") { e.preventDefault(); cancelAsk(); }
        if (e.key === "Enter") {
          e.preventDefault();
          const words = input.value.trim();
          if (words) send(asking, words);
        }
      });
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

    function send(pin, words) {
      cancelAsk();
      const el = pin.el;
      const r = rectOf(el);
      const doc = { x: r.x + scrollX, y: r.y + scrollY, w: r.w, h: r.h };
      const sel = selector(el);
      last = { selector: sel, rect: doc };
      finish(JSON.stringify({
        words: cut(words, 4000),
        selector: sel,
        tag: el.localName,
        text: cut(el.innerText || el.textContent || "", 2000),
        html: markup(el),
        rect: doc,
      }));
    }

    // Pin mode ends with each answer and the host's next call starts it
    // again, so a host that stops asking leaves the page as it was.
    function finish(answer) {
      const done = waiting;
      waiting = null;
      cancelAsk();
      hideHover();
      if (layer.catcher) layer.catcher.classList.remove("on");
      removeEventListener("keydown", onKey, true);
      document.removeEventListener("visibilitychange", onHidden);
      if (done) done(answer);
    }

    function onKey(e) {
      if (!e.isTrusted || e.key !== "Escape") return;
      e.preventDefault();
      e.stopImmediatePropagation();
      if (asking) cancelAsk(); else finish(null);
    }

    function onHidden() {
      if (document.visibilityState === "hidden") finish(null);
    }

    function anchorRect(p) {
      let el = null;
      try { el = document.querySelector(p.selector); } catch (_) { el = null; }
      if (el) {
        const r = el.getBoundingClientRect();
        if (r.width || r.height) return { x: r.left, y: r.top, w: r.width, h: r.height };
      }
      return { x: p.rect.x - scrollX, y: p.rect.y - scrollY, w: p.rect.w, h: p.rect.h };
    }

    function redraw() {
      layer.pins.replaceChildren();
      for (const d of drawn) layer.pins.append(d.box, d.rune);
      if (!frame) frame = requestAnimationFrame(follow);
    }

    function rune(p) {
      const box = document.createElement("div");
      box.className = "pin-box";
      const mark = document.createElement("div");
      mark.className = "rune";
      mark.innerHTML =
        `<svg viewBox="0 0 24 24" aria-hidden="true">` +
        `<path class="stone" d="${RUNE}"/>` +
        `<path class="trace" d="${RUNE}" pathLength="100"/></svg><b></b>`;
      mark.querySelector("b").textContent = String(drawn.length + 1);
      mark.style.animationDelay = `${(drawn.length % 6) * -0.7}s`;
      mark.firstChild.lastChild.style.animationDelay = `${(drawn.length % 6) * -0.5}s`;
      drawn.push({ p, box, rune: mark });
      redraw();
    }

    // Every frame while runes are up: pages move things by script, and no
    // single event covers all of it.
    function follow() {
      frame = 0;
      if (!drawn.length) return;
      for (const d of drawn) {
        const r = anchorRect(d.p);
        const shown = r.y + r.h > -24 && r.y < innerHeight + 24;
        d.box.classList.toggle("on", shown);
        d.rune.classList.toggle("on", shown);
        if (!shown) continue;
        place(d.box, r);
        const x = Math.max(2, Math.min(innerWidth - 30, r.x - 13));
        const y = Math.max(2, Math.min(innerHeight - 30, r.y - 13));
        d.rune.style.transform = `translate(${x}px, ${y}px)`;
      }
      frame = requestAnimationFrame(follow);
    }

    return {
      next(took) {
        if (waiting) finish(null);
        if (!overlay()) return Promise.resolve(null);
        if (took && last) rune(last);
        last = null;
        return new Promise((done) => {
          waiting = done;
          layer.catcher.classList.add("on");
          addEventListener("keydown", onKey, true);
          document.addEventListener("visibilitychange", onHidden);
        });
      },
      stop(took) {
        if (took && last) rune(last);
        last = null;
        finish(null);
      },
    };
  }
})
