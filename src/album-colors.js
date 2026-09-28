// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Album colors, shared by the Toaster and the toast: finds the main color of
// a cover and turns it into a palette (accent, background tint, ...).
//
//   const hue = await albumHue(imageKey);        // what color the cover is
//   paintColors(root, albumPalette(hue, dark));  // use it
//   paintColors(root, {});                       // back to the default colors

const { convertFileSrc } = window.__TAURI__.core;

// The colors a palette can change (all others stay at their defaults)
export const ALBUM_COLOR_PROPS = ["--accent", "--bg", "--card", "--track", "--border", "--on-accent"];

const albumHueCache = new Map(); // image key -> { h, s, muted } or { mono: true }

function hsl(h, s, l) {
  return `hsl(${Math.round(h)} ${Math.round(s * 100)}% ${Math.round(l * 100)}%)`;
}

// [hue 0-360, saturation 0-1, lightness 0-1]
function rgbToHsl(r, g, b) {
  r /= 255;
  g /= 255;
  b /= 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (max === min) return [0, 0, l];
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h;
  if (max === r) h = (g - b) / d + (g < b ? 6 : 0);
  else if (max === g) h = (b - r) / d + 2;
  else h = (r - g) / d + 4;
  return [h * 60, s, l];
}

// Groups pixels into 24 hue slices (15° each), weighting each pixel by
// `weigh`; returns the heaviest slice (counting half of each neighbour, so a
// color on a slice edge isn't split) as { h, s, share of all the weight }
function strongestHue(pixelsHsl, weigh) {
  const slices = Array.from({ length: 24 }, () => ({ weight: 0, x: 0, y: 0, s: 0 }));
  let total = 0;
  for (const [h, s, l] of pixelsHsl) {
    const weight = weigh(s, l);
    if (weight <= 0) continue;
    const slice = slices[Math.floor(h / 15) % 24];
    slice.weight += weight;
    // Hue averaged as an angle, so 355° and 5° average to 0°, not 180°
    slice.x += Math.cos((h * Math.PI) / 180) * weight;
    slice.y += Math.sin((h * Math.PI) / 180) * weight;
    slice.s += s * weight;
    total += weight;
  }
  if (total === 0) return null;
  const around = (i) =>
    slices[i].weight + 0.5 * (slices[(i + 23) % 24].weight + slices[(i + 1) % 24].weight);
  let best = 0;
  for (let i = 1; i < 24; i++) if (around(i) > around(best)) best = i;
  const slice = slices[best];
  if (slice.weight === 0) return null;
  return {
    h: ((Math.atan2(slice.y, slice.x) * 180) / Math.PI + 360) % 360,
    s: slice.s / slice.weight,
    share: around(best) / total,
  };
}

// The album's main color as { h, s, muted }, or { mono: true } for art with
// no clear color (black and white). Vivid color wins, even a small accent
// like a logo if it's all one hue; covers without any (dark, washed-out art)
// get a muted color instead, if enough of the cover shares one hue.
export async function albumHue(imageKey) {
  if (albumHueCache.has(imageKey)) return albumHueCache.get(imageKey);

  const img = new Image();
  img.crossOrigin = "anonymous"; // the art server allows reading its pixels
  img.src = convertFileSrc(imageKey, "roonimg") + "?width=64&height=64";
  await img.decode();

  const size = 32;
  const canvas = document.createElement("canvas");
  canvas.width = size;
  canvas.height = size;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  ctx.drawImage(img, 0, 0, size, size);
  const data = ctx.getImageData(0, 0, size, size).data;
  const pixelsHsl = [];
  for (let i = 0; i < data.length; i += 4) pixelsHsl.push(rgbToHsl(data[i], data[i + 1], data[i + 2]));
  const count = pixelsHsl.length;

  let result = null;

  // 1. Vivid color, weighting vivid, mid-brightness pixels most: at least 4%
  //    of the cover, or 2% (a small logo or lettering) if it's mostly one hue
  const isVivid = (s, l) => s >= 0.25 && l >= 0.12 && l <= 0.9;
  const vividCount = pixelsHsl.filter(([, s, l]) => isVivid(s, l)).length;
  if (vividCount >= count * 0.02) {
    const hue = strongestHue(pixelsHsl, (s, l) => (isVivid(s, l) ? s * (1 - Math.abs(l - 0.5)) : 0));
    const enough = vividCount >= count * 0.04 || (hue && hue.share >= 0.5);
    if (hue && enough) result = { h: hue.h, s: hue.s, muted: false };
  }

  // 2. Muted color: at least 20% of the cover faintly colored, and most of it
  //    one hue (so specks or JPEG noise on grey art don't count)
  if (!result) {
    const isMuted = (s, l) => s >= 0.08 && l >= 0.06 && l <= 0.92;
    const mutedCount = pixelsHsl.filter(([, s, l]) => isMuted(s, l)).length;
    if (mutedCount >= count * 0.2) {
      const hue = strongestHue(pixelsHsl, (s, l) => (isMuted(s, l) ? s : 0));
      if (hue && hue.share >= 0.5) result = { h: hue.h, s: hue.s, muted: true };
    }
  }

  // 3. No clear color: black and white, which gets a white accent
  if (!result) result = { mono: true };

  albumHueCache.set(imageKey, result);
  return result;
}

// The Toaster's colors for an album color, in dark or light mode. Colors not
// listed stay at their defaults.
export function albumPalette({ h, s, muted, mono }, dark) {
  // Black and white: a white accent with dark icons on it (graphite with white
  // icons in light mode), on the normal background
  if (mono) {
    return dark
      ? { "--accent": "hsl(240 5% 88%)", "--on-accent": "hsl(240 7% 11%)" }
      : { "--accent": "hsl(240 5% 22%)" };
  }
  // Muted covers get a softer accent and a lighter tint
  const vivid = muted ? Math.min(0.5, Math.max(0.3, s * 1.5)) : Math.min(0.85, Math.max(0.5, s));
  const tint = muted ? Math.min(0.18, s * 0.8) : Math.min(0.3, s * 0.5);
  // Light hues (yellows, greens, cyans) get a darker accent so white icons on
  // it (the play button) stay readable
  const lightHue = h >= 40 && h <= 190;
  if (dark) {
    return {
      "--accent": hsl(h, vivid, lightHue ? 0.45 : 0.58),
      "--bg": hsl(h, tint, 0.1),
      "--card": hsl(h, tint, 0.15),
      "--track": hsl(h, tint, 0.25),
      "--border": hsl(h, tint, 0.21),
    };
  }
  return {
    "--accent": hsl(h, vivid, lightHue ? 0.36 : 0.44),
    "--bg": hsl(h, tint, 0.95),
    "--card": hsl(h, tint * 0.6, 0.99),
    "--track": hsl(h, tint, 0.84),
    "--border": hsl(h, tint, 0.88),
  };
}

// Sets a palette's colors on an element (usually the page's root), and
// removes any it doesn't have, so they go back to the default
export function paintColors(element, palette) {
  for (const name of ALBUM_COLOR_PROPS) {
    if (palette[name]) element.style.setProperty(name, palette[name]);
    else element.style.removeProperty(name);
  }
}
