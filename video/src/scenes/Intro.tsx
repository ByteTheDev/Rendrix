import React from "react";
import { AbsoluteFill, interpolate, spring, useCurrentFrame, useVideoConfig } from "remotion";
import { SceneFade } from "../components/SceneFade";
import { RMark, Sweep, WordmarkText } from "../components/Mark";
import { Rise } from "../components/Type";
import { hold } from "../components/Backdrop";
import { theme, sec } from "../theme";
import { useLayout } from "../layout";

/** Lockup shared by intro and closing. */
export const Lockup: React.FC<{ at: number; markW: number; wordSize: number; sweepAt?: number; breathe?: boolean }> = ({
  at,
  markW,
  wordSize,
  sweepAt,
  breathe = true,
}) => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const impact = spring({ frame: f - at, fps, config: { damping: 18, stiffness: 140, mass: 0.9 } });
  const reveal = interpolate(f - at, [0, sec(0.7)], [0, 1], {
    easing: theme.ease.out,
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  const b = breathe ? 1 + Math.sin(f / 40) * 0.006 : 1;
  const markH = (markW * 424) / 545;
  const letter = (i: number) =>
    spring({ frame: f - at - sec(0.55) - i * 3, fps, config: theme.spring.smooth });
  const sweepP = sweepAt === undefined ? -1 : interpolate(f - sweepAt, [0, sec(1.3)], [0, 1], {
    easing: theme.ease.inOut,
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        gap: wordSize * 0.42,
        transform: `scale(${b * interpolate(impact, [0, 1], [1.12, 1])})`,
      }}
    >
      <div style={{ position: "relative" }}>
        {/* bloom behind the mark */}
        <div
          style={{
            position: "absolute",
            left: -markW * 0.6,
            top: -markH * 0.8,
            width: markW * 2.2,
            height: markH * 2.6,
            background: "radial-gradient(ellipse at center, rgba(255,255,255,0.16), transparent 62%)",
            opacity: reveal,
          }}
        />
        <Sweep progress={sweepP} width={markW} height={markH}>
          <RMark width={markW} reveal={reveal} style={{ filter: "drop-shadow(0 0 40px rgba(255,255,255,0.22))" }} />
        </Sweep>
      </div>
      <WordmarkText size={wordSize} letter={letter} />
    </div>
  );
};

/** Scene 3: the reveal. */
export const Intro: React.FC = () => {
  const f = useCurrentFrame();
  const { pick, w } = useLayout();
  // light streak that fires on the bass hit
  const streak = interpolate(f, [0, sec(0.5)], [0, 1], { easing: theme.ease.out, extrapolateRight: "clamp" });
  const streakFade = 1 - hold(f, sec(0.3), sec(1.1));
  return (
    <SceneFade inFrames={sec(0.15)} outFrames={sec(0.5)}>
      <AbsoluteFill style={{ alignItems: "center", justifyContent: "center" }}>
        <div
          style={{
            position: "absolute",
            top: "50%",
            left: "50%",
            height: 2,
            width: w * 1.1 * streak,
            marginLeft: -w * 0.55 * streak,
            background: "linear-gradient(90deg, transparent, #fff, transparent)",
            opacity: streakFade,
            filter: "blur(1px)",
            boxShadow: "0 0 60px 8px rgba(255,255,255,0.35)",
          }}
        />
        <div style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: pick(64, 80) }}>
          <Lockup at={sec(0.1)} markW={pick(210, 250)} wordSize={pick(150, 150)} sweepAt={sec(1.4)} />
          <Rise at={sec(3.3)} distance={20}>
            <div
              style={{
                fontFamily: theme.fonts.display,
                fontWeight: 400,
                fontSize: pick(44, 44),
                letterSpacing: "0.01em",
                color: theme.colors.dim,
                textAlign: "center",
                maxWidth: pick(1200, 800),
              }}
            >
              A new direction for native development.
            </div>
          </Rise>
        </div>
      </AbsoluteFill>
    </SceneFade>
  );
};
