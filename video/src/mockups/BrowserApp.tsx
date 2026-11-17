import React from "react";
import { interpolate, spring } from "remotion";
import { theme } from "../theme";
import { Icon, Label, MockProps, WindowFrame, ui } from "./ui";

const TABS = ["Rendrix Docs", "Compiler notes", "wgpu guide", "New tab"];

/** A browser interface, built as a vision piece. */
export const BrowserApp: React.FC<MockProps> = ({ width, height, f, compact }) => {
  const palette = spring({ frame: f - 34, fps: 30, config: theme.spring.smooth });
  const scroll = interpolate(f, [0, 90], [0, 90], { easing: theme.ease.inOut, extrapolateRight: "clamp" });
  const sideW = compact ? 0 : 280;
  return (
    <WindowFrame width={width} height={height} titlebar={false}>
      {/* tab strip */}
      <div style={{ height: 50, flex: "none", background: "#0e0e10", display: "flex", alignItems: "flex-end", padding: "0 14px", gap: 4, borderBottom: `1px solid ${ui.line}` }}>
        <div style={{ display: "flex", gap: 8, alignSelf: "center", marginRight: 16 }}>
          {[0, 1, 2].map((i) => <div key={i} style={{ width: 12, height: 12, borderRadius: 6, background: "#34343b" }} />)}
        </div>
        {TABS.slice(0, compact ? 3 : 4).map((n, i) => (
          <div key={n} style={{ height: 38, padding: "0 20px", display: "flex", alignItems: "center", gap: 10, fontSize: 14.5, borderRadius: "10px 10px 0 0", background: i === 0 ? ui.panel2 : "transparent", color: i === 0 ? "#fff" : ui.faint, minWidth: compact ? 150 : 190 }}>
            <div style={{ width: 14, height: 14, borderRadius: 4, background: i === 0 ? "#fff" : ui.ghost }} />
            {n}
          </div>
        ))}
        <div style={{ alignSelf: "center", marginLeft: 6 }}><Icon name="plus" size={16} color={ui.faint} /></div>
      </div>
      {/* toolbar */}
      <div style={{ height: 54, flex: "none", background: ui.panel2, display: "flex", alignItems: "center", gap: 14, padding: "0 18px", borderBottom: `1px solid ${ui.line}` }}>
        <Icon name="back" size={18} color={ui.dim} />
        <Icon name="forward" size={18} color={ui.ghost} />
        <Icon name="reload" size={18} color={ui.dim} />
        <div style={{ flex: 1, height: 34, borderRadius: 17, background: "#0b0b0d", border: `1px solid ${ui.line2}`, display: "flex", alignItems: "center", padding: "0 16px", gap: 10, fontSize: 15, color: ui.dim }}>
          <Icon name="lock" size={14} color={ui.faint} />
          <span style={{ color: ui.faint }}>rendrix.dev</span>
          <span>/docs/rendering</span>
          <span style={{ marginLeft: "auto" }}><Icon name="star" size={15} color={ui.faint} /></span>
        </div>
        <div style={{ width: 30, height: 30, borderRadius: 15, background: "linear-gradient(135deg, #fff, #52525b)" }} />
      </div>
      <div style={{ flex: 1, minHeight: 0, display: "flex", position: "relative", overflow: "hidden", background: ui.bg }}>
        {!compact && (
          <div style={{ width: sideW, borderRight: `1px solid ${ui.line}`, padding: "26px 22px", flex: "none" }}>
            <Label>Documentation</Label>
            {["Getting started", "Rendering", "Layout", "Styling", "Runtime", "Compiler"].map((n, i) => (
              <div key={n} style={{ padding: "10px 12px", marginTop: 6, borderRadius: 8, fontSize: 16, color: i === 1 ? "#fff" : ui.dim, background: i === 1 ? "rgba(255,255,255,0.08)" : "transparent" }}>{n}</div>
            ))}
          </div>
        )}
        <div style={{ flex: 1, minWidth: 0, overflow: "hidden", position: "relative" }}>
          <div style={{ padding: compact ? "34px 40px" : "44px 72px", transform: `translateY(${-scroll}px)` }}>
            <Label>Guide</Label>
            <div style={{ fontSize: compact ? 54 : 64, fontWeight: 800, letterSpacing: "-0.035em", lineHeight: 1.02, margin: "16px 0 20px" }}>How rendering works</div>
            <div style={{ fontSize: 21, lineHeight: 1.6, color: ui.dim, maxWidth: 760 }}>
              Markup becomes a DOM, styles resolve into a layout tree, and the engine draws every frame on the GPU. Nothing in the path is a web view.
            </div>
            <div style={{ display: "flex", gap: 16, marginTop: 34 }}>
              {["Parse", "Style", "Layout", "Paint"].slice(0, compact ? 3 : 4).map((n, i) => (
                <div key={n} style={{ flex: 1, padding: 20, borderRadius: 14, background: theme.glass.fill, border: `1px solid ${ui.line2}` }}>
                  <div style={{ fontFamily: theme.fonts.mono, fontSize: 13, color: ui.faint }}>0{i + 1}</div>
                  <div style={{ fontSize: 24, fontWeight: 650, marginTop: 36 }}>{n}</div>
                </div>
              ))}
            </div>
            {[0.92, 0.78, 0.86, 0.5].map((wd, i) => (
              <div key={i} style={{ height: 10, borderRadius: 5, background: "rgba(255,255,255,0.08)", width: `${wd * 70}%`, marginTop: i === 0 ? 40 : 14 }} />
            ))}
          </div>
          {/* command palette */}
          <div style={{ position: "absolute", left: "50%", top: 70, width: compact ? width - 120 : 640, marginLeft: compact ? -(width - 120) / 2 : -320, opacity: palette, transform: `translateY(${(1 - palette) * -18}px) scale(${0.96 + palette * 0.04})`, borderRadius: 16, background: "#17171b", border: "1px solid rgba(255,255,255,0.18)", boxShadow: "0 40px 100px rgba(0,0,0,0.8)", overflow: "hidden" }}>
            <div style={{ display: "flex", alignItems: "center", gap: 12, padding: "16px 20px", borderBottom: `1px solid ${ui.line2}`, fontSize: 18 }}>
              <Icon name="search" size={18} color={ui.dim} />
              layout<span style={{ width: 2, height: 22, background: "#fff", opacity: Math.floor(f / 15) % 2 ? 0.2 : 1 }} />
            </div>
            {[["Layout", "rendrix.dev/docs/layout"], ["Flexbox engine notes", "Tab: Compiler notes"], ["Open DevTools", "Command"]].map(([a, b], i) => (
              <div key={a} style={{ display: "flex", padding: "13px 20px", fontSize: 16, background: i === 0 ? "rgba(255,255,255,0.08)" : "transparent", color: i === 0 ? "#fff" : ui.dim }}>
                {a}
                <span style={{ marginLeft: "auto", fontFamily: theme.fonts.mono, fontSize: 12.5, color: ui.faint }}>{b}</span>
              </div>
            ))}
          </div>
        </div>
      </div>
    </WindowFrame>
  );
};
