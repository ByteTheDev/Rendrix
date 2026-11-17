// Original sound design for the Rendrix launch video.
// Fully synthesized, deterministic, no samples: safe to ship anywhere.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const TL = JSON.parse(fs.readFileSync(path.join(here, "../src/timeline.json"), "utf8"));
const SR = 44100;
const N = Math.floor(TL.total * SR);
const dry = [new Float32Array(N), new Float32Array(N)];
const wet = [new Float32Array(N), new Float32Array(N)]; // reverb send
const echo = new Float32Array(N); // delay send (mono)

// deterministic RNG
let seed = 1337;
const rnd = () => ((seed = (seed * 1664525 + 1013904223) >>> 0) / 4294967296) * 2 - 1;
const prand = (() => { let s = 99; return () => ((s = (s * 1103515245 + 12345) >>> 0) / 4294967296); })();

const clamp01 = (x) => Math.max(0, Math.min(1, x));
const ease = (x) => { x = clamp01(x); return x * x * (3 - 2 * x); };
const lerp = (a, b, t) => a + (b - a) * t;
const mtof = (m) => 440 * Math.pow(2, (m - 69) / 12);

const lp = () => ({ y: 0 });
const lpStep = (s, x, a) => (s.y += a * (x - s.y));
const cutoffA = (f) => 1 - Math.exp((-2 * Math.PI * f) / SR);

// ---- harmony: Am | F | C | G, 8 seconds each (4 bars at 120 bpm) ----
const prog = [
  { root: 33, notes: [57, 60, 64, 67] }, // Am
  { root: 29, notes: [57, 60, 65, 69] }, // F
  { root: 36, notes: [55, 60, 64, 67] }, // C
  { root: 31, notes: [55, 59, 62, 67] }, // G
];
const chordAt = (t) => prog[Math.floor(Math.max(0, t) / 8) % 4];

// ---- master intensity curve (0..1) following the brief ----
const energy = (t) => {
  if (t < 6) return lerp(0.05, 0.14, t / 6);
  if (t < 14) return lerp(0.14, 0.34, (t - 6) / 8);
  if (t < 21) return 0.55;
  if (t < 34) return lerp(0.55, 0.75, (t - 21) / 13);
  if (t < 46) return lerp(0.8, 1.0, (t - 34) / 12);
  if (t < 55) return lerp(0.45, 0.3, (t - 46) / 9);
  return 0.5;
};

// ---- PAD: detuned saws through a rising lowpass ----
{
  const fL = [lp(), lp()], fR = [lp(), lp()];
  const phases = Array.from({ length: 16 }, () => prand());
  for (let i = 0; i < N; i++) {
    const t = i / SR;
    const ch = chordAt(t < 14 ? 0 : t);
    const e = energy(t);
    let g = e * 0.11 * ease(t / 5);
    if (t > 55) g *= 1 - ease((t - 56) / 7);
    if (g < 1e-5) continue;
    const cut = 160 + 1700 * Math.pow(e, 1.6) + 250 * Math.sin(t * 0.37);
    const a = cutoffA(cut);
    let l = 0, r = 0, k = 0;
    for (const n of ch.notes) {
      for (const d of [-0.12, 0.1]) {
        const f = mtof(n) * Math.pow(2, d / 12);
        phases[k] = (phases[k] + f / SR) % 1;
        const s = phases[k] * 2 - 1;
        if (k % 2) r += s; else l += s;
        k++;
      }
    }
    const L = lpStep(fL[1], lpStep(fL[0], l, a), a) * g;
    const R = lpStep(fR[1], lpStep(fR[0], r, a), a) * g;
    dry[0][i] += L; dry[1][i] += R;
    wet[0][i] += L * 0.5; wet[1][i] += R * 0.5;
  }
}

// ---- SUB BASS: sustained, side-chain pumping once the showcase begins ----
{
  let ph = 0;
  const beat = 60 / TL.bpm;
  for (let i = 0; i < N; i++) {
    const t = i / SR;
    if (t < 13.6 || t > 56) continue;
    let g = 0.34 * ease((t - 13.8) / 0.4);
    if (t > 54.6) g *= 1 - ease((t - 54.6) / 1.4);
    if (t < 14) g *= 0.4;
    if (t > 34 && t < 46) g *= 0.65 + 0.35 * Math.pow((t % beat) / beat, 0.6);
    if (t > 46 && t < 55) g *= 0.55;
    const ch = chordAt(t);
    ph = (ph + mtof(ch.root) / SR) % 1;
    const s = Math.sin(2 * Math.PI * ph) + 0.2 * Math.sin(4 * Math.PI * ph);
    dry[0][i] += s * g * 0.6; dry[1][i] += s * g * 0.6;
  }
}

// ---- ARP: soft plucks from the "under the hood" scene onward ----
{
  const step = 60 / TL.bpm / 2;
  const pattern = [0, 2, 1, 3, 2, 1, 3, 2];
  for (let t = 21; t < 46; t += step) {
    const idx = Math.round((t - 21) / step);
    const ch = chordAt(t);
    const oct = idx % 8 === 7 ? 24 : 12;
    const note = ch.notes[pattern[idx % 8]] + oct;
    const e = energy(t);
    const vol = 0.028 + 0.05 * clamp01((t - 21) / 25) + (t > 34 ? 0.018 : 0);
    const f = mtof(note);
    const dur = 0.32, st = Math.floor(t * SR);
    const s1 = lp(), s2 = lp();
    let ph = 0;
    const pan = Math.sin(idx * 1.7) * 0.5;
    for (let k = 0; k < dur * SR; k++) {
      const i = st + k; if (i >= N) break;
      const tt = k / SR;
      ph = (ph + f / SR) % 1;
      const saw = ph * 2 - 1;
      const sq = ph < 0.5 ? 1 : -1;
      const a = cutoffA(500 + 4200 * e * Math.exp(-tt * 14));
      const sig = lpStep(s2, lpStep(s1, saw * 0.6 + sq * 0.4, a), a) * Math.exp(-tt * 9) * vol;
      const l = sig * (pan <= 0 ? 1 : 1 - pan), r = sig * (pan >= 0 ? 1 : 1 + pan);
      dry[0][i] += l; dry[1][i] += r;
      wet[0][i] += l * 0.35; wet[1][i] += r * 0.35;
      echo[i] += sig * 0.45;
    }
  }
}

// ---- DRUMS: restrained four-on-the-floor during the showcase ----
{
  const beat = 60 / TL.bpm;
  for (let t = 34; t < 46; t += beat) {
    const st = Math.floor(t * SR);
    let ph = 0;
    for (let k = 0; k < 0.4 * SR; k++) {
      const i = st + k; if (i >= N) break;
      const tt = k / SR;
      const f = 45 + 110 * Math.exp(-tt * 28);
      ph += (2 * Math.PI * f) / SR;
      const s = Math.sin(ph) * Math.exp(-tt * 9) * 0.34 * (1 + (tt < 0.006 ? 0.8 : 0));
      dry[0][i] += s; dry[1][i] += s;
    }
    const h = Math.floor((t + beat / 2) * SR);
    const hp = lp();
    for (let k = 0; k < 0.06 * SR; k++) {
      const i = h + k; if (i >= N) break;
      const n = rnd(); const hpv = n - lpStep(hp, n, 0.35);
      const s = hpv * Math.exp((-k / SR) * 70) * 0.05;
      dry[0][i] += s * 0.9; dry[1][i] += s;
      wet[0][i] += s * 0.2; wet[1][i] += s * 0.2;
    }
  }
}

// ---- SWELLS: tonal+noise risers, kept subtle ----
for (const [a, b] of TL.cues.swells) {
  const st = Math.floor(a * SR), len = Math.floor((b - a) * SR);
  const s1 = lp(), s2 = lp();
  let ph = 0;
  for (let k = 0; k < len; k++) {
    const i = st + k; if (i >= N) break;
    const p = k / len;
    const g = Math.pow(p, 2.4) * 0.11;
    const cut = 300 + 5200 * p * p;
    const n = lpStep(s2, lpStep(s1, rnd(), cutoffA(cut)), cutoffA(cut));
    ph += (2 * Math.PI * (110 + 220 * p)) / SR;
    const s = (n * 1.8 + Math.sin(ph) * 0.35) * g;
    dry[0][i] += s; dry[1][i] += s * 0.95;
    wet[0][i] += s * 0.4; wet[1][i] += s * 0.4;
  }
}

// ---- HITS: sub drop + body + air tail ----
for (const t of TL.cues.hits) {
  const big = t > 50 ? 0.85 : 1;
  const st = Math.floor(t * SR);
  let ph = 0;
  const air = lp();
  for (let k = 0; k < 4.0 * SR; k++) {
    const i = st + k; if (i >= N) break;
    const tt = k / SR;
    const f = 28 + 90 * Math.exp(-tt * 6);
    ph += (2 * Math.PI * f) / SR;
    const body = Math.sin(ph) * Math.exp(-tt * 1.1) * 0.7;
    const click = lpStep(air, rnd(), cutoffA(1400)) * Math.exp(-tt * 14) * 0.4;
    const s = (body + click) * big;
    dry[0][i] += s; dry[1][i] += s;
    wet[0][i] += s * 0.55; wet[1][i] += s * 0.55;
  }
}

// ---- THUDS (Chromium. Electron. WebViews.): muted, low, dry ----
for (const t of TL.cues.thuds) {
  const st = Math.floor(t * SR);
  let ph = 0;
  for (let k = 0; k < 0.5 * SR; k++) {
    const i = st + k; if (i >= N) break;
    const tt = k / SR;
    ph += (2 * Math.PI * (52 + 50 * Math.exp(-tt * 20))) / SR;
    const s = Math.sin(ph) * Math.exp(-tt * 8) * 0.42;
    dry[0][i] += s; dry[1][i] += s;
    wet[0][i] += s * 0.2; wet[1][i] += s * 0.2;
  }
}

// ---- TYPOGRAPHY TICKS: tiny, glassy, never a notification chime ----
TL.cues.ticks.forEach((t, n) => {
  const st = Math.floor(t * SR);
  const f = [1760, 2093, 2637][n % 3] * 0.5;
  const hp = lp();
  const pan = ((n * 37) % 11) / 11 - 0.5;
  for (let k = 0; k < 0.09 * SR; k++) {
    const i = st + k; if (i >= N) break;
    const tt = k / SR;
    const nz = rnd(); const h = nz - lpStep(hp, nz, 0.2);
    const s = (Math.sin(2 * Math.PI * f * tt) * 0.5 + h * 0.5) * Math.exp(-tt * 70) * 0.075;
    dry[0][i] += s * (1 - Math.max(0, pan)); dry[1][i] += s * (1 + Math.min(0, pan));
    wet[0][i] += s * 0.5; wet[1][i] += s * 0.5;
    echo[i] += s * 0.25;
  }
});

// ---- INTERFACE CUTS: soft low pluck + air as each showcase shot arrives ----
for (const t of TL.cues.cuts) {
  const st = Math.floor(t * SR);
  const s1 = lp();
  for (let k = 0; k < 0.5 * SR; k++) {
    const i = st + k; if (i >= N) break;
    const tt = k / SR;
    const n = lpStep(s1, rnd(), cutoffA(900 + 2500 * Math.exp(-tt * 12)));
    const s = n * Math.exp(-tt * 9) * 0.1 + Math.sin(2 * Math.PI * 196 * tt) * Math.exp(-tt * 12) * 0.05;
    dry[0][i] += s; dry[1][i] += s;
    wet[0][i] += s * 0.5; wet[1][i] += s * 0.5;
  }
}

// ---- echo bus (dotted-eighth feedback delay) ----
{
  const d = Math.floor((60 / TL.bpm) * 0.75 * SR);
  const buf = new Float32Array(N);
  for (let i = 0; i < N; i++) {
    buf[i] = echo[i] + (i >= d ? buf[i - d] * 0.45 : 0);
    dry[0][i] += buf[i] * 0.8; dry[1][i] += buf[i] * 0.8;
    wet[0][i] += buf[i] * 0.3; wet[1][i] += buf[i] * 0.3;
  }
}

// ---- Schroeder-ish reverb ----
function reverb(input, delays, fb) {
  const out = new Float32Array(N);
  for (const dl of delays) {
    const buf = new Float32Array(dl);
    let p = 0, damp = 0;
    for (let i = 0; i < N; i++) {
      const y = buf[p];
      damp += 0.35 * (y - damp);
      buf[p] = input[i] + damp * fb;
      out[i] += y;
      p = (p + 1) % dl;
    }
  }
  for (const dl of [556, 441]) {
    const buf = new Float32Array(dl); let p = 0;
    for (let i = 0; i < N; i++) {
      const y = buf[p]; const x = out[i] + y * 0.5;
      buf[p] = x; out[i] = y - x * 0.5; p = (p + 1) % dl;
    }
  }
  return out;
}
const revL = reverb(wet[0], [1687, 1931, 2203, 2539, 3011], 0.86);
const revR = reverb(wet[1], [1733, 1987, 2251, 2593, 3067], 0.86);

// ---- master ----
const pcm = Buffer.alloc(N * 4);
let peak = 0;
const mix = [new Float32Array(N), new Float32Array(N)];
for (let i = 0; i < N; i++) {
  const t = i / SR;
  const fadeIn = ease(t / 2.5);
  const fadeOut = 1 - ease((t - (TL.total - 2.5)) / 2.5);
  mix[0][i] = (dry[0][i] + revL[i] * 0.16) * fadeIn * fadeOut;
  mix[1][i] = (dry[1][i] + revR[i] * 0.16) * fadeIn * fadeOut;
  peak = Math.max(peak, Math.abs(mix[0][i]), Math.abs(mix[1][i]));
}
const norm = 0.82 / peak;
for (let i = 0; i < N; i++) {
  for (let c = 0; c < 2; c++) {
    const v = Math.tanh(mix[c][i] * norm * 1.1) * 0.95;
    pcm.writeInt16LE(Math.round(v * 32767), i * 4 + c * 2);
  }
}
const header = Buffer.alloc(44);
header.write("RIFF", 0); header.writeUInt32LE(36 + pcm.length, 4); header.write("WAVE", 8);
header.write("fmt ", 12); header.writeUInt32LE(16, 16); header.writeUInt16LE(1, 20);
header.writeUInt16LE(2, 22); header.writeUInt32LE(SR, 24); header.writeUInt32LE(SR * 4, 28);
header.writeUInt16LE(4, 32); header.writeUInt16LE(16, 34); header.write("data", 36);
header.writeUInt32LE(pcm.length, 40);
const outFile = path.join(here, "../public/audio/rendrix-score.wav");
fs.writeFileSync(outFile, Buffer.concat([header, pcm]));
console.log("wrote", outFile, (pcm.length / 1e6).toFixed(1) + " MB, peak", peak.toFixed(3));
