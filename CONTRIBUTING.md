# 参与贡献

谢谢愿意花时间。无论是报一个 bug、提一句建议，还是直接改代码，都是有用的。这份文档说明怎么参与、有哪些约定，以及几个容易踩的坑。

## 目录

- [报告问题](#报告问题)
- [提功能建议](#提功能建议)
- [参与代码](#参与代码)
- [代码约定](#代码约定)
- [提交信息](#提交信息)
- [容易踩的坑](#容易踩的坑)

## 报告问题

用仓库的 Issue 模板提交，模板会引导你填环境、复现步骤和日志。三件事最能加快定位：

1. **能不能稳定复现**。偶发问题请说明触发频率，以及当时是不是刚启动、刚改过设置。
2. **日志**。服务日志在日志窗口里可以选中复制（勾选文本时会自动暂停刷新）。
3. **环境**。Windows 版本、JDK 发行版与版本、有没有装 360 / 火绒这类安全软件。

提之前先看一下 [常见问题](README.md#常见问题)。端口一直显示 `—`、退出后 Java 还在跑、被安全软件拦截，这几类在那里已有解释。

## 提功能建议

重点讲**场景**，而不是直接给方案。比如「同时跑 8 个服务时，想知道谁在吃内存，得一行行看」比「加一个排序按钮」有用得多——同样的痛点可能有更合适的解法。

不确定算不算 bug、或者只是想问问怎么配置，也直接提 Issue 就行，标题注明是提问，不必勉强套用模板。

## 参与代码

### 环境准备

| 需要 | 版本 | 说明 |
| --- | --- | --- |
| Node.js | 18+ | 前端构建 |
| Rust | stable，MSVC target | 后端，需要 `rustup default stable-x86_64-pc-windows-msvc` |
| WebView2 | Win10/11 自带 | 没有的话装一次 Evergreen Runtime |
| JDK | 任意 | 只用于测试服务，不参与构建 |

Windows 是唯一的构建目标，暂不适配 macOS / Linux。

### 跑起来

```bash
npm install
npm run app:dev      # 开发模式，前端热更新
```

只改前端时也可以 `npm run dev` 起 Vite，但 Tauri 命令会不可用，界面拿不到数据。

构建可执行文件：

```bash
npm run app:build    # 产物：src-tauri/target/release/jarbox.exe
```

项目配置了 `bundle.active = false`，构建不会生成安装包，只产出这一个 exe。

### 改之前先看看这些

- `src-tauri/src/commands.rs` 是核心：服务启停、配置读写、日志、端口识别、进程接管与恢复都在这里，改动面最大，改完最好把 `cargo test --lib` 跑一遍。
- `src-tauri/src/model.rs` 定义落盘结构。改字段要考虑已有 `store.json` 还能不能读。
- `src-tauri/src/port.rs` 只做一件事：从日志尾部识别端口。有单元测试，加新写法时一起补测试。
- 前端没有状态管理库，状态集中在 `src/App.tsx`，组件通过 props 向下传。别为了一个小功能引入 Redux 之类的依赖。

### 提交 PR

1. Fork 后从 `main` 开一个分支，分支名随意，描述清楚即可（`fix/port-detection`、`feat/log-export`）。
2. 一个 PR 做一件事。顺手格式化整个文件、改无关文案会让 review 变难。
3. PR 描述里写清「改了什么、为什么、怎么验证的」，模板有对应栏目。
4. 界面改动请附截图，进程相关的改动请说明用了几个服务、什么 JDK。

提交前至少跑这两条：

```bash
cd src-tauri && cargo test --lib    # 后端单元测试
npx tsc --noEmit                    # 前端类型检查
```

## 代码约定

- **语言**：界面文案、注释、日志、提交信息都用中文。代码标识符用英文。
- **Rust**：`cargo fmt` 保持格式统一；错误信息里要带上下文，别只写「失败」。
- **TypeScript**：不用 `any` 逃过类型；与后端交互的数据结构在 `src/types.ts` 里对齐，改后端字段时同步改这里。
- **样式**：全部写在 `src/styles.css`，没有 CSS-in-JS，也没有 Tailwind。新增样式尽量复用已有的变量与类名。
- **依赖**：能不新增就不新增。前端目前只有 React 和 Tauri 插件，后端的关键能力（写注册表、抢前台焦点）都是直接用 `winreg` 和 Win32 FFI 实现的，没有引入额外 crate。

## 提交信息

一条提交一件事，第一行说清做了什么，需要时在正文补动机：

```
修掉强杀后旧快照复活已停止服务的问题

last_session.json 原先只在正常退出时刷新，运行中停掉的服务会被旧快照带回来。
现在启动、停止、进程退出、接管巡检四处都会同步刷新。
```

不需要 `feat:` / `fix:` 前缀，也不需要 emoji。

## 容易踩的坑

**开发模式和正式版会互相抢单实例**

单实例锁按应用的 identifier（`com.jarbox.app`）命名，`npm run app:dev` 起的调试实例和你 `D:\jarbox\JarBox.exe` 的正式实例是同一把锁。调试前先把正式实例从托盘退出，否则新起的会立刻退出，看起来像是「启动就崩」。

**调试会写真实配置**

开发模式同样读写 `%APPDATA%\com.jarbox.app\store.json`。想隔离，可以先备份这个目录，或者临时改 `tauri.conf.json` 的 identifier（注意这同时会改变数据目录和互斥体名）。

**改图标要重新构建才进 exe**

`src-tauri/build.rs` 里显式登记了图标文件作为构建依赖。只替换 `src-tauri/icons/` 里的文件、不做 release 重建的话，exe 的资源段还是旧图标——窗口和托盘图标是编译期注入的 PNG，会先变，容易误判成「已经生效」。图标源图由 `scripts/gen-icon.mjs` 生成，再用 `npx tauri icon` 导出各尺寸。

**结束子进程别只杀父进程**

服务是 `java -jar` 起的独立进程，`taskkill /PID <jarbox> /F` 不会带走它。调试清理时按进程名或 `running.json` 里的 PID 处理。

**不要提交构建产物**

`target/`、`dist/`、`*.exe`、`node_modules/` 都在 `.gitignore` 里。`demo-workspace/` 也不要提交，里面有体积不小的测试 JAR。

## 许可证

贡献的代码按项目的 [MIT 许可](LICENSE)发布。
