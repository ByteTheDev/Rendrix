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

// The same desc API on a GPU-backed host: depth-tested, multi-light, alpha-blended,
// culled meshes rendered by the wgpu forward pass instead of the CPU rasterizer.
// A small procedural texture, uploaded through the public API and sampled with
// the primitive's UVs. Checker squares survive bilinear filtering at this size.
const texSize = 8;
const checker = new Uint8Array(texSize * texSize * 4);
for (let y = 0; y < texSize; y++) {
  for (let x = 0; x < texSize; x++) {
    const i = (y * texSize + x) * 4;
    const c = ((x >> 1) + (y >> 1)) % 2 === 0 ? [255, 205, 110, 255] : [35, 85, 165, 255];
    checker.set(c, i);
  }
}
RendrixScene.setTexture(1, checker, texSize, texSize);

const gpuCanvas = document.getElementById("scene-gpu") as HTMLCanvasElement;
const gpuDesc: RendrixSceneDesc = {
  camera: { fovY: 45, position: [0, 0, 5], lookAt: [0, 0, 0] },
  meshes: [
    {
      geo: { kind: "box", size: 1.3 },
      material: { color: [0.3, 0.7, 1, 1], metal: 0.1, rough: 0.4, cull: "back" },
      transform: { pos: [-0.9, 0, 0] },
    },
    {
      geo: { kind: "sphere", size: 0.65 },
      material: { color: [1, 1, 1, 0.9], textureId: 1 },
      transform: { pos: [0.9, 0.3, 0] },
    },
  ],
  lights: [
    { kind: "dir", dir: [0.3, -0.6, -1], color: [1, 1, 1], intensity: 1.1 },
    { kind: "point", pos: [2, 1, 3], color: [0.4, 0.6, 1], intensity: 1.5, range: 8 },
    { kind: "ambient", color: [0.2, 0.2, 0.25], intensity: 1 },
  ],
};
const gpuScene = RendrixScene.createScene(gpuCanvas, gpuDesc);

function frame(t: number): void {
  const angle = ((t / 1000) * 45) % 360;
  // Push the new rotation; the scene re-renders into the canvas bitmap on the next frame.
  const current = scene.desc;
  const mesh = current.meshes?.[0];
  if (mesh?.transform) {
    mesh.transform.rotEulerDeg = [25, angle, 0] as [number, number, number];
    scene.update(current);
  }
  const gpuCurrent = gpuScene.desc;
  const gpuMesh = gpuCurrent.meshes?.[0];
  if (gpuMesh?.transform) {
    gpuMesh.transform.rotEulerDeg = [15, -angle, 0] as [number, number, number];
    gpuScene.update(gpuCurrent);
  }
  status.textContent = `cube ${angle.toFixed(0)}° · ${scene.desc.meshes?.length ?? 0} CPU mesh · ${gpuCurrent.meshes?.length ?? 0} GPU mesh`;
  requestAnimationFrame(frame);
}

requestAnimationFrame(frame);
console.log("3d app mounted");
