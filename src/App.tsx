import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { api, onAutoStart, onServiceExit } from "./api";
import type { Bootstrap, Service, ServiceStat, ServiceView, Store } from "./types";
import { basename, errText, formatDuration } from "./utils";
import { ServiceRow } from "./components/ServiceRow";
import { ServiceDialog } from "./components/ServiceDialog";
import { SettingsView } from "./components/SettingsView";
import { LogViewer } from "./components/LogViewer";
import { JarInspector } from "./components/JarInspector";
import { Modal } from "./components/Modal";
import {
  AlertIcon,
  CubeIcon,
  FolderIcon,
  PlusIcon,
  RefreshIcon,
  SlidersIcon,
  StopIcon,
  TerminalIcon,
  TrashIcon,
} from "./components/Icons";

type Toast = { id: number; msg: string; type: "info" | "error" };

function blankService(jarPath: string, jdkId: string): Service {
  return {
    id: "",
    name: jarPath ? basename(jarPath).replace(/\.(jar|war)$/i, "") : "",
    jarPath,
    jdkId,
    jvmArgs: "",
    programArgs: "",
    workDir: "",
    note: "",
    createdAt: "",
  };
}

export default function App() {
  const [boot, setBoot] = useState<Bootstrap | null>(null);
  const [services, setServices] = useState<ServiceView[]>([]);
  const [view, setView] = useState<"services" | "settings">("services");
  const [dialog, setDialog] = useState<{ service: Service | null } | undefined>();
  const [logTarget, setLogTarget] = useState<ServiceView | null>(null);
  const [jarTarget, setJarTarget] = useState<ServiceView | null>(null);
  const [busy, setBusy] = useState<Record<string, string>>({});
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [dragging, setDragging] = useState(false);
  const [confirm, setConfirm] = useState<
    | {
        title: string;
        message: string;
        danger?: boolean;
        kind?: "delete" | "stop";
        okText?: string;
        onOk: () => void;
      }
    | null
  >(null);
  const [workJars, setWorkJars] = useState<string[]>([]);
  const [stats, setStats] = useState<Record<string, ServiceStat>>({});
  const [now, setNow] = useState(Date.now());
  const [fatal, setFatal] = useState("");

  const bootRef = useRef<Bootstrap | null>(null);
  bootRef.current = boot;
  const toastId = useRef(1);

  const notify = useCallback((msg: string, type: "info" | "error" = "info") => {
    const id = toastId.current++;
    setToasts((t) => [...t, { id, msg, type }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 3600);
  }, []);

  const refreshServices = useCallback(async () => {
    try {
      setServices(await api.listServices());
    } catch {
      /* 忽略轮询错误 */
    }
  }, []);

  const refreshJars = useCallback(async () => {
    try {
      setWorkJars(await api.listWorkJars());
    } catch {
      /* ignore */
    }
  }, []);

  const loadBoot = useCallback(async () => {
    try {
      const b = await api.bootstrap();
      setBoot(b);
      setServices(b.services);
      void refreshJars();
    } catch (e) {
      setFatal(String(e));
    }
  }, [refreshJars]);

  useEffect(() => {
    void loadBoot();
  }, [loadBoot]);

  // 状态轮询
  useEffect(() => {
    const t = setInterval(() => void refreshServices(), 2000);
    return () => clearInterval(t);
  }, [refreshServices]);

  // 运行时长计时
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);

  // CPU / 内存占用采集
  useEffect(() => {
    let alive = true;
    const tick = async () => {
      try {
        const list = await api.serviceStats();
        if (!alive) return;
        const map: Record<string, ServiceStat> = {};
        list.forEach((s) => (map[s.id] = s));
        setStats(map);
      } catch {
        /* ignore */
      }
    };
    void tick();
    const t = setInterval(tick, 1600);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, []);

  // 进程退出事件
  useEffect(() => {
    let un: (() => void) | undefined;
    onServiceExit((p) => {
      notify(
        `${p.name} 已退出${p.code !== null ? `（退出码 ${p.code}）` : ""}`,
        p.code && p.code !== 0 ? "error" : "info"
      );
      void refreshServices();
    }).then((u) => (un = u));
    return () => un?.();
  }, [notify, refreshServices]);

  // 启动时自动恢复了上次运行的服务
  useEffect(() => {
    let un: (() => void) | undefined;
    onAutoStart((p) => {
      if (p.started.length > 0) {
        notify(`已恢复上次运行的 ${p.started.length} 个服务`);
      }
      if (p.failed.length > 0) {
        notify(`有 ${p.failed.length} 个服务没能自动启动：${p.failed[0]}`, "error");
      }
      void refreshServices();
    }).then((u) => (un = u));
    return () => un?.();
  }, [notify, refreshServices]);

  const importPaths = useCallback(
    async (paths: string[]) => {
      const jars = paths.filter((p) => /\.(jar|war)$/i.test(p));
      if (jars.length === 0) {
        notify("只支持导入 .jar / .war 文件", "error");
        return;
      }
      if (!bootRef.current?.store.workDir) {
        setView("settings");
        notify("请先设置工作目录，再导入 JAR", "error");
        return;
      }
      try {
        const list = await api.importJars(jars);
        notify(`已导入 ${list.length} 个 JAR，已按 JAR 名创建独立目录`);
        await refreshJars();
        setDialog({ service: blankService(list[0], bootRef.current?.store.defaultJdk || "") });
      } catch (e) {
        notify(errText(e), "error");
      }
    },
    [notify, refreshJars]
  );

  // 拖拽导入
  useEffect(() => {
    let un: (() => void) | undefined;
    let disposed = false;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        const p = event.payload;
        if (p.type === "over") setDragging(true);
        else if (p.type === "leave") setDragging(false);
        else if (p.type === "drop") {
          setDragging(false);
          void importPaths(p.paths);
        }
      })
      .then((u) => {
        if (disposed) u();
        else un = u;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      un?.();
    };
  }, [importPaths]);

  async function pickAndImport() {
    const sel = await open({
      multiple: true,
      filters: [{ name: "Java 应用包", extensions: ["jar", "war"] }],
      defaultPath: boot?.store.workDir ? `${boot.store.workDir}\\jars` : undefined,
    });
    if (!sel) return;
    await importPaths(Array.isArray(sel) ? sel : [sel]);
  }

  async function start(id: string) {
    setBusy((b) => ({ ...b, [id]: "starting" }));
    try {
      await api.startService(id);
      await refreshServices();
      notify("服务已启动");
    } catch (e) {
      notify(errText(e), "error");
    } finally {
      setBusy((b) => {
        const n = { ...b };
        delete n[id];
        return n;
      });
    }
  }

  async function stop(id: string) {
    setBusy((b) => ({ ...b, [id]: "stopping" }));
    try {
      await api.stopService(id);
      setTimeout(() => void refreshServices(), 600);
      await refreshServices();
      notify("已发送停止指令");
    } catch (e) {
      notify(errText(e), "error");
    } finally {
      setBusy((b) => {
        const n = { ...b };
        delete n[id];
        return n;
      });
    }
  }

  async function handleSubmit(svc: Service, startAfter: boolean) {
    const saved = await api.upsertService(svc);
    await refreshServices();
    setDialog(undefined);
    void refreshJars();
    notify(svc.id ? "服务配置已更新" : "服务创建成功");
    if (startAfter) await start(saved.id);
  }

  function askStop(svc: ServiceView) {
    const uptime = svc.startedAt
      ? `它已运行 ${formatDuration(Date.now() - svc.startedAt)}，`
      : "";
    setConfirm({
      title: "停止服务",
      message: `确定停止「${svc.name}」吗？${uptime}Java 进程会被直接结束，正在处理的请求会中断，未落盘的数据可能丢失。`,
      danger: true,
      kind: "stop",
      okText: "停止服务",
      onOk: () => void stop(svc.id),
    });
  }

  function askDelete(svc: ServiceView) {
    setConfirm({
      title: "删除服务",
      message: `确定删除「${svc.name}」吗？服务的启动配置会被移除，同时清理它的历史日志；JAR 文件本身不会被删除。`,
      danger: true,
      kind: "delete",
      okText: "确认删除",
      onOk: async () => {
        try {
          await api.deleteService(svc.id, true);
          await refreshServices();
          notify("服务已删除");
        } catch (e) {
          notify(errText(e), "error");
        }
      },
    });
  }

  const handleStoreSaved = (s: Store, autostartEnabled: boolean) => {
    // 设置是即时保存的，可能一秒内改好几次；只有工作目录变了才需要重扫一遍 jar
    const dirChanged = !boot || boot.store.workDir !== s.workDir;
    setBoot((b) => (b ? { ...b, store: s, autostartEnabled } : b));
    if (dirChanged) void refreshJars();
  };

  if (fatal) {
    return (
      <div className="boot">
        <div className="boot-card">
          <AlertIcon size={26} />
          <h1>JarBox 启动失败</h1>
          <p>{fatal}</p>
        </div>
      </div>
    );
  }

  if (!boot) {
    return (
      <div className="boot">
        <div className="boot-card">
          <div className="brand-mark lg">J</div>
          <p>正在加载…</p>
        </div>
      </div>
    );
  }

  const runningCount = services.filter((s) => s.status === "running").length;
  const jdkName = (id: string) => boot.store.jdks.find((j) => j.id === id)?.name || "未绑定";

  return (
    <div className="app">
      <aside className="rail">
        <div className="brand">
          <div className="brand-mark">J</div>
          <div className="brand-text">
            <b>JarBox</b>
            <span>JAR 服务管理器</span>
          </div>
        </div>

        <nav className="rail-nav">
          <button
            className={view === "services" ? "active" : ""}
            onClick={() => setView("services")}
          >
            <CubeIcon size={17} /> 服务列表
            <em className="nav-count">{services.length}</em>
          </button>
          <button
            className={view === "settings" ? "active" : ""}
            onClick={() => setView("settings")}
          >
            <SlidersIcon size={17} /> 设置
          </button>
        </nav>

        <div className="rail-stats">
          <div className="rail-stat">
            <span className={`status-dot ${runningCount > 0 ? "on" : "off"}`} />
            <b>{runningCount}</b> 个运行中
          </div>
          <div className="rail-stat">
            <CubeIcon size={13} /> JDK {boot.store.jdks.length} 个
          </div>
          <div className="rail-stat" title={boot.store.workDir}>
            <FolderIcon size={13} />
            {boot.store.workDir ? "工作目录已配置" : "未配置工作目录"}
          </div>
          <div className="rail-stat" title={boot.logsRoot}>
            <TerminalIcon size={13} /> 日志本地留存
          </div>
        </div>

        <div className="rail-footer">
          <div className="tips">关闭窗口会最小化到托盘，服务继续运行</div>
          <div className="ver">JarBox v{boot.version}</div>
        </div>
      </aside>

      <main className="main">
        {view === "services" ? (
          <>
            <header className="topbar">
              <div className="topbar-title">
                <h1>服务列表</h1>
                <p>
                  {services.length} 个服务 · {runningCount} 个运行中
                  {boot.store.workDir && (
                    <>
                      {" · "}
                      <span
                        className="path-link"
                        title={boot.store.workDir}
                        onClick={() =>
                          api.openPath(boot.store.workDir, true).catch((e) => notify(errText(e), "error"))
                        }
                      >
                        {boot.store.workDir}
                      </span>
                    </>
                  )}
                </p>
              </div>
              <div className="topbar-actions">
                <button className="btn ghost" onClick={() => void refreshServices()}>
                  <RefreshIcon size={15} /> 刷新
                </button>
                <button className="btn ghost" onClick={pickAndImport}>
                  <PlusIcon size={15} /> 导入 JAR
                </button>
                <button
                  className="btn primary"
                  onClick={() =>
                    setDialog({
                      service: blankService("", boot.store.defaultJdk || boot.store.jdks[0]?.id || ""),
                    })
                  }
                >
                  <PlusIcon size={15} /> 新建服务
                </button>
              </div>
            </header>

            <div className="content">
              {services.length === 0 ? (
                <div className="empty-state">
                  <div className="empty-mark">
                    <CubeIcon size={30} />
                  </div>
                  <h2>还没有任何服务</h2>
                  <p>
                    把 JAR 文件直接拖到窗口里，或点击「导入 JAR」，JarBox 会在工作目录下为它建一个独立目录并生成服务配置。
                  </p>
                  <div className="empty-actions">
                    <button className="btn primary" onClick={pickAndImport}>
                      <PlusIcon size={15} /> 导入 JAR 文件
                    </button>
                    <button className="btn ghost" onClick={() => setView("settings")}>
                      <SlidersIcon size={15} /> 先配置 JDK 与工作目录
                    </button>
                  </div>
                  <div className="empty-foot">
                    工作目录：{boot.store.workDir || "未配置"} · JDK：{boot.store.jdks.length} 个
                  </div>
                </div>
              ) : (
                <div className="svc-list">
                  {services.map((s) => (
                    <ServiceRow
                      key={s.id}
                      svc={s}
                      stat={stats[s.id]}
                      jdkName={jdkName(s.jdkId)}
                      now={now}
                      busy={busy[s.id]}
                      onStart={() => void start(s.id)}
                      onStop={() => askStop(s)}
                      onLogs={() => setLogTarget(s)}
                      onConfig={() => setJarTarget(s)}
                      onLocate={() =>
                        api.openPath(s.jarPath, true).catch((e) => notify(errText(e), "error"))
                      }
                      onEdit={() => setDialog({ service: s })}
                      onDelete={() => askDelete(s)}
                    />
                  ))}
                </div>
              )}
            </div>
          </>
        ) : (
          <>
            <header className="topbar">
              <div className="topbar-title">
                <h1>设置</h1>
                <p>JDK 环境、工作目录与启动行为</p>
              </div>
            </header>
            <div className="content">
              <SettingsView
                boot={boot}
                onSaved={handleStoreSaved}
                notify={notify}
                onServicesChanged={() => {
                  void refreshServices();
                  void refreshJars();
                }}
              />
            </div>
          </>
        )}
      </main>

      {dragging && (
        <div className="drop-overlay">
          <div className="drop-box">
            <CubeIcon size={34} />
            <h2>松开以导入 JAR</h2>
            <p>文件会被复制到工作目录下以 JAR 命名的独立目录，并自动进入新建服务流程</p>
          </div>
        </div>
      )}

      <div className="toasts">
        {toasts.map((t) => (
          <div key={t.id} className={`toast ${t.type}`}>
            {t.type === "error" ? <AlertIcon size={15} /> : <span className="tick" />}
            {t.msg}
          </div>
        ))}
      </div>

      {dialog && (
        <ServiceDialog
          initial={dialog.service}
          jdks={boot.store.jdks}
          workDir={boot.store.workDir}
          workJars={workJars}
          defaultJdk={boot.store.defaultJdk}
          onClose={() => setDialog(undefined)}
          onSubmit={handleSubmit}
          onGoSettings={() => {
            setDialog(undefined);
            setView("settings");
          }}
        />
      )}

      {logTarget && (
        <LogViewer
          service={logTarget}
          notify={notify}
          onClose={() => setLogTarget(null)}
        />
      )}

      {jarTarget && (
        <JarInspector
          service={jarTarget}
          notify={notify}
          onClose={() => setJarTarget(null)}
        />
      )}

      {confirm && (
        <Modal
          title={confirm.title}
          icon={confirm.kind === "stop" ? <StopIcon size={17} /> : <TrashIcon size={17} />}
          onClose={() => setConfirm(null)}
          width={470}
          footer={
            <>
              <div className="foot-left" />
              <button className="btn ghost" onClick={() => setConfirm(null)}>
                取消
              </button>
              <button
                className={`btn ${confirm.danger ? "danger" : "primary"}`}
                onClick={() => {
                  confirm.onOk();
                  setConfirm(null);
                }}
              >
                {confirm.okText ?? "确认"}
              </button>
            </>
          }
        >
          <p className="confirm-text">{confirm.message}</p>
        </Modal>
      )}
    </div>
  );
}
