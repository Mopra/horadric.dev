// A stand in for the host, for the editor's test pages. Load it before
// forge.js: it gives the page a fake window.chrome.webview, answers `write`,
// `render` and `save-image` as the Forge would, and serves `?raw` from its
// own copy of the file. Pictures are PUT to the server under assets/.
//
// The file on "disk" is the text in <script type="text/plain"
// id="forge-source"> when the page has one (markdown and CSV), else the
// page's own source fetched from the server (HTML). Writes to an HTML page
// are also PUT to the server, so a server that takes PUT shows them in the
// preview; one that does not just leaves the preview as it was.
//
// From the console:
//   __stub.disk                 the file as the host holds it
//   __stub.claude("new text")   Claude writes the file: the next write is
//                               refused as changed
//   __stub.delay = 400          how long a write takes, in ms
(() => {
  "use strict";
  if (window.top !== window) {
    // The preview frame: it reads the real file and has no host.
    return;
  }
  const listeners = [];
  const sent = [];
  let disk = null;
  const realFetch = window.fetch.bind(window);
  const path = location.pathname;
  // Asked late: this runs in the head, before the page says what it is.
  const isHtml = () => !document.getElementById("forge-source") && /\.html?$/i.test(path);

  async function ensureDisk() {
    if (disk !== null) return disk;
    const holder = document.getElementById("forge-source");
    if (holder) disk = holder.textContent.replace(/^\n/, "");
    else disk = await (await realFetch(path, { cache: "no-store" })).text();
    return disk;
  }

  function deliver(msg) {
    for (const fn of listeners) fn({ data: msg });
  }

  async function put(text) {
    if (!isHtml() || location.protocol === "file:") return;
    try { await realFetch(path, { method: "PUT", body: text }); } catch (_) { /* no PUT here */ }
  }

  async function onPost(msg) {
    sent.push(msg);
    if (msg && msg.forge === "render") {
      deliver({ forge: "rendered", id: msg.id, ok: true, html: renderMarkdown(String(msg.text || "")) });
      return;
    }
    if (msg && msg.forge === "save-image") {
      deliver({ forge: "image-saved", id: msg.id, ...(await saveImage(msg)) });
      return;
    }
    if (!msg || msg.forge !== "write") return;
    await ensureDisk();
    await new Promise((r) => setTimeout(r, stub.delay));
    if (msg.base !== disk) {
      deliver({ forge: "write-result", id: msg.id, ok: false, why: "changed", current: disk });
      return;
    }
    disk = msg.text;
    await put(disk);
    const reply = { forge: "write-result", id: msg.id, ok: true };
    if (document.querySelector("main.forge-md")) reply.html = renderMarkdown(disk);
    deliver(reply);
  }

  const fake = {
    postMessage: (m) => { onPost(JSON.parse(JSON.stringify(m))); },
    addEventListener: (type, fn) => { if (type === "message") listeners.push(fn); },
  };
  // Inside WebView2 the real one is a getter; the test must not reach the
  // real host, so the whole chrome object is swapped.
  try {
    Object.defineProperty(window, "chrome", { value: { webview: fake }, configurable: true, writable: true });
  } catch (_) {
    window.chrome = { webview: fake };
  }

  window.fetch = async (input, init) => {
    const url = new URL(typeof input === "string" ? input : input.url, location.href);
    if (url.pathname === path && (url.search === "?raw" || (isHtml() && url.search === "" && !(init && init.method)))) {
      return new Response(await ensureDisk(), { headers: { "content-type": "text/plain; charset=utf-8" } });
    }
    return realFetch(input, init);
  };

  // A small markdown renderer that marks each top-level block with its
  // lines ("a-b"), as the Forge's own does: headings, lists with tasks,
  // quotes, code and Mermaid fences, rules, tables, pictures and text.
  // The real one is horadric_core::forge::markdown_body; this is enough to
  // try the editor without the app.
  function renderMarkdown(src) {
    const lines = src.replace(/^﻿/, "").split("\n").map((l) => l.replace(/\r$/, ""));
    if (lines.length && lines[lines.length - 1] === "") lines.pop();
    const esc = (s) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
    const inline = (s) => esc(s)
      .replace(/`([^`]+)`/g, "<code>$1</code>")
      .replace(/!\[([^\]]*)\]\(([^)\s]*)\)/g, '<img src="$2" alt="$1">')
      .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
      .replace(/\*([^*]+)\*/g, "<em>$1</em>")
      .replace(/~~([^~]+)~~/g, "<del>$1</del>")
      .replace(/\[([^\]]*)\]\(([^)]*)\)/g, '<a href="$2">$1</a>');
    const at = (a, b) => `data-lines="${a}-${b}"`;
    const LIST = /^(\s*)([-*+]|\d+[.)])\s+/;
    const cells = (l) => l.trim().replace(/^\||\|$/g, "").split(/(?<!\\)\|/).map((c) => c.trim());
    const starts = (l) => /^(#{1,6}\s|```|~~~|>)/.test(l) || LIST.test(l) || /^\s*([-*_])(\s*\1){2,}\s*$/.test(l);
    let out = "";
    let i = 0;
    while (i < lines.length) {
      const l = lines[i];
      const n = i + 1;
      if (!l.trim()) { i++; continue; }
      let m;
      if ((m = /^(#{1,6})(?:\s+(.*))?$/.exec(l))) {
        out += `<h${m[1].length} ${at(n, n)}>${inline(m[2] || "")}</h${m[1].length}>\n`;
        i++;
      } else if ((m = /^(`{3,}|~{3,})\s*([^\s`]*)/.exec(l))) {
        let j = i + 1;
        while (j < lines.length && !lines[j].startsWith(m[1])) j++;
        const body = esc(lines.slice(i + 1, j).join("\n"));
        const end = Math.min(j + 1, lines.length);
        if (m[2] === "mermaid") out += `<pre class="mermaid" ${at(n, end)}>${body}\n</pre>\n`;
        else out += `<pre ${at(n, end)}><code${m[2] ? ` class="language-${esc(m[2])}"` : ""}>${body}${body ? "\n" : ""}</code></pre>\n`;
        i = j + 1;
      } else if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(l)) {
        out += `<hr ${at(n, n)}>\n`;
        i++;
      } else if (/^\s*\|/.test(l) && i + 1 < lines.length && /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/.test(lines[i + 1])) {
        const head = cells(l);
        let j = i + 2;
        let body = "";
        while (j < lines.length && /^\s*\|/.test(lines[j])) {
          const c = cells(lines[j]);
          body += `<tr>${head.map((_, k) => `<td>${inline(c[k] || "")}</td>`).join("")}</tr>\n`;
          j++;
        }
        out += `<table ${at(n, j)}>\n<thead>\n<tr>${head.map((h) => `<th>${inline(h)}</th>`).join("")}</tr>\n</thead>\n` +
          (body ? `<tbody>\n${body}</tbody>\n` : "") + "</table>\n";
        i = j;
      } else if (LIST.test(l)) {
        const ordered = /^\s*\d/.test(l);
        let j = i;
        let items = "";
        while (j < lines.length && (LIST.test(lines[j]) || (/^\s+\S/.test(lines[j]) && j > i))) {
          if (!LIST.test(lines[j])) { j++; continue; }
          const text = lines[j].replace(LIST, "");
          const t = /^\[([ xX])\]\s+(.*)$/.exec(text);
          items += t
            ? `<li class="task"><input type="checkbox" disabled${t[1] === " " ? "" : " checked"}>${inline(t[2])}</li>\n`
            : `<li>${inline(text)}</li>\n`;
          j++;
        }
        const tag = ordered ? "ol" : "ul";
        out += `<${tag} ${at(n, j)}>\n${items}</${tag}>\n`;
        i = j;
      } else if (/^\s*>/.test(l)) {
        let j = i;
        while (j < lines.length && /^\s*>/.test(lines[j])) j++;
        out += `<blockquote ${at(n, j)}>\n<p>${inline(lines.slice(i, j).map((x) => x.replace(/^\s*>\s?/, "")).join("\n"))}</p>\n</blockquote>\n`;
        i = j;
      } else {
        let j = i;
        while (j < lines.length && lines[j].trim() && (j === i || !starts(lines[j]))) j++;
        out += `<p ${at(n, j)}>${inline(lines.slice(i, j).join("\n"))}</p>\n`;
        i = j;
      }
    }
    return out;
  }

  // A picture saved as the host saves it: into assets/ beside the file, a
  // name of its own. PUT to the test server, so the page can show it.
  const saved = new Set();
  async function saveImage(msg) {
    const stem = (msg.name || "image").replace(/\.[^.]*$/, "").toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "") || "image";
    const bytes = Uint8Array.from(atob(msg.data || ""), (c) => c.charCodeAt(0));
    const ext = bytes[0] === 0x89 ? "png" : bytes[0] === 0xff ? "jpg" : bytes[0] === 0x47 ? "gif" : bytes[0] === 0x52 ? "webp" : null;
    if (!ext) return { ok: false, why: "only PNG, JPEG, GIF and WebP pictures are saved" };
    let name = `${stem}.${ext}`;
    for (let n = 2; saved.has(name); n++) name = `${stem}-${n}.${ext}`;
    saved.add(name);
    const dir = path.slice(0, path.lastIndexOf("/") + 1);
    try { await realFetch(`${dir}assets/${name}`, { method: "PUT", body: bytes }); } catch (_) { /* shown broken */ }
    return { ok: true, path: `assets/${name}` };
  }

  // The CSV page as csv.rs makes it: the first record the header, each
  // cell marked with its record and column.
  function renderCsv(src) {
    const s = src.replace(/^﻿/, "");
    const first = s.split(/\r?\n/)[0] || "";
    const counts = [",", ";", "\t"].map((d) => [d, first.split(d).length - 1]);
    const delim = counts.reduce((a, b) => (b[1] > a[1] ? b : a))[0];
    const rows = [];
    let row = [];
    let f = "";
    let q = false;
    for (let i = 0; i < s.length; i++) {
      const c = s[i];
      if (q) {
        if (c === '"') { if (s[i + 1] === '"') { f += '"'; i++; } else q = false; } else f += c;
      } else if (c === '"' && f === "") q = true;
      else if (c === delim) { row.push(f); f = ""; }
      else if (c === "\n" || c === "\r") {
        if (c === "\r" && s[i + 1] === "\n") i++;
        row.push(f); rows.push(row); row = []; f = "";
      } else f += c;
    }
    if (f || row.length) { row.push(f); rows.push(row); }
    const esc = (t) => t.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
    const width = Math.max(0, ...rows.map((r) => r.length));
    const num = (t) => { const x = t.replace(/\s/g, "").replace(",", "."); return x !== "" && isFinite(x); };
    let h = '<table>\n<thead><tr data-row="0">';
    for (let c = 0; c < width; c++) h += `<th data-row="0" data-col="${c}">${esc((rows[0] || [])[c] || "")}</th>`;
    h += "</tr></thead>\n<tbody>\n";
    for (let r = 1; r < rows.length; r++) {
      h += `<tr data-row="${r}">`;
      for (let c = 0; c < width; c++) {
        const t = rows[r][c] || "";
        h += `<td data-row="${r}" data-col="${c}"${num(t) ? ' class="num"' : ""}>${esc(t)}</td>`;
      }
      h += "</tr>\n";
    }
    return h + "</tbody></table>\n";
  }

  // The host gives the chrome on a page of the human's its colours; here
  // they come from the test pages' copy of the Forge's look.
  {
    realFetch(new URL("page.css", document.currentScript.src)).then((r) => r.text()).then((css) => {
      const vars = /:root\s*\{([^}]*)\}/.exec(css);
      if (!vars) return;
      const style = document.createElement("style");
      style.textContent = `forge-overlay { ${vars[1]} }`;
      document.documentElement.appendChild(style);
    }, () => {});
  }

  // The Forge's pages are rendered by the host; here the stub renders the
  // page from its copy of the file once the DOM is in.
  document.addEventListener("DOMContentLoaded", async () => {
    const main = document.querySelector("main.forge-md, main.forge-csv");
    if (!main) return;
    await ensureDisk();
    main.innerHTML = main.classList.contains("forge-md") ? renderMarkdown(disk) : renderCsv(disk);
  });

  const stub = {
    delay: 120,
    sent,
    get disk() { return disk; },
    claude(text) { disk = text; put(text); },
    receive: deliver,
  };
  window.__stub = stub;
})();
