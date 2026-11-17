import React from "react";
import { theme } from "../theme";

export type MockProps = { width: number; height: number; f: number; compact: boolean };

export const ui = {
  bg: "#0b0b0d",
  panel: "#111114",
  panel2: "#16161a",
  line: "#222227",
  line2: "#2c2c32",
  text: "#f4f4f5",
  dim: "#a1a1aa",
  faint: "#71717a",
  ghost: "#4a4a52",
};

/** Native-looking window frame (monochrome). */
export const WindowFrame: React.FC<{
  width: number;
  height: number;
  title?: string;
  children: React.ReactNode;
  right?: React.ReactNode;
  titlebar?: boolean;
}> = ({ width, height, title, children, right, titlebar = true }) => (
  <div
    style={{
      width,
      height,
      borderRadius: 18,
      overflow: "hidden",
      background: ui.bg,
      border: "1px solid rgba(255,255,255,0.14)",
      boxShadow: "0 60px 140px -30px rgba(0,0,0,0.95), 0 0 0 1px rgba(0,0,0,0.6), inset 0 1px 0 rgba(255,255,255,0.1)",
      display: "flex",
      flexDirection: "column",
      fontFamily: theme.fonts.display,
      color: ui.text,
      position: "relative",
    }}
  >
    {titlebar && (
      <div
        style={{
          height: 42,
          flex: "none",
          display: "flex",
          alignItems: "center",
          padding: "0 16px",
          background: "linear-gradient(180deg, #17171a, #111114)",
          borderBottom: `1px solid ${ui.line}`,
          position: "relative",
        }}
      >
        <div style={{ display: "flex", gap: 8 }}>
          {[0, 1, 2].map((i) => (
            <div key={i} style={{ width: 12, height: 12, borderRadius: 6, background: "#34343b" }} />
          ))}
        </div>
        <div
          style={{
            position: "absolute",
            left: 0,
            right: 0,
            textAlign: "center",
            fontSize: 15,
            color: ui.dim,
            letterSpacing: "0.01em",
            pointerEvents: "none",
          }}
        >
          {title}
        </div>
        <div style={{ marginLeft: "auto" }}>{right}</div>
      </div>
    )}
    <div style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: "column" }}>{children}</div>
  </div>
);

export const Label: React.FC<{ children: React.ReactNode; style?: React.CSSProperties }> = ({ children, style }) => (
  <div
    style={{
      fontFamily: theme.fonts.mono,
      fontSize: 12,
      letterSpacing: "0.16em",
      textTransform: "uppercase",
      color: ui.faint,
      ...style,
    }}
  >
    {children}
  </div>
);

export const Icon: React.FC<{ name: string; size?: number; color?: string }> = ({ name, size = 18, color = ui.dim }) => {
  const s = { stroke: color, strokeWidth: 1.6, fill: "none", strokeLinecap: "round" as const, strokeLinejoin: "round" as const };
  const paths: Record<string, React.ReactNode> = {
    move: <path d="M12 3v18M3 12h18M12 3l-3 3M12 3l3 3M12 21l-3-3M12 21l3-3M3 12l3-3M3 12l3 3M21 12l-3-3M21 12l-3 3" {...s} />,
    rotate: <path d="M20 12a8 8 0 1 1-3-6.2M20 4v4h-4" {...s} />,
    scale: <path d="M4 20V10M4 20h10M4 20L20 4M14 4h6v6" {...s} />,
    select: <path d="M5 3l14 8-6 2-2 6z" {...s} />,
    play: <path d="M7 4l13 8-13 8z" {...s} />,
    cube: <path d="M12 3l8 4.5v9L12 21l-8-4.5v-9zM12 12l8-4.5M12 12v9M12 12L4 7.5" {...s} />,
    folder: <path d="M3 7h6l2 2h10v10H3z" {...s} />,
    file: <path d="M6 3h8l4 4v14H6zM14 3v4h4" {...s} />,
    search: <path d="M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM20 20l-4-4" {...s} />,
    home: <path d="M4 11l8-7 8 7v9h-5v-6H9v6H4z" {...s} />,
    chart: <path d="M4 20V4M4 20h16M8 16v-5M12 16V8M16 16v-3" {...s} />,
    users: <path d="M9 11a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7zM2 20c0-4 3-6 7-6s7 2 7 6M17 5a3 3 0 0 1 0 6M22 19c0-3-2-5-4-5.5" {...s} />,
    gear: <path d="M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M5.6 18.4l2.1-2.1M16.3 7.7l2.1-2.1" {...s} />,
    back: <path d="M15 5l-7 7 7 7" {...s} />,
    forward: <path d="M9 5l7 7-7 7" {...s} />,
    reload: <path d="M20 12a8 8 0 1 1-2.3-5.7M20 4v5h-5" {...s} />,
    lock: <path d="M6 11h12v9H6zM8.5 11V8a3.5 3.5 0 0 1 7 0v3" {...s} />,
    star: <path d="M12 3l2.7 5.6 6.1.9-4.4 4.3 1 6.1L12 17l-5.4 2.9 1-6.1L3.2 9.5l6.1-.9z" {...s} />,
    plus: <path d="M12 5v14M5 12h14" {...s} />,
    sculpt: <path d="M5 19c4-1 4-8 8-9s5 3 6 6M14 5l5 5" {...s} />,
    brush: <path d="M18 3l3 3-9 9-3-3zM9 12c-3 0-4 3-4 5s-1 3-1 3 5 1 6-1 1-4-1-7z" {...s} />,
    loop: <path d="M4 12a5 5 0 0 1 5-5h9l-3-3M20 12a5 5 0 0 1-5 5H6l3 3" {...s} />,
    eye: <path d="M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z" {...s} />,
    grid: <path d="M4 4h16v16H4zM4 12h16M12 4v16" {...s} />,
  };
  return (
    <svg width={size} height={size} viewBox="0 0 24 24">
      {paths[name] ?? null}
    </svg>
  );
};
