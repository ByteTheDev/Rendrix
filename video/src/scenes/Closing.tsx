import React from "react";
import { AbsoluteFill, interpolate, useCurrentFrame, useVideoConfig } from "remotion";
import { SceneFade } from "../components/SceneFade";
import { Rise, Words } from "../components/Type";
import { RMark } from "../components/Mark";
import { Lockup } from "./Intro";
import { hold } from "../components/Backdrop";
import { theme, sec } from "../theme";
import { useLayout } from "../layout";

/** Scene 7: the closing lockup, a restrained impact, then the identity alone on black. */
export const Closing: React.FC = () => {
  const f = useCurrentFrame();
  const { durationInFrames } = useVideoConfig();
  const { pick, w } = useLayout();

  // lockup + text leave; the mark stays
  const leave = hold(f, sec(7.0), sec(7.9));
  const markIn = hold(f, sec(7.3), sec(8.4));
  const ring = interpolate(f, [sec(0.5), sec(2.0)], [0, 1], { easing: theme.ease.out, extrapolateLeft: "clamp", extrapolateRight: "clamp" });

  return (
    <SceneFade inFrames={sec(0.2)} noOut>
      <AbsoluteFill style={{ alignItems: "center", justifyContent: "center" }}>
        {/* shockwave ring on impact */}
        <div
          style={{
            position: "absolute",
            width: 400 + ring * 1700,
            height: 400 + ring * 1700,
            borderRadius: "50%",
            border: "1px solid rgba(255,255,255,0.5)",
            opacity: (1 - ring) * 0.5,
          }}
        />
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            gap: pick(54, 70),
            opacity: 1 - leave,
            transform: `translateY(${-leave * 24}px) scale(${1 - leave * 0.04})`,
            filter: leave > 0 ? `blur(${leave * 10}px)` : undefined,
          }}
        >
          <Lockup at={sec(0.5)} markW={pick(190, 220)} wordSize={pick(190, 170)} sweepAt={sec(1.8)} />
          <div style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: 30 }}>
            <Words
              text="THE FUTURE OF NATIVE DEVELOPMENT."
              at={sec(3.0)}
              per={4}
              size={pick(44, 42)}
              weight={600}
              tracking={0.2}
              color={theme.colors.soft}
              maxWidth={pick(1500, 900)}
              lineHeight={1.4}
              rise={20}
            />
            <Rise at={sec(5.0)} distance={14}>
              <div style={{ fontFamily: theme.fonts.mono, fontSize: pick(26, 26), letterSpacing: "0.1em", color: theme.colors.faint }}>
                Built with Rust. Designed for TypeScript.
              </div>
            </Rise>
          </div>
        </div>

        {/* end card: the identity on clean black */}
        <div
          style={{
            position: "absolute",
            opacity: markIn,
            transform: `scale(${interpolate(markIn, [0, 1], [0.94, 1])})`,
            filter: `blur(${(1 - markIn) * 8}px)`,
          }}
        >
          <RMark width={pick(150, 170)} style={{ opacity: 0.95 + Math.sin(f / 40) * 0.05 }} />
        </div>
      </AbsoluteFill>
      {w > 0 && durationInFrames > 0 && null}
    </SceneFade>
  );
};
