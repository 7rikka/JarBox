// 生成 JarBox 应用图标源图（1024x1024 PNG），无需第三方依赖。
// 造型：靛蓝到青的渐变圆角方块 + 居中的白色 J。
// 所有几何都用 0..1 归一化坐标，改尺寸不影响比例。
import { deflateSync } from "node:zlib";
import { writeFileSync, mkdirSync } from "node:fs";

const S = 1024;
const data = Buffer.alloc(S * S * 4);

const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
const mix = (a, b, t) => a + (b - a) * t;

// 圆角矩形 SDF（中心 + 半宽 + 半径）
function sdRoundRect(px, py, cx, cy, hw, hh, r) {
  const dx = Math.abs(px - cx) - (hw - r);
  const dy = Math.abs(py - cy) - (hh - r);
  const ax = Math.max(dx, 0);
  const ay = Math.max(dy, 0);
  return Math.hypot(ax, ay) + Math.min(Math.max(dx, dy), 0) - r;
}

function sdSegment(px, py, ax, ay, bx, by) {
  const pax = px - ax, pay = py - ay;
  const bax = bx - ax, bay = by - ay;
  const h = clamp((pax * bax + pay * bay) / (bax * bax + bay * bay), 0, 1);
  return Math.hypot(pax - bax * h, pay - bay * h);
}

// J：竖干 + 底部半圆钩。竖干 x=0.64（y 0.30→0.56），钩心 (0.50,0.56) 半径 0.14，
// 笔画半宽 0.0625（等于 12.5/100）。圆头端点由线段 SDF 与端点距离共同给出。
const STEM_X = 0.64;
const STEM_TOP = 0.3;
const HOOK_Y = 0.56;
const HOOK_CX = 0.5;
const HOOK_R = 0.14;
const HALF = 0.0625;

function sdJ(px, py) {
  const stem = sdSegment(px, py, STEM_X, STEM_TOP, STEM_X, HOOK_Y);
  let hook;
  if (py >= HOOK_Y) {
    // 下半环
    hook = Math.abs(Math.hypot(px - HOOK_CX, py - HOOK_Y) - HOOK_R);
  } else {
    // 上半：只有两端是圆的
    hook = Math.min(
      Math.hypot(px - (HOOK_CX + HOOK_R), py - HOOK_Y),
      Math.hypot(px - (HOOK_CX - HOOK_R), py - HOOK_Y)
    );
  }
  return Math.min(stem, hook);
}

const smooth = (e0, e1, x) => {
  const t = clamp((x - e0) / (e1 - e0), 0, 1);
  return t * t * (3 - 2 * t);
};

// 底板：四周留 4% 边距的圆角方，圆角半径 0.21
const BG_HW = 0.46;
const BG_R = 0.21;

for (let y = 0; y < S; y++) {
  for (let x = 0; x < S; x++) {
    const px = (x + 0.5) / S;
    const py = (y + 0.5) / S;
    const i = (y * S + x) * 4;

    const dBg = sdRoundRect(px, py, 0.5, 0.5, BG_HW, BG_HW, BG_R);
    const bgA = clamp(0.5 - dBg * S * 2.2, 0, 1);
    if (bgA <= 0) continue;

    // 对角渐变：靛蓝 #4F46E5 -> 青 #0D9488
    const t = clamp(((px + py) / 2 - 0.05) / 0.95, 0, 1);
    const r0 = mix(79, 13, t);
    const g0 = mix(70, 148, t);
    const b0 = mix(229, 136, t);

    // 白色 J
    const jA = clamp(0.5 - (sdJ(px, py) - HALF) * S * 2.0, 0, 1);

    data[i] = Math.round(mix(r0, 255, jA));
    data[i + 1] = Math.round(mix(g0, 255, jA));
    data[i + 2] = Math.round(mix(b0, 255, jA));
    data[i + 3] = Math.round(bgA * 255);
  }
}

// 极轻的描边高光，让图标在浅色背景上也有边界
for (let y = 0; y < S; y++) {
  for (let x = 0; x < S; x++) {
    const px = (x + 0.5) / S;
    const py = (y + 0.5) / S;
    const i = (y * S + x) * 4;
    if (data[i + 3] === 0) continue;
    const d = sdRoundRect(px, py, 0.5, 0.5, BG_HW, BG_HW, BG_R);
    const border = smooth(0.0, -0.006, d) * smooth(-0.02, -0.008, d);
    if (border > 0) {
      const k = border * 0.14;
      data[i] = Math.round(mix(data[i], 255, k));
      data[i + 1] = Math.round(mix(data[i + 1], 255, k));
      data[i + 2] = Math.round(mix(data[i + 2], 255, k));
    }
  }
}

// ---- PNG 编码 ----
const raw = Buffer.alloc((S * 4 + 1) * S);
for (let y = 0; y < S; y++) {
  raw[y * (S * 4 + 1)] = 0;
  data.copy(raw, y * (S * 4 + 1) + 1, y * S * 4, (y + 1) * S * 4);
}

const crcTable = (() => {
  const t = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c;
  }
  return t;
})();
function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = crcTable[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(type, body) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(body.length);
  const t = Buffer.from(type, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([t, body])));
  return Buffer.concat([len, t, body, crc]);
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(S, 0);
ihdr.writeUInt32BE(S, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // RGBA
ihdr[10] = 0;
ihdr[11] = 0;
ihdr[12] = 0;

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);

mkdirSync("src-tauri/icons", { recursive: true });
writeFileSync("src-tauri/icons/source.png", png);
console.log("icon written", png.length);
