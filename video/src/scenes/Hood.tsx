import React from "react";
import { AbsoluteFill, interpolate, spring, useCurrentFrame, useVideoConfig } from "remotion";
import { SceneFade } from "../components/SceneFade";
import { Rise } from "../components/Type";
import { hold } from "../components/Backdrop";
import { theme, sec } from "../theme";
import { useLayout } from "../layout";

type Rect = { x: number; y: number; w: number; h: number };
type NodeDef = {
  id: string;
  label: string;
  sub: string;
  at: number; // seconds (local)
  rect: Rect;
  kind?: "engine" | "child";
  group?: "code" | "engine" | "native";
};

const mono = theme.fonts.mono;

/** Scene 4: the architecture as an animated technical breakdown. */
export const Hood: React.FC = () => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const { vertical, w, h, pick } = useLayout();

  // ---- geometry per format ----
  let nodes: NodeDef[];
  let bracket: { a: number; b: number; y: number; vertical: boolean; x?: number };
  if (!vertical) {
    const bw = 236, bh = 190, gap = 62, ew = 540, eh = 260;
    const total = bw * 4 + ew + gap * 4;
    const x0 = (w - total) / 2;
    const cy = 500;
    let x = x0;
    const col = (width: number) => {
      const r = x;
      x += width + gap;
      return r;
    };
    const tsX = col(bw), cX = col(bw), rX = col(bw), eX = col(ew), aX = col(bw);
    nodes = [
      { id: "ts", label: "TypeScript / JavaScript", sub: "your application", at: 1.0, rect: { x: tsX, y: cy - bh / 2, w: bw, h: bh }, group: "code" },
      { id: "compiler", label: "Rendrix Compiler", sub: "build pipeline", at: 2.0, rect: { x: cX, y: cy - bh / 2, w: bw, h: bh }, group: "engine" },
      { id: "runtime", label: "Rendrix Runtime", sub: "executes your code", at: 3.0, rect: { x: rX, y: cy - bh / 2, w: bw, h: bh }, group: "engine" },
      { id: "engine", label: "Native Rust Engine", sub: "", at: 4.0, rect: { x: eX, y: cy - eh / 2, w: ew, h: eh }, kind: "engine", group: "engine" },
      { id: "dom", label: "DOM / CSS / Layout", sub: "own implementation", at: 5.0, rect: { x: eX + 24, y: cy - eh / 2 + 70, w: ew / 2 - 36, h: eh - 94 }, kind: "child", group: "engine" },
      { id: "gpu", label: "wgpu Rendering", sub: "GPU pixels", at: 6.0, rect: { x: eX + ew / 2 + 12, y: cy - eh / 2 + 70, w: ew / 2 - 36, h: eh - 94 }, kind: "child", group: "engine" },
      { id: "apps", label: "Native Applications", sub: "desktop, games, tools", at: 7.0, rect: { x: aX, y: cy - bh / 2, w: bw, h: bh }, group: "native" },
    ];
    bracket = { a: cX, b: eX + ew, y: cy + eh / 2 + 56, vertical: false };
  } else {
    const bw = 840, bh = 140, gap = 44, eh = 300;
    const x0 = (w - bw) / 2;
    let y = 330;
    const row = (hgt: number) => { const r = y; y += hgt + gap; return r; };
    const tsY = row(bh), cY = row(bh), rY = row(bh), eY = row(eh), aY = row(bh);
    nodes = [
      { id: "ts", label: "TypeScript / JavaScript", sub: "your application", at: 1.0, rect: { x: x0, y: tsY, w: bw, h: bh }, group: "code" },
      { id: "compiler", label: "Rendrix Compiler", sub: "build pipeline", at: 2.0, rect: { x: x0, y: cY, w: bw, h: bh }, group: "engine" },
      { id: "runtime", label: "Rendrix Runtime", sub: "executes your code", at: 3.0, rect: { x: x0, y: rY, w: bw, h: bh }, group: "engine" },
      { id: "engine", label: "Native Rust Engine", sub: "", at: 4.0, rect: { x: x0, y: eY, w: bw, h: eh }, kind: "engine", group: "engine" },
      { id: "dom", label: "DOM / CSS / Layout", sub: "own implementation", at: 5.0, rect: { x: x0 + 24, y: eY + 78, w: bw / 2 - 36, h: eh - 102 }, kind: "child", group: "engine" },
      { id: "gpu", label: "wgpu Rendering", sub: "GPU pixels", at: 6.0, rect: { x: x0 + bw / 2 + 12, y: eY + 78, w: bw / 2 - 36, h: eh - 102 }, kind: "child", group: "engine" },
      { id: "apps", label: "Native Applications", sub: "desktop, games, tools", at: 7.0, rect: { x: x0, y: aY, w: bw, h: bh }, group: "native" },
    ];
    bracket = { a: cY, b: eY + eh, y: 0, x: x0 - 34, vertical: true };
  }
  const main = nodes.filter((n) => n.kind !== "child");

  // text phases
  const tFinal = sec(9.0);
  const settle = hold(f, tFinal, tFinal + sec(0.9));
  const phase = f < sec(9.6) ? "none" : f < sec(10.8) ? "code" : f < sec(12.0) ? "engine" : "native";
  const glow = (g?: string) => (phase === "none" ? 0.0 : phase === g ? 1 : 0.0);

  const diagramShift = interpolate(settle, [0, 1], [0, pick(-70, -150)]);
  const diagramScale = interpolate(settle, [0, 1], [1, pick(0.93, 0.9)]);

  const connectors = main.slice(0, -1).map((n, i) => ({ from: n, to: main[i + 1], i }));
  // connect engine -> apps via the engine container; children connect internally
  const edge = (a: Rect, b: Rect) =>
    vertical
      ? { x1: a.x + a.w / 2, y1: a.y + a.h, x2: b.x + b.w / 2, y2: b.y }
      : { x1: a.x + a.w, y1: a.y + a.h / 2, x2: b.x, y2: b.y + b.h / 2 };

  // bracket
  const bp = hold(f, sec(8.0), sec(8.9));

  // ghost stack: not in the path
  const ghostP = hold(f, sec(5.2), sec(6.2));
  const ghostStrike = hold(f, sec(6.0), sec(6.6));

  return (
    <SceneFade inFrames={sec(0.5)} outFrames={sec(0.5)}>
      <AbsoluteFill>
        {/* faint technical grid, masked toward the centre */}
        <AbsoluteFill
          style={{
            backgroundImage:
              "linear-gradient(rgba(255,255,255,0.04) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,0.04) 1px, transparent 1px)",
            backgroundSize: "64px 64px",
            WebkitMaskImage: "radial-gradient(ellipse at 50% 45%, #000 0%, transparent 62%)",
            maskImage: "radial-gradient(ellipse at 50% 45%, #000 0%, transparent 62%)",
            opacity: hold(f, 0, sec(1.2)),
            transform: `translateY(${(f * 0.25) % 64}px)`,
          }}
        />

        <AbsoluteFill style={{ transform: `translateY(${diagramShift}px) scale(${diagramScale})` }}>
          {/* header kicker */}
          <Kicker f={f} />

          {/* connectors */}
          <svg width={w} height={h} style={{ position: "absolute", inset: 0, overflow: "visible" }}>
            {connectors.map(({ from, to, i }) => {
              const e = edge(from.rect, to.rect);
              const p = hold(f, sec(to.at - 0.45), sec(to.at + 0.15));
              const px = e.x1 + (e.x2 - e.x1) * p;
              const py = e.y1 + (e.y2 - e.y1) * p;
              // packets travel once everything is lit
              const flow = hold(f, sec(7.6), sec(8.0));
              const pk = [0, 1].map((k) => {
                const t = ((f / sec(1.6) + k * 0.5 + i * 0.17) % 1);
                return { x: e.x1 + (e.x2 - e.x1) * t, y: e.y1 + (e.y2 - e.y1) * t, o: Math.sin(t * Math.PI) * flow };
              });
              return (
                <g key={i}>
                  <line x1={e.x1} y1={e.y1} x2={px} y2={py} stroke="rgba(255,255,255,0.5)" strokeWidth={1.5} />
                  {pk.map((q, k) => (
                    <circle key={k} cx={q.x} cy={q.y} r={4.5} fill="#fff" opacity={q.o}
                      style={{ filter: "drop-shadow(0 0 8px rgba(255,255,255,0.9))" }} />
                  ))}
                </g>
              );
            })}
          </svg>

          {/* nodes */}
          {nodes.map((n) => {
            const p = spring({ frame: f - sec(n.at), fps, config: { damping: 20, stiffness: 110, mass: 0.9 } });
            const isEngine = n.kind === "engine";
            const isChild = n.kind === "child";
            const lit = glow(n.group);
            const idleY = Math.sin(f / 36 + n.at) * (isChild ? 0 : 3);
            return (
              <div
                key={n.id}
                style={{
                  position: "absolute",
                  left: n.rect.x,
                  top: n.rect.y,
                  width: n.rect.w,
                  height: n.rect.h,
                  borderRadius: isChild ? 12 : 18,
                  border: `1px solid rgba(255,255,255,${interpolate(lit, [0, 1], [isEngine ? 0.22 : 0.18, 0.75])})`,
                  background: isEngine
                    ? "linear-gradient(180deg, rgba(255,255,255,0.065), rgba(255,255,255,0.015))"
                    : isChild
                    ? "rgba(255,255,255,0.045)"
                    : theme.glass.fill,
                  boxShadow: isChild ? "none" : theme.glass.shadow,
                  opacity: interpolate(p, [0, 0.5], [0, 1], { extrapolateRight: "clamp" }),
                  transform: `translateY(${interpolate(p, [0, 1], [pick(40, 30), 0]) + idleY}px) scale(${interpolate(p, [0, 1], [0.94, 1])})`,
                  padding: isChild ? "18px 20px" : "22px 24px",
                  display: "flex",
                  flexDirection: "column",
                  justifyContent: isEngine ? "flex-start" : isChild ? "flex-end" : "space-between",
                  fontFamily: theme.fonts.display,
                  color: theme.colors.text,
                }}
              >
                {n.id === "ts" && <CodeLines f={f} at={sec(n.at + 0.3)} />}
                {n.id === "compiler" && <Bars f={f} at={sec(n.at + 0.3)} />}
                {n.id === "runtime" && <Ticks f={f} at={sec(n.at + 0.3)} />}
                {n.id === "dom" && <TreeGlyph f={f} at={sec(n.at + 0.2)} />}
                {n.id === "gpu" && <GpuGlyph f={f} at={sec(n.at + 0.2)} />}
                {n.id === "apps" && <WindowGlyph f={f} at={sec(n.at + 0.3)} />}
                <div>
                  <div style={{ fontSize: isEngine ? 32 : isChild ? 26 : vertical ? 34 : 28, fontWeight: 650, letterSpacing: "-0.015em", lineHeight: 1.15 }}>
                    {n.label}
                  </div>
                  {n.sub && (
                    <div style={{ fontFamily: mono, fontSize: 17, color: theme.colors.faint, marginTop: 6, letterSpacing: "0.02em" }}>
                      {n.sub}
                    </div>
                  )}
                </div>
              </div>
            );
          })}

          {/* ghost: not in the path */}
          <GhostRow f={f} p={ghostP} strike={ghostStrike} vertical={vertical} w={w} y={pick(190, 150)} />

          {/* bracket under / beside the owned stack */}
          <Bracket b={bracket} p={bp} w={w} />
        </AbsoluteFill>

        {/* closing statement */}
        <AbsoluteFill style={{ justifyContent: "flex-end", alignItems: "center", paddingBottom: pick(120, 230) }}>
          <div
            style={{
              display: "flex",
              flexDirection: pick("row", "column"),
              gap: pick(36, 4),
              alignItems: "center",
              fontFamily: theme.fonts.display,
              fontWeight: 800,
              letterSpacing: "-0.035em",
              fontSize: pick(76, 84),
            }}
          >
            {[
              { t: "Your code.", at: 9.6, on: phase === "code" },
              { t: "Our engine.", at: 10.8, on: phase === "engine" },
              { t: "Native execution.", at: 12.0, on: phase === "native" },
            ].map((l, i) => (
              <Rise key={i} at={sec(l.at)} distance={36}>
                <span style={{ color: l.on ? theme.colors.text : phase === "none" ? theme.colors.text : theme.colors.faint }}>{l.t}</span>
              </Rise>
            ))}
          </div>
        </AbsoluteFill>
      </AbsoluteFill>
    </SceneFade>
  );
};

const Kicker: React.FC<{ f: number }> = ({ f }) => {
  const { pick, w } = useLayout();
  return (
    <div
      style={{
        position: "absolute",
        top: pick(88, 150),
        width: w,
        textAlign: "center",
        fontFamily: mono,
        fontSize: 22,
        letterSpacing: "0.32em",
        color: theme.colors.faint,
        opacity: hold(f, sec(0.2), sec(0.9)),
        transform: `translateY(${(1 - hold(f, sec(0.2), sec(0.9))) * 14}px)`,
      }}
    >
      UNDER THE HOOD
    </div>
  );
};

/** Tiny animated type snippets inside nodes. */
const CodeLines: React.FC<{ f: number; at: number }> = ({ f, at }) => {
  const lines = [0.8, 0.55, 0.7, 0.4];
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 7 }}>
      {lines.map((l, i) => (
        <div key={i} style={{ height: 6, borderRadius: 3, background: i === 0 ? "rgba(255,255,255,0.7)" : "rgba(255,255,255,0.22)",
          width: `${l * interpolate(f - at - i * 4, [0, 14], [0, 100], { easing: theme.ease.out, extrapolateLeft: "clamp", extrapolateRight: "clamp" })}%`, marginLeft: i === 1 || i === 2 ? 14 : 0 }} />
      ))}
    </div>
  );
};
const Bars: React.FC<{ f: number; at: number }> = ({ f, at }) => (
  <div style={{ display: "flex", gap: 6, alignItems: "flex-end", height: 34 }}>
    {[0.4, 0.8, 0.55, 1, 0.7, 0.9, 0.5].map((v, i) => (
      <div key={i} style={{ width: 8, borderRadius: 2, background: "rgba(255,255,255,0.6)",
        height: 34 * v * interpolate(f - at - i * 2, [0, 14], [0, 1], { easing: theme.ease.out, extrapolateLeft: "clamp", extrapolateRight: "clamp" }) * (0.82 + 0.18 * Math.sin(f / 9 + i)) }} />
    ))}
  </div>
);
const Ticks: React.FC<{ f: number; at: number }> = ({ f, at }) => (
  <div style={{ display: "flex", gap: 5 }}>
    {Array.from({ length: 12 }).map((_, i) => {
      const on = Math.floor(f / 4) % 12 === i && f > at;
      return <div key={i} style={{ width: 8, height: 26, borderRadius: 2, background: on ? "#fff" : "rgba(255,255,255,0.2)" }} />;
    })}
  </div>
);
const TreeGlyph: React.FC<{ f: number; at: number }> = ({ f, at }) => {
  const p = hold(f, at, at + 18);
  return (
    <svg width="100%" height={54} viewBox="0 0 200 54" style={{ position: "absolute", top: 14, left: 0, opacity: p }}>
      <g stroke="rgba(255,255,255,0.5)" strokeWidth={1.5} fill="none">
        <path d="M100 8 V20 M100 20 H50 V32 M100 20 H150 V32 M50 32 H30 V46 M50 32 H70 V46" />
      </g>
      {[[100, 8], [50, 32], [150, 32], [30, 46], [70, 46]].map(([x, y], i) => (
        <circle key={i} cx={x} cy={y} r={4.5} fill={i === 0 ? "#fff" : "rgba(255,255,255,0.55)"} />
      ))}
    </svg>
  );
};
const GpuGlyph: React.FC<{ f: number; at: number }> = ({ f, at }) => {
  const p = hold(f, at, at + 18);
  const pts = "100,6 150,46 50,46";
  return (
    <svg width="100%" height={54} viewBox="0 0 200 54" style={{ position: "absolute", top: 14, left: 0, opacity: p }}>
      <polygon points={pts} fill="rgba(255,255,255,0.08)" stroke="rgba(255,255,255,0.7)" strokeWidth={1.5} />
      <line x1={100} y1={6} x2={100} y2={46} stroke="rgba(255,255,255,0.3)" strokeWidth={1} />
      <circle cx={100 + Math.sin(f / 18) * 26} cy={34} r={3.5} fill="#fff" />
    </svg>
  );
};
const WindowGlyph: React.FC<{ f: number; at: number }> = ({ f, at }) => {
  const p = hold(f, at, at + 18);
  return (
    <div style={{ opacity: p, border: "1px solid rgba(255,255,255,0.4)", borderRadius: 6, height: 40, width: 78, padding: 6 }}>
      <div style={{ display: "flex", gap: 3 }}>
        {[0, 1, 2].map((i) => <div key={i} style={{ width: 5, height: 5, borderRadius: 3, background: "rgba(255,255,255,0.55)" }} />)}
      </div>
      <div style={{ marginTop: 6, height: 5, width: "70%", background: "rgba(255,255,255,0.3)", borderRadius: 2 }} />
    </div>
  );
};

const GhostRow: React.FC<{ f: number; p: number; strike: number; vertical: boolean; w: number; y: number }> = ({ f, p, strike, vertical, w, y }) => {
  const names = ["Chromium", "Electron", "WebView"];
  return (
    <div style={{ position: "absolute", top: y, left: 0, width: w, display: "flex", justifyContent: "center", alignItems: "center", gap: 18,
      opacity: p * (1 - hold(f, sec(9.0), sec(9.6))), transform: `translateY(${(1 - p) * 20}px)` }}>
      <span style={{ fontFamily: mono, fontSize: 17, color: theme.colors.ghost, letterSpacing: "0.12em", marginRight: 10 }}>NOT IN THIS PATH</span>
      {names.map((n) => (
        <div key={n} style={{ position: "relative", border: "1px dashed rgba(255,255,255,0.22)", borderRadius: 999, padding: "8px 22px",
          fontFamily: mono, fontSize: 20, color: theme.colors.faint }}>
          {n}
          <div style={{ position: "absolute", left: 14, right: 14, top: "50%", height: 1.5, background: "#fff", transformOrigin: "left", transform: `scaleX(${strike})`, opacity: 0.8 }} />
        </div>
      ))}
    </div>
  );
};

const Bracket: React.FC<{ b: { a: number; b: number; y: number; vertical: boolean; x?: number }; p: number; w: number }> = ({ b, p }) => {
  if (p <= 0.001) return null;
  if (b.vertical) {
    const len = (b.b - b.a) * p;
    return (
      <div style={{ position: "absolute", left: b.x, top: b.a, width: 14, height: len, borderLeft: "1.5px solid rgba(255,255,255,0.55)", borderTop: "1.5px solid rgba(255,255,255,0.55)", borderBottom: "1.5px solid rgba(255,255,255,0.55)", overflow: "visible" }}>
        <div style={{ position: "absolute", left: -44, top: "50%", transform: "translate(-50%, -50%) rotate(-90deg)", whiteSpace: "nowrap", fontFamily: mono, fontSize: 18, letterSpacing: "0.3em", color: theme.colors.dim, opacity: p }}>
          THE RENDRIX STACK
        </div>
      </div>
    );
  }
  const len = (b.b - b.a) * p;
  return (
    <div style={{ position: "absolute", left: b.a, top: b.y, width: len, height: 14, borderLeft: "1.5px solid rgba(255,255,255,0.55)", borderRight: len > (b.b - b.a) - 1 ? "1.5px solid rgba(255,255,255,0.55)" : "none", borderBottom: "1.5px solid rgba(255,255,255,0.55)" }}>
      <div style={{ position: "absolute", top: 26, left: 0, width: b.b - b.a, textAlign: "center", fontFamily: mono, fontSize: 18, letterSpacing: "0.3em", color: theme.colors.dim, opacity: p }}>
        THE RENDRIX STACK
      </div>
    </div>
  );
};
