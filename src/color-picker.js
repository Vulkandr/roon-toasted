// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// A color picker like Photoshop's: a square for saturation (left to right)
// and brightness (bottom to top), a hue strip under it, and a hex field that
// takes a pasted color. Used for the custom accent (Settings and Wrapped)
// and Now Playing's custom background and inverted colors.
//
//   const picker = new ColorPicker(container, { value, onInput, onChange });
//   onInput: while dragging (for a live preview); onChange: when a color is
//   settled (the drag let go, or a whole hex typed or pasted).
//
//   openColorPopover(anchor, value, { onInput, onChange, tabs }): the same
//   picker in a little box under `anchor`; a click outside or Escape closes
//   it. With `tabs`, a toggle on top picks which color it edits (see there).

// Any CSS color ("hsl(...)", "rgb(...)", a name) -> "#rrggbb"
export function cssToHex(css) {
  const ctx = document.createElement("canvas").getContext("2d");
  ctx.fillStyle = "#000";
  ctx.fillStyle = String(css ?? "");
  return normalizeHex(ctx.fillStyle) ?? "#000000";
}

// "#abc", "abc", "#aabbcc", " AABBCC " -> "#aabbcc"; anything else -> null
export function normalizeHex(text) {
  let s = String(text ?? "").trim().replace(/^#/, "");
  if (/^[0-9a-f]{3}$/i.test(s)) s = [...s].map((c) => c + c).join("");
  return /^[0-9a-f]{6}$/i.test(s) ? `#${s.toLowerCase()}` : null;
}

export function hexToRgb(hex) {
  const n = parseInt(normalizeHex(hex)?.slice(1) ?? "000000", 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

function rgbToHex(r, g, b) {
  return `#${[r, g, b].map((x) => Math.round(x).toString(16).padStart(2, "0")).join("")}`;
}

// h 0-360, s and v 0-1
function hsvToHex(h, s, v) {
  const f = (n) => {
    const k = (n + h / 60) % 6;
    return v - v * s * Math.max(0, Math.min(k, 4 - k, 1));
  };
  return rgbToHex(f(5) * 255, f(3) * 255, f(1) * 255);
}

function hexToHsv(hex) {
  const [r, g, b] = hexToRgb(hex).map((x) => x / 255);
  const max = Math.max(r, g, b);
  const d = max - Math.min(r, g, b);
  let h = 0;
  if (d) {
    if (max === r) h = ((g - b) / d) % 6;
    else if (max === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
    h *= 60;
    if (h < 0) h += 360;
  }
  return { h, s: max ? d / max : 0, v: max };
}

const clamp01 = (x) => Math.min(1, Math.max(0, x));

export class ColorPicker {
  constructor(container, { value = "#7b5fc7", onInput, onChange } = {}) {
    this.onInput = onInput;
    this.onChange = onChange;
    this.el = document.createElement("div");
    this.el.className = "cpick";
    this.el.innerHTML = `
      <div class="cpick-sv"><span class="cpick-knob"></span></div>
      <div class="cpick-hue"><span class="cpick-knob"></span></div>
      <div class="cpick-row">
        <span class="cpick-swatch"></span>
        <span class="cpick-label">Hex</span>
        <input class="cpick-hex" type="text" spellcheck="false" maxlength="9" />
      </div>`;
    container.append(this.el);
    this.sv = this.el.querySelector(".cpick-sv");
    this.hue = this.el.querySelector(".cpick-hue");
    this.swatch = this.el.querySelector(".cpick-swatch");
    this.hex = this.el.querySelector(".cpick-hex");
    this.set(value);

    this.drag(this.sv, (x, y) => {
      this.hsv.s = x;
      this.hsv.v = 1 - y;
    });
    this.drag(this.hue, (x) => {
      this.hsv.h = x * 360;
    });
    // A whole color typed or pasted takes at once; anything else waits,
    // and goes back to the color when the field is left
    this.hex.addEventListener("input", () => {
      const hex = normalizeHex(this.hex.value);
      if (!hex || hex === this.value) return;
      this.hsv = hexToHsv(hex);
      this.value = hex;
      this.paint(false);
      this.onInput?.(hex);
      this.onChange?.(hex);
    });
    this.hex.addEventListener("change", () => this.paint());
    this.hex.addEventListener("keydown", (event) => {
      if (event.key === "Enter") this.hex.blur();
    });
  }

  // Sets the color from outside (no callbacks)
  set(hex) {
    const color = normalizeHex(hex) ?? "#7b5fc7";
    // (a grey keeps the hue it was on, so the strip doesn't jump)
    const hsv = hexToHsv(color);
    if (this.hsv && (hsv.s === 0 || hsv.v === 0)) hsv.h = this.hsv.h;
    this.hsv = hsv;
    this.value = color;
    this.paint();
  }

  paint(withField = true) {
    const { h, s, v } = this.hsv;
    this.sv.style.setProperty("--hue", `hsl(${h} 100% 50%)`);
    const [svKnob, hueKnob] = [this.sv, this.hue].map((el) => el.firstElementChild);
    svKnob.style.left = `${s * 100}%`;
    svKnob.style.top = `${(1 - v) * 100}%`;
    svKnob.style.background = this.value;
    hueKnob.style.left = `${(h / 360) * 100}%`;
    hueKnob.style.background = `hsl(${h} 100% 50%)`;
    this.swatch.style.background = this.value;
    if (withField) this.hex.value = this.value.toUpperCase();
  }

  // Pointer down in `area`, then dragging: `move(x, y)` with 0-1 positions
  drag(area, move) {
    const at = (event) => {
      const r = area.getBoundingClientRect();
      move(clamp01((event.clientX - r.left) / r.width), clamp01((event.clientY - r.top) / r.height));
      this.value = hsvToHex(this.hsv.h, this.hsv.s, this.hsv.v);
      this.paint();
      this.onInput?.(this.value);
    };
    area.addEventListener("pointerdown", (event) => {
      if (event.button !== 0) return;
      area.setPointerCapture(event.pointerId);
      at(event);
      const onMove = (e) => at(e);
      const onUp = () => {
        area.removeEventListener("pointermove", onMove);
        area.removeEventListener("pointerup", onUp);
        area.removeEventListener("pointercancel", onUp);
        this.onChange?.(this.value);
      };
      area.addEventListener("pointermove", onMove);
      area.addEventListener("pointerup", onUp);
      area.addEventListener("pointercancel", onUp);
    });
  }
}

// The picker in a box under `anchor` (one at a time)
let openPopover = null;
let openAnchor = null;

export function closeColorPopover() {
  openPopover?.();
}

// (clicking the anchor again closes it: null then)
//
// `tabs`: [{ key, label, value, follows?, followValue?, followLabel?,
// followNote? }], the colors the toggle on top switches between; onInput and
// onChange get (hex, key). A tab with `follows` can go back to following
// another color: while it does, the picker shows `followValue()` and the
// note, and once it's changed a button (`followLabel`) puts it back, with
// onChange(null, key).
export function openColorPopover(anchor, value, { onInput, onChange, tabs } = {}) {
  const again = openAnchor === anchor;
  closeColorPopover();
  if (again) return null;
  const box = document.createElement("div");
  box.className = "cpick-popover";
  document.body.append(box);
  let picker;
  if (tabs?.length) {
    let current = tabs[0];
    const bar = document.createElement("div");
    bar.className = "cpick-tabs";
    const foot = document.createElement("div");
    foot.className = "cpick-foot";
    const showFoot = () => {
      foot.replaceChildren();
      foot.hidden = !("follows" in current);
      if (foot.hidden) return;
      if (current.follows) {
        const note = document.createElement("span");
        note.className = "cpick-note";
        note.textContent = current.followNote || "";
        foot.append(note);
      } else {
        const back = document.createElement("button");
        back.type = "button";
        back.className = "cpick-follow";
        back.textContent = current.followLabel || "Reset";
        back.addEventListener("click", () => {
          current.follows = true;
          onChange?.(null, current.key);
          picker.set(current.followValue?.() ?? current.value);
          showFoot();
        });
        foot.append(back);
      }
    };
    const picked = (hex, done) => {
      current.value = hex;
      if (current.follows) {
        current.follows = false;
        showFoot();
      }
      (done ? onChange : onInput)?.(hex, current.key);
    };
    const buttons = tabs.map((tab) => {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "cpick-tab";
      button.textContent = tab.label;
      button.addEventListener("click", () => select(tab));
      bar.append(button);
      return button;
    });
    box.append(bar);
    picker = new ColorPicker(box, {
      value: tabs[0].value,
      onInput: (hex) => picked(hex, false),
      onChange: (hex) => picked(hex, true),
    });
    box.append(foot);
    function select(tab) {
      current = tab;
      buttons.forEach((button, i) => button.classList.toggle("selected", tabs[i] === tab));
      picker.set(tab.follows && tab.followValue ? tab.followValue() : tab.value);
      showFoot();
    }
    select(tabs[0]);
  } else {
    picker = new ColorPicker(box, { value, onInput, onChange });
  }

  // Under the anchor, kept inside the window
  const a = anchor.getBoundingClientRect();
  const w = box.offsetWidth;
  const h = box.offsetHeight;
  let left = Math.min(Math.max(8, a.left + a.width / 2 - w / 2), window.innerWidth - w - 8);
  let top = a.bottom + 6;
  if (top + h > window.innerHeight - 8) top = Math.max(8, a.top - h - 6);
  box.style.left = `${left}px`;
  box.style.top = `${top}px`;

  const outside = (event) => {
    if (!box.contains(event.target) && !anchor.contains(event.target)) close();
  };
  // (Escape closes just the picker, not what it's open over too)
  const escape = (event) => {
    if (event.key !== "Escape") return;
    event.stopPropagation();
    close();
  };
  function close() {
    box.remove();
    document.removeEventListener("pointerdown", outside, true);
    document.removeEventListener("keydown", escape, true);
    if (openPopover === close) {
      openPopover = null;
      openAnchor = null;
    }
  }
  // (after this click, so the click that opened it doesn't close it)
  setTimeout(() => {
    document.addEventListener("pointerdown", outside, true);
    document.addEventListener("keydown", escape, true);
  });
  openPopover = close;
  openAnchor = anchor;
  return picker;
}
