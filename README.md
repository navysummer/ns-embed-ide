# ns-embed-ide

跨平台嵌入式开发软件（IDE），支持 C、C++、Rust、Zig、ZR 几种语言。
一切工具链行为（编译/仿真/调试/烧写/运行）由项目根目录 `project.toml` 驱动，`project.toml` 是唯一权威配置。

功能规划见 [AGENT.md](AGENT.md)；其中 **§1.2 工程纪律**约束所有修改（零警告零错误、完整实现、交付即验证、文档同步）。

## 技术栈

- Rust + [GPUI](https://crates.io/crates/gpui) 0.2.2（Zed 的 GUI 框架）
- 工作区 crate：
  - `ns-project` — `project.toml` 模型/校验/模板/最近项目/断点持久化
  - `ns-build` — 构建执行与 gcc/clang/rust 诊断解析（debug 配置自动加 `-g` / `-C debuginfo=2`）
  - `ns-debug` — 调试会话抽象（DAP 客户端）：发现并启动 `lldb-dap`，断点/调用栈/变量/控制台
  - `ns-ide` — GPUI 应用

## 当前实现状态

已实现并验证（详见 AGENT.md §5.1）：

- 新建项目向导、打开/最近项目、文件树、多标签编辑器（行号槽 + 断点槽）；命令面板 `Cmd/Ctrl+Shift+P`（命令搜索/执行）
- C/C++/Rust/Zig/ZR 内建词法高亮，构建诊断解析与问题面板
- 按 `project.toml` 构建、构建输出面板、错误定位跳转
- 全局文件搜索：`Cmd/Ctrl+Shift+F`，文件名 + 内容大小写不敏感搜索（跳过构建目录/二进制），↑↓ 选择、Enter/点击跳转；编辑器虚拟视口，跳转自动滚动到目标行
- 断点增删：行号槽点击、`F9` 切换当前行、`Cmd/Ctrl+Shift+F9` 清除全部、右键菜单（添加/启用·禁用/编辑属性/删除/清除本文件）、条件·命中次数·日志断点编辑（持久化到 `.ns-ide/breakpoints.json`）；断点面板（按文件分组、启用/禁用、跳转源码、删除、清除全部、条件/命中次数/日志摘要）与逐行状态（已验证/命中/未绑定/未同步/失效/已禁用，含未绑定/失效原因）；源文件被删除/重命名或行号越界时自动标记失效并停止下发，恢复后自动重同步；**观察点（数据断点）**：断点面板「观察点」区新增/编辑/删除/启停，访问类型在读取/写入/读写间切换，变量行「👁」一键添加、编辑器右键/命令面板按光标标识符预填，随项目持久化并在每次暂停后重新解析地址，已验证/未绑定状态与原因回到面板
- **调试（宿主机构件）**：`F5` 或顶部「调试」构建并启动 `lldb-dap` 会话；`Shift+F5` 停止；`F10` 步过 / `F11` 步入 / `Shift+F11` 跳出；命中断点行高亮（jade + ▶），调用栈（点击跳转源码）、变量面板（作用域切换 + 可递归展开，含 **Registers**）、内存视图（hex dump，地址输入）、反汇编源码混排视图（PC 行高亮 + 可点击源码行映射）、调试控制台；增删/启停/清除断点即时同步，并回填每个断点的 DAP `verified`/`reason` 状态；条件断点、命中次数断点（第 N 次命中）、日志断点（`{expr}` 输出到控制台）均下发并生效

未完成：嵌入式目标 attach（OpenOCD / ST-Link / J-Link）、烧写、一键运行编排、仿真、LSP、设备库、ZR 完善等，见 AGENT.md §5.1 🚧 清单。

## 构建与运行

```sh
cargo build --workspace          # 或 cargo check --workspace
cargo test --workspace           # 单元测试 + ns-debug DAP 端到端测试（真实 lldb-dap，缺适配器时自动跳过）
./target/debug/ns-embed-ide [project目录]
```

首次运行可直接执行左上角「＋ 新建」向导，或传入一个含 `project.toml` 的项目目录。

## 调试说明

- 适配器：`lldb-dap`（随 Xcode Command Line Tools / Xcode 提供）。自动发现顺序：`NS_DEBUG_ADAPTER` 环境变量 → `PATH` → `xcrun --find lldb-dap` → 常见安装路径。
- 目前只支持**宿主机构建产物**的启动调试（`project.toml` 的 `[build] config = "debug"` 即可，无需额外配置）；嵌入式目标 attach 未实现。
- 调试会话中增删/启停断点会实时下发到适配器；断点保存在 `.ns-ide/breakpoints.json`，不写入 `project.toml`。

## 环境备注

- `.cargo/config.toml` 配置了 `rsproxy.cn` 镜像；若网络可达 crates.io，可删除该文件恢复官方源。
- GPUI 以 `runtime_shaders` feature 启用：在本机缺少 `xcrun metal`（Metal 着色器编译器）时，着色器在运行时编译而非构建期。
- 已知上游 `future-incompat` note：`block v0.1.6`、`proc-macro-error2 v2.0.1`（均经 GPUI 的 macOS 依赖引入），本地无法消除，不影响功能。

## 工程纪律

修改本仓库前请先阅读 [AGENT.md](AGENT.md) §1.2：

1. 每次修改必须 `cargo check --workspace` 0 警告 0 错误、测试全绿；
2. 功能必须完整实现，禁止假实现/占位/残缺路径；
3. 提交前在本机运行验证关键路径；
4. 文档与实现保持同步。
