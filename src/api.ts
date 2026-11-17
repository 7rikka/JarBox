import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  Bootstrap,
  JarEntry,
  JarEntryContent,
  JavaInfo,
  JdkConfig,
  LogFileInfo,
  LogResult,
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
