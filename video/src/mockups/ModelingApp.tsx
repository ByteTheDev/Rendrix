import React from "react";
import { interpolate } from "remotion";
import { Canvas3D, Tri, V3, drawTris, makeProjector, torusKnot } from "../components/r3d";
import { theme } from "../theme";
import { Icon, Label, MockProps, WindowFrame, ui } from "./ui";

let KNOT: Tri[] | null = null;

export const ModelingApp: React.FC<MockProps> = ({ width, height, f, compact }) => {
  if (!KNOT) KNOT = torusKnot(2.2, 0.55, 140, 14);
  const toolW = 64;
  const rightW = compact ? 0 : 300;
  const tlH = compact ? 0 : 130;
  const vw = Math.round(width - toolW - rightW - 2);
  const vh = Math.round(height - 42 - tlH);

  const draw = (ctx: CanvasRenderingContext2D, w: number, h: number) => {
    const t = f / 30;
    const bg = ctx.createRadialGradient(w / 2, h * 0.45, 0, w / 2, h * 0.45, h * 0.9);
    bg.addColorStop(0, "#2a2a2e");
    bg.addColorStop(1, "#0b0b0c");
    ctx.fillStyle = bg;
    ctx.fillRect(0, 0, w, h);

    const a = 0.8 + t * 0.55;
    const cam = {
      pos: [Math.sin(a) * 9.2, 3.4 + Math.sin(t * 0.5) * 0.5, -Math.cos(a) * 9.2] as V3,
      target: [0, 0.2, 0] as V3,
      fov: 40,
    };
    const proj = makeProjector(cam, w, h);

    // floor grid
    ctx.lineWidth = 1;
    for (let i = -10; i <= 10; i++) {
      const alpha = i === 0 ? 0.28 : 0.08;
      ctx.strokeStyle = `rgba(255,255,255,${alpha})`;
      for (const [p0, p1] of [
        [[i, -2.6, -10], [i, -2.6, 10]],
        [[-10, -2.6, i], [10, -2.6, i]],
      ] as [V3, V3][]) {
        const q0 = proj(p0), q1 = proj(p1);
        if (!q0 || !q1) continue;
        ctx.beginPath();
        ctx.moveTo(q0.x, q0.y);
        ctx.lineTo(q1.x, q1.y);
        ctx.stroke();
      }
    }
    // model builds from wireframe into shaded surface
    const build = interpolate(f, [8, 62], [0, 140], { easing: theme.ease.inOut, extrapolateLeft: "clamp", extrapolateRight: "clamp" });
    drawTris(ctx, w, h, KNOT!, {
      cam,
      sun: [0.6, 0.9, -0.4],
      fogColor: [10, 10, 11],
      fogNear: 14,
      fogFar: 40,
      ambient: 0.14,
      wireBelow: 140 - build,
      wireAlpha: 0.5,
    });
    // selected vertex handles
    for (let i = 0; i < 9; i++) {
      const ti = (i * 15 + 7) % 140;
      const tri = KNOT![ti * 28];
      if (!tri) continue;
      const q = proj(tri.a);
      if (!q) continue;
      ctx.fillStyle = i % 3 === 0 ? "#fff" : "rgba(255,255,255,0.6)";
      ctx.fillRect(q.x - 3.5, q.y - 3.5, 7, 7);
    }
    // orientation gizmo
    const gx = w - 70, gy = 70;
    const o = proj([0, 0, 0]);
    void o;
    ctx.font = `600 13px ${theme.fonts.mono}`;
    const fwd: V3 = [cam.target[0] - cam.pos[0], 0, cam.target[2] - cam.pos[2]];
    const len = Math.hypot(fwd[0], fwd[2]);
    const fx = fwd[0] / len, fz = fwd[2] / len;
    const rx = -fz, rz = fx;
    const axes: [string, number, number, number][] = [
      ["X", 1, 0, 0],
      ["Z", 0, 0, 1],
      ["Y", 0, 1, 0],
    ];
    for (const [nm, x, y, z] of axes) {
      const sx = (x * rx + z * rz) * 30, sy = -y * 30 + (x * fx + z * fz) * 8;
      ctx.strokeStyle = nm === "Y" ? "#fff" : "rgba(255,255,255,0.5)";
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(gx, gy);
      ctx.lineTo(gx + sx, gy + sy);
      ctx.stroke();
      ctx.fillStyle = "#fff";
      ctx.fillText(nm, gx + sx * 1.3 - 4, gy + sy * 1.3 + 4);
    }
  };

  const tools = ["select", "move", "rotate", "scale", "sculpt", "brush"];
  return (
    <WindowFrame width={width} height={height} title="Atelier  ·  knot_study.model" right={<div style={{ fontFamily: theme.fonts.mono, fontSize: 12, color: ui.faint }}>EDIT MODE</div>}>
      <div style={{ display: "flex", flex: 1, minHeight: 0 }}>
        <div style={{ width: toolW, background: ui.panel, borderRight: `1px solid ${ui.line}`, padding: "16px 0", display: "flex", flexDirection: "column", alignItems: "center", gap: 8 }}>
          {tools.map((n, i) => (
            <div key={n} style={{ width: 40, height: 40, borderRadius: 10, display: "flex", alignItems: "center", justifyContent: "center", background: i === 4 ? "rgba(255,255,255,0.13)" : "transparent" }}>
              <Icon name={n} size={20} color={i === 4 ? "#fff" : ui.dim} />
            </div>
          ))}
        </div>
        <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column" }}>
          <div style={{ flex: 1, minHeight: 0, position: "relative" }}>
            <Canvas3D width={vw} height={vh} draw={draw} />
            <div style={{ position: "absolute", left: 16, top: 14, fontFamily: theme.fonts.mono, fontSize: 12, color: "rgba(255,255,255,0.6)", letterSpacing: "0.1em" }}>
              USER PERSPECTIVE  ·  KNOT
            </div>
          </div>
          {!compact && (
            <div style={{ height: tlH, flex: "none", background: ui.panel, borderTop: `1px solid ${ui.line}`, padding: "12px 18px" }}>
              <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 10 }}>
                <Label>Timeline</Label>
                <Label>24 fps</Label>
              </div>
              <div style={{ position: "relative", height: 62 }}>
                {Array.from({ length: 49 }).map((_, i) => (
                  <div key={i} style={{ position: "absolute", left: `${(i / 48) * 100}%`, top: 0, width: 1, height: i % 6 === 0 ? 14 : 7, background: ui.ghost }} />
                ))}
                {[8, 20, 31, 42].map((k, i) => (
                  <div key={k} style={{ position: "absolute", left: `${(k / 48) * 100}%`, top: 32, width: 12, height: 12, marginLeft: -6, background: i === 1 ? "#fff" : ui.dim, transform: "rotate(45deg)" }} />
                ))}
                <div style={{ position: "absolute", left: `${((f * 0.55) % 48) / 48 * 100}%`, top: -2, bottom: -6, width: 2, background: "#fff" }} />
              </div>
            </div>
          )}
        </div>
        {!compact && (
          <div style={{ width: rightW, background: ui.panel, borderLeft: `1px solid ${ui.line}`, padding: 18 }}>
            <Label>Modifiers</Label>
            {[["Subdivision", "Level 2"], ["Mirror", "Y axis"], ["Bevel", "0.02"]].map(([n, v]) => (
              <div key={n} style={{ display: "flex", alignItems: "center", padding: "11px 12px", marginTop: 10, borderRadius: 9, border: `1px solid ${ui.line2}`, fontSize: 15 }}>
                <Icon name="grid" size={15} color={ui.faint} />
                <span style={{ marginLeft: 10 }}>{n}</span>
                <span style={{ marginLeft: "auto", color: ui.faint, fontFamily: theme.fonts.mono, fontSize: 13 }}>{v}</span>
              </div>
            ))}
            <Label style={{ margin: "26px 0 12px" }}>Material</Label>
            <div style={{ display: "flex", gap: 14, alignItems: "center" }}>
              <div style={{ width: 64, height: 64, borderRadius: 32, background: "radial-gradient(circle at 32% 28%, #fff, #8d8d93 45%, #1a1a1d 100%)" }} />
              <div style={{ flex: 1 }}>
                {["Roughness", "Metallic"].map((n, i) => (
                  <div key={n} style={{ marginBottom: 12 }}>
                    <div style={{ fontSize: 13, color: ui.faint, marginBottom: 5 }}>{n}</div>
                    <div style={{ height: 4, borderRadius: 2, background: ui.line2 }}>
                      <div style={{ height: 4, borderRadius: 2, width: `${[34, 88][i]}%`, background: "#fff" }} />
                    </div>
                  </div>
                ))}
              </div>
            </div>
            <Label style={{ margin: "26px 0 12px" }}>Statistics</Label>
            {[["Vertices", "2,240"], ["Faces", "2,240"], ["Selected", "9"]].map(([n, v]) => (
              <div key={n} style={{ display: "flex", padding: "7px 0", fontSize: 15, color: ui.dim }}>
                {n}
                <span style={{ marginLeft: "auto", fontFamily: theme.fonts.mono, color: ui.text }}>{v}</span>
              </div>
            ))}
          </div>
        )}
      </div>
    </WindowFrame>
  );
};
