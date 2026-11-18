import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  Bootstrap,
  JarBackups,
  JarEntry,
  JarEntryContent,
  JavaInfo,
  JdkConfig,
  LogFileInfo,
  LogResult,
  ReplaceResult,
  SaveResult,
  Service,
  ServiceStat,
  ServiceView,
  Store,
} from "./types";

export const api = {
  bootstrap: () => invoke<Bootstrap>("bootstrap"),
  listServices: () => invoke<ServiceView[]>("list_services"),
  serviceStats: () => invoke<ServiceStat[]>("service_stats"),
  saveSettings: (p: {
    workDir: string;
    jdks: JdkConfig[];
    autostart: boolean;
    stopOnExit: boolean;
    defaultJdk: string;
  }) => invoke<SaveResult>("save_settings", p),
  /** 只提交改动的那一项，设置页即时保存用 */
  updateSettings: (patch: {
    workDir?: string;
    jdks?: JdkConfig[];
    autostart?: boolean;
    stopOnExit?: boolean;
    resumeOnStart?: boolean;
    importMode?: "copy" | "move";
    keepJarVersions?: number;
    defaultJdk?: string;
  }) => invoke<SaveResult>("update_settings", { patch }),
  detectJava: (path: string) => invoke<JavaInfo>("detect_java", { path }),
  scanJdks: () => invoke<JdkConfig[]>("scan_jdks"),
  importJars: (paths: string[]) => invoke<string[]>("import_jars", { paths }),
  listWorkJars: () => invoke<string[]>("list_work_jars"),
  upsertService: (service: Service) => invoke<Service>("upsert_service", { service }),
  deleteService: (id: string, deleteLogs: boolean) =>
    invoke<void>("delete_service", { id, deleteLogs }),
  startService: (id: string) => invoke<ServiceView>("start_service", { id }),
  stopService: (id: string) => invoke<void>("stop_service", { id }),
  /** 停止后重新拉起（不是简单 resume，会等旧进程真正退出） */
  restartService: (id: string) => invoke<ServiceView>("restart_service", { id }),
  /** 用新 JAR 覆盖当前版本，旧包备份进同目录 bak */
  replaceJar: (id: string, newPath: string) =>
    invoke<ReplaceResult>("replace_jar", { id, newPath }),
  /** 列出某个服务的历史 JAR 备份 */
  listJarBackups: (id: string) => invoke<JarBackups>("list_jar_backups", { id }),
  /** 回滚到指定备份（当前包也会进 bak） */
  rollbackJar: (id: string, name: string) =>
    invoke<ReplaceResult>("rollback_jar", { id, name }),
  listJarEntries: (jarPath: string) => invoke<JarEntry[]>("list_jar_entries", { jarPath }),
  readJarEntry: (jarPath: string, name: string) =>
    invoke<JarEntryContent>("read_jar_entry", { jarPath, name }),
  listLogs: (serviceId: string) => invoke<LogFileInfo[]>("list_logs", { serviceId }),
  readLog: (p: {
    serviceId: string;
    file: string;
    keyword: string;
    levels: string[];
    limit: number;
  }) => invoke<LogResult>("read_log", p),
  openPath: (path: string, reveal = true) => invoke<void>("open_path", { path, reveal }),
  setAutostart: (enabled: boolean) => invoke<boolean>("set_autostart", { enabled }),
  quitApp: () => invoke<void>("quit_app"),
  openDataDir: () => invoke<void>("open_data_dir"),
};

export const onServiceExit = (
  cb: (payload: { id: string; name: string; code: number | null }) => void
) => listen<{ id: string; name: string; code: number | null }>("service-exit", (e) => cb(e.payload));

/** 启动时自动恢复服务完成后的汇总事件 */
export const onAutoStart = (
  cb: (payload: { started: string[]; failed: string[] }) => void
) => listen<{ started: string[]; failed: string[] }>("auto-start", (e) => cb(e.payload));
