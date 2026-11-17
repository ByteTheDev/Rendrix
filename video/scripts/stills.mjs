// Render a set of verification stills: node scripts/stills.mjs <CompId> <secs,comma,separated> [outDir]
import path from "node:path";
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { bundle } from "@remotion/bundler";
import { renderStill, selectComposition } from "@remotion/renderer";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(here, "..");
const [id = "Launch", secs = "3", outDir = path.join(root, "out", "check")] = process.argv.slice(2);
fs.mkdirSync(outDir, { recursive: true });
const browserExecutable = process.env.BROWSER;
const serveUrl = await bundle({ entryPoint: path.join(root, "src/index.ts"), publicDir: path.join(root, "public") });
const comp = await selectComposition({ serveUrl, id, browserExecutable });
for (const s of secs.split(",")) {
  const frame = Math.min(comp.durationInFrames - 1, Math.round(parseFloat(s) * comp.fps));
  const out = path.join(outDir, `${id}_${String(s).replace(".", "_")}s.png`);
  await renderStill({ browserExecutable, composition: comp, serveUrl, output: out, frame, imageFormat: "png" });
  console.log("still", out);
}
process.exit(0);
