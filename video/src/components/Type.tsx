import React from "react";
import { interpolate, spring, useCurrentFrame, useVideoConfig } from "remotion";
import { theme } from "../theme";

type WordsProps = {
  text: string;
  /** local frame at which the first word starts */
  at: number;
  /** frames between words */
  per?: number;
  size: number;
  weight?: number;
  color?: string;
  tracking?: number; // em
  caps?: boolean;
  align?: "left" | "center";
  /** local frame at which the line begins to exit (undefined: never) */
  exitAt?: number;
  /** per-word color overrides by index */
  accent?: Record<number, string>;
  maxWidth?: number;
  family?: string;
  lineHeight?: number;
  rise?: number;
  gap?: number;
  style?: React.CSSProperties;
};

/**
 * Word-by-word reveal: opacity + rise + blur + scale.
 * Exit is faster than entry (10 frames).
 */
export const Words: React.FC<WordsProps> = ({
  text,
  at,
  per = 4,
  size,
  weight = 700,
  color = theme.colors.text,
  tracking = -0.03,
  caps,
  align = "center",
  exitAt,
  accent,
  maxWidth,
  family = theme.fonts.display,
  lineHeight = 1.1,
  rise,
  gap,
  style,
}) => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const words = text.split(" ");
  const exit =
    exitAt === undefined
      ? 0
      : interpolate(f, [exitAt, exitAt + 10], [0, 1], {
          easing: theme.ease.in,
          extrapolateLeft: "clamp",
          extrapolateRight: "clamp",
        });
  return (
    <div
      style={{
        display: "flex",
        flexWrap: "wrap",
        justifyContent: align === "center" ? "center" : "flex-start",
        columnGap: gap ?? size * 0.28,
        rowGap: size * 0.08,
        maxWidth,
        fontFamily: family,
        fontWeight: weight,
        fontSize: size,
        lineHeight,
        letterSpacing: `${tracking}em`,
        textTransform: caps ? "uppercase" : undefined,
        color,
        opacity: 1 - exit,
        transform: `translateY(${-exit * size * 0.25}px)`,
        filter: exit > 0 ? `blur(${exit * 8}px)` : undefined,
        ...style,
      }}
    >
      {words.map((w, i) => {
        const p = spring({ frame: f - at - i * per, fps, config: theme.spring.smooth });
        const wc = accent?.[i];
        return (
          <span
            key={i}
            style={{
              display: "inline-block",
              opacity: interpolate(p, [0, 0.6], [0, 1], { extrapolateRight: "clamp" }),
              transform: `translateY(${interpolate(p, [0, 1], [rise ?? size * 0.5, 0])}px) scale(${interpolate(p, [0, 1], [0.96, 1])})`,
              filter: `blur(${interpolate(p, [0, 1], [size * 0.14, 0], { extrapolateRight: "clamp" })}px)`,
              color: wc ?? undefined,
            }}
          >
            {w}
          </span>
        );
      })}
    </div>
  );
};

/** Single block entrance: opacity + rise + blur + scale. */
export const Rise: React.FC<{
  at: number;
  children: React.ReactNode;
  distance?: number;
  style?: React.CSSProperties;
  config?: keyof typeof theme.spring;
  scaleFrom?: number;
}> = ({ at, children, distance = 28, style, config = "smooth", scaleFrom = 0.97 }) => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const p = spring({ frame: f - at, fps, config: theme.spring[config] });
  return (
    <div
      style={{
        opacity: interpolate(p, [0, 0.5], [0, 1], { extrapolateRight: "clamp" }),
        transform: `translateY(${interpolate(p, [0, 1], [distance, 0])}px) scale(${interpolate(p, [0, 1], [scaleFrom, 1])})`,
        filter: `blur(${interpolate(p, [0, 1], [10, 0], { extrapolateRight: "clamp" })}px)`,
        ...style,
      }}
    >
      {children}
    </div>
  );
};
