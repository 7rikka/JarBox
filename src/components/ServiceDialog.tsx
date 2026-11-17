import { useEffect, useMemo, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { JdkConfig, Service } from "../types";
import { Modal } from "./Modal";
import { CubeIcon, FolderIcon, LinkIcon } from "./Icons";
import { basename, dirname } from "../utils";

export function ServiceDialog({
  initial,
  jdks,
  workDir,
  workJars,
  defaultJdk,
  onClose,
  onSubmit,
  onGoSettings,
}: {
  initial?: Service | null;
  jdks: JdkConfig[];
  workDir: string;
  workJars: string[];
  defaultJdk: string;
  onClose: () => void;
  onSubmit: (svc: Service, startAfter: boolean) => Promise<void>;
  onGoSettings: () => void;
}) {
  const isNew = !initial?.id;
  const [form, setForm] = useState<Service>(
    initial ?? {
      id: "",
      name: "",
      jarPath: "",
      jdkId: defaultJdk || jdks[0]?.id || "",
      jvmArgs: "",
      programArgs: "",
      workDir: "",
      note: "",
      createdAt: "",
    }
  );
  const [busy, setBusy] = useState<"save" | "start" | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    if (!form.jdkId && (defaultJdk || jdks[0])) {
      setForm((f) => ({ ...f, jdkId: defaultJdk || jdks[0].id }));
    }
  }, [jdks, defaultJdk]);

  const set = <K extends keyof Service>(k: K, v: Service[K]) =>
    setForm((f) => ({ ...f, [k]: v }));

  const jdkOptions = useMemo(() => jdks, [jdks]);

  async function pickJar() {
    const sel = await open({
      multiple: false,
      filters: [{ name: "Java 归档文件", extensions: ["jar", "war"] }],
      defaultPath: workDir ? `${workDir}\\jars` : undefined,
    });
    if (typeof sel === "string") {
      setForm((f) => ({
        ...f,
        jarPath: sel,
        name: f.name || basename(sel).replace(/\.(jar|war)$/i, ""),
      }));
    }
  }

  async function pickDir() {
    const sel = await open({ directory: true });
    if (typeof sel === "string") set("workDir", sel);
  }

  async function submit(startAfter: boolean) {
    setError("");
    if (!form.name.trim()) return setError("请填写服务名称");
    if (!form.jarPath.trim()) return setError("请选择要运行的 JAR 文件");
    if (!form.jdkId) return setError("请选择运行使用的 JDK");
    setBusy(startAfter ? "start" : "save");
    try {
      await onSubmit(
        {
          ...form,
          name: form.name.trim(),
          jarPath: form.jarPath.trim(),
          jdkId: form.jdkId,
          jvmArgs: form.jvmArgs.trim(),
          programArgs: form.programArgs.trim(),
          workDir: form.workDir.trim(),
          note: form.note.trim(),
        },
        startAfter
      );
    } catch (e) {
      setError(String(e));
      setBusy(null);
    }
  }

  return (
    <Modal
      title={isNew ? "新建服务" : "编辑服务"}
      subtitle={isNew ? "为一个 JAR 配置运行环境，保存后即可启动" : form.name}
      icon={<CubeIcon size={18} />}
      onClose={onClose}
      width={720}
      footer={
        <>
          <div className="foot-left">
            {form.jarPath && <span className="muted">JAR：{basename(form.jarPath)}</span>}
          </div>
          <button className="btn ghost" onClick={onClose} disabled={!!busy}>
            取消
          </button>
          <button className="btn outline" onClick={() => submit(false)} disabled={!!busy}>
            {busy === "save" ? "保存中…" : "仅保存"}
          </button>
          <button className="btn primary" onClick={() => submit(true)} disabled={!!busy}>
            {busy === "start" ? "启动中…" : "保存并启动"}
          </button>
        </>
      }
    >
      <div className="form">
        <label className="field">
          <span>服务名称</span>
          <input
            value={form.name}
            placeholder="例如：订单服务"
            onChange={(e) => set("name", e.target.value)}
            autoFocus
          />
        </label>

        <div className="field">
          <span>JAR 文件</span>
          <div className="input-group">
            <input
              value={form.jarPath}
              placeholder="选择或拖入 JAR 文件"
              onChange={(e) => set("jarPath", e.target.value)}
            />
            {workJars.length > 0 && (
              <select
                className="inline-select"
                value=""
                onChange={(e) => {
                  if (!e.target.value) return;
                  const p = e.target.value;
                  setForm((f) => ({
                    ...f,
                    jarPath: p,
                    name: f.name || basename(p).replace(/\.jar$/i, ""),
                  }));
                }}
              >
                <option value="">从工作目录选择…</option>
                {workJars.map((p) => (
                  <option key={p} value={p}>
                    {basename(p)}
                  </option>
                ))}
              </select>
            )}
            <button className="btn ghost" onClick={pickJar}>
              <FolderIcon size={15} /> 浏览
            </button>
          </div>
          {form.jarPath && (
            <div className="field-hint">
              所在目录：{dirname(form.jarPath)}
            </div>
          )}
        </div>

        <div className="field">
          <span>运行 JDK</span>
          <div className="input-group">
            <select value={form.jdkId} onChange={(e) => set("jdkId", e.target.value)}>
              <option value="">请选择 JDK</option>
              {jdkOptions.map((j) => (
                <option key={j.id} value={j.id}>
                  {j.name} · {j.path}
                </option>
              ))}
            </select>
            <button className="btn ghost" onClick={onGoSettings}>
              <LinkIcon size={15} /> 管理 JDK
            </button>
          </div>
          {jdkOptions.length === 0 && (
            <div className="field-hint warn-text">还没有配置 JDK，请先在设置中添加。</div>
          )}
        </div>

        <div className="grid-2">
          <label className="field">
            <span>
              JVM 参数 <em className="hint-inline">空格分隔</em>
            </span>
            <textarea
              rows={3}
              value={form.jvmArgs}
              placeholder="-Xms256m -Xmx1024m -Dspring.profiles.active=prod"
              onChange={(e) => set("jvmArgs", e.target.value)}
            />
          </label>
          <label className="field">
            <span>
              程序参数 <em className="hint-inline">传给 main</em>
            </span>
            <textarea
              rows={3}
              value={form.programArgs}
              placeholder="--spring.profiles.active=dev"
              onChange={(e) => set("programArgs", e.target.value)}
            />
            <div className="field-hint">
              端口不用填：服务启动后会自动从它的运行日志里识别监听端口，显示在服务列表上。
            </div>
          </label>
        </div>

        <label className="field">
          <span>
            工作目录 <em className="hint-inline">留空则使用 JAR 所在目录</em>
          </span>
          <div className="input-group">
            <input
              value={form.workDir}
              placeholder="进程的工作目录（影响配置文件相对路径）"
              onChange={(e) => set("workDir", e.target.value)}
            />
            <button className="btn ghost" onClick={pickDir}>
              <FolderIcon size={15} /> 选择
            </button>
          </div>
        </label>

        <label className="field">
          <span>备注</span>
          <input
            value={form.note}
            placeholder="可选，例如：测试环境，内存 1G"
            onChange={(e) => set("note", e.target.value)}
          />
        </label>

        {error && <div className="form-error">{error}</div>}
      </div>
    </Modal>
  );
}
