import React from "react";
import { theme } from "../theme";
import { Icon, Label, MockProps, WindowFrame, ui } from "./ui";

const CODE = `import { render } from "rendrix/jsx-runtime";

interface Track {
  title: string;
  artist: string;
  seconds: number;
}

// State lives in plain TypeScript.
const library: Track[] = await loadLibrary();
let current = 0;

function Player({ track }: { track: Track }) {
  return (
    <section class="player">
      <h1>{track.title}</h1>
      <p class="artist">{track.artist}</p>
      <Waveform progress={0.42} />
    </section>
  );
}

function next() {
  current = (current + 1) % library.length;
  render(<Player track={library[current]} />, document.body);
}

render(<Player track={library[current]} />, document.body);`;

const KEYWORDS = /\b(import|from|const|let|function|return|interface|await|string|number|export)\b/;
const tokenize = (line: string): { t: string; c: string; w?: number; i?: boolean }[] => {
  if (line.trim().startsWith("//")) return [{ t: line, c: ui.ghost, i: true }];
  const out: { t: string; c: string; w?: number; i?: boolean }[] = [];
  const re = /("[^"]*")|(<\/?[A-Za-z0-9]+)|(\b(?:import|from|const|let|function|return|interface|await|string|number|export)\b)|(\b[A-Z][A-Za-z]+\b)|(\b\d+(?:\.\d+)?\b)|([^"<A-Za-z0-9]+|[A-Za-z0-9]+|.)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(line))) {
    if (m[1]) out.push({ t: m[1], c: "#d4d4d8", i: true });
    else if (m[2]) out.push({ t: m[2], c: "#ffffff", w: 600 });
    else if (m[3] && KEYWORDS.test(m[3])) out.push({ t: m[3], c: "#ffffff", w: 700 });
    else if (m[4]) out.push({ t: m[4], c: "#e4e4e7", w: 500 });
    else if (m[5]) out.push({ t: m[5], c: "#ffffff" });
    else out.push({ t: m[0], c: ui.dim });
  }
  return out;
};

export const CodeEditor: React.FC<MockProps> = ({ width, height, f, compact }) => {
  const sideW = compact ? 0 : 250;
  const miniW = compact ? 0 : 120;
  const chars = Math.floor(330 + f * 6);
  const shown = CODE.slice(0, chars);
  const lines = CODE.split("\n");
  const shownLines = shown.split("\n");
  const cursorLine = shownLines.length - 1;
  const lineH = 29;
  const fontSize = 19;
  const visibleLines = Math.floor((height - 42 - 41 - 30) / lineH);
  const scroll = Math.max(0, cursorLine - (visibleLines - 3)) * lineH;
  const blink = Math.floor(f / 15) % 2 === 0;

  return (
    <WindowFrame width={width} height={height} title="player.tsx — music-app" right={<Icon name="gear" size={16} color={ui.faint} />}>
      <div style={{ display: "flex", flex: 1, minHeight: 0 }}>
        {!compact && (
          <div style={{ width: sideW, background: ui.panel, borderRight: `1px solid ${ui.line}`, padding: 16 }}>
            <Label>Explorer</Label>
            <div style={{ marginTop: 14, fontSize: 15, display: "flex", flexDirection: "column", gap: 2 }}>
              {[
                ["music-app", 0, true],
                ["src", 1, true],
                ["player.tsx", 2, false, true],
                ["waveform.tsx", 2],
                ["library.ts", 2],
                ["styles.css", 2],
                ["rendrix.json", 1],
                ["tsconfig.json", 1],
              ].map(([n, d, dir, act]) => (
                <div key={n as string} style={{ display: "flex", alignItems: "center", gap: 9, padding: "6px 10px", marginLeft: (d as number) * 14, borderRadius: 6, background: act ? "rgba(255,255,255,0.09)" : "transparent", color: act ? "#fff" : ui.dim }}>
                  <Icon name={dir ? "folder" : "file"} size={15} color={act ? "#fff" : ui.faint} />
                  {n as string}
                </div>
              ))}
            </div>
          </div>
        )}
        <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column" }}>
          <div style={{ height: 41, flex: "none", display: "flex", background: ui.panel, borderBottom: `1px solid ${ui.line}` }}>
            {["player.tsx", "waveform.tsx", "library.ts"].map((n, i) => (
              <div key={n} style={{ padding: "0 22px", display: "flex", alignItems: "center", gap: 8, fontSize: 14, color: i === 0 ? "#fff" : ui.faint, background: i === 0 ? ui.bg : "transparent", borderRight: `1px solid ${ui.line}`, borderTop: i === 0 ? "1.5px solid #fff" : "1.5px solid transparent" }}>
                {n}
              </div>
            ))}
          </div>
          <div style={{ flex: 1, minHeight: 0, display: "flex", overflow: "hidden", position: "relative" }}>
            <div style={{ position: "absolute", inset: 0, transform: `translateY(${-scroll}px)`, padding: "14px 0" }}>
              {lines.map((ln, i) => {
                const typed = shownLines[i];
                if (typed === undefined) return <div key={i} style={{ height: lineH }} />;
                return (
                  <div key={i} style={{ display: "flex", height: lineH, alignItems: "center", background: i === cursorLine ? "rgba(255,255,255,0.04)" : "transparent" }}>
                    <div style={{ width: 62, textAlign: "right", paddingRight: 22, fontFamily: theme.fonts.mono, fontSize: 15, color: i === cursorLine ? ui.dim : ui.ghost }}>{i + 1}</div>
                    <div style={{ fontFamily: theme.fonts.mono, fontSize, whiteSpace: "pre", letterSpacing: "-0.01em" }}>
                      {tokenize(typed).map((tk, k) => (
                        <span key={k} style={{ color: tk.c, fontWeight: tk.w ?? 400, fontStyle: tk.i ? "italic" : "normal" }}>{tk.t}</span>
                      ))}
                      {i === cursorLine && <span style={{ display: "inline-block", width: 2, height: 23, background: "#fff", marginLeft: 1, verticalAlign: "middle", opacity: blink ? 1 : 0.15 }} />}
                    </div>
                  </div>
                );
              })}
            </div>
            {!compact && (
              <div style={{ width: miniW, flex: "none", marginLeft: "auto", borderLeft: `1px solid ${ui.line}`, background: ui.panel, padding: "14px 12px", position: "relative", zIndex: 1 }}>
                {lines.map((ln, i) => (
                  <div key={i} style={{ height: 4, marginBottom: 2.4, width: `${Math.min(100, ln.length * 2.4)}%`, marginLeft: ln.search(/\S|$/) * 3, borderRadius: 2, background: i <= cursorLine ? "rgba(255,255,255,0.32)" : "rgba(255,255,255,0.08)" }} />
                ))}
              </div>
            )}
          </div>
          <div style={{ height: 30, flex: "none", background: "#16161a", borderTop: `1px solid ${ui.line}`, display: "flex", alignItems: "center", padding: "0 16px", gap: 22, fontFamily: theme.fonts.mono, fontSize: 12.5, color: ui.faint }}>
            <span>TypeScript</span>
            <span>Ln {cursorLine + 1}, Col {(shownLines[cursorLine] ?? "").length + 1}</span>
            <span>No problems</span>
            <span style={{ marginLeft: "auto" }}>UTF-8</span>
          </div>
        </div>
      </div>
    </WindowFrame>
  );
};
