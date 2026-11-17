import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import type { LogFileInfo, LogResult, ServiceView } from "../types";
import { formatBytes, formatTime, lineLevel } from "../utils";
import { Modal } from "./Modal";
import { CopyIcon, FolderIcon, RefreshIcon, SearchIcon, TerminalIcon } from "./Icons";

const LEVELS = [
  { key: "error", label: "错误" },
  { key: "warn", label: "警告" },
  { key: "info", label: "信息" },
  { key: "debug", label: "调试" },
  { key: "trace", label: "跟踪" },
];

/** 日志正文字号档位；存 localStorage，下次打开还是这个字号 */
const FONT_SIZES = [
  { size: 12, label: "小" },
  { size: 13, label: "标准" },
  { size: 14, label: "大" },
  { size: 16, label: "特大" },
  { size: 18, label: "超大" },
];
const FONT_KEY = "jarbox.logFontSize";
const DEFAULT_FONT = 13;

function readFontSize(): number {
  const raw = Number(localStorage.getItem(FONT_KEY));
  return FONT_SIZES.some((f) => f.size === raw) ? raw : DEFAULT_FONT;
}

export function LogViewer({
  service,
  onClose,
  notify,
}: {
  service: ServiceView;
  onClose: () => void;
  notify: (msg: string, type?: "info" | "error") => void;
}) {
  const [files, setFiles] = useState<LogFileInfo[]>([]);
  const [file, setFile] = useState("");
  const [keyword, setKeyword] = useState("");
  const [applied, setApplied] = useState("");
  const [levels, setLevels] = useState<string[]>([]);
  const [limit, setLimit] = useState(2000);
  const [data, setData] = useState<LogResult | null>(null);
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [autoScroll, setAutoScroll] = useState(true);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [fontSize, setFontSize] = useState<number>(readFontSize);
  // 鼠标在日志里选中了内容时，暂停刷新，避免文字被换掉导致选区丢失
  const [hasSelection, setHasSelection] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const pinnedRef = useRef(true);
  const selRef = useRef(false);

  const running = service.status === "running";

  const loadFiles = useCallback(async () => {
    try {
      const list = await api.listLogs(service.id);
      setFiles(list);
      setFile((prev) => {
        if (prev && list.some((f) => f.name === prev)) return prev;
        const cur = list.find((f) => f.isCurrent) || list[0];
        return cur ? cur.name : "";
      });
    } catch (e) {
      setError(String(e));
    }
  }, [service.id]);

  useEffect(() => {
    void loadFiles();
  }, [loadFiles, running]);

  const fetchLog = useCallback(
    async (withLoading = false) => {
      if (!file) {
        setData(null);
        return;
      }
      if (withLoading) setLoading(true);
      try {
        const r = await api.readLog({
          serviceId: service.id,
          file,
          keyword: applied,
          levels,
          limit,
        });
        setData(r);
        setError("");
      } catch (e) {
        setError(String(e));
      } finally {
        if (withLoading) setLoading(false);
      }
    },
    [service.id, file, applied, levels, limit]
  );

  useEffect(() => {
    void fetchLog(true);
  }, [fetchLog]);

  useEffect(() => {
    if (!autoRefresh) return;
    const t = setInterval(() => {
      if (document.hidden) return;
      // 有选区就先不刷新，等用户复制/取消选中
      if (selRef.current) return;
      void fetchLog(false);
      if (running) void loadFiles();
    }, 1500);
    return () => clearInterval(t);
  }, [autoRefresh, fetchLog, running, loadFiles]);

  useEffect(() => {
    localStorage.setItem(FONT_KEY, String(fontSize));
  }, [fontSize]);

  useEffect(() => {
    selRef.current = hasSelection;
  }, [hasSelection]);

  useEffect(() => {
    const onSelect = () => {
      const sel = window.getSelection();
      const body = scrollRef.current;
      const inside =
        !!sel &&
        !!body &&
        sel.rangeCount > 0 &&
        !sel.isCollapsed &&
        (body.contains(sel.anchorNode) || body.contains(sel.focusNode));
      setHasSelection(Boolean(inside));
    };
    document.addEventListener("selectionchange", onSelect);
    return () => document.removeEventListener("selectionchange", onSelect);
  }, []);

  useEffect(() => {
    const t = setTimeout(() => setApplied(keyword.trim()), 280);
    return () => clearTimeout(t);
  }, [keyword]);

  useEffect(() => {
    if (autoScroll && pinnedRef.current && scrollRef.current) {
      const el = scrollRef.current;
      el.scrollTop = el.scrollHeight;
    }
  }, [data, autoScroll]);

  const counts = data?.counts;
  const total = data?.total || 0;

  const visibleLines = useMemo(() => data?.lines || [], [data]);

  const stats = [
    { label: "总行数", value: total.toLocaleString() },
    { label: "匹配", value: (data?.matched || 0).toLocaleString() },
    { label: "文件", value: data ? formatBytes(data.size) : "—" },
  ];

  return (
    <Modal
      title={`运行日志 · ${service.name}`}
      subtitle={
        running
          ? "服务运行中，日志实时刷新中"
          : "服务已停止，可查看历史日志并筛选"
      }
      icon={<TerminalIcon size={18} />}
      onClose={onClose}
      width={1180}
      className="modal-wide"
    >
      <div className="log-layout">
        <aside className="log-files">
          <div className="log-files-head">
            <span>日志文件</span>
            <button className="icon-btn sm" onClick={() => void loadFiles()} title="刷新列表">
              <RefreshIcon size={14} />
            </button>
          </div>
          <div className="log-file-list">
            {files.length === 0 && <div className="empty-mini">暂无日志</div>}
            {files.map((f) => (
              <button
                key={f.name}
                className={`log-file ${f.name === file ? "active" : ""}`}
                onClick={() => setFile(f.name)}
                title={f.path}
              >
                <span className="lf-name">{f.name}</span>
                <span className="lf-meta">
                  {formatBytes(f.size)}
                  {f.isCurrent && <em className="live-tag">运行中</em>}
                </span>
                <span className="lf-time">{formatTime(f.modified)}</span>
              </button>
            ))}
          </div>
          <button
            className="btn ghost block"
            onClick={() => void api.openPath(service.logDir, true)}
          >
            <FolderIcon size={15} /> 打开日志目录
          </button>
        </aside>

        <section className="log-main">
          <div className="log-toolbar">
            <div className="search-box">
              <SearchIcon size={15} />
              <input
                value={keyword}
                placeholder="筛选关键字，如 ERROR / OutOfMemory / 端口"
                onChange={(e) => setKeyword(e.target.value)}
              />
              {keyword && (
                <button className="link-btn" onClick={() => setKeyword("")}>
                  清空
                </button>
              )}
            </div>
            <div className="level-chips">
              <button
                className={`lchip all ${levels.length === 0 ? "on" : ""}`}
                onClick={() => setLevels([])}
              >
                全部 <b>{total.toLocaleString()}</b>
              </button>
              {LEVELS.map((l) => {
                const on = levels.includes(l.key);
                const count = counts ? (counts as any)[l.key] : 0;
                return (
                  <button
                    key={l.key}
                    className={`lchip ${l.key} ${on ? "on" : ""}`}
                    onClick={() =>
                      setLevels((prev) =>
                        prev.includes(l.key)
                          ? prev.filter((x) => x !== l.key)
                          : [...prev, l.key]
                      )
                    }
                  >
                    {l.label} <b>{count}</b>
                  </button>
                );
              })}
            </div>
            <div className="toolbar-right">
              <select
                value={limit}
                onChange={(e) => setLimit(Number(e.target.value))}
                title="读取最近多少行"
              >
                <option value={500}>最近 500 行</option>
                <option value={1000}>最近 1000 行</option>
                <option value={2000}>最近 2000 行</option>
                <option value={5000}>最近 5000 行</option>
              </select>
              <select
                value={fontSize}
                onChange={(e) => setFontSize(Number(e.target.value))}
                title="日志字号"
              >
                {FONT_SIZES.map((f) => (
                  <option key={f.size} value={f.size}>
                    字号 {f.label}
                  </option>
                ))}
              </select>
              <label className="switch">
                <input
                  type="checkbox"
                  checked={autoRefresh}
                  onChange={(e) => setAutoRefresh(e.target.checked)}
                />
                <span />
                实时
              </label>
              <label className="switch">
                <input
                  type="checkbox"
                  checked={autoScroll}
                  onChange={(e) => setAutoScroll(e.target.checked)}
                />
                <span />
                跟随
              </label>
              <button
                className="icon-btn sm"
                title={hasSelection ? "复制选中的日志" : "复制当前显示的全部日志"}
                onClick={async () => {
                  const sel = window.getSelection();
                  const picked =
                    hasSelection && sel && !sel.isCollapsed ? sel.toString() : "";
                  const text = picked || visibleLines.join("\n");
                  try {
                    await navigator.clipboard.writeText(text);
                    notify(picked ? "已复制选中的日志" : "日志已复制到剪贴板");
                  } catch {
                    notify("复制失败", "error");
                  }
                }}
              >
                <CopyIcon size={15} />
              </button>
            </div>
          </div>

          <div
            className="log-body"
            ref={scrollRef}
            tabIndex={0}
            style={{ ["--log-fs" as string]: `${fontSize}px` }}
            onKeyDown={(e) => {
              if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
                e.preventDefault();
                const body = scrollRef.current;
                if (!body) return;
                const range = document.createRange();
                range.selectNodeContents(body);
                const sel = window.getSelection();
                sel?.removeAllRanges();
                sel?.addRange(range);
              }
            }}
            onScroll={(e) => {
              const el = e.currentTarget;
              pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
            }}
          >
            {loading && !data && <div className="empty-mini">加载中…</div>}
            {error && <div className="empty-mini error">{error}</div>}
            {!error && data && visibleLines.length === 0 && (
              <div className="empty-mini">
                {total === 0 ? "日志为空，启动服务后会自动写入" : "没有符合筛选条件的日志"}
              </div>
            )}
            {!error &&
              visibleLines.map((line, i) => {
                const lvl = lineLevel(line);
                return (
                  <div className={`log-line ${lvl || ""}`} key={i}>
                    <span className="ln">{i + 1}</span>
                    <span className="lt">{line || " "}</span>
                  </div>
                );
              })}
          </div>

          <div className="log-foot">
            <div className="log-stats">
              {stats.map((s) => (
                <span key={s.label}>
                  {s.label} <b>{s.value}</b>
                </span>
              ))}
              {data?.truncated && (
                <span className="warn-text">仅显示匹配结果中最后 {limit} 行</span>
              )}
            </div>
            <div className="log-foot-right">
              {hasSelection && <span className="sel-tip">已选中 · 暂停刷新</span>}
              <span className="muted">{data ? formatBytes(data.size) : ""}</span>
            </div>
          </div>
        </section>
      </div>
    </Modal>
  );
}
