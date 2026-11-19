import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "../api";
import type { Bootstrap, JavaInfo, JdkConfig, Store } from "../types";
import { basename, errText, uid } from "../utils";
import {
  CheckIcon,
  CopyIcon,
  CubeIcon,
  FileIcon,
  FolderIcon,
  InfoIcon,
  PlusIcon,
  RefreshIcon,
  SlidersIcon,
  TrashIcon,
} from "./Icons";

type Patch = {
  workDir?: string;
  jdks?: JdkConfig[];
  autostart?: boolean;
  stopOnExit?: boolean;
  resumeOnStart?: boolean;
  importMode?: "copy" | "move";
  keepJarVersions?: number;
  defaultJdk?: string;
};

const KEEP_OPTIONS = [5, 10, 20, 30, 40, 50];

export function SettingsView({
  boot,
  onSaved,
  notify,
  onServicesChanged,
}: {
  boot: Bootstrap;
  onSaved: (s: Store, autostartEnabled: boolean) => void;
  notify: (msg: string, type?: "info" | "error") => void;
  onServicesChanged: () => void;
}) {
  const [workDir, setWorkDir] = useState(boot.store.workDir);
  const [jdks, setJdks] = useState<JdkConfig[]>(boot.store.jdks);
  const [defaultJdk, setDefaultJdk] = useState(boot.store.defaultJdk);
  const [autostart, setAutostart] = useState(boot.store.autostart);
  const [stopOnExit, setStopOnExit] = useState(boot.store.stopOnExit);
  const [resumeOnStart, setResumeOnStart] = useState(boot.store.resumeOnStart);
  const [importMode, setImportMode] = useState<"copy" | "move">(boot.store.importMode ?? "copy");
  const [keepJarVersions, setKeepJarVersions] = useState(boot.store.keepJarVersions ?? 10);
  const [newPath, setNewPath] = useState("");
  const [versions, setVersions] = useState<Record<string, JavaInfo>>({});
  const [scanning, setScanning] = useState(false);
  const [status, setStatus] = useState<"idle" | "saving" | "saved">("idle");
  const [savedAt, setSavedAt] = useState("");
  /** 串行化：连点两下开关时也要按顺序落盘，避免后一次被前一次覆盖 */
  const chain = useRef<Promise<void>>(Promise.resolve());

  useEffect(() => {
    jdks.forEach((j) => {
      if (versions[j.path] !== undefined) return;
      api
        .detectJava(j.path)
        .then((info) => setVersions((v) => ({ ...v, [j.path]: info })))
        .catch(() =>
          setVersions((v) => ({
            ...v,
            [j.path]: { ok: false, version: "", binary: "", message: "检测失败" },
          }))
        );
    });
  }, [jdks]);

  useEffect(() => {
    if (status !== "saved") return;
    const timer = setTimeout(() => setStatus("idle"), 2500);
    return () => clearTimeout(timer);
  }, [status]);

  /** 保存失败时把界面拉回后端的真实值，避免显示的和存下来的不一致 */
  async function reload() {
    try {
      const b = await api.bootstrap();
      setWorkDir(b.store.workDir);
      setJdks(b.store.jdks);
      setDefaultJdk(b.store.defaultJdk);
      setAutostart(b.store.autostart);
      setStopOnExit(b.store.stopOnExit);
      setResumeOnStart(b.store.resumeOnStart);
      setImportMode(b.store.importMode ?? "copy");
      setKeepJarVersions(b.store.keepJarVersions ?? 10);
      onSaved(b.store, b.autostartEnabled);
    } catch {
      /* 拉取都失败就只能保持现状，别再弹错 */
    }
  }

  /**
   * 即时保存：只提交改动的那一项。
   * 服务端返回后仅回填 patch 里涉及到的字段，避免打断用户正在别的输入框里打字。
   */
  function commit(patch: Patch) {
    chain.current = chain.current.then(async () => {
      setStatus("saving");
      try {
        const res = await api.updateSettings(patch);
        onSaved(res.store, res.autostartEnabled);

        if (patch.workDir !== undefined) setWorkDir(res.store.workDir);
        if (patch.jdks !== undefined) setJdks(res.store.jdks);
        if (patch.jdks !== undefined || patch.defaultJdk !== undefined) {
          setDefaultJdk(res.store.defaultJdk);
        }
        if (patch.autostart !== undefined) setAutostart(res.store.autostart);
        if (patch.stopOnExit !== undefined) setStopOnExit(res.store.stopOnExit);
        if (patch.resumeOnStart !== undefined) setResumeOnStart(res.store.resumeOnStart);
        if (patch.importMode !== undefined) setImportMode(res.store.importMode ?? "copy");
        if (patch.keepJarVersions !== undefined) setKeepJarVersions(res.store.keepJarVersions ?? 10);

        if (res.autostartWarning) {
          notify(`开机自启没生效：${res.autostartWarning}`, "error");
        }
        setSavedAt(
          new Date().toLocaleTimeString("zh-CN", { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" })
        );
        setStatus("saved");
        if (patch.workDir !== undefined) onServicesChanged();
      } catch (e) {
        setStatus("idle");
        notify(errText(e), "error");
        await reload();
      }
    });
  }

  /** 工作目录：输入框失去焦点/回车时提交，不必逐字符写盘 */
  function commitWorkDir() {
    const next = workDir.trim();
    if (next === boot.store.workDir.trim()) return;
    commit({ workDir: next });
  }

  function jdkListChanged(): boolean {
    if (jdks.length !== boot.store.jdks.length) return true;
    return jdks.some((j) => {
      const old = boot.store.jdks.find((x) => x.id === j.id);
      return !old || old.name !== j.name || old.path !== j.path;
    });
  }

  async function pickWorkDir() {
    const sel = await open({ directory: true });
    if (typeof sel === "string") {
      setWorkDir(sel);
      commit({ workDir: sel.trim() });
    }
  }

  async function addJdk(path: string) {
    const p = path.trim();
    if (!p) return;
    if (jdks.some((j) => j.path.toLowerCase() === p.toLowerCase())) {
      notify("该 JDK 已在列表中", "error");
      return;
    }
    const info = await api.detectJava(p);
    if (!info.ok) {
      notify(`不是有效的 JDK：${info.message}`, "error");
      return;
    }
    const jdk: JdkConfig = {
      id: uid(),
      name: basename(p) || "JDK",
      path: p,
    };
    const next = [...jdks, jdk];
    setJdks(next);
    setVersions((v) => ({ ...v, [p]: info }));
    // 原来没有默认 JDK 时，后端会把第一个设为默认，这里同步一下界面
    if (!defaultJdk) setDefaultJdk(jdk.id);
    setNewPath("");
    notify("已添加 JDK");
    commit({ jdks: next });
  }

  function removeJdk(id: string) {
    const next = jdks.filter((x) => x.id !== id);
    setJdks(next);
    if (defaultJdk === id) setDefaultJdk("");
    commit({ jdks: next });
  }

  function setDefault(id: string) {
    if (id === defaultJdk) return;
    setDefaultJdk(id);
    commit({ defaultJdk: id });
  }

  async function scan() {
    setScanning(true);
    try {
      const found = await api.scanJdks();
      const fresh = found.filter(
        (f) => !jdks.some((j) => j.path.toLowerCase() === f.path.toLowerCase())
      );
      if (fresh.length === 0) {
        notify("没有发现新的 JDK");
      } else {
        const next = [...jdks, ...fresh];
        setJdks(next);
        if (!defaultJdk && fresh[0]) setDefaultJdk(fresh[0].id);
        notify(`自动发现 ${fresh.length} 个 JDK`);
        commit({ jdks: next });
      }
    } catch (e) {
      notify(errText(e), "error");
    } finally {
      setScanning(false);
    }
  }

  // 空闲时不显示任何东西：设置是即时保存的，常驻一条提示只是干扰
  const statusText = status === "saving" ? "保存中…" : `已保存 · ${savedAt}`;

  return (
    <div className="settings">
      <section className="panel">
        <header className="panel-head">
          <div className="panel-title">
            <span className="panel-ico">
              <FolderIcon size={16} />
            </span>
            <div>
              <h2>工作目录</h2>
              <p>每个 JAR 会在该目录下拥有独立文件夹，运行日志也写在该文件夹的 logs 子目录</p>
            </div>
          </div>
        </header>
        <div className="panel-body">
          <div className="input-group">
            <input
              value={workDir}
              placeholder="例如：D:\jar-services"
              onChange={(e) => setWorkDir(e.target.value)}
              onBlur={commitWorkDir}
              onKeyDown={(e) => {
                if (e.key === "Enter") (e.target as HTMLInputElement).blur();
              }}
            />
            <button className="btn ghost" onClick={pickWorkDir}>
              <FolderIcon size={15} /> 选择目录
            </button>
            {workDir && (
              <button
                className="btn ghost"
                onClick={() => api.openPath(workDir, true).catch((e) => notify(errText(e), "error"))}
              >
                打开
              </button>
            )}
          </div>
          {workDir && (
            <div className="field-hint">
              导入后结构：
              {`${workDir.replace(/[\\/]+$/, "")}\\服务名\\服务名.jar`}
              ，运行日志写在同目录的 logs 文件夹中
            </div>
          )}
        </div>
      </section>

      <section className="panel">
        <header className="panel-head">
          <div className="panel-title">
            <span className="panel-ico">
              <FileIcon size={16} />
            </span>
            <div>
              <h2>导入与备份</h2>
              <p>导入时怎么处理源文件，以及替换 JAR 时保留多少个历史版本</p>
            </div>
          </div>
        </header>
        <div className="panel-body">
          <div className="toggle-row">
            <div>
              <b>导入 JAR 时</b>
              <span>
                复制：保留原位置的文件（默认）。移动：导入成功后删除原文件，适合把散落各处的包收拢到工作目录。
              </span>
            </div>
            <div className="seg">
              <button
                className={`chip ${importMode === "copy" ? "chip-primary" : ""}`}
                onClick={() => {
                  if (importMode === "copy") return;
                  setImportMode("copy");
                  commit({ importMode: "copy" });
                }}
              >
                复制
              </button>
              <button
                className={`chip ${importMode === "move" ? "chip-primary" : ""}`}
                onClick={() => {
                  if (importMode === "move") return;
                  setImportMode("move");
                  commit({ importMode: "move" });
                }}
              >
                移动
              </button>
            </div>
          </div>
          <div className="toggle-row">
            <div>
              <b>保留历史版本 JAR 个数</b>
              <span>
                替换或回滚时旧包会备份到 JAR 同目录的 <code>bak</code> 文件夹，超过这个数量自动清理最旧的。
                在服务行的「历史版本 / 回滚」里可以随时切回去。
              </span>
            </div>
            <select
              className="select-mini"
              value={keepJarVersions}
              onChange={(e) => {
                const n = Number(e.target.value);
                setKeepJarVersions(n);
                commit({ keepJarVersions: n });
              }}
            >
              {!KEEP_OPTIONS.includes(keepJarVersions) && (
                <option value={keepJarVersions}>{keepJarVersions} 个</option>
              )}
              {KEEP_OPTIONS.map((n) => (
                <option key={n} value={n}>
                  {n} 个
                </option>
              ))}
            </select>
          </div>
        </div>
      </section>

      <section className="panel">
        <header className="panel-head">
          <div className="panel-title">
            <span className="panel-ico">
              <CubeIcon size={16} />
            </span>
            <div>
              <h2>JDK 环境</h2>
              <p>可配置多个 JDK，每个服务启动时单独指定</p>
            </div>
          </div>
          <button className="btn ghost" onClick={scan} disabled={scanning}>
            <RefreshIcon size={15} /> {scanning ? "探测中…" : "自动探测本机 JDK"}
          </button>
        </header>
        <div className="panel-body">
          <div className="jdk-list">
            {jdks.length === 0 && <div className="empty-mini">还没有 JDK，先添加一个吧</div>}
            {jdks.map((j) => {
              const info = versions[j.path];
              return (
                <div className={`jdk-row ${defaultJdk === j.id ? "is-default" : ""}`} key={j.id}>
                  <div className="jdk-main">
                    <input
                      className="jdk-name"
                      value={j.name}
                      onChange={(e) =>
                        setJdks((list) =>
                          list.map((x) => (x.id === j.id ? { ...x, name: e.target.value } : x))
                        )
                      }
                      onBlur={() => {
                        if (jdkListChanged()) commit({ jdks });
                      }}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                      }}
                    />
                    <div className="jdk-path" title={j.path}>
                      {j.path}
                    </div>
                    <div className={`jdk-ver ${info?.ok ? "ok" : info ? "bad" : ""}`}>
                      {info
                        ? info.ok
                          ? info.version
                          : info.message
                        : "检测中…"}
                    </div>
                  </div>
                  <div className="jdk-actions">
                    <button
                      className={`chip ${defaultJdk === j.id ? "chip-primary" : ""}`}
                      onClick={() => setDefault(j.id)}
                      title="新建服务时默认使用"
                    >
                      {defaultJdk === j.id ? (
                        <>
                          <CheckIcon size={13} /> 默认
                        </>
                      ) : (
                        "设为默认"
                      )}
                    </button>
                    <button className="icon-btn" title="删除" onClick={() => removeJdk(j.id)}>
                      <TrashIcon size={15} />
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
          <div className="add-jdk">
            <input
              value={newPath}
              placeholder="JDK 路径：JDK 根目录 / bin 目录 / java.exe 均可"
              onChange={(e) => setNewPath(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void addJdk(newPath);
              }}
            />
            <button
              className="btn ghost"
              onClick={async () => {
                const sel = await open({ directory: true });
                if (typeof sel === "string") setNewPath(sel);
              }}
            >
              <FolderIcon size={15} /> 浏览
            </button>
            <button className="btn outline" onClick={() => void addJdk(newPath)}>
              <PlusIcon size={15} /> 添加
            </button>
          </div>
        </div>
      </section>

      <section className="panel">
        <header className="panel-head">
          <div className="panel-title">
            <span className="panel-ico">
              <SlidersIcon size={16} />
            </span>
            <div>
              <h2>启动与退出</h2>
              <p>开机自启动、关闭行为与数据目录</p>
            </div>
          </div>
        </header>
        <div className="panel-body">
          <label className="toggle-row">
            <div>
              <b>开机自动启动</b>
              <span>
                {boot.autostartSkipped
                  ? "开发模式下不写入注册表，安装版启动后会自动注册"
                  : autostart
                    ? "已注册到系统启动项，开机后自动在托盘运行"
                    : "不会随开机自动运行"}
              </span>
            </div>
            <span className="switch">
              <input
                type="checkbox"
                checked={autostart}
                onChange={(e) => {
                  const next = e.target.checked;
                  setAutostart(next);
                  commit({ autostart: next });
                }}
              />
              <span />
            </span>
          </label>
          {autostart && !boot.autostartEnabled && !boot.autostartSkipped && (
            <div className="field-hint warn-text">
              系统里还没查到 JarBox 的启动项：可能被杀毒软件拦截，或被任务管理器禁用了。
              把开关关掉再打开即可重试，也可以把 JarBox 加入安全软件白名单、或右键
              JarBox「以管理员身份运行」后再试。
            </div>
          )}
          <label className="toggle-row">
            <div>
              <b>退出时停止所有服务</b>
              <span>
                关闭窗口只是最小化到托盘。关闭此项后，退出 JarBox 仍会让服务继续在后台运行，
                下次打开会自动接管它们
              </span>
            </div>
            <span className="switch">
              <input
                type="checkbox"
                checked={stopOnExit}
                onChange={(e) => {
                  const next = e.target.checked;
                  setStopOnExit(next);
                  commit({ stopOnExit: next });
                }}
              />
              <span />
            </span>
          </label>
          <label className="toggle-row">
            <div>
              <b>启动时恢复上次运行的服务</b>
              <span>
                记住退出那一刻哪些服务在运行，下次打开 JarBox（含开机自启）时自动把它们重新拉起来；
                手动停止过的服务不会被恢复
              </span>
            </div>
            <span className="switch">
              <input
                type="checkbox"
                checked={resumeOnStart}
                onChange={(e) => {
                  const next = e.target.checked;
                  setResumeOnStart(next);
                  commit({ resumeOnStart: next });
                }}
              />
              <span />
            </span>
          </label>
          <div className="toggle-row">
            <div>
              <b>数据目录</b>
              <span>{boot.dataDir}</span>
            </div>
            <button
              className="btn ghost"
              onClick={() => api.openDataDir().catch((e) => notify(errText(e), "error"))}
            >
              打开
            </button>
          </div>
          <div className="toggle-row">
            <div>
              <b>退出 JarBox</b>
              <span>完全退出程序（关闭窗口只是最小化到托盘）</span>
            </div>
            <button className="btn danger-ghost" onClick={() => void api.quitApp()}>
              退出应用
            </button>
          </div>
        </div>
      </section>

      <section className="panel">
        <header className="panel-head">
          <div className="panel-title">
            <span className="panel-ico">
              <InfoIcon size={16} />
            </span>
            <div>
              <h2>关于</h2>
              <p>当前程序版本与发行方式</p>
            </div>
          </div>
        </header>
        <div className="panel-body">
          <div className="toggle-row">
            <div>
              <b>JarBox 版本</b>
              <span>便携版（绿色免安装），程序文件可随意移动；设置与服务列表保存在上面的「数据目录」里。</span>
            </div>
            <div className="about-ver">
              <span className="ver-pill">v{boot.version}</span>
              <button
                className="icon-btn"
                title="复制版本号"
                onClick={async () => {
                  try {
                    await navigator.clipboard.writeText(boot.version);
                    notify(`已复制版本号 v${boot.version}`);
                  } catch {
                    notify("复制失败，请手动记下版本号", "error");
                  }
                }}
              >
                <CopyIcon size={14} />
              </button>
            </div>
          </div>
        </div>
      </section>

      {status !== "idle" && (
        <div className="save-bar">
          <span>{statusText}</span>
        </div>
      )}
    </div>
  );
}
