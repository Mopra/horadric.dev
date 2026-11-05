// Mermaid on its own, loaded on first use by runtime.js (or directly by a
// page that only has diagrams, such as rendered markdown).

import mermaid from "mermaid";

// dagre, not Mermaid 12's default ELK: ELK is left out of the bundle (see build.mjs).
mermaid.initialize({ startOnLoad: false, securityLevel: "strict", theme: "default", layout: "dagre" });

let seq = 0;

// Markdown renderers write ```mermaid as <pre><code class="language-mermaid">;
// turn those into the <pre class="mermaid"> Mermaid itself looks for.
function adopt(root) {
  for (const code of root.querySelectorAll("pre > code.language-mermaid")) {
    const pre = code.parentElement;
    pre.className = "mermaid";
    pre.textContent = code.textContent;
  }
}

async function render(root = document.body, onError) {
  adopt(root);
  const nodes = [...root.querySelectorAll("pre.mermaid:not([data-forge-drawn]), div.mermaid:not([data-forge-drawn])")];
  let failed = 0;
  for (const node of nodes) {
    const text = node.dataset.forgeSource ?? node.textContent;
    node.dataset.forgeSource = text;
    try {
      const { svg, bindFunctions } = await mermaid.render(`forge-mermaid-${++seq}`, text);
      node.innerHTML = svg;
      bindFunctions?.(node);
      node.dataset.forgeDrawn = "";
    } catch (e) {
      failed++;
      // Mermaid leaves its own error drawing in the body; the card below replaces it.
      document.getElementById(`dforge-mermaid-${seq}`)?.remove();
      node.textContent = "";
      const card = document.createElement("div");
      card.dataset.forgeError = "";
      card.style.cssText =
        "padding:12px 16px;border:1px solid #e5484d;border-radius:8px;background:#fff5f5;color:#3b0d0c;font:12px/1.5 ui-monospace,Consolas,monospace;white-space:pre-wrap";
      card.textContent = `Could not draw the diagram: ${e?.message ?? e}`;
      node.appendChild(card);
      const err = e instanceof Error ? e : new Error(String(e));
      err.kind = "mermaid";
      onError?.(err);
    }
  }
  return { drawn: nodes.length - failed, failed };
}

window.__forgeMermaid = { render, mermaid };
