import React from "react";
import { AbsoluteFill, interpolate, useCurrentFrame } from "remotion";
import { SceneFade } from "../components/SceneFade";
import { Words } from "../components/Type";
import { hold } from "../components/Backdrop";
import { theme, sec } from "../theme";
import { useLayout } from "../layout";

/** Scene 1: pure black, one hairline, two deliberate lines. */
export const Opening: React.FC = () => {
  const f = useCurrentFrame();
  const { pick } = useLayout();
  const size1 = pick(50, 48);
  const size2 = pick(112, 100);
  const lineW = interpolate(f, [sec(0.3), sec(1.8)], [0, 1], {
    easing: theme.ease.inOut,
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  const dimLine1 = hold(f, sec(3.8), sec(4.6));
  return (
    <SceneFade noIn outFrames={sec(0.5)}>
      <AbsoluteFill style={{ alignItems: "center", justifyContent: "center" }}>
        <div style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: pick(44, 56) }}>
          {/* hairline that opens the shot */}
          <div
            style={{
              width: pick(220, 180) * lineW,
              height: 1,
              background: "linear-gradient(90deg, transparent, rgba(255,255,255,0.7), transparent)",
              opacity: 1 - hold(f, sec(2.4), sec(3.2)) * 0.8,
            }}
          />
          <Words
            text="WHAT IF YOUR NEXT APPLICATION..."
            at={sec(1.0)}
            per={sec(0.25)}
            size={size1}
            weight={500}
            tracking={0.16}
            color={dimLine1 > 0 ? theme.colors.faint : theme.colors.dim}
            maxWidth={pick(1500, 900)}
            lineHeight={1.35}
            style={{ opacity: interpolate(dimLine1, [0, 1], [1, 0.55]) }}
            rise={size1 * 0.4}
          />
          <Words
            text="...WAS TRULY NATIVE?"
            at={sec(3.9)}
            per={sec(0.25)}
            size={size2}
            weight={800}
            tracking={0.02}
            maxWidth={pick(1700, 900)}
            accent={{ 2: theme.colors.text }}
            lineHeight={1.12}
          />
        </div>
      </AbsoluteFill>
    </SceneFade>
  );
};
