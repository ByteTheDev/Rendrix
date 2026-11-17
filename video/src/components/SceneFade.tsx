import React from "react";
import { AbsoluteFill, interpolate, useCurrentFrame, useVideoConfig } from "remotion";
import { theme, sec } from "../theme";

/** Scene wrapper: soft in, faster scale-through + blur out. */
export const SceneFade: React.FC<{
  children: React.ReactNode;
  inFrames?: number;
  outFrames?: number;
  noIn?: boolean;
  noOut?: boolean;
}> = ({ children, inFrames = sec(0.5), outFrames = sec(0.4), noIn, noOut }) => {
  const f = useCurrentFrame();
  const { durationInFrames } = useVideoConfig();
  const o = noIn ? 1 : interpolate(f, [0, inFrames], [0, 1], { easing: theme.ease.out, extrapolateRight: "clamp" });
  const e = noOut
    ? 0
    : interpolate(f, [durationInFrames - outFrames, durationInFrames], [0, 1], {
        easing: theme.ease.in,
        extrapolateLeft: "clamp",
        extrapolateRight: "clamp",
      });
  return (
    <AbsoluteFill
      style={{
        opacity: o * (1 - e),
        transform: `scale(${1 + e * 0.035})`,
        filter: e > 0 ? `blur(${e * 10}px)` : undefined,
      }}
    >
      {children}
    </AbsoluteFill>
  );
};
