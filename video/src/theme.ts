// Single source of truth: colors, easing, springs, fonts, timing.
import { Easing } from "remotion";
import { loadFont as loadArchivo } from "@remotion/google-fonts/Archivo";
import { loadFont as loadMono } from "@remotion/google-fonts/JetBrainsMono";
import timeline from "./timeline.json";

const archivo = loadArchivo("normal", { weights: ["300", "400", "500", "600", "700", "800", "900"], subsets: ["latin"] });
loadArchivo("italic", { weights: ["800", "900"], subsets: ["latin"] });
const mono = loadMono("normal", { weights: ["400", "500", "700"], subsets: ["latin"] });

export const theme = {
  colors: {
    bg: "#000000",
    s1: "#09090b",
    s2: "#111114",
    s3: "#18181c",
    line: "#222227",
    line2: "#34343b",
    text: "#ffffff",
    soft: "#d4d4d8",
    dim: "#a1a1aa",
    faint: "#71717a",
    ghost: "#3f3f46",
  },
  fonts: {
    display: `${archivo.fontFamily}, "Segoe UI", sans-serif`,
    mono: `${mono.fontFamily}, ui-monospace, Consolas, monospace`,
  },
  ease: {
    out: Easing.bezier(0.16, 1, 0.3, 1),
    inOut: Easing.bezier(0.83, 0, 0.17, 1),
    soft: Easing.bezier(0.45, 0, 0.15, 1),
    in: Easing.bezier(0.7, 0, 0.84, 0),
  },
  spring: {
    snappy: { damping: 16, stiffness: 150, mass: 0.7 },
    smooth: { damping: 24, stiffness: 80, mass: 1 },
    heavy: { damping: 20, stiffness: 120, mass: 1.4 },
  },
  glass: {
    fill: "linear-gradient(180deg, rgba(255,255,255,0.075), rgba(255,255,255,0.02))",
    border: "1px solid rgba(255,255,255,0.14)",
    shadow: "0 40px 90px -30px rgba(0,0,0,0.9), inset 0 1px 0 rgba(255,255,255,0.12)",
  },
} as const;

export const FPS = timeline.fps;
export const TOTAL_FRAMES = Math.round(timeline.total * FPS);
/** seconds -> frames */
export const sec = (s: number) => Math.round(s * FPS);
export const scene = timeline.scenes;
export type SceneKey = keyof typeof timeline.scenes;
export const sceneRange = (k: SceneKey) => ({
  from: sec(scene[k].start),
  duration: sec(scene[k].end - scene[k].start),
});
