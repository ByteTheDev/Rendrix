import React from "react";
import { AbsoluteFill, interpolate, useCurrentFrame } from "remotion";
import { SceneFade } from "../components/SceneFade";
import { Words } from "../components/Type";
import { hold } from "../components/Backdrop";
import { theme, sec } from "../theme";
import { useLayout } from "../layout";

/** Scene 6: let the type breathe. A browser frame appears, then the type walks past it. */
export const Philosophy: React.FC = () => {
  const f = useCurrentFrame();
  const { pick, w, h } = useLayout();
  const size = pick(132, 116);

  // browser-window outline that appears with line 2 and is outgrown at "BEYOND"
  const appear = hold(f, sec(4.3), sec(5.1));
  const burst = interpolate(f, [sec(6.2), sec(7.6)], [0, 1], { easing: theme.ease.inOut, extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  const fw = pick(780, 640), fh = pick(440, 420);
  const scale = 1 + burst * 3.2;
  const frameOpacity = appear * (1 - hold(f, sec(7.0), sec(7.8))) * 0.5;

  return (
    <SceneFade inFrames={sec(0.6)} outFrames={sec(0.5)}>
      <AbsoluteFill style={{ alignItems: "center", justifyContent: "center" }}>
        {/* thin horizon light */}
        <div
          style={{
            position: "absolute",
            top: "50%",
            width: w * 0.8 * hold(f, sec(0.2), sec(1.8)),
            height: 1,
            background: "linear-gradient(90deg, transparent, rgba(255,255,255,0.28), transparent)",
            opacity: 0.6 * (1 - hold(f, sec(3.2), sec(3.9))),
          }}
        />
        {/* browser outline */}
        <div
          style={{
            position: "absolute",
            width: fw,
            height: fh,
            borderRadius: 18,
            border: "1.5px solid rgba(255,255,255,0.8)",
            opacity: frameOpacity,
            transform: `translateY(${(1 - appear) * 30}px) scale(${scale})`,
          }}
        >
          <div style={{ height: 44, borderBottom: "1.5px solid rgba(255,255,255,0.8)", display: "flex", alignItems: "center", gap: 8, padding: "0 16px" }}>
            {[0, 1, 2].map((i) => (
              <div key={i} style={{ width: 11, height: 11, borderRadius: 6, border: "1.5px solid rgba(255,255,255,0.8)" }} />
            ))}
            <div style={{ marginLeft: 18, flex: 1, height: 18, borderRadius: 9, border: "1.5px solid rgba(255,255,255,0.5)", maxWidth: fw * 0.5 }} />
          </div>
        </div>

        {/* line 1 */}
        <div style={{ position: "absolute", display: "flex", flexDirection: "column", alignItems: "center" }}>
          <Words
            text="TYPE LESS."
            at={sec(0.9)}
            per={sec(0.35)}
            size={size}
            weight={800}
            tracking={0.01}
            exitAt={sec(3.9)}
            color={theme.colors.dim}
          />
          <Words
            text="BUILD MORE."
            at={sec(1.8)}
            per={sec(0.35)}
            size={size}
            weight={800}
            tracking={0.01}
            exitAt={sec(3.9)}
          />
        </div>

        {/* line 2 */}
        <div style={{ position: "absolute", display: "flex", flexDirection: "column", alignItems: "center" }}>
          <Words
            text="THINK"
            at={sec(4.6)}
            per={sec(0.3)}
            size={size}
            weight={800}
            tracking={0.01}
            color={theme.colors.dim}
          />
          <Words
            text="BEYOND THE BROWSER."
            at={sec(5.3)}
            per={sec(0.3)}
            size={pick(size, size * 0.86)}
            weight={800}
            tracking={0.01}
            maxWidth={pick(1700, 960)}
          />
        </div>
      </AbsoluteFill>
      {h > 0 && null}
    </SceneFade>
  );
};
