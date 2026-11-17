import React from "react";
import { Canvas3D, Tri, V3, box, buildTerrain, cone, drawTris, hash, heightAt, makeProjector } from "../components/r3d";
import { theme } from "../theme";
import { Icon, Label, MockProps, WindowFrame, ui } from "./ui";

// ---- scene is built once ----
const buildScene = (): { tris: Tri[]; focus: V3 } => {
  const tris: Tri[] = [...buildTerrain(150, 54)];
  // forest
  let placed = 0;
  for (let i = 0; i < 520 && placed < 120; i++) {
    const x = (hash(i * 3.1) - 0.5) * 140;
    const z = (hash(i * 7.7) - 0.5) * 140 + 20;
    if (Math.hypot(x - 6, z - 18) < 20) continue;
    const y = heightAt(x, z);
    const s = 1.2 + hash(i * 5.3) * 1.6;
    tris.push(...cone(x, y, z, 1.1 * s, 3.4 * s, 6, 0.34));
    tris.push(...cone(x, y + 1.7 * s, z, 0.85 * s, 3.0 * s, 6, 0.44));
    placed++;
  }
  // watchtower on the plateau
  const bx = 6, bz = 18, by = heightAt(bx, bz);
  tris.push(...box(bx, by, bz, 5, 3, 5, 0.55));
  tris.push(...box(bx, by + 3, bz, 3.4, 9, 3.4, 0.78));
  tris.push(...box(bx, by + 12, bz, 5.2, 1.2, 5.2, 0.9));
  tris.push(...box(bx, by + 13.2, bz, 3.4, 1.4, 3.4, 0.35, 0.55));
  tris.push(...cone(bx, by + 14.6, bz, 3.1, 3.4, 4, 0.7));
  // low walls / rocks
  for (let i = 0; i < 18; i++) {
    const a = (i / 18) * Math.PI * 2;
    const x = bx + Math.cos(a) * 14, z = bz + Math.sin(a) * 14;
    tris.push(...box(x, heightAt(x, z) - 0.2, z, 2.4 + hash(i) * 1.6, 1.6 + hash(i * 2) * 1.5, 2.2, 0.5));
  }
  return { tris, focus: [bx, by + 6, bz] };
};
let SCENE: ReturnType<typeof buildScene> | null = null;

export const GameEditor: React.FC<MockProps> = ({ width, height, f, compact }) => {
  if (!SCENE) SCENE = buildScene();
  const scene = SCENE;
  const sideW = compact ? 0 : 270;
  const rightW = compact ? 0 : 310;
  const bottomH = compact ? 0 : 124;
  const vw = Math.round(width - sideW - rightW - 2);
  const vh = Math.round(height - 42 - bottomH - 46);

  const draw = (ctx: CanvasRenderingContext2D, w: number, h: number) => {
    const t = f / 30;
    // sky
    const sky = ctx.createLinearGradient(0, 0, 0, h);
    sky.addColorStop(0, "#050506");
    sky.addColorStop(0.55, "#222226");
    sky.addColorStop(0.78, "#6a6a70");
    sky.addColorStop(1, "#8a8a90");
    ctx.fillStyle = sky;
    ctx.fillRect(0, 0, w, h);
    // sun glow
    const ang = -0.9 + t * 0.03;
    const sunX = w * (0.5 + 0.38 * Math.sin(ang)), sunY = h * 0.5;
    const glow = ctx.createRadialGradient(sunX, sunY, 0, sunX, sunY, h * 0.7);
    glow.addColorStop(0, "rgba(255,255,255,0.75)");
    glow.addColorStop(0.12, "rgba(255,255,255,0.28)");
    glow.addColorStop(1, "rgba(255,255,255,0)");
    ctx.fillStyle = glow;
    ctx.fillRect(0, 0, w, h);
    // stars
    for (let i = 0; i < 70; i++) {
      const x = hash(i * 1.3) * w, y = hash(i * 2.9) * h * 0.4;
      ctx.fillStyle = `rgba(255,255,255,${0.15 + hash(i) * 0.4 * (1 - y / (h * 0.4))})`;
      ctx.fillRect(x, y, 1.5, 1.5);
    }
    // camera: slow cinematic orbit + dolly
    const a = 0.55 + t * 0.075;
    const dist = 62 - t * 1.6;
    const cam = {
      pos: [scene.focus[0] + Math.sin(a) * dist, 20 + Math.sin(t * 0.3) * 1.5, scene.focus[2] - Math.cos(a) * dist] as V3,
      target: [scene.focus[0], scene.focus[1] - 2, scene.focus[2]] as V3,
      fov: 48,
    };
    drawTris(ctx, w, h, scene.tris, {
      cam,
      sun: [Math.sin(ang) * 0.9, 0.5, -Math.cos(ang) * 0.4 + 0.3],
      fogColor: [120, 120, 126],
      fogNear: 40,
      fogFar: 170,
      ambient: 0.16,
    });
    // selection + gizmo at the tower
    const proj = makeProjector(cam, w, h);
    const o = proj([scene.focus[0], scene.focus[1] - 5, scene.focus[2]]);
    if (o) {
      const L = 70;
      const axes: [V3, string, boolean][] = [
        [[3.2, 0, 0], "X", false],
        [[0, 3.2, 0], "Y", true],
        [[0, 0, 3.2], "Z", false],
      ];
      ctx.lineWidth = 2.5;
      for (const [d, name, strong] of axes) {
        const e = proj([scene.focus[0] + d[0], scene.focus[1] - 5 + d[1], scene.focus[2] + d[2]]);
        if (!e) continue;
        ctx.strokeStyle = strong ? "#ffffff" : "rgba(255,255,255,0.55)";
        ctx.beginPath();
        ctx.moveTo(o.x, o.y);
        ctx.lineTo(e.x, e.y);
        ctx.stroke();
        ctx.fillStyle = strong ? "#fff" : "rgba(255,255,255,0.7)";
        ctx.beginPath();
        ctx.arc(e.x, e.y, 6, 0, Math.PI * 2);
        ctx.fill();
        ctx.font = `600 13px ${theme.fonts.mono}`;
        ctx.fillText(name, e.x + 10, e.y - 8);
      }
      // bounding box outline
      ctx.setLineDash([6, 5]);
      ctx.strokeStyle = "rgba(255,255,255,0.8)";
      ctx.lineWidth = 1.3;
      const top = proj([scene.focus[0], scene.focus[1] + 11, scene.focus[2]]);
      if (top) {
        const half = Math.abs(o.y - top.y) * 0.32;
        ctx.strokeRect(o.x - half, top.y - 4, half * 2, Math.abs(o.y - top.y) + 8);
      }
      ctx.setLineDash([]);
      void L;
    }
  };

  const items = ["Scene", "Environment", "Directional Light", "Terrain", "Forest (120)", "Watchtower", "Player Start", "Main Camera"];
  return (
    <WindowFrame
      width={width}
      height={height}
      title="Frostline  ·  Level_03.scene"
      right={
        <div style={{ display: "flex", gap: 8, alignItems: "center", fontSize: 13, color: ui.dim }}>
          <div style={{ padding: "4px 12px", borderRadius: 7, border: `1px solid ${ui.line2}`, display: "flex", gap: 6, alignItems: "center", color: "#fff" }}>
            <Icon name="play" size={13} color="#fff" /> Play
          </div>
        </div>
      }
    >
      <div style={{ display: "flex", flex: 1, minHeight: 0 }}>
        {!compact && (
          <div style={{ width: sideW, borderRight: `1px solid ${ui.line}`, background: ui.panel, padding: 16 }}>
            <Label>Hierarchy</Label>
            <div style={{ marginTop: 14, display: "flex", flexDirection: "column", gap: 3 }}>
              {items.map((n, i) => (
                <div
                  key={n}
                  style={{
                    display: "flex", alignItems: "center", gap: 10, padding: "7px 10px", borderRadius: 7,
                    marginLeft: i === 0 ? 0 : 16, fontSize: 15,
                    background: n === "Watchtower" ? "rgba(255,255,255,0.1)" : "transparent",
                    color: n === "Watchtower" ? "#fff" : ui.dim,
                  }}
                >
                  <Icon name={i === 0 ? "folder" : "cube"} size={15} color={n === "Watchtower" ? "#fff" : ui.faint} />
                  {n}
                </div>
              ))}
            </div>
          </div>
        )}
        <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column" }}>
          <div style={{ height: 46, flex: "none", display: "flex", alignItems: "center", gap: 6, padding: "0 14px", background: ui.panel, borderBottom: `1px solid ${ui.line}` }}>
            {["select", "move", "rotate", "scale"].map((n, i) => (
              <div key={n} style={{ width: 32, height: 32, borderRadius: 7, display: "flex", alignItems: "center", justifyContent: "center", background: i === 1 ? "rgba(255,255,255,0.12)" : "transparent" }}>
                <Icon name={n} size={17} color={i === 1 ? "#fff" : ui.dim} />
              </div>
            ))}
            <div style={{ width: 1, height: 20, background: ui.line2, margin: "0 8px" }} />
            <div style={{ fontFamily: theme.fonts.mono, fontSize: 13, color: ui.dim }}>Perspective · Lit · Shadows</div>
          </div>
          <div style={{ flex: 1, minHeight: 0, position: "relative" }}>
            <Canvas3D width={vw} height={vh} draw={draw} />
            <div style={{ position: "absolute", left: 16, bottom: 14, fontFamily: theme.fonts.mono, fontSize: 12, color: "rgba(255,255,255,0.65)", letterSpacing: "0.08em" }}>
              WATCHTOWER  ·  MOVE
            </div>
          </div>
          {!compact && (
            <div style={{ height: bottomH, flex: "none", background: ui.panel, borderTop: `1px solid ${ui.line}`, padding: "12px 16px", display: "flex", flexDirection: "column", gap: 10 }}>
              <Label>Assets</Label>
              <div style={{ display: "flex", gap: 10 }}>
                {["Terrain_A", "Pine_Low", "Tower", "Rock_03", "Skybox", "Wind.ogg", "Player", "Fog.mat"].map((n, i) => (
                  <div key={n} style={{ width: 92 }}>
                    <div style={{ height: 52, borderRadius: 8, background: `linear-gradient(${135 + i * 20}deg, rgba(255,255,255,${0.1 + (i % 3) * 0.05}), rgba(255,255,255,0.02))`, border: `1px solid ${ui.line2}` }} />
                    <div style={{ fontSize: 12, color: ui.faint, marginTop: 5, whiteSpace: "nowrap" }}>{n}</div>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
        {!compact && (
          <div style={{ width: rightW, borderLeft: `1px solid ${ui.line}`, background: ui.panel, padding: 18 }}>
            <Label>Inspector</Label>
            <div style={{ fontSize: 22, fontWeight: 600, margin: "12px 0 18px" }}>Watchtower</div>
            <Label style={{ marginBottom: 10 }}>Transform</Label>
            {[["Position", "6.0", "0.0", "18.0"], ["Rotation", "0", `${(f * 0.2).toFixed(0)}`, "0"], ["Scale", "1.0", "1.0", "1.0"]].map((r) => (
              <div key={r[0]} style={{ display: "flex", alignItems: "center", marginBottom: 9 }}>
                <div style={{ width: 82, fontSize: 14, color: ui.faint }}>{r[0]}</div>
                {r.slice(1).map((v, i) => (
                  <div key={i} style={{ flex: 1, marginRight: 6, padding: "6px 8px", borderRadius: 6, background: "#0b0b0d", border: `1px solid ${ui.line}`, fontFamily: theme.fonts.mono, fontSize: 13, color: ui.dim }}>{v}</div>
                ))}
              </div>
            ))}
            <Label style={{ margin: "20px 0 10px" }}>Material</Label>
            {["Roughness", "Metallic", "Emission"].map((n, i) => (
              <div key={n} style={{ marginBottom: 12 }}>
                <div style={{ fontSize: 14, color: ui.faint, marginBottom: 6 }}>{n}</div>
                <div style={{ height: 4, borderRadius: 2, background: ui.line2 }}>
                  <div style={{ height: 4, borderRadius: 2, width: `${[72, 18, 40][i]}%`, background: "#fff" }} />
                </div>
              </div>
            ))}
            <Label style={{ margin: "20px 0 10px" }}>Components</Label>
            <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
              {["Mesh", "Collider", "Light Probe", "Script"].map((n) => (
                <div key={n} style={{ padding: "5px 11px", borderRadius: 999, border: `1px solid ${ui.line2}`, fontSize: 13, color: ui.dim }}>{n}</div>
              ))}
            </div>
          </div>
        )}
      </div>
    </WindowFrame>
  );
};
