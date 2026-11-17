import React from "react";
import { interpolate } from "remotion";
import { theme } from "../theme";
import { Icon, Label, MockProps, WindowFrame, ui } from "./ui";

const SERIES_A = [22, 30, 26, 38, 34, 46, 42, 58, 52, 66, 61, 78];
const SERIES_B = [30, 28, 34, 32, 40, 38, 44, 47, 50, 49, 56, 60];

const path = (vals: number[], w: number, h: number, p: number) => {
  const max = 90;
  const n = Math.max(2, Math.floor(vals.length * p + 1));
  const pts = vals.slice(0, n).map((v, i) => [(i / (vals.length - 1)) * w, h - (v / max) * h]);
  let d = `M${pts[0][0]},${pts[0][1]}`;
  for (let i = 1; i < pts.length; i++) {
    const [x0, y0] = pts[i - 1], [x1, y1] = pts[i];
    const cx = (x0 + x1) / 2;
    d += ` C${cx},${y0} ${cx},${y1} ${x1},${y1}`;
  }
  return d;
};

/** A calm, polished desktop app: Meridian, a project dashboard. */
export const DesktopApp: React.FC<MockProps> = ({ width, height, f, compact }) => {
  const sideW = compact ? 76 : 240;
  const p = interpolate(f, [4, 50], [0, 1], { easing: theme.ease.out, extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  const cw = width - sideW - 80;
  const chartW = compact ? cw - 8 : Math.round(cw * 0.64);
  const chartH = compact ? 230 : 330;
  const nav = [["home", "Overview"], ["chart", "Analytics"], ["users", "Team"], ["folder", "Projects"], ["gear", "Settings"]];
  const stats = [
    ["Active projects", 24, ""],
    ["Open tasks", 138, ""],
    ["Shipped this month", 17, ""],
  ];
  return (
    <WindowFrame width={width} height={height} title="Meridian">
      <div style={{ display: "flex", flex: 1, minHeight: 0 }}>
        <div style={{ width: sideW, background: ui.panel, borderRight: `1px solid ${ui.line}`, padding: compact ? "22px 14px" : "24px 18px", display: "flex", flexDirection: "column", gap: 4 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 12, padding: "0 8px 22px", justifyContent: compact ? "center" : "flex-start" }}>
            <div style={{ width: 30, height: 30, borderRadius: 9, background: "#fff" }} />
            {!compact && <div style={{ fontSize: 21, fontWeight: 700, letterSpacing: "-0.02em" }}>Meridian</div>}
          </div>
          {nav.map(([ic, n], i) => (
            <div key={n} style={{ display: "flex", alignItems: "center", gap: 14, padding: "11px 12px", borderRadius: 9, justifyContent: compact ? "center" : "flex-start", background: i === 0 ? "rgba(255,255,255,0.09)" : "transparent", color: i === 0 ? "#fff" : ui.dim, fontSize: 16 }}>
              <Icon name={ic} size={19} color={i === 0 ? "#fff" : ui.faint} />
              {!compact && n}
            </div>
          ))}
        </div>
        <div style={{ flex: 1, padding: "28px 40px", display: "flex", flexDirection: "column", gap: 22, minWidth: 0 }}>
          <div style={{ display: "flex", alignItems: "center" }}>
            <div>
              <div style={{ fontSize: 32, fontWeight: 700, letterSpacing: "-0.025em" }}>Overview</div>
              <div style={{ fontSize: 15, color: ui.faint, marginTop: 4 }}>Sample data</div>
            </div>
            <div style={{ marginLeft: "auto", display: "flex", alignItems: "center", gap: 10, padding: "9px 16px", borderRadius: 10, border: `1px solid ${ui.line2}`, color: ui.faint, fontSize: 15, width: compact ? 0 : 260, overflow: "hidden" }}>
              <Icon name="search" size={16} color={ui.faint} />
              {!compact && "Search"}
            </div>
          </div>
          <div style={{ display: "flex", gap: 16 }}>
            {stats.slice(0, compact ? 2 : 3).map(([n, v], i) => (
              <div key={n as string} style={{ flex: 1, padding: "18px 20px", borderRadius: 14, background: theme.glass.fill, border: `1px solid ${ui.line2}` }}>
                <Label>{n as string}</Label>
                <div style={{ fontSize: 44, fontWeight: 700, letterSpacing: "-0.03em", marginTop: 10, fontVariantNumeric: "tabular-nums" }}>
                  {Math.round((v as number) * interpolate(f, [4 + i * 5, 46 + i * 5], [0, 1], { easing: theme.ease.out, extrapolateLeft: "clamp", extrapolateRight: "clamp" }))}
                </div>
              </div>
            ))}
          </div>
          <div style={{ display: "flex", gap: 16, flex: 1, minHeight: 0 }}>
            <div style={{ width: chartW + 40, padding: 20, borderRadius: 14, background: ui.panel, border: `1px solid ${ui.line}` }}>
              <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 14 }}>
                <div style={{ fontSize: 18, fontWeight: 600 }}>Throughput</div>
                <div style={{ fontFamily: theme.fonts.mono, fontSize: 13, color: ui.faint }}>12 WEEKS</div>
              </div>
              <svg width={chartW} height={chartH + 10} style={{ overflow: "visible" }}>
                {[0, 1, 2, 3].map((i) => (
                  <line key={i} x1={0} x2={chartW} y1={(chartH / 3) * i} y2={(chartH / 3) * i} stroke={ui.line} strokeWidth={1} />
                ))}
                <path d={`${path(SERIES_A, chartW, chartH, p)} L${chartW * p},${chartH} L0,${chartH} Z`} fill="rgba(255,255,255,0.07)" />
                <path d={path(SERIES_B, chartW, chartH, p)} stroke="rgba(255,255,255,0.35)" strokeWidth={2} fill="none" strokeDasharray="6 6" />
                <path d={path(SERIES_A, chartW, chartH, p)} stroke="#fff" strokeWidth={2.5} fill="none" />
              </svg>
            </div>
            {!compact && (
              <div style={{ flex: 1, padding: 20, borderRadius: 14, background: ui.panel, border: `1px solid ${ui.line}` }}>
                <div style={{ fontSize: 18, fontWeight: 600, marginBottom: 14 }}>Recent</div>
                {["Release notes", "Design review", "Onboarding flow", "Billing audit"].map((t, i) => (
                  <div key={t} style={{ display: "flex", alignItems: "center", padding: "11px 0", borderTop: i ? `1px solid ${ui.line}` : "none", fontSize: 15, color: ui.dim }}>
                    <div style={{ width: 8, height: 8, borderRadius: 4, background: i === 0 ? "#fff" : ui.ghost, marginRight: 12 }} />
                    {t}
                    <span style={{ marginLeft: "auto", fontFamily: theme.fonts.mono, fontSize: 12, color: ui.faint }}>{["2h", "5h", "1d", "2d"][i]}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      </div>
    </WindowFrame>
  );
};
