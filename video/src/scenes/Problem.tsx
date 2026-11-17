import React from "react";
import { AbsoluteFill, interpolate, spring, useCurrentFrame, useVideoConfig } from "remotion";
import { SceneFade } from "../components/SceneFade";
import { Words } from "../components/Type";
import { hold } from "../components/Backdrop";
import { theme, sec } from "../theme";
import { useLayout } from "../layout";

const LAYERS = [
  { label: "Your application", sub: "app code", strong: true },
  { label: "UI framework", sub: "virtual DOM, state" },
  { label: "HTML + CSS", sub: "markup, styles" },
  { label: "Browser engine", sub: "Chromium" },
  { label: "Desktop shell", sub: "Electron" },
  { label: "System WebView", sub: "embedded browser" },
  { label: "Operating system", sub: "windowing, GPU" },
];

/** Scene 2: a stack of layers, drawn as glass slabs in 3D. Descriptive, no benchmarks. */
export const Problem: React.FC = () => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const { pick, w, h } = useLayout();

  const slabW = pick(760, 720);
  const slabH = pick(150, 150);
  const spacing = pick(66, 62);
  const total = LAYERS.length;

  // after "Chromium. Electron. WebViews." starts, the stack recedes
  const recede = hold(f, sec(4.2), sec(5.2));
  const rot = interpolate(f, [0, sec(8.4)], [-34, -22]);
  const stackScale = pick(1, 0.88) * interpolate(recede, [0, 1], [1, 1.12]);

  const stackLeft = pick(w * 0.7, w * 0.5);
  const stackTop = pick(h * 0.52, h * 0.64);

  const textX = pick(150, w / 2);

  return (
    <SceneFade inFrames={sec(0.6)}>
      {/* the stack */}
      <AbsoluteFill
        style={{
          perspective: 2400,
          perspectiveOrigin: "50% 40%",
          opacity: interpolate(recede, [0, 1], [1, 0.28]),
          filter: `blur(${recede * 3}px)`,
        }}
      >
        <div
          style={{
            position: "absolute",
            left: stackLeft,
            top: stackTop,
            width: 0,
            height: 0,
            transformStyle: "preserve-3d",
            transform: `scale(${stackScale}) rotateX(62deg) rotateZ(${rot}deg)`,
          }}
        >
          {LAYERS.map((L, i) => {
            const fromTop = total - 1 - i; // bottom layers settle first
            const p = spring({
              frame: f - sec(0.9) - fromTop * 5,
              fps,
              config: { damping: 18, stiffness: 70, mass: 1.2 },
            });
            const z = (total - 1 - i) * spacing - ((total - 1) * spacing) / 2;
            const lift = interpolate(p, [0, 1], [900, 0]);
            const breathe = Math.sin(f / 34 + i * 0.7) * 4;
            return (
              <div
                key={L.label}
                style={{
                  position: "absolute",
                  width: slabW,
                  height: slabH,
                  left: -slabW / 2,
                  top: -slabH / 2,
                  borderRadius: 14,
                  border: L.strong ? "1px solid rgba(255,255,255,0.55)" : "1px solid rgba(255,255,255,0.16)",
                  background: L.strong
                    ? "linear-gradient(135deg, rgba(255,255,255,0.22), rgba(255,255,255,0.05))"
                    : "linear-gradient(135deg, rgba(255,255,255,0.08), rgba(255,255,255,0.015))",
                  boxShadow: "0 30px 60px rgba(0,0,0,0.6)",
                  opacity: interpolate(p, [0, 0.4], [0, 1], { extrapolateRight: "clamp" }),
                  transform: `translateZ(${z + lift + breathe}px)`,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "space-between",
                  padding: "0 36px",
                  fontFamily: theme.fonts.display,
                  color: theme.colors.text,
                }}
              >
                <span style={{ fontSize: 34, fontWeight: 600, letterSpacing: "-0.01em" }}>{L.label}</span>
                <span
                  style={{
                    fontFamily: theme.fonts.mono,
                    fontSize: 22,
                    color: theme.colors.dim,
                    letterSpacing: "0.04em",
                  }}
                >
                  {L.sub}
                </span>
              </div>
            );
          })}
        </div>
      </AbsoluteFill>

      {/* statement */}
      <AbsoluteFill
        style={{
          alignItems: pick("flex-start", "center"),
          justifyContent: pick("center", "flex-start"),
          paddingLeft: pick(textX, 0),
          paddingTop: pick(0, h * 0.1),
        }}
      >
        <div style={{ width: pick(760, 900) }}>
          <Words
            text="Desktop development has relied on the same compromises for too long."
            at={sec(0.7)}
            per={3}
            size={pick(70, 68)}
            weight={700}
            tracking={-0.035}
            align={pick("left" as const, "center" as const)}
            exitAt={sec(4.3)}
            lineHeight={1.08}
            accent={{ 6: theme.colors.text }}
            style={{ color: theme.colors.soft }}
          />
        </div>
      </AbsoluteFill>

      {/* the three names */}
      <AbsoluteFill style={{ alignItems: "center", justifyContent: "center" }}>
        <div
          style={{
            display: "flex",
            flexDirection: pick("row", "column"),
            alignItems: "center",
            gap: pick(48, 8),
            fontFamily: theme.fonts.display,
            fontWeight: 800,
            letterSpacing: "-0.04em",
            fontSize: pick(92, 132),
            textShadow: "0 0 80px rgba(0,0,0,0.9), 0 0 30px rgba(0,0,0,0.8)",
          }}
        >
          {["Chromium.", "Electron.", "WebViews."].map((t, i) => (
            <Words
              key={t}
              text={t}
              at={sec(5.0 + i * 0.8)}
              per={1}
              size={pick(92, 132)}
              weight={800}
              tracking={-0.04}
              color={i === 2 ? theme.colors.text : theme.colors.dim}
              rise={40}
            />
          ))}
        </div>
      </AbsoluteFill>
    </SceneFade>
  );
};
