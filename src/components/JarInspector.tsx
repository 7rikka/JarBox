import { useEffect, useMemo, useState } from "react";
import { api } from "../api";
import type { JarEntry, JarEntryContent, ServiceView } from "../types";
import { basename, formatBytes } from "../utils";
import { Modal } from "./Modal";
import { CopyIcon, FileIcon, SearchIcon } from "./Icons";

const KIND_LABEL: Record<string, string> = {
  config: "配置",
  class: "类",
  lib: "依赖",
  other: "其他",
  dir: "目录",
};

const PREFERRED = [
  "application.yml",
  "application.yaml",
  "application.properties",
  "bootstrap.yml",
  "bootstrap.yaml",
  "bootstrap.properties",
];

export function JarInspector({
  service,
  onClose,
  notify,
}: {
  service: ServiceView;
  onClose: () => void;
  notify: (msg: string, type?: "info" | "error") => void;
}) {
  const [entries, setEntries] = useState<JarEntry[]>([]);
  const [filter, setFilter] = useState("");
  const [configOnly, setConfigOnly] = useState(true);
  const [selected, setSelected] = useState<string>("");
  const [content, setContent] = useState<JarEntryContent | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [wrap, setWrap] = useState(true);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    api
      .listJarEntries(service.jarPath)
      .then((list) => {
        if (!alive) return;
        setEntries(list);
        const files = list.filter((e) => !e.isDir);
        const preferred =
          PREFERRED.map((p) =>
            files.find((f) => f.name.toLowerCase().endsWith(p))
          ).find(Boolean) ||
          files.find((f) => f.kind === "config") ||
          files[0];
        if (preferred) setSelected(preferred.name);
        setError("");
      })
      .catch((e) => alive && setError(String(e)))
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
  }, [service.jarPath]);

  useEffect(() => {
    if (!selected) {
      setContent(null);
      return;
    }
    let alive = true;
    api
      .readJarEntry(service.jarPath, selected)
      .then((c) => alive && setContent(c))
      .catch((e) => alive && setError(String(e)));
    return () => {
      alive = false;
    };
  }, [selected, service.jarPath]);

  const shown = useMemo(() => {
    const kw = filter.trim().toLowerCase();
    return entries.filter((e) => {
      if (e.isDir) return false;
      if (configOnly && e.kind !== "config") return false;
      if (kw && !e.name.toLowerCase().includes(kw)) return false;
      return true;
    });
  }, [entries, filter, configOnly]);

  const configCount = entries.filter((e) => e.kind === "config").length;

  return (
    <Modal
      title={`JAR 配置 · ${service.name}`}
      subtitle={basename(service.jarPath)}
      icon={<FileIcon size={18} />}
      onClose={onClose}
      width={1180}
      className="modal-wide"
    >
      <div className="jar-layout">
        <aside className="jar-list">
          <div className="jar-filter">
            <div className="search-box sm">
              <SearchIcon size={14} />
              <input
                value={filter}
                placeholder="搜索条目"
                onChange={(e) => setFilter(e.target.value)}
              />
            </div>
            <label className="checkline">
              <input
                type="checkbox"
                checked={configOnly}
                onChange={(e) => setConfigOnly(e.target.checked)}
              />
              只看配置文件
            </label>
          </div>
          <div className="jar-entries">
            {loading && <div className="empty-mini">读取 JAR 中…</div>}
            {!loading && shown.length === 0 && (
              <div className="empty-mini">没有匹配的条目</div>
            )}
            {shown.map((e) => (
              <button
                key={e.name}
                className={`jar-entry ${e.name === selected ? "active" : ""} kind-${e.kind}`}
                onClick={() => setSelected(e.name)}
                title={e.name}
              >
                <span className="je-kind">{KIND_LABEL[e.kind] || "文件"}</span>
                <span className="je-name">{e.name}</span>
                <span className="je-size">{formatBytes(e.size)}</span>
              </button>
            ))}
          </div>
          <div className="jar-summary muted">
            共 {entries.filter((e) => !e.isDir).length} 个文件 · {configCount} 个配置
          </div>
        </aside>

        <section className="jar-content">
          <div className="jar-content-head">
            <div className="jar-path" title={selected}>
              {selected || "未选择文件"}
            </div>
            <div className="jar-content-actions">
              {content && (
                <span className="muted">
                  {formatBytes(content.size)} · {content.encoding.toUpperCase()}
                </span>
              )}
              <button
                className={`btn ghost sm ${wrap ? "is-on" : ""}`}
                onClick={() => setWrap((v) => !v)}
                title="自动换行"
              >
                换行
              </button>
              <button
                className="btn ghost sm"
                disabled={!content}
                onClick={async () => {
                  if (!content) return;
                  try {
                    await navigator.clipboard.writeText(content.content);
                    notify("配置内容已复制");
                  } catch {
                    notify("复制失败", "error");
                  }
                }}
              >
                <CopyIcon size={15} /> 复制
              </button>
            </div>
          </div>
          <div className="jar-content-body">
            {error && <div className="empty-mini error">{error}</div>}
            {!error && !content && <div className="empty-mini">选择左侧文件查看内容</div>}
            {!error && content?.binary && (
              <div className="empty-mini">这是二进制文件，无法以文本方式查看。</div>
            )}
            {!error && content && !content.binary && (
              <pre className={`jar-code ${wrap ? "wrap" : ""}`}>{content.content || " "}</pre>
            )}
            {content?.truncated && (
              <div className="truncate-tip">文件较大，仅显示前 512 KB 内容</div>
            )}
          </div>
        </section>
      </div>
    </Modal>
  );
}
