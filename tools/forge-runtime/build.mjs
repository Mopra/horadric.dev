// Builds the Forge's artefact runtime into crates/horadric-ui/assets/forge/runtime/.
// The output is committed, so building Horadric never needs npm.

import { build } from "esbuild";
import { copyFile, mkdir, readFile, readdir, stat } from "node:fs/promises";
import { gzipSync } from "node:zlib";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "../../crates/horadric-ui/assets/forge/runtime");
await mkdir(out, { recursive: true });

const pkg = JSON.parse(await readFile(join(here, "package.json"), "utf8"));
const versions = {};
for (const name of Object.keys(pkg.dependencies)) {
  const p = JSON.parse(await readFile(join(here, "node_modules", name, "package.json"), "utf8"));
  versions[name] = p.version;
}

const common = {
  bundle: true,
  minify: true,
  format: "iife",
  target: ["chrome120", "safari17"],
  legalComments: "none",
  logLevel: "warning",
  define: { "process.env.NODE_ENV": '"production"', __FORGE_VERSIONS__: JSON.stringify(versions) },
};

await build({
  ...common,
  entryPoints: { runtime: join(here, "src/runtime.jsx") },
  outdir: out,
  jsx: "automatic",
  loader: { ".js": "jsx" },
});

// ELK is a megabyte and a half on its own. Mermaid 12 made it the default
// layout; mermaid.js sets dagre back (what claude.ai and GitHub draw with), so
// only a diagram asking for ELK by name gets the error card.
const noElk = {
  name: "no-elk",
  setup(b) {
    b.onResolve({ filter: /^elkjs(\/.*)?$/ }, () => ({ path: "elkjs", namespace: "no-elk" }));
    b.onLoad({ filter: /.*/, namespace: "no-elk" }, () => ({
      contents:
        "export default class ELK { layout() { return Promise.reject(new Error('The ELK layout is not in the Forge runtime; leave layout unset to use dagre.')); } }",
    }));
  },
};

await build({
  ...common,
  entryPoints: { mermaid: join(here, "src/mermaid.js") },
  outdir: out,
  plugins: [noElk],
});

await copyFile(join(here, "src/artefact.html"), join(out, "artefact.html"));

let total = 0;
let totalGz = 0;
for (const f of (await readdir(out)).sort()) {
  if (f === "README.md") continue;
  const buf = await readFile(join(out, f));
  const gz = gzipSync(buf, { level: 9 }).length;
  total += (await stat(join(out, f))).size;
  totalGz += gz;
  console.log(`${f.padEnd(16)} ${(buf.length / 1024).toFixed(0).padStart(6)} KB  gzip ${(gz / 1024).toFixed(0).padStart(5)} KB`);
}
console.log(`${"total".padEnd(16)} ${(total / 1024).toFixed(0).padStart(6)} KB  gzip ${(totalGz / 1024).toFixed(0).padStart(5)} KB`);
