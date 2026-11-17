// A tiny deterministic software 3D renderer for the showcase scenes.
// Flat-shaded triangles, painter's algorithm, fog. No dependencies, purely frame-driven.
import React, { useLayoutEffect, useRef } from "react";

export type V3 = [number, number, number];
export type Tri = { a: V3; b: V3; c: V3; albedo: number; emit?: number; wire?: boolean; id?: number };
export type Camera = { pos: V3; target: V3; fov: number };

const sub = (a: V3, b: V3): V3 => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
const cross = (a: V3, b: V3): V3 => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
const dot = (a: V3, b: V3) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
const norm = (a: V3): V3 => {
  const l = Math.hypot(a[0], a[1], a[2]) || 1;
  return [a[0] / l, a[1] / l, a[2] / l];
};

export const makeProjector = (cam: Camera, w: number, h: number) => {
  const fwd = norm(sub(cam.target, cam.pos));
  const right = norm(cross(fwd, [0, 1, 0]));
  const up = cross(right, fwd);
  const f = 1 / Math.tan((cam.fov * Math.PI) / 360);
  const aspect = w / h;
  return (p: V3) => {
    const d = sub(p, cam.pos);
    const z = dot(d, fwd);
    if (z < 0.05) return null;
    const x = dot(d, right);
    const y = dot(d, up);
    return { x: w / 2 + ((x * f) / (z * aspect)) * (w / 2), y: h / 2 - ((y * f) / z) * (h / 2), z };
  };
};

// ---- geometry builders ----
export const hash = (n: number) => {
  const s = Math.sin(n * 127.1 + 311.7) * 43758.5453;
  return s - Math.floor(s);
};
const noise2 = (x: number, y: number) => {
  const xi = Math.floor(x), yi = Math.floor(y);
  const xf = x - xi, yf = y - yi;
  const s = (t: number) => t * t * (3 - 2 * t);
  const h = (a: number, b: number) => hash(a * 57 + b * 131);
  const a = h(xi, yi), b = h(xi + 1, yi), c = h(xi, yi + 1), d = h(xi + 1, yi + 1);
  return a + (b - a) * s(xf) + (c - a) * s(yf) + (a - b - c + d) * s(xf) * s(yf);
};
export const heightAt = (x: number, z: number) => {
  let h = 0, amp = 1, fr = 0.045;
  for (let o = 0; o < 4; o++) {
    h += (noise2(x * fr + 13, z * fr + 7) - 0.5) * amp;
    amp *= 0.5;
    fr *= 2.1;
  }
  // flatten a plateau in the middle for the "level"
  const d = Math.hypot(x - 6, z - 18);
  const plateau = Math.max(0, 1 - d / 34);
  return h * 24 * (1 - plateau * 0.85) + plateau * 3;
};

export const buildTerrain = (size = 150, cells = 56): Tri[] => {
  const out: Tri[] = [];
  const step = size / cells;
  const p = (i: number, j: number): V3 => {
    const x = -size / 2 + i * step, z = -size / 2 + j * step + 20;
    return [x, heightAt(x, z), z];
  };
  for (let i = 0; i < cells; i++)
    for (let j = 0; j < cells; j++) {
      const a = p(i, j), b = p(i + 1, j), c = p(i + 1, j + 1), d = p(i, j + 1);
      out.push({ a, b, c: d, albedo: 0.62 }, { a: b, b: c, c: d, albedo: 0.62 });
    }
  return out;
};

export const cone = (cx: number, cy: number, cz: number, r: number, hgt: number, seg = 7, albedo = 0.5): Tri[] => {
  const out: Tri[] = [];
  const tip: V3 = [cx, cy + hgt, cz];
  for (let i = 0; i < seg; i++) {
    const a0 = (i / seg) * Math.PI * 2, a1 = ((i + 1) / seg) * Math.PI * 2;
    const p0: V3 = [cx + Math.cos(a0) * r, cy, cz + Math.sin(a0) * r];
    const p1: V3 = [cx + Math.cos(a1) * r, cy, cz + Math.sin(a1) * r];
    out.push({ a: p0, b: p1, c: tip, albedo });
  }
  return out;
};

export const box = (cx: number, cy: number, cz: number, sx: number, sy: number, sz: number, albedo = 0.7, emit = 0): Tri[] => {
  const x0 = cx - sx / 2, x1 = cx + sx / 2, y0 = cy, y1 = cy + sy, z0 = cz - sz / 2, z1 = cz + sz / 2;
  const v: V3[] = [
    [x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1],
    [x0, y1, z0], [x1, y1, z0], [x1, y1, z1], [x0, y1, z1],
  ];
  const q = (a: number, b: number, c: number, d: number): Tri[] => [
    { a: v[a], b: v[b], c: v[c], albedo, emit },
    { a: v[a], b: v[c], c: v[d], albedo, emit },
  ];
  return [...q(4, 5, 6, 7), ...q(0, 1, 5, 4), ...q(1, 2, 6, 5), ...q(2, 3, 7, 6), ...q(3, 0, 4, 7)];
};

export const torusKnot = (R = 2.2, r = 0.62, seg = 150, tube = 16, p = 2, q = 3): Tri[] => {
  const pos = (t: number): V3 => {
    const rr = R * (0.55 + 0.25 * Math.cos(q * t));
    return [rr * Math.cos(p * t), rr * Math.sin(p * t), R * 0.5 * Math.sin(q * t)];
  };
  const rings: V3[][] = [];
  for (let i = 0; i <= seg; i++) {
    const t = (i / seg) * Math.PI * 2;
    const c = pos(t), c2 = pos(t + 0.01);
    const T = norm(sub(c2, c));
    const N = norm(cross(T, [0.3, 0.2, 1]));
    const B = cross(T, N);
    const ring: V3[] = [];
    for (let j = 0; j < tube; j++) {
      const a = (j / tube) * Math.PI * 2;
      ring.push([c[0] + r * (Math.cos(a) * N[0] + Math.sin(a) * B[0]), c[1] + r * (Math.cos(a) * N[1] + Math.sin(a) * B[1]), c[2] + r * (Math.cos(a) * N[2] + Math.sin(a) * B[2])]);
    }
    rings.push(ring);
  }
  const out: Tri[] = [];
  for (let i = 0; i < seg; i++)
    for (let j = 0; j < tube; j++) {
      const a = rings[i][j], b = rings[i + 1][j], c = rings[i + 1][(j + 1) % tube], d = rings[i][(j + 1) % tube];
      out.push({ a, b, c, albedo: 0.8, id: i }, { a, b: c, c: d, albedo: 0.8, id: i });
    }
  return out;
};

// ---- rendering ----
export type RenderOpts = {
  cam: Camera;
  sun: V3;
  fogColor: [number, number, number];
  fogNear: number;
  fogFar: number;
  ambient?: number;
  wireAlpha?: number;
  /** triangles with id < wireBelow are wire-only; used for modeling reveal */
  wireBelow?: number;
};

export const drawTris = (ctx: CanvasRenderingContext2D, w: number, h: number, tris: Tri[], o: RenderOpts) => {
  const proj = makeProjector(o.cam, w, h);
  const sun = norm(o.sun);
  const items: { z: number; pts: { x: number; y: number }[]; shade: number; fog: number; wire: boolean }[] = [];
  for (const t of tris) {
    const pa = proj(t.a), pb = proj(t.b), pc = proj(t.c);
    if (!pa || !pb || !pc) continue;
    if (pa.x < -200 && pb.x < -200 && pc.x < -200) continue;
    if (pa.x > w + 200 && pb.x > w + 200 && pc.x > w + 200) continue;
    const n = norm(cross(sub(t.b, t.a), sub(t.c, t.a)));
    const center: V3 = [(t.a[0] + t.b[0] + t.c[0]) / 3, (t.a[1] + t.b[1] + t.c[1]) / 3, (t.a[2] + t.b[2] + t.c[2]) / 3];
    const toCam = norm(sub(o.cam.pos, center));
    const facing = dot(n, toCam) < 0 ? -1 : 1; // two-sided
    const lam = Math.max(0, dot([n[0] * facing, n[1] * facing, n[2] * facing], sun));
    const shade = Math.min(1, (o.ambient ?? 0.12) + lam * 0.95) * t.albedo + (t.emit ?? 0);
    const z = (pa.z + pb.z + pc.z) / 3;
    const fog = Math.min(1, Math.max(0, (z - o.fogNear) / (o.fogFar - o.fogNear)));
    items.push({ z, pts: [pa, pb, pc], shade, fog, wire: o.wireBelow !== undefined && t.id !== undefined && t.id < o.wireBelow });
  }
  items.sort((a, b) => b.z - a.z);
  ctx.lineJoin = "round";
  for (const it of items) {
    const s = Math.min(1, it.shade);
    const g = (v: number) => Math.round(v * 255 * (1 - it.fog) + 0.0 + it.fog * 0);
    const base = (v: number) => g(s) * (v === 0 ? 1 : 1);
    const fr = Math.round(base(0) + o.fogColor[0] * it.fog);
    const fg = Math.round(base(0) + o.fogColor[1] * it.fog);
    const fb = Math.round(base(0) + o.fogColor[2] * it.fog);
    ctx.beginPath();
    ctx.moveTo(it.pts[0].x, it.pts[0].y);
    ctx.lineTo(it.pts[1].x, it.pts[1].y);
    ctx.lineTo(it.pts[2].x, it.pts[2].y);
    ctx.closePath();
    if (it.wire) {
      ctx.strokeStyle = `rgba(255,255,255,${(o.wireAlpha ?? 0.35) * (1 - it.fog)})`;
      ctx.lineWidth = 1;
      ctx.stroke();
    } else {
      const col = `rgb(${fr},${fg},${fb})`;
      ctx.fillStyle = col;
      ctx.strokeStyle = col;
      ctx.lineWidth = 0.8;
      ctx.fill();
      ctx.stroke();
    }
  }
};

/** Canvas that redraws synchronously every render (frame-driven, deterministic). */
export const Canvas3D: React.FC<{
  width: number;
  height: number;
  draw: (ctx: CanvasRenderingContext2D, w: number, h: number) => void;
  style?: React.CSSProperties;
}> = ({ width, height, draw, style }) => {
  const ref = useRef<HTMLCanvasElement>(null);
  useLayoutEffect(() => {
    const c = ref.current;
    if (!c) return;
    const ctx = c.getContext("2d");
    if (!ctx) return;
    ctx.clearRect(0, 0, width, height);
    draw(ctx, width, height);
  });
  return <canvas ref={ref} width={width} height={height} style={{ width: "100%", height: "100%", display: "block", ...style }} />;
};
