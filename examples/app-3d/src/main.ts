// A minimal Rendrix 3D application: a desc-based scene on a <canvas>, repainted via rAF.
// Nothing here runs in a browser.
import type { RendrixSceneDesc } from "rendrix";

const canvas = document.getElementById("scene") as HTMLCanvasElement;
const status = document.getElementById("status") as HTMLParagraphElement;

// Primary API: a desc built with the Rendrix3D helper, hosted via RendrixScene.
const desc: RendrixSceneDesc = Rendrix3D.createBoxScene({
  color: [0.25, 0.55, 1, 1],
  fovY: 40,
  camPos: [0, 0, 6],
  size: 2.2,
});
const scene = RendrixScene.createScene(canvas, desc);

function frame(t: number): void {
  const angle = ((t / 1000) * 45) % 360;
  // Push the new rotation; the scene re-renders into the canvas bitmap on the next frame.
  const current = scene.desc;
  const mesh = current.meshes?.[0];
  if (mesh?.transform) {
    mesh.transform.rotEulerDeg = [25, angle, 0] as [number, number, number];
    scene.update(current);
  }
  status.textContent = `cube ${angle.toFixed(0)}° · ${scene.desc.meshes?.length ?? 0} mesh`;
  requestAnimationFrame(frame);
}

requestAnimationFrame(frame);
console.log("3d app mounted");
