import React from "react";
import { AbsoluteFill, interpolate, useCurrentFrame, useVideoConfig } from "remotion";
import { SceneFade } from "../components/SceneFade";
import { Rise } from "../components/Type";
import { hold } from "../components/Backdrop";
import { theme, sec } from "../theme";
import { useLayout } from "../layout";
import { GameEditor } from "../mockups/GameEditor";
import { CodeEditor } from "../mockups/CodeEditor";
import { DesktopApp } from "../mockups/DesktopApp";
import { ModelingApp } from "../mockups/ModelingApp";
import { BrowserApp } from "../mockups/BrowserApp";
import { MockProps } from "../mockups/ui";

type Shot = {
  label: string;
  Comp: React.FC<MockProps>;
  /** camera: start/end rotateY, rotateX, scale, translate */
  cam: { ry: [number, number]; rx: [number, number]; s: [number, number]; x: [number, number]; y: [number, number] };
};

const SHOTS: Shot[] = [
  { label: "GAME ENGINE EDITOR", Comp: GameEditor, cam: { ry: [-9, -2], rx: [3, 1], s: [1.0, 1.1], x: [-60, 20], y: [0, 0] } },
  { label: "CODE EDITOR", Comp: CodeEditor, cam: { ry: [7, 1], rx: [2, 0], s: [1.08, 1.0], x: [40, -20], y: [10, 0] } },
  { label: "NATIVE DESKTOP APPLICATION", Comp: DesktopApp, cam: { ry: [-5, 6], rx: [4, 1], s: [1.0, 1.08], x: [-30, 30], y: [0, -10] } },
  { label: "3D MODELING ENVIRONMENT", Comp: ModelingApp, cam: { ry: [8, -4], rx: [2, 3], s: [1.05, 1.0], x: [30, -30], y: [0, 10] } },
  { label: "BROWSER", Comp: BrowserApp, cam: { ry: [-6, 0], rx: [1, 0], s: [1.0, 1.1], x: [-40, 0], y: [8, 0] } },
];

const SHOT_LEN = sec(2.4);
const OVERLAP = sec(0.5);

/** Scene 5: five vision shots with cinematic camera moves. */
export const Beyond: React.FC = () => {
  const f = useCurrentFrame();
  const { durationInFrames } = useVideoConfig();
  const { w, h, pick, vertical } = useLayout();

  const winW = pick(1560, 960);
  const winH = pick(760, 1180);
  const winTop = pick(92, 210);
  const compact = vertical;

  return (
    <SceneFade inFrames={sec(0.3)} outFrames={sec(0.5)}>
      <AbsoluteFill style={{ perspective: 2600, perspectiveOrigin: "50% 38%" }}>
        {SHOTS.map((S, i) => {
          const start = i * SHOT_LEN;
          const local = f - start;
          const end = SHOT_LEN + (i === SHOTS.length - 1 ? 9999 : OVERLAP);
          if (local < -OVERLAP || local > end) return null;
          const dur = SHOT_LEN + OVERLAP;
          const t = interpolate(local, [-OVERLAP, dur], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp", easing: theme.ease.soft });
          const lerp = (a: [number, number]) => a[0] + (a[1] - a[0]) * t;
          const inP = interpolate(local, [-OVERLAP, sec(0.3)], [0, 1], { easing: theme.ease.out, extrapolateLeft: "clamp", extrapolateRight: "clamp" });
          const isLast = i === SHOTS.length - 1;
          const outP = isLast
            ? interpolate(f, [durationInFrames - sec(0.6), durationInFrames], [0, 1], { easing: theme.ease.in, extrapolateLeft: "clamp", extrapolateRight: "clamp" })
            : interpolate(local, [SHOT_LEN - sec(0.1), SHOT_LEN + OVERLAP], [0, 1], { easing: theme.ease.in, extrapolateLeft: "clamp", extrapolateRight: "clamp" });
          const vf = Math.max(0, local);
          return (
            <div
              key={i}
              style={{
                position: "absolute",
                left: (w - winW) / 2,
                top: winTop,
                width: winW,
                height: winH,
                opacity: inP * (1 - outP),
                filter: outP > 0 ? `blur(${outP * 14}px)` : inP < 1 ? `blur(${(1 - inP) * 10}px)` : undefined,
                transform: `translate(${lerp(S.cam.x) * pick(1, 0.5)}px, ${lerp(S.cam.y) + (1 - inP) * 50}px) rotateY(${lerp(S.cam.ry) * pick(1, 0.7)}deg) rotateX(${lerp(S.cam.rx)}deg) scale(${lerp(S.cam.s) * (1 + outP * 0.06)})`,
                transformOrigin: "50% 50%",
                transformStyle: "preserve-3d",
              }}
            >
              <S.Comp width={winW} height={winH} f={vf} compact={compact} />
              {/* soft reflection floor line */}
              <div style={{ position: "absolute", left: 0, right: 0, bottom: -54, height: 2, background: "linear-gradient(90deg, transparent, rgba(255,255,255,0.25), transparent)", opacity: 0.5 }} />
            </div>
          );
        })}
      </AbsoluteFill>

      {/* shot index + disclaimer */}
      <AbsoluteFill style={{ pointerEvents: "none" }}>
        <div style={{ position: "absolute", top: pick(1010, 1790), left: 0, right: 0, textAlign: "center", fontFamily: theme.fonts.mono, fontSize: 15, letterSpacing: "0.26em", color: theme.colors.ghost }}>
          VISION CONCEPTS  -  ILLUSTRATIVE, NOT SHIPPING PRODUCTS
        </div>
        <div style={{ position: "absolute", top: pick(28, 130), left: 0, right: 0, display: "flex", justifyContent: "center" }}>
          {SHOTS.map((S, i) => {
            const local = f - i * SHOT_LEN;
            const on = local >= 0 && local < SHOT_LEN;
            const p = hold(local, 0, sec(0.35));
            return on ? (
              <div key={i} style={{ fontFamily: theme.fonts.mono, fontSize: 17, letterSpacing: "0.3em", color: theme.colors.dim, opacity: p * (1 - hold(local, SHOT_LEN - sec(0.25), SHOT_LEN)), transform: `translateY(${(1 - p) * 10}px)` }}>
                {String(i + 1).padStart(2, "0")} / 05  ·  {S.label}
              </div>
            ) : null;
          })}
        </div>
      </AbsoluteFill>

      {/* captions */}
      <AbsoluteFill style={{ pointerEvents: "none", justifyContent: "flex-end", alignItems: "center", paddingBottom: pick(66, 200) }}>
        <div style={{ position: "relative", width: w, height: pick(120, 200) }}>
          <Caption text="Not just applications." from={sec(0.5)} to={sec(4.9)} f={f} />
          <Caption text="Not just games." from={sec(5.1)} to={sec(9.3)} f={f} />
          <Caption text="A platform to build almost anything." from={sec(9.5)} to={durationInFrames} f={f} big />
        </div>
      </AbsoluteFill>
    </SceneFade>
  );
};

const Caption: React.FC<{ text: string; from: number; to: number; f: number; big?: boolean }> = ({ text, from, to, f, big }) => {
  const { pick, w } = useLayout();
  const inP = hold(f, from, from + sec(0.55));
  const outP = hold(f, to - sec(0.3), to);
  if (f < from - 2 || f > to) return null;
  return (
    <div
      style={{
        position: "absolute",
        left: 0,
        right: 0,
        top: 0,
        display: "flex",
        justifyContent: "center",
        opacity: inP * (1 - outP),
        transform: `translateY(${(1 - inP) * 36 - outP * 20}px)`,
        filter: `blur(${(1 - inP) * 12 + outP * 8}px)`,
      }}
    >
      <div
        style={{
          fontFamily: theme.fonts.display,
          fontWeight: 800,
          letterSpacing: "-0.035em",
          fontSize: big ? pick(92, 84) : pick(84, 84),
          textAlign: "center",
          maxWidth: w - 120,
          lineHeight: 1.05,
          textShadow: "0 0 50px #000, 0 0 24px #000, 0 6px 40px #000",
          color: big ? theme.colors.text : theme.colors.soft,
        }}
      >
        {text}
      </div>
    </div>
  );
};
