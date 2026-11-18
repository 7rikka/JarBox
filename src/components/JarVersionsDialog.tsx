import { useEffect, useState } from "react";
import { api } from "../api";
import type { JarBackups, ServiceView } from "../types";
import { errText, formatBytes, formatTime } from "../utils";
import { ClockIcon, FolderIcon, RefreshIcon } from "./Icons";
import { Modal } from "./Modal";

export function JarVersionsDialog({
  service,
  busy,
  onClose,
  onRollback,
}: {
  service: ServiceView;
  busy: boolean;
  onClose: () => void;
  onRollback: (name: string) => void;
}) {
  const [data, setData] = useState<JarBackups | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);

  async function load() {
    setLoading(true);
    setError("");
    try {
      setData(await api.listJarBackups(service.id));
    } catch (e) {
      setError(errText(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [service.id]);

  const items = data?.items ?? [];

  return (
    <Modal
      title="历史 JAR 版本"
      subtitle={service.name}
      icon={<ClockIcon size={18} />}
      onClose={onClose}
      width={720}
      footer={
        <>
          <div className="foot-left">
            <span className="muted">
              回滚时当前包也会存进 bak，来回切换不会丢版本
            </span>
          </div>
          <button className="btn ghost" onClick={onClose}>
            关闭
          </button>
        </>
      }
    >
      <div className="ver-current">
        <div className="ver-badge">当前</div>
        <div className="ver-main">
          <b>{data?.currentName || "—"}</b>
          <span className="muted">
            {data?.currentExists
              ? `${formatBytes(data.currentSize)} · ${formatTime(data.currentModified)}`
              : "文件不存在（可能已被手动删除）"}
          </span>
        </div>
      </div>

      {error && <div className="field-hint warn-text">{error}</div>}

      <div className="ver-list">
        {loading && <div className="empty-mini">正在读取备份目录…</div>}
        {!loading && items.length === 0 && (
          <div className="empty-mini">
            还没有历史版本。替换一次 JAR 后，旧包会自动备份到这里。
          </div>
        )}
        {items.map((it) => (
          <div className="ver-row" key={it.name}>
            <div className="ver-main">
              <b title={it.name}>{it.name}</b>
              <span className="muted">
                {formatBytes(it.size)} · {formatTime(it.modified)}
              </span>
            </div>
            <button
              className="btn outline"
              disabled={busy}
              onClick={() => onRollback(it.name)}
              title={service.status === "running" ? "会先停止服务，切换后自动重启" : "切换为当前 JAR"}
            >
              {busy ? "切换中…" : "切换到此版本"}
            </button>
          </div>
        ))}
      </div>

      <div className="ver-foot">
        <button
          className="btn ghost"
          onClick={() => api.openPath(data?.dir || "", true).catch(() => undefined)}
          disabled={!data?.dir}
        >
          <FolderIcon size={15} /> 打开备份目录
        </button>
        <button className="btn ghost" onClick={() => void load()} disabled={loading}>
          <RefreshIcon size={15} /> 刷新
        </button>
        <span className="muted">
          只保留最近 {data?.keep ?? 0} 个备份（可在设置里调整）
        </span>
      </div>
    </Modal>
  );
}
