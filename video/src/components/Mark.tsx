import React from "react";
import { interpolate } from "remotion";
import { theme } from "../theme";

const PATH =
  "M603 304H885C965 304 1020 358 1020 430C1020 500 962 552 877 562L1013 713H870L679 487H836C882 487 908 464 908 436C908 406 884 385 852 385H689L606 687H487Z";

/** The Rendrix "R" mark (from website/assets/rendrix-mark.svg). Aspect 545:424. */
export const RMark: React.FC<{
  width: number;
  color?: string;
  style?: React.CSSProperties;
  /** 0..1 vertical reveal progress (clip from bottom to top) */
  reveal?: number;
  outline?: boolean;
}> = ({ width, color = theme.colors.text, style, reveal = 1, outline }) => {
  const h = (width * 424) / 545;
  const clip = `inset(${(1 - reveal) * 100}% 0 0 0)`;
  return (
    <svg
      viewBox="480 296 545 424"
      width={width}
      height={h}
      style={{ display: "block", overflow: "visible", clipPath: reveal < 1 ? clip : undefined, ...style }}
    >
      <path
        d={PATH}
        fill={outline ? "none" : color}
        stroke={outline ? color : "none"}
        strokeWidth={outline ? 2 : 0}
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
};

/** Wordmark text in the site's style: heavy italic Archivo, tight tracking. */
export const WordmarkText: React.FC<{
  size: number;
  letter?: (i: number) => number;
  tracking?: number;
  color?: string;
}> = ({ size, letter, tracking = -0.03, color = theme.colors.text }) => {
  const text = "RENDRIX".split("");
  return (
    <div
      style={{
        display: "flex",
        fontFamily: theme.fonts.display,
        fontStyle: "italic",
        fontWeight: 900,
        fontSize: size,
        lineHeight: 1,
        color,
        letterSpacing: `${tracking}em`,
        paddingRight: size * 0.12,
      }}
    >
      {text.map((c, i) => {
        const p = letter ? letter(i) : 1;
        return (
          <span
            key={i}
            style={{
              display: "inline-block",
              opacity: p,
              transform: `translateY(${interpolate(p, [0, 1], [size * 0.4, 0])}px)`,
              filter: `blur(${interpolate(p, [0, 1], [size * 0.07, 0])}px)`,
            }}
          >
            {c}
          </span>
        );
      })}
    </div>
  );
};

/** Diagonal light sweep clipped to its children. */
export const Sweep: React.FC<{ progress: number; children: React.ReactNode; width: number; height: number }> = ({
  progress,
  children,
  width,
  height,
}) => (
  <div style={{ position: "relative", width, height }}>
    {children}
    <div style={{ position: "absolute", inset: 0, overflow: "hidden", mixBlendMode: "overlay", pointerEvents: "none" }}>
      <div
        style={{
          position: "absolute",
          top: -height,
          bottom: -height,
          width: width * 0.35,
          left: interpolate(progress, [0, 1], [-width * 0.5, width * 1.2]),
          transform: "rotate(18deg)",
          background: "linear-gradient(90deg, transparent, rgba(255,255,255,0.95), transparent)",
        }}
      />
    </div>
  </div>
);
