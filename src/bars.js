// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The "Wave" bar for Now Playing's seek and volume (the Wrapper's Edit
// layout), in the spirit of Muviz's waveform visualizers: a wave along the
// bar, rippling gently, swelling into jagged peaks around the playhead,
// drawn as layered colors from the outside in with a white core; the part
// past the playhead is dimmed. Mirrored (both sides of a center line) or
// one-sided (rising from one edge). It's drawn on a canvas exactly behind
// the real slider, which stays invisible on top for clicks and drags, and
// the wave always fits inside it. Works along either axis.

// The colors, outermost first, for each side
const RAINBOW = { top: ["#ff2a2a", "#ffe600"], bottom: ["#1f3bff", "#00c8ff"] };

export class WaveBar {
  constructor(container, input) {
    this.input = input;
    this.canvas = document.createElement("canvas");
    this.canvas.className = "bar-wave";
    container.insertBefore(this.canvas, input);
    this.phase = Math.random() * 100;
    this.last = 0;
    this.playing = false;
    this.frame = null;
    this.opts = { vertical: false, mirror: true, side: "top", rainbow: true, color: "#7b5fc7", dot: "#7b5fc7", fontSize: 13 };
    // Fixed jitter per sample, so the peaks don't crawl
    this.jitter = Array.from({ length: 2048 }, (_, i) => {
      const x = Math.sin(i * 12.9898 + 78.233) * 43758.5453;
      return x - Math.floor(x);
    });
  }

  remove() {
    this.stop();
    this.canvas.remove();
  }

  /// Puts the canvas exactly over the slider and draws.
  /// opts: vertical, mirror, side ("top" | "bottom": which side a
  /// one-sided wave rises toward), rainbow (the app's look) or color (one
  /// color in shades), dot (the playhead's color), fontSize (the piece's).
  fit(opts) {
    this.opts = { ...this.opts, ...opts };
    const input = this.input;
    const dpr = window.devicePixelRatio || 1;
    const c = this.canvas;
    const w = input.offsetWidth;
    const h = input.offsetHeight;
    c.style.left = `${input.offsetLeft}px`;
    c.style.top = `${input.offsetTop}px`;
    c.style.width = `${w}px`;
    c.style.height = `${h}px`;
    const pw = Math.round(w * dpr);
    const ph = Math.round(h * dpr);
    if (c.width !== pw || c.height !== ph) {
      c.width = pw;
      c.height = ph;
    }
    this.length = this.opts.vertical ? h : w;
    this.thick = this.opts.vertical ? w : h;
    this.draw();
  }

  /// Keeps the ripples moving while playing.
  setPlaying(playing) {
    this.playing = playing;
    if (playing) this.start();
    else this.stop();
  }

  start() {
    if (this.frame != null) return;
    const tick = (now) => {
      this.frame = null;
      if (!this.playing || !this.canvas.isConnected) return;
      if (now - this.last > 40) {
        this.last = now;
        this.phase += 0.035;
        this.draw();
      }
      this.frame = requestAnimationFrame(tick);
    };
    this.frame = requestAnimationFrame(tick);
  }

  stop() {
    if (this.frame != null) cancelAnimationFrame(this.frame);
    this.frame = null;
  }

  fraction() {
    const min = Number(this.input.min);
    const max = Number(this.input.max);
    return max > min ? (Number(this.input.value) - min) / (max - min) : 0;
  }

  draw() {
    const c = this.canvas;
    const ctx = c.getContext("2d");
    const L = this.length;
    const T = this.thick;
    if (!ctx || !L || !T) return;
    const o = this.opts;
    const dpr = window.devicePixelRatio || 1;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, c.width / dpr, c.height / dpr);
    // Drawn along x; a vertical bar is the same picture turned so it runs
    // bottom to top (the wave's "top" then faces left)
    if (o.vertical) ctx.setTransform(0, -dpr, dpr, 0, 0, L * dpr);

    // The slider's knob travels between half a knob from each end; the
    // playhead is drawn where the knob is
    const em = o.fontSize;
    const pad = Math.min(em * 0.46, L / 4);
    const span = L - 2 * pad;
    const head = pad + this.fraction() * span;
    const dotR = Math.min(em * 0.34, T * 0.3);
    const ring = Math.max(1, em * 0.09);

    // Where the wave starts from, and how far it may reach (always
    // inside). One-sided, it starts right at the edge, so the bar can sit
    // flush against the window's edge, and the dot rests on it.
    let line;
    let reach;
    let sides;
    let dotY;
    if (o.mirror) {
      line = T / 2;
      reach = T / 2 - 1;
      sides = [[-1, "top"], [1, "bottom"]];
      dotY = line;
    } else if (o.side === "bottom") {
      line = 0;
      reach = T - 1;
      sides = [[1, "bottom"]];
      dotY = dotR + ring;
    } else {
      line = T;
      reach = T - 1;
      sides = [[-1, "top"]];
      dotY = T - dotR - ring;
    }

    // The wave's height at each sample, 0..1
    const step = Math.max(3, em * 0.3);
    const n = Math.max(1, Math.floor(span / step));
    const sigma = Math.max(28, span * 0.075);
    const ys = new Float32Array(n + 1);
    for (let i = 0; i <= n; i++) {
      const x = pad + i * step;
      const j = this.jitter[i % this.jitter.length];
      const ripple = (0.3 + 0.7 * Math.abs(Math.sin(x * 0.09 + this.phase * 1.7) * Math.sin(x * 0.023 - this.phase))) * (0.6 + 0.4 * j);
      const bump = Math.exp(-((x - head) * (x - head)) / (2 * sigma * sigma));
      const spike = 0.35 + 0.65 * j;
      ys[i] = Math.min(1, 0.22 * ripple + 0.78 * bump * spike);
    }

    const colorsFor = (side) => (o.rainbow ? RAINBOW[side] : [alpha(o.color, 0.45), o.color]);
    const scales = [1, 0.74, 0.5];
    const shape = (scale, sign) => {
      ctx.beginPath();
      ctx.moveTo(pad, line);
      for (let i = 0; i <= n; i++) ctx.lineTo(pad + i * step, line + sign * ys[i] * reach * scale);
      ctx.lineTo(pad + n * step, line);
      ctx.closePath();
    };
    const paint = (dim) => {
      ctx.globalAlpha = dim ? 0.3 : 1;
      for (const [sign, side] of sides) {
        const all = [...colorsFor(side), "#ffffff"];
        for (let k = 0; k < all.length; k++) {
          shape(scales[k], sign);
          ctx.fillStyle = all[k];
          ctx.fill();
        }
      }
      ctx.globalAlpha = 1;
    };
    // Played part bright, the rest dimmed
    for (const [from, to, dim] of [[0, head, false], [head, L, true]]) {
      ctx.save();
      ctx.beginPath();
      ctx.rect(from, 0, to - from, T);
      ctx.clip();
      paint(dim);
      ctx.restore();
    }

    // The playhead: a dot in the playing album's color, ringed in white
    ctx.beginPath();
    ctx.arc(head, dotY, dotR + ring / 2, 0, Math.PI * 2);
    ctx.fillStyle = "#ffffff";
    ctx.fill();
    ctx.beginPath();
    ctx.arc(head, dotY, dotR - ring / 2, 0, Math.PI * 2);
    ctx.fillStyle = o.dot;
    ctx.fill();
  }
}

// A CSS color with an alpha
function alpha(color, a) {
  if (color.startsWith("#") && color.length >= 7) {
    const r = parseInt(color.slice(1, 3), 16);
    const g = parseInt(color.slice(3, 5), 16);
    const b = parseInt(color.slice(5, 7), 16);
    return `rgba(${r},${g},${b},${a})`;
  }
  if (color.startsWith("hsl")) return color.replace(/\)\s*$/, ` / ${a})`);
  const m = color.match(/[\d.]+/g);
  if (m && m.length >= 3) return `rgba(${m[0]},${m[1]},${m[2]},${a})`;
  return color;
}
