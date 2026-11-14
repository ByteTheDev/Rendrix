/** The `rendrix` module: application, window, clipboard and (permission-gated) filesystem APIs. */
export const version: string;
export function quit(): void;
export const window: {
  setTitle(title: string): void;
  close(): void;
  resize(width: number, height: number): void;
};
export const clipboard: { readText(): Promise<string>; writeText(text: string): Promise<void> };
export const fs: {
  readTextFile(path: string): Promise<string>;
  writeTextFile(path: string, data: string): Promise<void>;
  exists(path: string): Promise<boolean>;
  readDir(path: string): Promise<string[]>;
};
declare const rendrix: { version: string; engine: "rendrix"; quit: typeof quit; window: typeof window; clipboard: typeof clipboard; fs: typeof fs };
export default rendrix;

/* ------------------------------------------------------------------ */
/* 3D scenes (v1 subset). Primary API: RendrixScene on <canvas>;       */
/* aliases: Rendrix3D desc helpers + a minimal WebGL-subset context.   */
/* See engine/crates/rendrix-js/src/scene3d.rs for the desc schema.    */
/* ------------------------------------------------------------------ */

/** Camera block of a 3D scene description. */
export interface RendrixSceneCamera {
  fovY?: number;
  near?: number;
  far?: number;
  /** World-space eye position. */
  position?: [number, number, number];
  /** World-space look-at target. */
  lookAt?: [number, number, number];
}

/** Geometry block. `size`: box `[x,y,z]`|scalar, plane `[w,h]`|scalar, sphere radius. */
export interface RendrixSceneGeo {
  kind: "box" | "cube" | "plane" | "quad" | "sphere" | "ball" | "tris" | "triangles";
  size?: number | [number] | [number, number] | [number, number, number];
  /** Triangle soup: flat `[x,y,z,…]` (multiple of 9) with optional index triples. */
  verts?: number[];
  indices?: number[];
}

export interface RendrixSceneMaterial {
  /** `[r,g,b]`/`[r,g,b,a]` in 0-1 (or 0-255) or a CSS color string. */
  color?: [number, number, number] | [number, number, number, number] | string;
  metal?: number;
  rough?: number;
  emissive?: number;
}

export interface RendrixSceneTransform {
  pos?: [number, number, number];
  rotEulerDeg?: [number, number, number];
  scale?: number | [number, number, number];
}

export interface RendrixSceneMesh {
  geo: RendrixSceneGeo;
  material?: RendrixSceneMaterial;
  transform?: RendrixSceneTransform;
}

export interface RendrixSceneLight {
  dir?: [number, number, number];
  direction?: [number, number, number];
}

/** Scene description shared with the GPU scene backend (forwards-compatible). */
export interface RendrixSceneDesc {
  camera?: RendrixSceneCamera;
  meshes?: RendrixSceneMesh[];
  lights?: RendrixSceneLight[];
  /** Transparent black by default; an opaque value wipes the canvas each upload. */
  background?: [number, number, number, number] | string;
}

export interface RendrixBoxOptions {
  size?: number | [number, number, number];
  color?: RendrixSceneMaterial["color"];
  pos?: [number, number, number];
  position?: [number, number, number];
  rot?: [number, number, number];
  rotEulerDeg?: [number, number, number];
  scale?: number | [number, number, number];
  emissive?: number;
}

export interface RendrixSceneHandle {
  readonly canvas: HTMLCanvasElement;
  readonly desc: RendrixSceneDesc;
  update(desc: RendrixSceneDesc): this;
  setCamera(cam: RendrixSceneCamera): this;
  addBox(opts?: RendrixBoxOptions): number;
  addPlane(opts?: RendrixBoxOptions): number;
  remove(): void;
}

export interface RendrixBoxSceneOptions {
  color?: [number, number, number] | [number, number, number, number];
  fovY?: number;
  camPos?: [number, number, number];
  size?: number | [number, number, number];
  angle?: [number, number, number];
}

declare global {
  /** Primary 3D API: desc-based scenes on `<canvas>` (and API-compatible `<rendrix-scene>` hosts). */
  const RendrixScene: {
    createScene(target: HTMLCanvasElement | Element | string, desc?: RendrixSceneDesc): RendrixSceneHandle;
    getScene(target: HTMLCanvasElement | Element | string): RendrixSceneDesc | null;
  };
  /** Convenience desc builders over the primary RendrixScene API. */
  const Rendrix3D: {
    createBoxScene(opts?: RendrixBoxSceneOptions): RendrixSceneDesc;
  };
  /**
   * Minimal WebGL-subset context (v1) returned by `canvas.getContext('webgl'|'webgl2')`:
   * clearColor/clear/enable(DEPTH_TEST)/viewport, buffer upload for triangle soup,
   * drawArrays(TRIANGLES), plus stubs that warn-and-ignore everything else. The value
   * is structurally assignable to the DOM `WebGL2RenderingContext` type; only the
   * members below are implemented at runtime (documented subset).
   */
  interface RendrixWebGLContext {
    readonly canvas: HTMLCanvasElement | null;
    readonly drawingBufferWidth: number;
    readonly drawingBufferHeight: number;
    clearColor(r: number, g: number, b: number, a: number): void;
    clear(mask: number): void;
    enable(cap: number): void;
    disable(cap: number): void;
    viewport(x: number, y: number, w: number, h: number): void;
    createBuffer(): object;
    bindBuffer(target: number, buf: object | null): void;
    bufferData(target: number, data: ArrayBufferView | ArrayBuffer | number[], usage: number): void;
    drawArrays(mode: number, first: number, count: number): void;
    getParameter(p: number): unknown;
    getError(): number;
  }
  interface HTMLElementTagNameMap {
    "rendrix-scene": HTMLElement;
  }
}
