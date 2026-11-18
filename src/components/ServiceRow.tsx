import { useState } from "react";
import type { ServiceStat, ServiceView } from "../types";
import { basename, formatBytes, formatDuration, formatTime } from "../utils";
import {
  ClockIcon,
  EditIcon,
  FileIcon,
  FolderIcon,
  MoreIcon,
  PlayIcon,
  RefreshIcon,
  StopIcon,
  TerminalIcon,
  TrashIcon,
} from "./Icons";

function Metric({
  label,
  value,
  ratio,
  tone,
  title,
  muted,
}: {
  label: string;
  value: string;
  ratio: number;
  tone: "cpu" | "mem";
  title?: string;
  muted?: boolean;
}) {
  return (
    <div className={`metric ${muted ? "muted" : ""}`} title={title}>
      <span className="m-label">{label}</span>
      <span className="m-val">{value}</span>
      <span className={`m-bar ${tone}`}>
        <i style={{ width: `${Math.max(0, Math.min(1, ratio)) * 100}%` }} />
      </span>
    </div>
  );
}

export function ServiceRow({
  svc,
  stat,
  jdkName,
  now,
  busy,
  onStart,
  onStop,
  onRestart,
  onReplace,
  onVersions,
  onLogs,
  onConfig,
  onLocate,
  onEdit,
  onDelete,
}: {
  svc: ServiceView;
  stat?: ServiceStat;
  jdkName: string;
  now: number;
  busy?: string;
  onStart: () => void;
  onStop: () => void;
  onRestart: () => void;
  onReplace: () => void;
  onVersions: () => void;
  onLogs: () => void;
  onConfig: () => void;
  onLocate: () => void;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const [menu, setMenu] = useState(false);
  const running = svc.status === "running";
  const crashed =
    !running && !svc.stoppedByUser && svc.exitCode !== null && svc.exitCode !== 0;
  const manuallyStopped = !running && svc.stoppedByUser;
  const warn = !svc.jarExists || !svc.jdkOk;
  const cpu = stat?.cpu ?? 0;
  const mem = stat?.memory ?? 0;

  return (
    <article className={`svc-row ${running ? "is-running" : ""}`}>
      <div className="row-ident">
        <span className={`status-dot ${running ? "on" : crashed ? "err" : "off"}`} />
        <div className="ident-text">
          <div className="ident-top">
            <h3 title={svc.note || svc.name}>{svc.name}</h3>
            <span className={`chip ${svc.jdkOk ? "" : "chip-warn"}`} title={svc.jdkPath || "未配置"}>
              {jdkName}
            </span>
          </div>
          <span className="svc-jar" title={svc.jarPath}>
            {basename(svc.jarPath)}
          </span>
        </div>
      </div>

      <div className="row-status">
        <div className="status-line">
          {running ? (
            <>
              <b className="state-ok">运行中</b>
              <span className="muted">已运行 {formatDuration(now - (svc.startedAt || now))}</span>
              <span className="pid">PID {svc.pid}</span>
              {svc.adopted && (
                <span
                  className="chip chip-adopt"
                  title="该进程在 JarBox 关闭前就已启动，本次启动后被重新接管，可正常停止"
                >
                  已接管
                </span>
              )}
            </>
          ) : crashed ? (
            <>
              <b className="state-bad">异常退出</b>
              <span className="muted">
                退出码 {svc.exitCode}
                {svc.exitedAt ? ` · ${formatTime(svc.exitedAt)}` : ""}
              </span>
            </>
          ) : manuallyStopped ? (
            <>
              <b className="state-idle">已停止</b>
              <span className="muted">
                手动停止{svc.exitedAt ? ` · ${formatTime(svc.exitedAt)}` : ""}
              </span>
            </>
          ) : (
            <>
              <b className="state-idle">未运行</b>
              {svc.note && <span className="muted ellipsis">{svc.note}</span>}
            </>
          )}
        </div>
        <div className="meta-line">
          <span
            title={
              svc.detectedPort
                ? "从最近一次运行的日志里识别到的监听端口"
                : "服务启动后会从它的运行日志里自动识别端口"
            }
          >
            端口 {svc.detectedPort ?? "—"}
          </span>
          <i />
          <span>启动于 {running || svc.startedAt ? formatTime(svc.startedAt) : "—"}</span>
          <i />
          <span className={svc.jarExists ? "" : "warn-text"}>
            {svc.jarExists ? "JAR 正常" : "JAR 丢失"}
          </span>
          {warn && <span className="warn-text">· 请检查配置</span>}
        </div>
      </div>

      <div className="row-metrics">
        <Metric
          label="CPU"
          value={running ? `${cpu.toFixed(1)}%` : "—"}
          ratio={running ? cpu / 100 : 0}
          tone="cpu"
          muted={!running}
        />
        <Metric
          label="内存"
          value={running ? formatBytes(mem) : "—"}
          ratio={running ? mem / (2 * 1024 * 1024 * 1024) : 0}
          tone="mem"
          title={running ? `物理内存 ${formatBytes(mem)} · 虚拟内存 ${formatBytes(stat?.virtualMemory || 0)}` : "服务未运行"}
          muted={!running}
        />
      </div>

      <div className="row-actions">
        {running ? (
          <>
            <button className="btn danger" onClick={onStop} disabled={busy === "stopping"}>
              <StopIcon size={14} /> {busy === "stopping" ? "停止中…" : "停止"}
            </button>
            <button
              className="tool-btn"
              onClick={onRestart}
              disabled={busy === "restarting" || busy === "stopping"}
              title="停止后重新启动，用于改了配置或替换 JAR 之后"
            >
              <RefreshIcon size={16} />
              <span>{busy === "restarting" ? "重启中…" : "重启"}</span>
            </button>
          </>
        ) : (
          <button className="btn primary" onClick={onStart} disabled={busy === "starting" || warn}>
            <PlayIcon size={14} /> {busy === "starting" ? "启动中…" : "启动"}
          </button>
        )}
        <button className="tool-btn" onClick={onLogs} title="查看运行日志">
          <TerminalIcon size={16} />
          <span>日志</span>
        </button>
        <button className="tool-btn" onClick={onLocate} title="在资源管理器中定位 JAR 文件">
          <FolderIcon size={16} />
          <span>位置</span>
        </button>
        <div className="menu-wrap">
          <button className="icon-btn" onClick={() => setMenu((v) => !v)} title="更多操作">
            <MoreIcon size={17} />
          </button>
          {menu && (
            <>
              <div className="menu-backdrop" onClick={() => setMenu(false)} />
              <div className="dropdown">
                <button
                  onClick={() => {
                    setMenu(false);
                    onEdit();
                  }}
                >
                  <EditIcon size={15} /> 编辑服务配置
                </button>
                <button
                  onClick={() => {
                    setMenu(false);
                    onReplace();
                  }}
                >
                  <FileIcon size={15} /> 替换 JAR 包…
                </button>
                <button
                  onClick={() => {
                    setMenu(false);
                    onVersions();
                  }}
                >
                  <ClockIcon size={15} /> 历史版本 / 回滚…
                </button>
                <button
                  onClick={() => {
                    setMenu(false);
                    onConfig();
                  }}
                >
                  <FileIcon size={15} /> 查看配置文件
                </button>
                <button
                  className="danger"
                  onClick={() => {
                    setMenu(false);
                    onDelete();
                  }}
                >
                  <TrashIcon size={15} /> 删除服务
                </button>
              </div>
            </>
          )}
        </div>
      </div>
    </article>
  );
}
