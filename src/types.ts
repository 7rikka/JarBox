export interface JdkConfig {
  id: string;
  name: string;
  path: string;
}

export interface Service {
  id: string;
  name: string;
  jarPath: string;
  jdkId: string;
  jvmArgs: string;
  programArgs: string;
  workDir: string;
  note: string;
  createdAt: string;
}

export interface ServiceView extends Service {
  status: "running" | "stopped";
  pid: number | null;
  startedAt: number | null;
  exitCode: number | null;
  exitedAt: number | null;
  stoppedByUser: boolean;
  adopted: boolean;
  detectedPort: number | null;
  logDir: string;
  currentLog: string;
  jarExists: boolean;
  jdkPath: string;
  jdkOk: boolean;
  cmdline: string;
}

export interface Store {
  workDir: string;
  jdks: JdkConfig[];
  services: Service[];
  autostart: boolean;
  stopOnExit: boolean;
  resumeOnStart: boolean;
  /** 导入 JAR 的方式：复制保留源文件，移动则导入后删除源文件 */
  importMode: "copy" | "move";
  /** 替换 JAR 时 bak 目录里保留的历史版本个数 */
  keepJarVersions: number;
  defaultJdk: string;
}

/** 一个历史 JAR 备份 */
export interface JarBackup {
  name: string;
  path: string;
  size: number;
  /** 毫秒时间戳 */
  modified: number;
}

/** 某个服务的版本清单：当前包 + 历史备份 */
export interface JarBackups {
  currentName: string;
  currentSize: number;
  currentModified: number;
  currentExists: boolean;
  dir: string;
  keep: number;
  items: JarBackup[];
}

/** 替换 / 回滚 JAR 的结果 */
export interface ReplaceResult {
  service: ServiceView;
  backup: string;
  restarted: boolean;
  restartError: string | null;
  pruned: number;
  /** bak 里已有同一版本，这次没有新增备份 */
  deduped: boolean;
}

export interface Bootstrap {
  store: Store;
  services: ServiceView[];
  dataDir: string;
  logsRoot: string;
  version: string;
  autostartEnabled: boolean;
  autostartSkipped: boolean;
}

export interface SaveResult {
  store: Store;
  autostartWarning: string | null;
  autostartEnabled: boolean;
}

export type JarKind = "config" | "class" | "lib" | "other" | "dir";

export interface JarEntry {
  name: string;
  size: number;
  isDir: boolean;
  kind: JarKind;
}

export interface JarEntryContent {
  name: string;
  content: string;
  size: number;
  truncated: boolean;
  binary: boolean;
  encoding: string;
}

export interface LogFileInfo {
  name: string;
  path: string;
  size: number;
  modified: number;
  isCurrent: boolean;
}

export interface LogCounts {
  total: number;
  error: number;
  warn: number;
  info: number;
  debug: number;
  trace: number;
}

export interface LogResult {
  file: string;
  path: string;
  size: number;
  lines: string[];
  matched: number;
  total: number;
  truncated: boolean;
  counts: LogCounts;
}

export interface ServiceStat {
  id: string;
  pid: number;
  cpu: number;
  memory: number;
  virtualMemory: number;
  uptimeMs: number;
}

export interface JavaInfo {
  ok: boolean;
  version: string;
  binary: string;
  message: string;
}
