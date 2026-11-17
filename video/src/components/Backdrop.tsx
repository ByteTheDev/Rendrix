import React from "react";
import { AbsoluteFill, interpolate, useCurrentFrame } from "remotion";
import { theme } from "../theme";

/** Bottom layer: deep black with an almost imperceptible, slowly drifting top light. */
export const Backdrop: React.FC = () => {
  const f = useCurrentFrame();
  const dx = Math.sin(f / 140) * 90;
  const dy = Math.cos(f / 170) * 40;
  return (
    <AbsoluteFill style={{ background: theme.colors.bg }}>
      <div
        style={{
          position: "absolute",
          left: "50%",
          top: "-30%",
          width: 1800,
          height: 1100,
          marginLeft: -900 + dx,
          transform: `translateY(${dy}px)`,
          background: "radial-gradient(ellipse at center, rgba(255,255,255,0.055), transparent 62%)",
          filter: "blur(30px)",
        }}
      />
      <div
        style={{
          position: "absolute",
          inset: 0,
          background: "radial-gradient(ellipse at 50% 120%, rgba(255,255,255,0.03), transparent 55%)",
        }}
      />
    </AbsoluteFill>
  );
};

const NOISE =
  "url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='260' height='260'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.85' numOctaves='2' stitchTiles='stitch'/%3E%3CfeColorMatrix type='saturate' values='0'/%3E%3C/filter%3E%3Crect width='260' height='260' filter='url(%23n)'/%3E%3C/svg%3E\")";

/** Top layers: ultra-light color grade, film grain, vignette. */
export const Finish: React.FC = () => {
  const f = useCurrentFrame();
  return (
    <AbsoluteFill style={{ pointerEvents: "none" }}>
      <AbsoluteFill
        style={{
          background:
            "linear-gradient(180deg, rgba(0,0,0,0.18), transparent 22%, transparent 78%, rgba(0,0,0,0.25))",
        }}
      />
      <AbsoluteFill
        style={{
          backgroundImage: NOISE,
          backgroundSize: "260px",
          backgroundPosition: `${(f * 53) % 260}px ${(f * 91) % 260}px`,
          opacity: 0.07,
          mixBlendMode: "overlay",
        }}
      />
      <AbsoluteFill
        style={{ background: "radial-gradient(ellipse at center, transparent 52%, rgba(0,0,0,0.55) 100%)" }}
      />
    </AbsoluteFill>
  );
};

export const hold = (f: number, a: number, b: number) =>
  interpolate(f, [a, b], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
