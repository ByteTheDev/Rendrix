import React from "react";
import { AbsoluteFill, Audio, Sequence, interpolate, staticFile, useCurrentFrame } from "remotion";
import { Backdrop, Finish } from "./components/Backdrop";
import { LayoutContext } from "./layout";
import { sceneRange, TOTAL_FRAMES } from "./theme";
import { Opening } from "./scenes/Opening";
import { Problem } from "./scenes/Problem";
import { Intro } from "./scenes/Intro";
import { Hood } from "./scenes/Hood";
import { Beyond } from "./scenes/Beyond";
import { Philosophy } from "./scenes/Philosophy";
import { Closing } from "./scenes/Closing";

const SCENES = [
  ["opening", Opening],
  ["problem", Problem],
  ["intro", Intro],
  ["hood", Hood],
  ["beyond", Beyond],
  ["philosophy", Philosophy],
  ["closing", Closing],
] as const;

export const Launch: React.FC<{ vertical?: boolean }> = ({ vertical = false }) => {
  const frame = useCurrentFrame();
  const layout = vertical ? { w: 1080, h: 1920, vertical: true } : { w: 1920, h: 1080, vertical: false };
  return (
    <LayoutContext.Provider value={layout}>
      <AbsoluteFill style={{ background: "#000" }}>
        <Backdrop />
        {SCENES.map(([key, Comp]) => {
          const r = sceneRange(key);
          return (
            <Sequence key={key} from={r.from} durationInFrames={r.duration} name={key}>
              <Comp />
            </Sequence>
          );
        })}
        <Finish />
        <Audio src={staticFile("audio/rendrix-score.wav")} volume={0.9} />
      </AbsoluteFill>
      {frame < 0 && null}
    </LayoutContext.Provider>
  );
};

void interpolate;
void TOTAL_FRAMES;
