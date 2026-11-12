// Notification sounds played when a turn finishes in the background.
// All original, synthesized with Web Audio (no bundled assets). WebKit/Chromium
// keep an AudioContext suspended until a user gesture, so `unlockChime` resumes
// it on the first pointer/key event; once running it keeps playing while the
// window is hidden or unfocused.

let ctx: AudioContext | null = null;

function getCtx(): AudioContext | null {
  if (ctx) return ctx;
  try {
    const AC = window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!AC) return null;
    ctx = new AC();
  } catch {
    ctx = null;
  }
  return ctx;
}

/** Resume the context from a user gesture so later background chimes play. */
export function unlockChime(): void {
  const c = getCtx();
  if (c && c.state !== "running") void c.resume().catch(() => {});
}

interface ToneOpts {
  type?: OscillatorType;
  /** Start pitch as a fraction of `freq`, ramped up/down over `glideMs`. */
  glideFrom?: number;
  glideMs?: number;
  /** Overtone as [frequency ratio, relative gain]. */
  partials?: [number, number][];
  attack?: number;
}

function tone(c: AudioContext, out: AudioNode, freq: number, start: number, dur: number, peak: number, o: ToneOpts = {}) {
  const voices: [number, number, number][] = [[1, 1, dur], ...(o.partials ?? []).map(([r, g]) => [r, g, dur * 0.6] as [number, number, number])];
  const attack = o.attack ?? 0.012;
  for (const [ratio, gain, len] of voices) {
    const osc = c.createOscillator();
    const g = c.createGain();
    osc.type = ratio === 1 ? (o.type ?? "sine") : "sine";
    const f = freq * ratio;
    if (o.glideFrom && ratio === 1) {
      osc.frequency.setValueAtTime(f * o.glideFrom, start);
      osc.frequency.exponentialRampToValueAtTime(f, start + (o.glideMs ?? 25) / 1000);
    } else {
      osc.frequency.setValueAtTime(f, start);
    }
    g.gain.setValueAtTime(0.0001, start);
    g.gain.exponentialRampToValueAtTime(Math.max(peak * gain, 0.0002), start + attack);
    g.gain.exponentialRampToValueAtTime(0.0001, start + len);
    osc.connect(g).connect(out);
    osc.start(start);
    osc.stop(start + len + 0.02);
  }
}

type Voice = (c: AudioContext, out: AudioNode, t: number) => number; // returns length in s

const bell: ToneOpts = { partials: [[2, 0.18]] };

const SOUNDS: Record<string, { label: string; play: Voice }> = {
  pop: {
    label: "Pop-ding",
    play: (c, out, t) => {
      tone(c, out, 987.77, t, 0.16, 0.9, { ...bell, glideFrom: 0.94 }); // B5
      tone(c, out, 1318.51, t + 0.09, 0.42, 0.8, { ...bell, glideFrom: 0.94 }); // E6
      return 0.55;
    },
  },
  chime: {
    label: "Chime",
    play: (c, out, t) => {
      [1046.5, 1318.51, 1567.98].forEach((f, i) => tone(c, out, f, t + i * 0.09, 0.5, 0.6, { partials: [[2, 0.15], [3, 0.05]] })); // C6 E6 G6
      return 0.75;
    },
  },
  bell: {
    label: "Bell",
    play: (c, out, t) => {
      tone(c, out, 880, t, 1.1, 0.75, { attack: 0.004, partials: [[2.76, 0.25], [5.4, 0.08], [2, 0.12]] }); // A5, inharmonic ring
      return 1.15;
    },
  },
  bubble: {
    label: "Bubble",
    play: (c, out, t) => {
      tone(c, out, 1200, t, 0.12, 0.85, { glideFrom: 0.5, glideMs: 70 });
      tone(c, out, 1600, t + 0.1, 0.14, 0.7, { glideFrom: 0.55, glideMs: 70 });
      return 0.3;
    },
  },
  marimba: {
    label: "Marimba",
    play: (c, out, t) => {
      const wood: ToneOpts = { type: "triangle", attack: 0.003, partials: [[4, 0.12]] };
      tone(c, out, 783.99, t, 0.22, 1, wood); // G5
      tone(c, out, 1046.5, t + 0.12, 0.3, 1, wood); // C6
      return 0.45;
    },
  },
  soft: {
    label: "Soft",
    play: (c, out, t) => {
      tone(c, out, 659.25, t, 0.45, 0.7, { attack: 0.04 }); // E5
      tone(c, out, 523.25, t + 0.18, 0.6, 0.6, { attack: 0.04 }); // C5
      return 0.8;
    },
  },
};

export type ChimeId = keyof typeof SOUNDS;
export const DEFAULT_CHIME: ChimeId = "marimba";
export const CHIME_OPTIONS: { id: ChimeId; label: string }[] = Object.entries(SOUNDS)
  .map(([id, s]) => ({ id, label: s.label }))
  // the default leads the dropdown
  .sort((a, b) => Number(b.id === DEFAULT_CHIME) - Number(a.id === DEFAULT_CHIME));

/** Unknown/stale stored ids fall back to the default. */
export function normalizeChime(id: string | null | undefined): ChimeId {
  return id && id in SOUNDS ? id : DEFAULT_CHIME;
}

/** Play a sound. Returns false when audio isn't available/unlocked, so the
 *  caller can fall back to the OS notification sound. Never throws. */
export async function playChime(id: string = DEFAULT_CHIME, volume = 0.22): Promise<boolean> {
  try {
    const c = getCtx();
    if (!c) return false;
    if (c.state !== "running") {
      await Promise.race([c.resume(), new Promise((r) => setTimeout(r, 300))]);
      if ((c.state as AudioContextState) !== "running") return false;
    }
    const master = c.createGain();
    master.gain.value = volume;
    master.connect(c.destination);
    const len = SOUNDS[normalizeChime(id)].play(c, master, c.currentTime + 0.01);
    setTimeout(() => master.disconnect(), (len + 0.4) * 1000);
    return true;
  } catch {
    return false;
  }
}
