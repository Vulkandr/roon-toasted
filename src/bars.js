// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The canvas bars for Now Playing's seek and volume (the Wrapper's Edit
// layout), in the spirit of Muviz's visualizers. All share one height
// profile along the bar (rippling gently, rising into jagged peaks close
// around the playhead) and differ in how it's drawn (`style`):
// - wave: layered colors from the outside in with a white core;
// - spectrum: glowing columns, bright at the base and deep at the tips;
// - beam: a soft light beam, hot at the base, fading out upward;
// - glow (Glow line): electricity, a jagged glowing bolt with a white-hot
//   core and a thinner second bolt, morphing smoothly from one strike to
//   the next, over faint columns.
// The part past the playhead is dimmed. Mirrored (both sides of a center
// line) or one-sided (rising from one edge). It's drawn on a canvas exactly
// behind the real slider, which stays invisible on top for clicks and
// drags, and always fits inside it. Works along either axis.
//
// House rules for this and every bar style that follows (Vulk, 2026-10-13):
// - The rise sits close around the playhead / volume level: a peak whose
//   width follows the bar's thickness and only a little its length, with
//   low shoulders (see `sigma` in draw), never a swell across the bar.
// - Near that peak things keep moving: each sample shifts at its own,
//   quicker pace while playing; the rest of the bar stays calm.
// - Both ends taper smoothly to nothing (`taper` in draw), no hard stops,
//   except the rise itself, which keeps its full height at an end.

// The Wave's app colors, outermost first, for each side
const RAINBOW = { top: ["#ff2a2a", "#ffe600"], bottom: ["#1f3bff", "#00c8ff"] };

// The other styles' app colors: the app's blue-purple (the Wrapped logo's
// #807DFB to #681AE6), with a violet-magenta for the hottest part
const APP_SHADES = { light: "#a9a6ff", main: "#7a6cf7", deep: "#3b21c2", hot: "#b356ff" };

// Waveform looks whose unplayed part is grey, as in Roon (the rest dim)
const GREY_UNPLAYED = new Set(["roonwave", "wfpills", "wfcaps", "wfpixel", "wfstitch"]);

const PAINTERS = {
  // Layered colors from the outside in, white core
  wave({ ctx, pad, step, n, ys, line, reach, sides }) {
    const o = this.opts;
    const colorsFor = (side) => (o.rainbow ? RAINBOW[side] : [alpha(o.color, 0.45), o.color]);
    const scales = [1, 0.74, 0.5];
    for (const [sign, side] of sides) {
      const all = [...colorsFor(side), "#ffffff"];
      for (let k = 0; k < all.length; k++) {
        ctx.beginPath();
        ctx.moveTo(pad, line);
        for (let i = 0; i <= n; i++) ctx.lineTo(pad + i * step, line + sign * ys[i] * reach * scales[k]);
        ctx.lineTo(pad + n * step, line);
        ctx.closePath();
        ctx.fillStyle = all[k];
        ctx.fill();
      }
    }
  },

  // Columns, bright at the base and deep at the tips, with a soft glow
  spectrum(g) {
    const { ctx, line, reach, sides, em } = g;
    const s = this.shades();
    const cols = columns(g, Math.max(3, em * 0.45), Math.max(4, em * 0.55));
    for (const [sign] of sides) {
      // The glow: the same columns, blurred and added on top
      ctx.save();
      ctx.filter = `blur(${Math.max(2, cols.w * 0.9).toFixed(1)}px)`;
      ctx.globalCompositeOperation = "lighter";
      ctx.fillStyle = alpha(s.light, 0.6);
      for (const c of cols.list) ctx.fillRect(c.x, line, cols.w, sign * c.h * reach * 0.8);
      ctx.restore();
      for (const c of cols.list) {
        const h = c.h * reach;
        if (h < 0.5) continue;
        const grad = ctx.createLinearGradient(0, line, 0, line + sign * h);
        grad.addColorStop(0, s.light);
        grad.addColorStop(0.45, s.main);
        grad.addColorStop(1, s.deep);
        ctx.fillStyle = grad;
        ctx.fillRect(c.x, line, cols.w, sign * h);
      }
    }
  },

  // A soft light beam: hot at the base, fading out upward
  beam(g) {
    const { ctx, pad, step, n, line, reach, sides, fades } = g;
    const s = this.shades();
    // A glowing floor along the whole bar, with the beam rising out of it
    const ys = smooth(g.ys, 3).map((y, i) => Math.max(y, 0.2 * fades[i]));
    const blur = Math.max(2, reach * 0.14);
    const layers = [
      [1, s.deep, 0.9, blur * 1.6],
      [0.75, s.main, 0.9, blur],
      [0.45, s.hot, 1, blur * 0.6],
    ];
    for (const [sign] of sides) {
      ctx.save();
      ctx.globalCompositeOperation = "lighter";
      for (const [scale, color, a, b] of layers) {
        const grad = ctx.createLinearGradient(0, line, 0, line + sign * reach);
        grad.addColorStop(0, alpha(color, a));
        grad.addColorStop(0.6, alpha(color, a * 0.55));
        grad.addColorStop(1, alpha(color, 0));
        ctx.filter = `blur(${b.toFixed(1)}px)`;
        ctx.fillStyle = grad;
        ctx.beginPath();
        ctx.moveTo(pad, line);
        for (let i = 0; i <= n; i++) ctx.lineTo(pad + i * step, line + sign * ys[i] * reach * scale);
        ctx.lineTo(pad + n * step, line);
        ctx.closePath();
        ctx.fill();
      }
      ctx.restore();
    }
  },

  // Electricity over faint columns: a glowing bolt with a white-hot core
  // and a thinner second bolt, both re-struck every frame while playing
  glow(g) {
    const { ctx, pad, step, n, line, reach, sides, em, fades } = g;
    const s = this.shades();
    // Lower away from the playhead (the peak keeps its height). On the
    // waveform (`g.wf`) the heights are the song's, so it's wild only near
    // the playhead, by distance
    const near = (y) => Math.min(1, y / 0.45); // 1 at the rise, small elsewhere
    const base = smooth(g.ys, 1).map((y) => (g.wf ? y : y * (0.5 + 0.5 * near(y))));
    const nearAt = (i) => (g.wf ? Math.exp(-((pad + i * step - g.head) ** 2) / (2 * (g.sigma * 1.5) ** 2)) : near(base[i]));
    const thick = Math.max(2, em * 0.16);
    const room = Math.max(1, reach - thick);
    // About 5 strikes a second, each sliding smoothly into the next (no
    // jumps between frames); the same picture while paused
    const at = this.phase / 0.175;
    const strike = Math.floor(at);
    const t0 = at - strike;
    const t = t0 * t0 * (3 - 2 * t0);
    // A bolt: the profile with sharp random kinks, wilder where it's high,
    // calm toward the tapered ends
    const bolt = (seed) => {
      const out = new Float32Array(n + 1);
      for (let i = 0; i <= n; i++) {
        const amp = (0.03 + 0.6 * nearAt(i) * base[i]) * fades[i];
        const kink = hash(i, strike * 7 + seed) * (1 - t) + hash(i, (strike + 1) * 7 + seed) * t;
        out[i] = Math.min(1, Math.max(0, base[i] + amp * (kink * 2 - 1)));
      }
      return out;
    };
    const trace = (ys, sign) => {
      ctx.beginPath();
      for (let i = 0; i <= n; i++) ctx.lineTo(pad + i * step, line + sign * ys[i] * room);
    };
    faintColumns(g, room);
    for (const [sign] of sides) {
      ctx.lineCap = "round";
      ctx.lineJoin = "miter";
      ctx.miterLimit = 3;
      // The second, thinner bolt
      trace(bolt(1), sign);
      ctx.lineWidth = Math.max(1, thick * 0.4);
      ctx.strokeStyle = alpha(s.main, 0.7);
      ctx.shadowColor = s.main;
      ctx.shadowBlur = thick * 2;
      ctx.stroke();
      // The main bolt: colored glow, then a white-hot core
      const main = bolt(0);
      trace(main, sign);
      ctx.lineWidth = thick;
      ctx.shadowBlur = thick * 4;
      ctx.strokeStyle = s.light;
      ctx.stroke();
      ctx.shadowBlur = thick * 1.5;
      ctx.stroke();
      ctx.shadowBlur = 0;
      trace(main, sign);
      ctx.lineWidth = Math.max(1, thick * 0.35);
      ctx.strokeStyle = "rgba(255, 255, 255, 0.9)";
      ctx.stroke();
    }
  },
  // LED meter: columns of round lights, each bouncing on its own under a
  // ceiling set by its distance from the playhead; the top one glows
  dots(g) {
    const { ctx, pad, step, n, line, reach, sides, head, sigma, fades } = g;
    const s = this.shades();
    // Six lights each way (Vulk: fewer looked like a left-right wobble);
    // only a very thin bar gets fewer, so they stay visible
    const rows = Math.min(6, Math.max(3, Math.floor(reach / 3)));
    const pitch = reach / rows;
    const size = pitch / 1.3;
    const cols = columns(g, size, size * 1.35);
    const r = size / 2;
    // Each column bounces on its own, like a real meter (Vulk). How high
    // it may go depends on how far it is from the playhead: up to the top
    // there, about a third elsewhere, less toward the tapered ends
    const count = cols.list.length;
    const ceilings = new Float32Array(count);
    const rates = new Float32Array(count);
    for (let k = 0; k < count; k++) {
      const x = cols.list[k].x + r;
      const d2 = (x - head) ** 2;
      const near = 0.75 * Math.exp(-d2 / (2 * sigma * sigma)) + 0.25 * Math.exp(-d2 / (2 * (sigma * 2.6) ** 2));
      const fade = fades[Math.min(n, Math.max(0, Math.round((x - pad) / step)))];
      ceilings[k] = Math.min(1, 0.32 * fade + near);
      // Livelier near the playhead (own clocks, so moving the playhead
      // never makes them jump)
      rates[k] = 1 + 2 * near;
    }
    const clocks = this.advance("dots", rates);
    const levels = cols.list.map((_, k) => {
      const t = clocks[k];
      const bounce = 0.58 + 0.3 * Math.sin(t * (1.5 + 1.5 * hash(k, 21))) + 0.2 * Math.sin(t * (3.2 + 2 * hash(k, 22)) + 1.3);
      return ceilings[k] * Math.min(1, Math.max(0, bounce));
    });
    for (const [sign] of sides) {
      for (const [ci, c] of cols.list.entries()) {
        // The bottom light is always on, like a meter's floor
        const lit = Math.max(1, Math.round(levels[ci] * rows));
        const cx = c.x + r;
        for (let k = 0; k < rows; k++) {
          const cy = line + sign * (k * pitch + r);
          ctx.beginPath();
          ctx.arc(cx, cy, r * 0.82, 0, Math.PI * 2);
          // (unlit lights aren't drawn: Vulk liked it better without the grid)
          if (k >= lit) continue;
          const top = k === lit - 1 && lit > 1;
          ctx.fillStyle = top ? s.light : k / rows < 0.5 ? s.main : mixWhite(s.main, 0.2);
          if (top) {
            ctx.shadowColor = s.hot;
            ctx.shadowBlur = size * 1.4;
          }
          ctx.fill();
          ctx.shadowBlur = 0;
        }
      }
    }
  },

  // ----- Waveform looks (the seek bar's Waveform option) -----
  //
  // All drawn from Roon's own waveform of the playing track (about 2000
  // values, 0 to 255, start to end; local files only). They show the real
  // song, so the made-up profile's rules (rise, flicker, taper) don't apply.
  // A column's height is the average it covers at 80% of the reach (Vulk
  // leveled it off to match Roon's): `w.level`, which in each look's Alive
  // version bounces gently near the playhead. With Loudness on, each part
  // takes a color by how loud it is (`loudRamp`).

  // Roon: thin columns with even gaps, like Roon's own seek bar
  roonwave(g) {
    const w = this.wave(g, Math.min(Math.max(1, g.em * 0.07), 2), 2);
    if (!w) return;
    const { ctx, line, reach, sides } = g;
    for (let k = 0; k < w.count; k++) {
      // Even silence keeps a sliver, like Roon's
      const h = Math.max(1, w.level[k] * reach * 0.8);
      ctx.fillStyle = w.color(w.avg[k]);
      for (const [sign] of sides) ctx.fillRect(w.x(k), sign < 0 ? line - h : line, w.width, h);
    }
  },

  // Pills: fewer, thicker columns with round ends, like a voice message
  wfpills(g) {
    const width = Math.min(Math.max(3, g.em * 0.3), 8);
    const w = this.wave(g, width, 1.75);
    if (!w) return;
    const { ctx, line, reach, sides } = g;
    const r = width / 2;
    for (let k = 0; k < w.count; k++) {
      const h = Math.max(r, w.level[k] * reach * 0.8);
      ctx.fillStyle = w.color(w.avg[k]);
      ctx.beginPath();
      // Mirrored: one pill across the middle; one-sided: rising from the base
      if (sides.length === 2) ctx.roundRect(w.x(k), line - h, width, 2 * h, r);
      else ctx.roundRect(w.x(k), sides[0][0] < 0 ? line - h : line, width, h, r);
      ctx.fill();
    }
  },

  // Solid shape: the outline as one smooth filled silhouette, deeper at the
  // base and lighter toward the edge, with a thin light rim
  wfshape(g) {
    const w = this.wave(g, 2, 1);
    if (!w) return;
    const { ctx, line, reach, sides } = g;
    const s = this.shades();
    const ys = smooth(w.level, 2);
    for (const [sign] of sides) {
      ctx.beginPath();
      ctx.moveTo(w.x(0), line);
      for (let k = 0; k < w.count; k++) ctx.lineTo(w.x(k) + w.width / 2, line + sign * Math.max(0.5, ys[k] * reach * 0.8));
      ctx.lineTo(w.x(w.count - 1) + w.width, line);
      ctx.closePath();
      if (this.opts.loudColor) {
        ctx.fillStyle = w.alongBar(ctx);
      } else {
        const grad = ctx.createLinearGradient(0, line, 0, line + sign * reach * 0.8);
        grad.addColorStop(0, s.deep);
        grad.addColorStop(1, s.light);
        ctx.fillStyle = grad;
      }
      ctx.fill();
      ctx.beginPath();
      for (let k = 0; k < w.count; k++) ctx.lineTo(w.x(k) + w.width / 2, line + sign * Math.max(0.5, ys[k] * reach * 0.8));
      ctx.lineWidth = 1;
      ctx.strokeStyle = alpha(s.light, 0.7);
      ctx.stroke();
    }
  },

  // Neon outline: only the waveform's edge, as a glowing line with a
  // white-hot core
  wfneon(g) {
    const w = this.wave(g, 2, 1);
    if (!w) return;
    const { ctx, line, reach, sides, em } = g;
    const s = this.shades();
    const thick = Math.max(1.5, em * 0.12);
    const ys = smooth(w.level, 1);
    const room = Math.max(1, reach - thick);
    const glow = this.opts.loudColor ? w.alongBar(ctx) : s.main;
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    for (const [sign] of sides) {
      const trace = () => {
        ctx.beginPath();
        for (let k = 0; k < w.count; k++) ctx.lineTo(w.x(k) + w.width / 2, line + sign * Math.max(0.5, ys[k] * room * 0.85));
      };
      trace();
      ctx.lineWidth = thick;
      ctx.strokeStyle = glow;
      ctx.shadowColor = s.main;
      ctx.shadowBlur = thick * 3;
      ctx.stroke();
      ctx.shadowBlur = 0;
      ctx.lineWidth = thick * 0.6;
      ctx.strokeStyle = this.opts.loudColor ? glow : s.light;
      ctx.stroke();
      ctx.lineWidth = Math.max(0.8, thick * 0.25);
      ctx.strokeStyle = "rgba(255, 255, 255, 0.9)";
      ctx.stroke();
    }
  },

  // LED: the song built from stacked lights like the LED meter, each column
  // lit to its real level
  wfled(g) {
    const { ctx, line, reach, sides } = g;
    const rows = Math.min(6, Math.max(3, Math.floor(reach / 3)));
    const pitch = reach / rows;
    const size = pitch / 1.3;
    const w = this.wave(g, size, 1.35);
    if (!w) return;
    const s = this.shades();
    const r = size / 2;
    for (let k = 0; k < w.count; k++) {
      const lit = Math.min(rows, Math.max(1, Math.round(w.level[k] * 0.8 * rows)));
      ctx.fillStyle = w.color(w.avg[k]);
      for (const [sign] of sides) {
        for (let j = 0; j < lit; j++) {
          ctx.beginPath();
          ctx.arc(w.x(k) + r, line + sign * (j * pitch + r), r * 0.82, 0, Math.PI * 2);
          // The top light brighter
          if (j === lit - 1 && lit > 1 && !this.opts.loudColor) ctx.fillStyle = s.light;
          ctx.fill();
        }
        ctx.fillStyle = w.color(w.avg[k]);
      }
    }
  },

  // Peak caps: thin, see-through columns, each with a small bright cap
  // floating just above its top
  wfcaps(g) {
    const width = Math.min(Math.max(1.5, g.em * 0.12), 3);
    const w = this.wave(g, width, 2.4);
    if (!w) return;
    const { ctx, line, reach, sides } = g;
    const s = this.shades();
    const cap = Math.max(1.5, width * 0.8);
    const gap = Math.max(1.5, cap);
    for (let k = 0; k < w.count; k++) {
      const h = Math.max(1, w.level[k] * reach * 0.72);
      const color = w.color(w.avg[k]);
      for (const [sign] of sides) {
        ctx.globalAlpha = 0.5;
        ctx.fillStyle = color;
        ctx.fillRect(w.x(k), sign < 0 ? line - h : line, width, h);
        ctx.globalAlpha = 1;
        const y = line + sign * (h + gap);
        ctx.fillStyle = this.opts.loudColor ? color : s.light;
        ctx.fillRect(w.x(k) - 0.5, sign < 0 ? y - cap : y, width + 1, cap);
      }
    }
  },

  // Line graph: one smooth line along the top, the area under it softly
  // filled
  wfline(g) {
    const w = this.wave(g, 2, 1);
    if (!w) return;
    const { ctx, line, reach, sides, em } = g;
    const s = this.shades();
    const ys = smooth(w.level, 2);
    const end = w.x(w.count - 1) + w.width;
    const along = this.opts.loudColor ? w.alongBar(ctx) : null;
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    for (const [sign] of sides) {
      const at = (k) => line + sign * Math.max(0.5, ys[k] * reach * 0.8);
      ctx.beginPath();
      ctx.moveTo(w.x(0), line);
      for (let k = 0; k < w.count; k++) ctx.lineTo(w.x(k) + w.width / 2, at(k));
      ctx.lineTo(end, line);
      ctx.closePath();
      if (along) {
        ctx.globalAlpha = 0.35;
        ctx.fillStyle = along;
      } else {
        const grad = ctx.createLinearGradient(0, line, 0, line + sign * reach * 0.8);
        grad.addColorStop(0, alpha(s.main, 0.06));
        grad.addColorStop(1, alpha(s.main, 0.5));
        ctx.fillStyle = grad;
      }
      ctx.fill();
      ctx.globalAlpha = 1;
      ctx.beginPath();
      for (let k = 0; k < w.count; k++) ctx.lineTo(w.x(k) + w.width / 2, at(k));
      ctx.lineWidth = Math.max(1.5, em * 0.11);
      ctx.strokeStyle = along || s.light;
      ctx.shadowColor = s.main;
      ctx.shadowBlur = em * 0.35;
      ctx.stroke();
      ctx.shadowBlur = 0;
    }
  },

  // Pixel: the song in square blocks, stacked like an old game's meter
  wfpixel(g) {
    const { ctx, line, reach, sides } = g;
    const rows = Math.min(6, Math.max(3, Math.floor(reach / 3)));
    const pitch = reach / rows;
    const size = Math.max(1, pitch - Math.max(1, pitch * 0.22));
    const w = this.wave(g, size, pitch / size);
    if (!w) return;
    const inset = (pitch - size) / 2;
    for (let k = 0; k < w.count; k++) {
      const lit = Math.min(rows, Math.max(1, Math.round(w.level[k] * 0.85 * rows)));
      ctx.fillStyle = w.color(w.avg[k]);
      for (const [sign] of sides) {
        for (let j = 0; j < lit; j++) {
          const y = sign < 0 ? line - (j + 1) * pitch + inset : line + j * pitch + inset;
          ctx.fillRect(w.x(k), y, size, size);
        }
      }
    }
  },

  // Contour: three nested outlines of the song, like a topographic map
  wfcontour(g) {
    const w = this.wave(g, 2, 1);
    if (!w) return;
    const { ctx, line, reach, sides, em } = g;
    const s = this.shades();
    const ys = smooth(w.level, 2);
    const end = w.x(w.count - 1) + w.width;
    const along = this.opts.loudColor ? w.alongBar(ctx) : null;
    ctx.lineJoin = "round";
    for (const [scale, color] of [[1, s.light], [0.66, s.main], [0.33, s.deep]]) {
      for (const [sign] of sides) {
        ctx.beginPath();
        ctx.moveTo(w.x(0), line);
        for (let k = 0; k < w.count; k++) ctx.lineTo(w.x(k) + w.width / 2, line + sign * Math.max(0.5, ys[k] * reach * 0.8 * scale));
        ctx.lineTo(end, line);
        ctx.lineWidth = Math.max(1, em * 0.08);
        ctx.strokeStyle = along || color;
        ctx.stroke();
      }
    }
  },

  // Stitch: each column made of short dashes stacked up, like stitching
  wfstitch(g) {
    const { ctx, line, reach, sides, em } = g;
    const dash = Math.max(1, Math.round(em * 0.1));
    const every = dash * 2;
    const width = Math.max(2, em * 0.25);
    const w = this.wave(g, width, 2);
    if (!w) return;
    for (let k = 0; k < w.count; k++) {
      const h = w.level[k] * reach * 0.8;
      const count = Math.max(1, Math.round(h / every));
      ctx.fillStyle = w.color(w.avg[k]);
      for (const [sign] of sides) {
        for (let j = 0; j < count; j++) {
          const y = line + sign * j * every;
          ctx.fillRect(w.x(k), sign < 0 ? y - dash : y, width, dash);
        }
      }
    }
  },

  // Album art: the waveform's shape filled with the playing cover, blurred
  // and stretched, with a white outline (a gradient until the cover loads)
  wfart(g) {
    const w = this.wave(g, 2, 1);
    if (!w) return;
    const { ctx, pad, line, reach, sides } = g;
    const s = this.shades();
    const ys = smooth(w.level, 1);
    const img = this.artReady ? this.artImage : null;
    const end = w.x(w.count - 1) + w.width;
    for (const [sign] of sides) {
      const top = (k) => line + sign * Math.max(0.5, ys[k] * reach * 0.8);
      ctx.save();
      ctx.beginPath();
      ctx.moveTo(w.x(0), line);
      for (let k = 0; k < w.count; k++) ctx.lineTo(w.x(k) + w.width / 2, top(k));
      ctx.lineTo(end, line);
      ctx.closePath();
      ctx.clip();
      const y = sign < 0 ? line - reach : line;
      if (img) {
        ctx.filter = "blur(2px) saturate(1.3)";
        ctx.drawImage(img, 0, img.naturalHeight * 0.4, img.naturalWidth, img.naturalHeight * 0.2, pad, y, end - pad, reach);
        ctx.filter = "none";
      } else {
        const grad = ctx.createLinearGradient(0, line, 0, line + sign * reach);
        grad.addColorStop(0, s.light);
        grad.addColorStop(1, s.deep);
        ctx.fillStyle = grad;
        ctx.fillRect(pad, y, end - pad, reach);
      }
      ctx.restore();
      ctx.beginPath();
      for (let k = 0; k < w.count; k++) ctx.lineTo(w.x(k) + w.width / 2, top(k));
      ctx.lineWidth = 1.2;
      ctx.strokeStyle = "rgba(255,255,255,0.75)";
      ctx.stroke();
    }
  },

  // Ribbons: three thin see-through lines weaving around each other, each a
  // little behind the last, twisting faster near the playhead
  ribbon(g) {
    const { ctx, pad, step, n, line, reach, sides, em, head, sigma } = g;
    const s = this.shades();
    const base = smooth(g.ys, 2);
    const colors = [s.light, s.main, s.hot];
    faintColumns(g, reach * 0.95);
    // Each point twists at its own pace, quicker near the playhead
    const rates = new Float32Array(n + 1);
    for (let i = 0; i <= n; i++) rates[i] = 1.2 + 3 * Math.exp(-((pad + i * step - head) ** 2) / (2 * (sigma * 2) ** 2));
    const turns = this.advance("ribbon", rates);
    for (const [sign] of sides) {
      ctx.lineJoin = "round";
      ctx.lineCap = "round";
      for (let k = 0; k < 3; k++) {
        ctx.beginPath();
        for (let i = 0; i <= n; i++) {
          const x = pad + i * step;
          const twist = Math.sin(turns[i] + i * 0.06 + k * 2.1);
          const y = base[i] * (0.72 + 0.28 * twist);
          ctx.lineTo(x, line + sign * y * reach * 0.95);
        }
        ctx.lineWidth = Math.max(1, em * 0.08);
        ctx.strokeStyle = alpha(colors[k], 0.75);
        ctx.shadowColor = colors[k];
        ctx.shadowBlur = em * 0.4;
        ctx.stroke();
      }
      ctx.shadowBlur = 0;
    }
  },

  // Neon: one smooth, thick glowing tube with a hollow center, flickering a
  // little near the playhead like a real tube
  neon(g) {
    const { ctx, pad, step, n, line, reach, sides, em, head, sigma } = g;
    const s = this.shades();
    const soft = smooth(g.ys, 4);
    const thick = Math.max(3, em * 0.26);
    const room = Math.max(1, reach - thick);
    const path = (from = 0, to = n) => {
      ctx.beginPath();
      for (let i = from; i <= to; i++) ctx.lineTo(pad + i * step, line + sign0 * soft[i] * room);
    };
    let sign0 = -1;
    faintColumns(g, room);
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    for (const [sign] of sides) {
      sign0 = sign;
      // The glass glowing in the color
      path();
      ctx.lineWidth = thick;
      ctx.strokeStyle = s.main;
      ctx.shadowColor = s.main;
      ctx.shadowBlur = thick * 2.5;
      ctx.stroke();
      ctx.shadowBlur = 0;
      // The hollow inside, then a bright filament along it
      ctx.lineWidth = thick * 0.45;
      ctx.strokeStyle = alpha(s.deep, 0.7);
      ctx.stroke();
      ctx.lineWidth = Math.max(1, thick * 0.16);
      ctx.strokeStyle = s.light;
      ctx.stroke();
      // Near the playhead: an extra flickering glow
      const at = Math.round((head - pad) / step);
      const span = Math.max(2, Math.round((sigma * 1.5) / step));
      const flicker = 0.55 + 0.45 * Math.abs(Math.sin(this.phase * 9) * Math.sin(this.phase * 23 + 1));
      path(Math.max(0, at - span), Math.min(n, at + span));
      ctx.lineWidth = thick;
      ctx.strokeStyle = alpha(s.light, 0.5 * flicker);
      ctx.shadowColor = s.hot;
      ctx.shadowBlur = thick * 3 * flicker;
      ctx.stroke();
      ctx.shadowBlur = 0;
    }
  },

  // Mountains: three layered silhouettes, darker behind and lighter in
  // front, each a little offset, like a range at dusk
  mountain(g) {
    const { ctx, pad, step, n, line, reach, sides } = g;
    const s = this.shades();
    const layers = [
      [smooth(g.ys, 1), 1, 6, s.deep],
      [smooth(g.ys, 2), 0.78, -4, s.main],
      [smooth(g.ys, 3), 0.55, 2, s.light],
    ];
    for (const [sign] of sides) {
      for (const [ys, scale, shift, color] of layers) {
        const grad = ctx.createLinearGradient(0, line, 0, line + sign * reach);
        grad.addColorStop(0, color);
        grad.addColorStop(1, mixWhite(color, 0.25));
        ctx.fillStyle = grad;
        ctx.beginPath();
        ctx.moveTo(pad, line);
        for (let i = 0; i <= n; i++) {
          const j = Math.min(n, Math.max(0, i + shift));
          ctx.lineTo(pad + i * step, line + sign * ys[j] * reach * scale);
        }
        ctx.lineTo(pad + n * step, line);
        ctx.closePath();
        ctx.fill();
      }
    }
  },

  // Particles: the profile as a cloud of small glowing specks, drifting a
  // little; dense at the rise, sparse toward the ends
  particles(g) {
    const { ctx, pad, step, n, line, reach, sides, em, head, sigma } = g;
    const s = this.shades();
    const r = Math.max(0.8, em * 0.07);
    ctx.save();
    ctx.globalCompositeOperation = "lighter";
    for (const [sign] of sides) {
      for (let i = 0; i <= n; i++) {
        const h = g.ys[i];
        const x0 = pad + i * step;
        // Near the playhead the specks dance further (Vulk: livelier there)
        const near = Math.exp(-((x0 - head) ** 2) / (2 * (sigma * 1.5) ** 2));
        const count = Math.round(1 + h * 7);
        for (let k = 0; k < count; k++) {
          const seed = i * 13 + k * 7 + (sign < 0 ? 0 : 5);
          const rise = hash(seed, 1) ** 1.4 * h;
          const speed = 1.5 + hash(seed, 2) * 2;
          const drift = Math.sin(this.phase * speed + seed) * em * (0.12 + 0.3 * near);
          const bob = Math.sin(this.phase * speed * 1.3 + seed * 2) * near * 0.08;
          const x = x0 + (hash(seed, 3) - 0.5) * step + drift;
          const y = line + sign * Math.min(1, Math.max(0, rise + bob)) * reach;
          ctx.beginPath();
          ctx.arc(x, y, r * (0.6 + hash(seed, 4) * 0.9), 0, Math.PI * 2);
          ctx.fillStyle = alpha(k % 3 === 0 ? s.light : s.main, 0.35 + 0.6 * (1 - rise));
          ctx.fill();
        }
      }
      // Sparks rising from the base around the playhead, each fading in
      // and out over its own short life, then starting again somewhere new
      const sparks = Math.max(10, Math.round(sigma / Math.max(2, em * 0.25)));
      for (let k = 0; k < sparks; k++) {
        const seed = k * 31 + (sign < 0 ? 0 : 17);
        const life = this.phase * (0.35 + 0.45 * hash(seed, 5)) + hash(seed, 6);
        const round = Math.floor(life);
        const t = life - round;
        const x = head + (hash(seed, round) + hash(seed, round + 0.5) - 1) * sigma * 1.6;
        const i = Math.min(n, Math.max(0, Math.round((x - pad) / step)));
        const top = g.ys[i] * (0.7 + 0.45 * hash(seed, round + 0.25));
        const y = line + sign * Math.min(1, t * top) * reach;
        ctx.beginPath();
        ctx.arc(x + Math.sin(t * 6 + seed) * em * 0.15, y, r * (0.9 + 0.8 * hash(seed, 7)), 0, Math.PI * 2);
        ctx.fillStyle = alpha(k % 2 ? s.light : s.hot, 0.9 * Math.sin(Math.PI * t));
        ctx.fill();
      }
    }
    ctx.restore();
  },

  // Plasma: a filled shape whose inside swirls between two shades, like a
  // lava lamp; it bubbles faster near the playhead
  plasma(g) {
    const { ctx, pad, step, n, line, reach, sides, em, head, sigma } = g;
    const s = this.shades();
    const ys = smooth(g.ys, 2);
    const span = n * step;
    // Each blob keeps its own clock, quicker near the playhead. (Speed
    // times the shared clock made every blob jump whenever the playhead
    // moved its speed a little: the "resetting" Vulk saw.)
    const blobs = Math.max(8, Math.round(span / (em * 1.6)));
    const rates = new Float32Array(blobs);
    for (let k = 0; k < blobs; k++) {
      const home = pad + ((k + 0.5) / blobs) * span;
      rates[k] = 0.6 + 2.5 * Math.exp(-((home - head) ** 2) / (2 * (sigma * 2.5) ** 2)) + hash(k, 9);
    }
    const swirl = this.advance("plasma", rates);
    for (const [sign] of sides) {
      ctx.save();
      ctx.beginPath();
      ctx.moveTo(pad, line);
      for (let i = 0; i <= n; i++) ctx.lineTo(pad + i * step, line + sign * ys[i] * reach);
      ctx.lineTo(pad + n * step, line);
      ctx.closePath();
      ctx.fillStyle = s.deep;
      ctx.fill();
      ctx.clip();
      // Soft blobs drifting through it
      ctx.globalCompositeOperation = "lighter";
      ctx.filter = `blur(${Math.max(1.5, reach * 0.12).toFixed(1)}px)`;
      for (let k = 0; k < blobs; k++) {
        const home = pad + ((k + 0.5) / blobs) * span;
        const x = home + Math.sin(swirl[k] + k * 1.7) * em * 1.2;
        const y = line + sign * reach * (0.25 + 0.3 * (0.5 + 0.5 * Math.sin(swirl[k] * 0.8 + k)));
        ctx.beginPath();
        ctx.arc(x, y, reach * (0.2 + 0.18 * hash(k, 11)), 0, Math.PI * 2);
        ctx.fillStyle = alpha(k % 3 === 0 ? s.light : s.hot, 0.75);
        ctx.fill();
      }
      ctx.restore();
    }
  },

  // Album art: the profile filled with a stretched, blurred strip of the
  // playing album's cover, outlined so it stays readable
  art(g) {
    const { ctx, pad, step, n, line, reach, sides } = g;
    const s = this.shades();
    const img = this.artReady ? this.artImage : null;
    for (const [sign] of sides) {
      ctx.save();
      ctx.beginPath();
      ctx.moveTo(pad, line);
      for (let i = 0; i <= n; i++) ctx.lineTo(pad + i * step, line + sign * g.ys[i] * reach);
      ctx.lineTo(pad + n * step, line);
      ctx.closePath();
      const outline = new Path2D();
      outline.moveTo(pad, line);
      for (let i = 0; i <= n; i++) outline.lineTo(pad + i * step, line + sign * g.ys[i] * reach);
      ctx.save();
      ctx.clip();
      const top = sign < 0 ? line - reach : line;
      if (img) {
        // A band across the middle of the cover, stretched along the bar
        ctx.filter = "blur(2px) saturate(1.3)";
        ctx.drawImage(img, 0, img.naturalHeight * 0.4, img.naturalWidth, img.naturalHeight * 0.2, pad, top, n * step, reach);
        ctx.filter = "none";
        // A light wash so it reads against the background
        const wash = ctx.createLinearGradient(0, line, 0, line + sign * reach);
        wash.addColorStop(0, "rgba(255,255,255,0.18)");
        wash.addColorStop(1, "rgba(255,255,255,0)");
        ctx.fillStyle = wash;
        ctx.fillRect(pad, top, n * step, reach);
      } else {
        const grad = ctx.createLinearGradient(0, line, 0, line + sign * reach);
        grad.addColorStop(0, s.light);
        grad.addColorStop(1, s.deep);
        ctx.fillStyle = grad;
        ctx.fillRect(pad, top, n * step, reach);
      }
      ctx.restore();
      ctx.lineWidth = 1.2;
      ctx.strokeStyle = "rgba(255,255,255,0.75)";
      ctx.stroke(outline);
      ctx.restore();
    }
  },
};

// Drawn once over the whole bar after the bright and dimmed parts, so
// nothing is cut in half at the playhead
const OVERLAYS = {
  // The beam itself: a bright shaft rising at the playhead
  beam({ ctx, pad, step, n, ys, line, reach, sides, head, sigma }) {
    const s = this.shades();
    const at = Math.round((head - pad) / step);
    let peak = 0;
    for (let i = Math.max(0, at - 2); i <= Math.min(n, at + 2); i++) peak = Math.max(peak, ys[i]);
    const h = Math.max(0.25, peak) * reach;
    const w = Math.max(6, sigma * 2.2);
    const mirrored = sides.length === 2;
    const up = sides[0][0] < 0;
    // Soft on every side without a blur filter: a vertical glow, faded out
    // sideways, on a small canvas of its own
    const dpr = window.devicePixelRatio || 1;
    const sw = Math.max(1, Math.ceil(w * dpr));
    const sh = Math.max(1, Math.ceil((mirrored ? 2 * h : h) * dpr));
    this.shaftCanvas ??= document.createElement("canvas");
    const sc = this.shaftCanvas;
    sc.width = sw;
    sc.height = sh;
    const x = sc.getContext("2d");
    const v = x.createLinearGradient(0, 0, 0, sh);
    const stops = mirrored
      ? [[0, alpha(s.main, 0)], [0.3, alpha(s.main, 0.75)], [0.5, alpha(s.hot, 0.95)], [0.7, alpha(s.main, 0.75)], [1, alpha(s.main, 0)]]
      : up
        ? [[0, alpha(s.main, 0)], [0.6, alpha(s.main, 0.75)], [1, alpha(s.hot, 0.95)]]
        : [[0, alpha(s.hot, 0.95)], [0.4, alpha(s.main, 0.75)], [1, alpha(s.main, 0)]];
    for (const [at, color] of stops) v.addColorStop(at, color);
    x.fillStyle = v;
    x.fillRect(0, 0, sw, sh);
    x.globalCompositeOperation = "destination-in";
    const across = x.createLinearGradient(0, 0, sw, 0);
    across.addColorStop(0, "rgba(0,0,0,0)");
    across.addColorStop(0.5, "rgba(0,0,0,1)");
    across.addColorStop(1, "rgba(0,0,0,0)");
    x.fillStyle = across;
    x.fillRect(0, 0, sw, sh);
    const top = mirrored ? line - h : up ? line - h : line;
    ctx.save();
    ctx.globalCompositeOperation = "lighter";
    ctx.drawImage(sc, head - w / 2, top, w, mirrored ? 2 * h : h);
    ctx.restore();
  },
};

// A light bar (like a lightsaber blade): a long, thin shape, widest in the
// middle and tapering to rounded tips, with a soft glow.
// Colored rim and white core, or (inverted) white rim and colored core.
function saber(ctx, x, y, w, h, color, inverted) {
  const r = w / 2;
  // Half an ellipse's taper, flattened a little so the middle stays full
  // and only the last stretch narrows (a superellipse, exponent 2.6)
  const pill = (px, py, pw, ph) => {
    const cx = px + pw / 2;
    const cy = py + ph / 2;
    const a = pw / 2;
    const b = ph / 2;
    const k = 2 / 2.6;
    ctx.beginPath();
    for (let s = 0; s <= 64; s++) {
      const t = (s / 64) * Math.PI * 2;
      const c = Math.cos(t);
      const n = Math.sin(t);
      ctx.lineTo(cx + a * Math.sign(c) * Math.abs(c) ** k, cy + b * Math.sign(n) * Math.abs(n) ** k);
    }
    ctx.closePath();
  };
  ctx.save();
  ctx.globalAlpha = 1;
  if (!inverted) {
    // Colored rim, white core (Vulk's first mockup)
    ctx.shadowColor = color;
    ctx.shadowBlur = w * 1.2;
    pill(x, y, w, h);
    ctx.fillStyle = color;
    ctx.fill();
    ctx.shadowBlur = 0;
    const inset = Math.max(1, r * 0.45);
    ctx.filter = `blur(${Math.max(0.6, inset * 0.6).toFixed(1)}px)`;
    pill(x + inset, y + inset * 1.6, w - 2 * inset, Math.max(1, h - 3.2 * inset));
    ctx.fillStyle = "#ffffff";
    ctx.fill();
    ctx.restore();
    return;
  }
  // White like the Glow line's bolt: a light stroke glowing in the color,
  // a white-hot center, and the color showing through in the middle
  ctx.shadowColor = color;
  ctx.shadowBlur = w * 2.2;
  pill(x, y, w, h);
  ctx.fillStyle = mixWhite(color, 0.55);
  ctx.fill();
  ctx.shadowBlur = w * 0.9;
  ctx.fill();
  ctx.shadowBlur = 0;
  // The white-hot line down the middle
  const inset = Math.max(0.5, r * 0.35);
  pill(x + inset, y + inset * 1.5, w - 2 * inset, Math.max(1, h - 3 * inset));
  ctx.fillStyle = "rgba(255, 255, 255, 0.95)";
  ctx.fill();
  // A touch of the color in its core, soft
  const core = Math.max(0.5, r * 0.62);
  ctx.filter = `blur(${Math.max(0.5, core * 0.5).toFixed(1)}px)`;
  pill(x + core, y + core * 2.5, w - 2 * core, Math.max(1, h - 5 * core));
  ctx.fillStyle = alpha(color, 0.55);
  ctx.fill();
  ctx.restore();
}

// A color mixed toward white (t = how much white)
function mixWhite(color, t) {
  return rgbText(mix(toRgb(color), [255, 255, 255], t));
}

// Columns along the bar: each as tall as the profile's highest point under it
function columns({ pad, step, n, ys }, width, every = width * 1.35) {
  const span = n * step;
  const list = [];
  const gap = Math.max(1, Math.round(every - width));
  const pitch = width + gap;
  const count = Math.max(1, Math.floor((span + gap) / pitch));
  const start = pad + (span - (count * pitch - gap)) / 2;
  for (let k = 0; k < count; k++) {
    const x = start + k * pitch;
    const from = Math.max(0, Math.floor((x - pad) / step));
    const to = Math.min(n, Math.ceil((x + width - pad) / step));
    let h = 0;
    for (let i = from; i <= to; i++) h = Math.max(h, ys[i]);
    list.push({ x, h });
  }
  return { w: width, list };
}

// The faint grey columns under the Glow line, Ribbon and Neon tube
function faintColumns(g, room) {
  const { ctx, line, sides, em } = g;
  const cols = columns({ ...g, ys: smooth(g.ys, 4) }, Math.max(2, em * 0.22), Math.max(4, em * 0.9));
  ctx.fillStyle = "rgba(160, 160, 190, 0.22)";
  for (const [sign] of sides) {
    for (const c of cols.list) ctx.fillRect(c.x, line, cols.w, sign * c.h * room);
  }
}

// A steady random number 0..1 for a sample and a seed
function hash(i, seed) {
  const x = Math.sin(i * 127.1 + seed * 311.7) * 43758.5453;
  return x - Math.floor(x);
}

// The profile smoothed (a few passes of a 3-point average)
// Waveform heights from Roon's loudness (`dynamics`, the seek bar's
// Dynamics in Edit layout). Stretching the values as they come blows the
// column-to-column jitter up as much as the song's real rises and falls (a
// dense, even song turned into noise: Vulk's Novembers Doom), so the two
// are taken apart:
// - the envelope: the loudness smoothed over about 1.5% of the track, the
//   song's sections, laid out on a curve in three parts (heights as a
//   share of the track's loud top):
//   - the loud body (the loudest sections, at 80% of the top or more, from
//     their bottom quarter up: below that it's mostly the blurred edges
//     where one section turns into the next, which made some songs look
//     more varied than they are) gets `span` of the height, stretched by at
//     most `gain` times its real spread, whether the song is loud all the
//     way (Novembers Doom) or rises and falls between loud stretches
//     (Merrimack);
//   - the quiet end (under 45% of the top) keeps `quiet` of its own height,
//     so intros, breaks and fades don't shrink away;
//   - between them, a straight run joining the two, with some height of
//     its own when the song has a fair share of in-between sections.
// - the texture: each column's difference from the envelope, put back at
//   `texture` times its real size (1: as Roon has it).
// Natural is Roon's values as they are; More is the one stretched look
// (Vulk, 2026-10-23, picked on five of his songs' real waveforms). There
// were More / Max / Extreme for a while, but they only differed in how far
// a song may stretch and most songs need less than any of them, so they
// looked the same; a "max" or "extreme" saved then is More.
const DYNAMICS = {
  natural: null,
  more: { span: 0.7, gain: 16, texture: 1.7, quiet: 0.9 },
};
function shapeHeights(avg, dynamics) {
  const how = DYNAMICS[dynamics];
  if (!how) return avg;
  const n = avg.length;
  const env = smooth(avg, Math.max(4, Math.round(n / 16)));
  const sorted = Float32Array.from(env).sort();
  const at = (list, q) => list[Math.round((list.length - 1) * q)];
  const top = Math.max(at(sorted, 0.98), 0.01);
  // (the loud part of the track: its spread, from its loudest sections
  // only, at 80% of the top or more: with the quieter sections counted, a
  // song that rises and falls (Merrimack) looked varied enough and its loud
  // stretches stayed flat)
  const body = sorted.filter((v) => v >= top * 0.8);
  const bodyLow = body.length > 8 ? at(body, 0.25) : at(sorted, 0.25);
  const spread = Math.min(0.9, Math.max(0.01, (top - bodyLow) / top));
  const span = Math.max(spread, Math.min(how.span, spread * how.gain));
  const xb = 1 - spread; // the loud body starts here (share of the top)
  const xq = Math.min(0.45, xb * 0.8); // the quiet end stops here
  const lowTop = how.quiet * xq;
  // (the sections between quiet and loud keep some room of their own, more
  // the more of the song they are, so opening up the loud body doesn't
  // squash them onto the quiet ones)
  let middle = 0;
  for (const v of env) if (v / top >= xq && v / top < xb) middle += 1;
  const midRoom = Math.min(0.25, (middle / n) * 0.6);
  const bodyFloor = Math.max(lowTop + midRoom, 1 - span);
  const curve = (x) => {
    if (x >= xb) return bodyFloor + ((Math.min(x, 1) - xb) / (1 - xb)) * (1 - bodyFloor);
    if (x >= xq) return lowTop + ((x - xq) / (xb - xq)) * (bodyFloor - lowTop);
    return how.quiet * x;
  };
  const out = new Float32Array(n);
  for (let k = 0; k < n; k++) {
    const h = curve(env[k] / top) + (how.texture * (avg[k] - env[k])) / top;
    out[k] = Math.min(1, Math.max(0, h));
  }
  return out;
}

function smooth(ys, passes) {
  let a = Float32Array.from(ys);
  for (let p = 0; p < passes; p++) {
    const b = new Float32Array(a.length);
    for (let i = 0; i < a.length; i++) b[i] = (a[Math.max(0, i - 1)] + 2 * a[i] + a[Math.min(a.length - 1, i + 1)]) / 4;
    a = b;
  }
  return a;
}

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
    this.opts = { labelScale: 1, style: "wave", vertical: false, mirror: true, side: "top", rainbow: true, color: "#7b5fc7", dot: "#7b5fc7", fontSize: 13 };
    // Fixed jitter per sample, so the peaks don't crawl (they shift in
    // place instead, see draw)
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
  /// opts: style (see PAINTERS), art (the cover, for the Album art style), vertical, mirror, side ("top" | "bottom": which side a
  /// one-sided wave rises toward), rainbow (the app's look) or color (one
  /// color in shades), dot (the playhead's color), fontSize (the piece's).
  fit(opts) {
    this.opts = { ...this.opts, ...opts };
    // The Album art style paints the cover into the bar: load it once per
    // cover (until it's in, the bar shows a plain gradient)
    const art = this.opts.style === "art" || this.opts.style === "wfart" ? this.opts.art || null : null;
    if (art !== this.artSrc) {
      this.artSrc = art;
      this.artReady = false;
      this.artImage = null;
      if (art) {
        const img = new Image();

        img.onload = () => {
          if (this.artSrc !== art) return;
          this.artImage = img;
          this.artReady = true;
          this.draw();
        };
        img.src = art;
      }
    }
    const input = this.input;
    const dpr = window.devicePixelRatio || 1;
    const c = this.canvas;
    const sw = input.offsetWidth;
    const sh = input.offsetHeight;
    // The canvas reaches a bit past the slider on both sides across the
    // bar, so glows and bolts have room to breathe; the bars themselves
    // keep the slider's height
    const room = Math.round((this.opts.vertical ? sw : sh) * 0.25);
    this.room = room;
    const w = this.opts.vertical ? sw + 2 * room : sw;
    const h = this.opts.vertical ? sh : sh + 2 * room;
    c.style.left = `${input.offsetLeft - (this.opts.vertical ? room : 0)}px`;
    c.style.top = `${input.offsetTop - (this.opts.vertical ? 0 : room)}px`;
    c.style.width = `${w}px`;
    c.style.height = `${h}px`;
    const pw = Math.round(w * dpr);
    const ph = Math.round(h * dpr);
    if (c.width !== pw || c.height !== ph) {
      c.width = pw;
      c.height = ph;
    }
    this.length = this.opts.vertical ? sh : sw;
    this.thick = this.opts.vertical ? sw : sh;
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

  /// Roon's waveform (`opts.waveform`) laid out in columns `width` wide,
  /// `pitch` widths apart, across the bar: each column's average value
  /// (0..1), where it starts (`x(k)`) and its color
  /// (`color(t)`: the bar's main color, or by loudness with Loudness on).
  /// Null when there's no waveform.
  wave(g, width, pitch) {
    const data = this.opts.waveform;
    if (!data?.length) return this.profileWave(g, width, pitch);
    const span = g.n * g.step;
    const every = width * pitch;
    const count = Math.max(2, Math.min(data.length, Math.floor(span / every)));
    const start = g.pad + (span - count * every + (every - width)) / 2;
    const dynamics = this.opts.dynamics === "natural" ? "natural" : "more";
    if (this.waveCache?.data !== data || this.waveCache.count !== count || this.waveCache.dynamics !== dynamics) {
      const avg = new Float32Array(count);
      for (let k = 0; k < count; k++) {
        const from = Math.floor((k / count) * data.length);
        const to = Math.max(from + 1, Math.floor(((k + 1) / count) * data.length));
        let sum = 0;
        for (let i = from; i < to; i++) sum += data[i];
        avg[k] = sum / (to - from) / 255;
      }
      // The track's own quietest and loudest, so Loudness spans its range
      // (a loud master would otherwise be one color)
      let lo = 1;
      let hi = 0;
      for (const v of avg) {
        lo = Math.min(lo, v);
        hi = Math.max(hi, v);
      }
      this.waveCache = { data, count, avg, lo, hi: Math.max(hi, lo + 0.05), dynamics, heights: shapeHeights(avg, dynamics) };
    }
    const { avg, lo, hi, heights } = this.waveCache;
    // Alive: near the playhead each column bounces gently around its real
    // height, on its own clock (the rest stays still)
    let level = heights;
    if (this.opts.alive) {
      const rates = new Float32Array(count);
      const near = new Float32Array(count);
      for (let k = 0; k < count; k++) {
        near[k] = Math.exp(-((start + k * every - g.head) ** 2) / (2 * (g.sigma * 1.2) ** 2));
        rates[k] = 2.5 + 2.5 * hash(k, 31);
      }
      const clocks = this.advance("alive", rates);
      let lift = new Float32Array(count);
      for (let k = 0; k < count; k++) lift[k] = near[k] * (0.22 * Math.sin(clocks[k]) + 0.12 * Math.sin(clocks[k] * 2.3 + 1));
      // (narrow columns, as in the shapes: smoothed, so edges don't fizz)
      if (width < 3) lift = smooth(lift, 2);
      level = heights.map((v, k) => Math.min(1.25, v * (1 + lift[k])));
    }
    return this.columnsOf(start, every, count, width, avg, level, lo, hi);
  }

  /// The waveform looks without a waveform (Waveform off, or nothing from
  /// Roon for the track): the same columns, from the made-up profile the
  /// other styles use (the rise at the playhead, the gentle ripple)
  profileWave(g, width, pitch) {
    if (!g.ys) return null;
    const span = g.n * g.step;
    const every = width * pitch;
    const count = Math.max(2, Math.floor(span / every));
    const start = g.pad + (span - count * every + (every - width)) / 2;
    const level = new Float32Array(count);
    for (let k = 0; k < count; k++) {
      const f = Math.min(g.n, Math.max(0, (start + k * every + width / 2 - g.pad) / g.step));
      const i = Math.floor(f);
      const t = f - i;
      level[k] = g.ys[i] * (1 - t) + g.ys[Math.min(g.n, i + 1)] * t;
    }
    return this.columnsOf(start, every, count, width, level, level, 0, 1);
  }

  /// Columns as the waveform looks draw them: where each starts, its
  /// height (`level`), its loudness (`avg`) and colors
  columnsOf(start, every, count, width, avg, level, lo, hi) {
    const s = this.shades();
    const ramp = this.opts.loudColor ? this.loudRamp(s) : null;
    const loudness = (t) => (t - lo) / (hi - lo);
    return {
      count,
      width,
      avg,
      level,
      x: (k) => start + k * every,
      color: (t) => (ramp ? ramp(loudness(t)) : s.main),
      // Loudness colors along the bar, for filled shapes and lines
      alongBar: (ctx) => {
        const grad = ctx.createLinearGradient(start, 0, start + count * every, 0);
        const stops = Math.min(count, 64);
        for (let i = 0; i < stops; i++) {
          const k = Math.round((i / (stops - 1)) * (count - 1));
          grad.addColorStop(i / (stops - 1), ramp ? ramp(loudness(avg[k])) : s.main);
        }
        return grad;
      },
    };
  }

  /// Colors by loudness: the deep shade when quiet, through the main color,
  /// to the hot one when loud
  loudRamp(s) {
    const key = s.deep + s.main + s.hot;
    if (this.rampKey !== key) {
      const [d, m, h] = [s.deep, s.main, s.hot].map(toRgb);
      this.rampTable = Array.from({ length: 33 }, (_, i) => {
        const t = i / 32;
        return rgbText(t < 0.5 ? mix(d, m, t * 2) : mix(m, h, (t - 0.5) * 2));
      });
      this.rampKey = key;
    }
    const table = this.rampTable;
    return (t) => table[Math.round(Math.min(1, Math.max(0, t)) * 32)];
  }

  /// Clocks that each run at their own, changing rate: every one moves on
  /// by how far the shared clock moved since the last frame, times its
  /// rate, so a change of rate never makes it jump
  advance(key, rates) {
    this.clocks ??= {};
    let c = this.clocks[key];
    if (!c || c.values.length !== rates.length) {
      const values = new Float64Array(rates.length);
      for (let i = 0; i < values.length; i++) values[i] = hash(i, 17) * Math.PI * 2;
      c = this.clocks[key] = { values, at: this.phase };
    }
    const d = this.phase - c.at;
    c.at = this.phase;
    for (let i = 0; i < rates.length; i++) c.values[i] += d * rates[i];
    return c.values;
  }

  stop() {
    if (this.frame != null) cancelAnimationFrame(this.frame);
    this.frame = null;
  }

  /// The colors for spectrum, beam and glow: the app's blue-purple, or
  /// shades of the bar's color (the album's or a picked one)
  shades() {
    const o = this.opts;
    if (o.rainbow) return APP_SHADES;
    const key = o.color;
    if (this.shadeKey === key) return this.shadeCache;
    const rgb = toRgb(o.color);
    this.shadeKey = key;
    this.shadeCache = {
      light: rgbText(mix(rgb, [255, 255, 255], 0.5)),
      main: rgbText(rgb),
      deep: rgbText(mix(rgb, [0, 0, 0], 0.5)),
      hot: rgbText(mix(rgb, [255, 255, 255], 0.25)),
    };
    return this.shadeCache;
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
    // The slider's box starts `room` in from the canvas's edge, across
    const room = this.room || 0;
    ctx.translate(0, room);

    // The slider's knob travels between half a knob from each end; the
    // playhead is drawn where the knob is
    const em = o.fontSize;
    // (the knob, like the times next to the bar, keeps the text size when
    // the bar is made taller: `labelScale` makes up for it)
    const lem = em * (o.labelScale || 1);
    const pad = Math.min(lem * 0.46, L / 4);
    const span = L - 2 * pad;
    const head = pad + this.fraction() * span;
    // Where the wave starts from, and how far it may reach (always
    // inside). One-sided, it starts right at the edge, so the bar can sit
    // flush against the window's edge, and the light bar rises from it.
    let line;
    let reach;
    let sides;
    if (o.mirror) {
      line = T / 2;
      reach = T / 2 - 1;
      sides = [[-1, "top"], [1, "bottom"]];
    } else if (o.side === "bottom") {
      // One-sided = one half of the mirrored bar: the same height as a
      // mirrored side, its base still flush with the edge
      line = 0;
      reach = T / 2 - 1;
      sides = [[1, "bottom"]];
    } else {
      line = T;
      reach = T / 2 - 1;
      sides = [[-1, "top"]];
    }

    // The wave's height at each sample, 0..1
    const step = Math.min(Math.max(3, em * 0.3), 8);
    const n = Math.max(1, Math.floor(span / step));
    // The rise stays near the playhead: a peak that widens a little with
    // the bar's thickness and length (far less than it used to), with low
    // shoulders
    const sigma = Math.min(Math.max(18 + span * 0.012, T * 0.3, step * 2), span * 0.06);
    const shoulder = sigma * 2.6;
    // Both ends taper smoothly to nothing instead of stopping short
    const taper = Math.min(span * 0.12, Math.max(30, lem * 3));
    const ease = (t) => (t <= 0 ? 0 : t >= 1 ? 1 : t * t * (3 - 2 * t));
    const ys = new Float32Array(n + 1);
    const fades = new Float32Array(n + 1);
    for (let i = 0; i <= n; i++) {
      const x = pad + i * step;
      const j = this.jitter[i % this.jitter.length];
      const ripple = (0.3 + 0.7 * Math.abs(Math.sin(x * 0.09 + this.phase * 1.7) * Math.sin(x * 0.023 - this.phase))) * (0.6 + 0.4 * j);
      const d2 = (x - head) * (x - head);
      const bump = 0.75 * Math.exp(-d2 / (2 * sigma * sigma)) + 0.25 * Math.exp(-d2 / (2 * shoulder * shoulder));
      // Around the playhead the peaks keep shifting, each at its own quicker
      // pace (the ripple elsewhere stays slow and gentle)
      const k = this.jitter[(i * 7 + 3) % this.jitter.length];
      const flicker = 0.5 + 0.5 * Math.sin(this.phase * (7 + 6 * k) + j * 6.283);
      const spike = 0.35 + 0.65 * (0.35 * j + 0.65 * flicker);
      const fade = ease((i * step) / taper) * ease((n * step - i * step) / taper);
      fades[i] = fade;
      // The ripple tapers off at the ends; the rise at the playhead
      // doesn't, so it stays full height even right at an end
      ys[i] = Math.min(1, 0.22 * ripple * fade + 0.78 * bump * spike);
    }

    // The whole bar dimmed, then the played part bright on top (laying
    // the halves side by side left a faint seam at the playhead). The Wave
    // is painted straight in, each time, so its dimmed colors stay as they
    // were; the blurred styles are painted once on a layer and laid down
    // twice (a blur painted into a clip fades out at the clip's edge).
    // Spectrum, Beam, Glow line, Ribbon and Neon tube with Waveform on: the
    // song's heights (with Dynamics and Alive) in place of the profile, at
    // the waveform looks' 80%, the whole length
    const wf = !!(o.waveYs && o.waveform?.length);
    if (wf) {
      const w = this.wave({ pad, step, n, head, sigma, em }, step, 1);
      if (w) {
        for (let i = 0; i <= n; i++) {
          ys[i] = w.level[Math.min(i, w.count - 1)] * 0.8;
          fades[i] = 1;
        }
      }
    }
    const geom = { ctx, pad, step, n, ys, fades, line, reach, sides, T, em, head, sigma, wf };
    const paint = PAINTERS[o.style] || PAINTERS.wave;
    let layer = null;
    if (paint !== PAINTERS.wave) {
      this.layer ??= document.createElement("canvas");
      layer = this.layer;
      if (layer.width !== c.width || layer.height !== c.height) {
        layer.width = c.width;
        layer.height = c.height;
      }
      const lctx = layer.getContext("2d");
      lctx.setTransform(1, 0, 0, 1, 0, 0);
      lctx.clearRect(0, 0, layer.width, layer.height);
      lctx.setTransform(ctx.getTransform());
      paint.call(this, { ...geom, ctx: lctx });
    }
    for (const [to, dim] of [[L, true], [head, false]]) {
      ctx.save();
      ctx.beginPath();
      // (one-sided, nothing spills past the base edge into the extra room)
      // (top = rising up from the bottom edge, bottom = hanging from the top)
      const from = o.mirror || o.side !== "bottom" ? -room : 0;
      const until = o.mirror || o.side === "bottom" ? T + room : T;
      ctx.rect(0, from, to, until - from);
      ctx.clip();
      ctx.globalAlpha = dim ? 0.3 : 1;
      // The Roon-like waveform looks: the unplayed part grey, as in Roon
      if (dim && GREY_UNPLAYED.has(o.style)) {
        ctx.globalAlpha = 0.45;
        ctx.filter = "saturate(0) brightness(1.3)";
      }
      if (layer) {
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        ctx.drawImage(layer, 0, 0);
      } else {
        paint.call(this, geom);
      }
      ctx.restore();
    }
    OVERLAYS[o.style]?.call(this, geom);

    // The playhead: a glowing light bar across the bar (it also covers the
    // seam where the bright part meets the dimmed one)
    // Always the mirrored bar's size, centered on the base line; one-sided,
    // only the half on the wave's side shows (cut off at the base line)
    const color = this.shades().main;
    // Thin, and sized like the text (not the bar's thickness)
    const bw = Math.min(Math.max(3, lem * 0.26), 11);
    const bh = 2 * (T / 2 - 1) * 0.92;
    ctx.save();
    if (sides.length === 1) {
      ctx.beginPath();
      if (sides[0][0] < 0) ctx.rect(head - bw * 2, line - bh, bw * 4, bh);
      else ctx.rect(head - bw * 2, line, bw * 4, bh);
      ctx.clip();
    }
    // White rim, colored core (tried against Vulk's colored-rim mockup:
    // the white edge stands apart from bars of the same color)
    saber(ctx, head - bw / 2, line - bh / 2, bw, bh, color, true);
    ctx.restore();
  }
}

// Any CSS color as [r, g, b] (the browser normalizes it)
let probe = null;
function toRgb(color) {
  probe ??= document.createElement("canvas").getContext("2d");
  probe.fillStyle = "#000";
  probe.fillStyle = color;
  const v = probe.fillStyle;
  if (v.startsWith("#")) return [1, 3, 5].map((i) => parseInt(v.slice(i, i + 2), 16));
  const m = v.match(/[\d.]+/g) || [0, 0, 0];
  return m.slice(0, 3).map(Number);
}

const mix = (a, b, t) => a.map((v, i) => Math.round(v + (b[i] - v) * t));
const rgbText = ([r, g, b]) => `rgb(${r},${g},${b})`;

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
