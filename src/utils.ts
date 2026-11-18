export function formatBytes(n: number): string {
  if (!n || n < 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

export function formatTime(ms: number | null | undefined): string {
  if (!ms) return "—";
  const d = new Date(ms);
  const p = (x: number) => String(x).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(
    d.getMinutes()
  )}:${p(d.getSeconds())}`;
}

export function formatDuration(ms: number): string {
  if (ms < 0) ms = 0;
  const s = Math.floor(ms / 1000);
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (d > 0) return `${d} 天 ${h} 小时`;
  if (h > 0) return `${h} 小时 ${m} 分`;
  if (m > 0) return `${m} 分 ${sec} 秒`;
  return `${sec} 秒`;
}

export function basename(p: string): string {
  if (!p) return "";
  const parts = p.split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

export function dirname(p: string): string {
  if (!p) return "";
  const idx = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  return idx > 0 ? p.slice(0, idx) : p;
}

/** 备份名里的时间戳：`app.20261118-105500.jar` → `app.jar`（与后端 original_name_from_backup 对齐） */
export function originalJarName(name: string): string {
  const m = name.match(/^(.+)\.(\d{8}-\d{6})\.(jar|war)$/i);
  return m ? `${m[1]}.${m[3]}` : name;
}

export function lineLevel(line: string): string | null {
  const l = line.toLowerCase();
  if (l.includes("error") || l.includes("exception") || l.includes("fatal")) return "error";
  if (l.includes("warn")) return "warn";
  if (l.includes("info")) return "info";
  if (l.includes("debug")) return "debug";
  if (l.includes("trace")) return "trace";
  return null;
}

export function shaShort(s: string): string {
  let h = 0;
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) | 0;
  return Math.abs(h).toString(36);
}

export function uid(): string {
  return `id-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

/** 把后端抛出的底层错误（尤其是 Windows 的错误码）翻译成能看懂的提示 */
export function errText(e: unknown): string {
  const raw = typeof e === "string" ? e : e instanceof Error ? e.message : String(e ?? "");
  if (/0x80070005|-2147024891/.test(raw) || /os error 5\b/.test(raw)) {
    return `${raw} —— 拒绝访问：多半是被安全软件或系统策略拦截了，可以把 JarBox 加入白名单，或右键「以管理员身份运行」后重试`;
  }
  return raw;
}
