# ns-embed-ide — 项目功能规划清单

> 跨平台嵌入式开发软件。本文件是**功能规划清单**，为后续开发提供范围与结构依据。
> 技术栈：**Rust + GPUI**（Zed 的 GUI 框架）。
> 功能参照 IAR Embedded Workbench 的能力模型，但**独立设计、不照抄**——工具链解耦、配置即文件、开放可替换。

---

## 1. 项目概述

| 项 | 内容 |
|---|---|
| 名称 | ns-embed-ide（商量嵌入式开发环境） |
| 定位 | 面向嵌入式 MCU/SoC 的跨平台集成开发环境（IDE） |
| 支持语言 | C、C++、Rust、Zig、**ZR**（外部语言 zrlang.github.io） |
| 核心机制 | 一切工具链行为（编译/仿真/调试/烧写/运行）由项目根目录 `project.toml` 驱动 |
| 平台 | Windows / macOS / Linux |
| 技术栈 | Rust + GPUI |
| 参考 | IAR Embedded Workbench（只参考能力模型，不复制实现/界面） |

### 1.1 核心设计原则

1. **配置即文件**：所有工具链由 `project.toml` 声明，IDE 不硬编码任何工具路径。
2. **开放可替换**：编译器、仿真器、调试器、烧写器都是可插拔适配器，可增可换。
3. **单项目单文件**：项目根目录唯一权威配置 `project.toml`。
4. **工具链适配器模式**：每种外部工具通过统一 Trait 抽象，IDE 核心不感知具体工具。
5. **语言无关的编辑器核心**：语法高亮、补全、诊断均通过语言服务抽象（LSP 或内建），语言可扩展。

### 1.2 工程纪律（所有修改必须遵守）

1. **零警告、零错误**：任何一次修改完成后，工作区 `cargo check --workspace` 必须 **0 警告、0 错误**，`cargo build` 可通过，`cargo test --workspace` 全绿。禁止用 `#[allow(dead_code)]`、`#[allow(unused)]` 等属性掩盖本项目的警告；未使用的代码必须真正接入界面/逻辑，否则删除。
   - 唯一例外：上游依赖自身触发的 `future-incompat` note（当前为 `block v0.1.6`（经 cocoa/metal）与 `proc-macro-error2 v2.0.1`（经 stacksafe-macro），均由 GPUI 的 macOS 依赖引入），本地无法消除；必须在 README 中如实记录，不得声称“无警告”时隐去此项。
2. **完整实现**：规格中列出的功能必须真实、完整地实现并经过验证，禁止以下做法：
   - 用 `unimplemented!()`、`todo!()`、空函数体、硬编码返回值冒充完成；
   - 界面控件“存在但点击无反应/只弹提示/只打印日志”；
   - 以“预留”“后续再做”“暂不实现”“待办”“先占位”为由交付核心路径；
   - 只用假数据、只跑通演示路径，却声称功能已完成。
   - 无法在当前迭代完成的能力，必须明确标注为**未完成**，列出缺口与验收方式，且不得出现在“已完成”清单中。
3. **交付即验证**：提交前必须在本机实际运行验证（`cargo check/build/test` + 启动应用走通关键路径）；UI 改动尽量截图核对；测试失败或存在警告不得提交。
4. **文档同步**：`AGENTS.md` / `README.md` 中的范围与完成状态必须与代码一致；新增功能须同步更新规划、验收标准与完成状态表。

---

## 2. 技术架构

```
┌─────────────────────────────────────────────────────┐
│                    GPUI 界面层                       │
│  Dock 布局 / 命令面板 / 文件树 / 编辑器 / 面板         │
├─────────────────────────────────────────────────────┤
│                   核心层 (crate)                     │
│  project (project.toml) · toolchain (适配器)         │
│  build · sim · debug · flash · run · target          │
│  lsp (语言服务) · terminal · logging                 │
├─────────────────────────────────────────────────────┤
│              工具链适配层 (外部进程)                   │
│  gcc/clang/zig/zr · gdb/llvm · openocd/jlink/...   │
│  仿真器 (内建或外部) · 烧写器 (openocd/pyocd/...)     │
└─────────────────────────────────────────────────────┘
```

### 2.1 建议 crate 划分

| crate | 职责 |
|---|---|
| `ns-ide`（bin） | GPUI 入口、窗口、启动 |
| `ns-ui` | 界面组件：Dock、编辑器、面板、向导 |
| `ns-project` | `project.toml` 数据模型、读写、校验、项目模板 |
| `ns-toolchain` | 工具发现、版本探测、工具链适配器 Trait |
| `ns-build` | 编译/构建执行、输出解析（错误/警告定位） |
| `ns-sim` | 仿真器适配、虚拟外设、仿真状态 |
| `ns-debug` | 调试会话抽象（GDB/LLDB 适配器）、断点/寄存器/内存 |
| `ns-flash` | 烧写适配器、固件格式（ELF/HEX/BIN）解析 |
| `ns-lsp` | 语言服务客户端（LSP over stdio），诊断/补全/跳转 |
| `ns-syntax` | 语法高亮（tree-sitter 或内建词法） |
| `ns-device` | 目标设备库：SVD 寄存器、启动文件、链接脚本模板 |
| `ns-zr` | （已移除；ZR 由外部 `zr` 语言提供，见 F4.2） |
| `ns-log` | 日志、事件总线 |

> crate 划分可在实现阶段调整；此处仅作规划约束。

---

## 3. 功能模块总览

| # | 模块 | 说明 |
|---|---|---|
| F1 | 项目管理 | 新建向导、打开/最近项目、文件树、项目模板 |
| F2 | project.toml | 项目唯一权威配置，驱动全部工具链 |
| F3 | 编辑器 | 多标签编辑、高亮、LSP 补全/跳转/诊断 |
| F4 | 语言支持 | C / C++ / Rust / Zig / ZR |
| F5 | 编译与构建 | 编译、清理、构建配置、错误定位 |
| F6 | 仿真 | 指令级/周期级仿真、虚拟外设、仿真视图 |
| F7 | 调试 | 调试会话、断点、寄存器/内存/反汇编视图 |
| F8 | 烧写 | 固件烧录、擦除、校验、外部烧写器 |
| F9 | 运行编排 | 一键 编译→烧写→调试/运行，按 project.toml 协调 |
| F10 | 工具链管理 | 工具自动发现、版本、路径配置、缺失提示 |
| F11 | 目标设备支持 | MCU 选择、SVD 寄存器、启动文件、链接脚本 |
| F12 | ZR 语言 | 外部 `zr` 语言：编译器与 LSP 接入 |
| F13 | 界面框架 | Dock 布局、命令面板、主题、多窗口、快捷键 |
| F14 | 辅助功能 | 终端、输出/日志、搜索、版本控制、静态分析、设置 |

---

## 4. 详细功能清单

### F1 项目管理

#### F1.1 新建项目向导（核心流程，顺序强制）
新建项目必须按固定顺序逐步选择，每步只能选一类：

1. **第 1 步 — 项目基本信息**：项目名称、保存位置。
2. **第 2 步 — 编程语言**：`C` | `C++` | `Rust` | `Zig` | `ZR`（单选，必选）。
3. **第 3 步 — 编译器**：按语言列出可用编译器（见 F4.5 编译器矩阵）。
4. **第 4 步 — 仿真软件**：可选仿真器（按语言/目标过滤）。
5. **第 5 步 — 调试软件**：可选调试器（GDB/LLDB/厂商调试器）。
6. **第 6 步 — 烧写软件**：可选烧写器（OpenOCD/pyOCD/J-Link 等）。
7. **第 7 步 — 目标设备**（可选/推荐）：MCU 型号，用于生成 SVD、启动文件、链接脚本。
8. **生成**：写出 `project.toml` + 项目骨架（源码目录、启动文件、链接脚本、构建文件），打开编辑器。

> 步骤 2–6 每一步都是"上一级决定下一级候选"的级联选择，避免选择不兼容组合。

#### F1.2 项目操作
- 打开项目（读取 `project.toml` 校验）。
- 最近项目列表（记录路径 + 最近打开时间）。
- 关闭/重命名/删除项目。
- 项目树（文件/目录树，拖拽移动，新建/重命名/删除文件）。
- 项目模板：Bare-Metal（空）、LED Blink、UART 回环、RTOS 模板（预留）。

### F2 project.toml

项目根目录唯一权威配置文件，**编译/仿真/调试/运行/烧写均读取它**来协调工具。

#### F2.1 数据模型（规划草案）

```toml
# 根目录 project.toml（由向导生成，也可手改）
[project]
name = "blinky"
version = "0.1.0"
language = "C"            # C | C++ | Rust | Zig | ZR
target = "stm32f103c8"    # 目标 MCU（可选）
# 以下四项由向导选择，运行时驱动对应工具：
compiler = "arm-none-eabi-gcc"   # 编译器
simulator = "ns-sim"             # 仿真软件
debugger = "gdb"                 # 调试软件
flasher = "openocd"              # 烧写软件

[compiler]
# 编译器实例配置（选项、路径、标准、优化）
path = ""                     # 空 = 自动查找
std = "c11"
opt = "-O2"
defines = []
includes = []

[debugger]
path = ""
interface = "swd"             # swd | jtag | ...
server = "openocd"            # 调试服务器（可选）

[flasher]
path = ""
interface = "swd"

[build]
config = "debug"              # debug | release
out_dir = "build"

[sim]
cpu = ""                      # 仿真 CPU 型号
period = 0                    # 仿真周期参数（可选）
```

#### F2.2 功能
- 读/写/校验（缺失字段、非法组合报错，定位到行）。
- 结构化编辑（IDE 内表格/表单编辑，保持合法）。
- 模板生成（向导写默认值）。
- 变更监听：保存后即时刷新工具链状态。

### F3 编辑器

- 多标签、多窗口编辑。
- 语法高亮（tree-sitter 或内建词法）。
- 代码补全、悬停提示、签名帮助（经 LSP）。
- 跳转到定义/声明、查找引用、重命名符号。
- 括号匹配、缩进、折叠、多光标。
- 诊断显示（错误/警告波浪线 + 问题面板，点击跳转）。
- 查找/替换（当前文件、全局）。
- 代码片段、文件大纲/面包屑、迷你地图。
#### F3.1 代码高亮

- 两套机制，按语言与可用性选择：
  - **tree-sitter 语法树**：增量解析、精确作用域；变量/函数/类型/宏/关键字分类着色、括号匹配、缩进、折叠、代码大纲、符号索引均基于语法树。
  - **内建词法（回退）**：无 tree-sitter 或未启用时，按关键词表/正则做基础高亮。
- 语义着色（类型/函数/变量/常量分级）由界面主题驱动（见第 8 章）；LSP `semanticTokens` 可用时叠加类型感知着色，精度更高。
- 高亮主题跟随界面主题（深/浅），颜色在主题令牌中定义。

#### F3.2 代码跳转

- **跳转到定义/声明**（快捷键/Ctrl+点击）。
- **跳转到包含头文件**：`#include`/`use` 解析定位。
- **符号跳转**：文件大纲、面包屑、全局符号搜索入口。
- **查找引用**、**重命名符号**（含跨文件重命名）。
- 跳转来源优先级：**LSP**（clangd / rust-analyzer / zls / zr lsp）→ tree-sitter 符号索引 → 文本扫描回退；回退时提示精确度受限。
- 多定义歧义（重载/宏）弹出候选列表；跨文件跳转自动打开并聚焦目标文件。
- **返回历史栈**（Alt+←/→）：跳转后按顺序回退/前进。
- 与诊断联动：问题面板点击错误/警告直接定位到源码行。

#### F3.3 代码提示

- **补全**：LSP 触发词（`clangd`/`rust-analyzer`/`zls`/`zr lsp`）；tree-sitter 符号索引回退补全（文件内符号 + 关键字 + 片段）。
- **悬停提示**：类型/签名/文档说明（LSP `hover`；回退为语义近似）。
- **签名帮助**：函数调用括号内显示参数列表与当前参数高亮（LSP `signatureHelp`）。
- **参数/内联提示**：实参名提示（`inlayHints`）、类型提示。
- 补全来源混合：内置关键字 + 文件/项目符号 + 头文件/依赖符号 + 片段；按触发上下文排序过滤。
- 补全窗口：分类图标、类型签名、文档预览；支持模糊/前缀匹配、Tab 确认、继续输入过滤。
- 无 LSP 语言或未连接时，自动降级为语法树/词法补全，不阻塞输入。

### F4 语言支持

#### F4.1 通用能力矩阵

| 能力 | C | C++ | Rust | Zig | ZR |
|---|---|---|---|---|---|
| 语法高亮 | ✅ | ✅ | ✅ | ✅ | ✅ |
| 补全/跳转（LSP） | clangd | clangd | rust-analyzer | zls | zr lsp |
| 构建系统 | Make/CMake | Make/CMake | Cargo | build.zig | zr 直接编译 |
| 单测集成 | ✅ | ✅ | ✅ | ✅ | ✅ |

#### F4.2 ZR 语言（外部语言）
- 接入外部 `zr` 语言（https://zrlang.github.io/），编译器为 `zr`。
- 构建：`zr <entry> --target <triple> -o <artifact>`（裸机 RISC-V → `riscv32-unknown-none`，ARM Cortex-M → `arm-none-eabi`）。
- LSP：`zr lsp`（stdio），与 clangd/rust-analyzer 同构驱动。
- 语法高亮、错误诊断由 `zr` 自身提供。

### F5 编译与构建

- 读取 `project.toml` 的 `compiler` + `[compiler]`，调用外部编译器。
- 构建动作：构建 / 重新构建 / 清理。
- 构建配置：debug / release（`[build]`）。
- 输出解析：错误/警告/信息 → 可点击定位到源码行。
- 构建进度与取消、并行构建。
- 产物管理（ELF/HEX/BIN），供烧写/仿真使用。
- 各语言构建后端：
  - C/C++：Make / CMake 生成或直接编译。
  - Rust：`cargo build --target <embedded-triple>`。
  - Zig：`zig build`。
  - ZR：`zr <entry> --target <triple> -o <artifact>`。

### F6 仿真

- 读取 `project.toml` 的 `simulator`，启动仿真器（内建或外部进程）。
- 仿真类型：
  - **指令级仿真**：执行 ELF/HEX 指令流。
  - **周期级仿真**（预留）：周期精确建模。
- 虚拟外设：GPIO、UART（虚拟终端回显）、TIMER、中断控制器（按设备库 F11 生成）。
- 仿真控制：运行/暂停/单步、时钟频率、仿真时间显示。
- 仿真视图：寄存器、内存、外设状态、虚拟串口输出。
- 仿真断点与观察点。

### F7 调试

- 读取 `project.toml` 的 `debugger`，建立调试会话。
- 适配器：GDB（`gdb`/`arm-none-eabi-gdb`）、LLDB（预留）、厂商调试器（预留）。
- 调试控制：继续、暂停、单步（step over/into/out）、运行到光标。
- 断点管理：行断点、条件断点、硬件断点、观察点；支持在编辑器中直接增删（见 F7.1）。
- 视图：
  - **寄存器**（CPU 寄存器，按设备库显示外设寄存器）。
  - **内存**（hex 查看/编辑、实时刷新）。
  - **反汇编**（混合源码/汇编）。
  - **调用栈**、**变量/监视**（局部、全局、watch 表达式）。
  - **串口/终端**输出。
- 调试会话与仿真/硬件切换：同一调试接口抽象，仿真目标与真实目标可互换。

#### F7.1 断点增删与调试联动

目标：用户可以像常规 IDE 一样，在源码中随时**添加、删除、启用/禁用断点**，并让断点在调试会话中真正生效。

- **编辑器内增删**：
  - 在行号槽（gutter）单击切换当前行断点，再次单击删除。
  - 右键菜单：添加断点 / 删除断点 / 禁用断点 / 编辑条件 / 清除本文件断点。
  - 快捷键：`F9` 切换当前行断点，`Cmd/Ctrl+Shift+F9` 清除全部断点。
  - 断点圆点显示在行号槽；当前行/命中行有独立高亮，不遮挡错误/警告标记。
- **断点列表管理**：
  - 独立“断点”面板，按文件分组列出断点：文件、行号、类型、条件、命中次数、启用状态。
  - 支持双击跳转到源码、勾选启用/禁用、删除单条、清除全部。
  - 调试会话中命中断点时列表同步高亮。
- **断点类型**：
  - 行断点：在源码位置暂停。
  - 条件断点：表达式为真时暂停（如 `count == 10`）。
  - 命中次数断点：第 N 次命中时暂停。
  - 日志断点：不暂停，将表达式输出到调试控制台。
  - 硬件断点：受目标硬件数量限制，创建失败时给出明确提示。
  - 观察点/数据断点：监视变量或地址的读/写。
- **状态与持久化**：
  - 断点状态：启用 / 禁用 / 已验证 / 未绑定（pending）/ 命中 / 失效，分别用颜色与图标区分。
  - 断点随项目保存到项目内 IDE 状态文件（如 `.ns-ide/breakpoints.json`），不写入 `project.toml`；重新打开项目时恢复。
  - 源码行移动或文件重命名后，尽量按位置/符号重新绑定；无法绑定的断点标记为 pending 并提示原因。
- **与调试适配器联动**：
  - 启动调试会话时，把启用断点下发到 `project.toml` 指定的 `debugger`（GDB/LLDB：`break`、`delete`、`condition`、`ignore`、`watch` 等）。
  - 调试中增删断点即时同步到调试器；删除失败或地址不可用要在问题/调试面板报错。
  - 命中断点后自动高亮源码行，并刷新调用栈、变量、寄存器与反汇编视图；继续/单步后断点保持有效。
  - 仿真调试复用同一断点抽象（见 F6），保证仿真目标与真实目标行为一致。

### F8 烧写

- 读取 `project.toml` 的 `flasher`，调用烧写工具。
- 烧写动作：烧写固件、擦除、校验、读回。
- 固件格式：ELF / HEX / BIN（自动转换）。
- 烧写器适配器：OpenOCD、pyOCD、J-Link Commander、厂商工具（可插拔）。
- 烧写进度、校验结果、目标连接检测。
- 烧写算法/外部 loader（预留，参考设备库）。

### F9 运行编排

一键运行 = 按 `project.toml` 顺序协调工具：

```
读取 project.toml
  → 编译 (compiler)         [失败即停，跳转到错误]
  → 烧写 (flasher)          [或直接仿真加载]
  → 启动调试/仿真 (debugger | simulator)
```

- 运行模式：**调试运行**（编译→烧写→连接调试器）与 **直接运行**（编译→烧写→复位运行，不附加调试器）。
- 每个环节失败：停止并定位错误，不静默继续。
- 停止/重启运行会话。

### F10 工具链管理

- 自动探测已安装工具：扫描 PATH + 常见安装目录。
- 版本探测（`--version` 解析）。
- 工具链路径配置（全局设置，覆盖自动探测）。
- 校验：项目选择的工具是否可用，缺失时给出安装指引。
- 工具注册表：语言 → 可用编译器列表（供向导级联选择）。

### F11 目标设备支持

- MCU 设备库：设备型号、核（ARM Cortex-M / RISC-V 等）、内存映射、外设。
- SVD 解析：外设/寄存器定义 → 调试/仿真寄存器视图、内存映射。
- 启动文件、链接脚本模板（按设备生成）。
- 设备选择入口：向导第 7 步 + 项目设置。

### F12 ZR 语言（外部，见 F4.2）

- 外部 `zr` 语言（https://zrlang.github.io/），编译器与 LSP 均由上游提供。
- `zr` 编译器调用约定（`zr <entry> --target <triple> -o <artifact>`）。
- `zr lsp` 语言服务器接入。

### F13 界面框架

- **Dock 布局**：可拖拽/停靠/浮动/折叠面板（文件树、编辑器、输出、问题、终端、寄存器、内存、调用栈、变量）。
- **命令面板**（Ctrl/Cmd+Shift+P）：命令搜索与执行。
- **主题**：深色/浅色，可扩展。
- **多窗口**、多工作区。
- **工具栏/状态栏**：构建、运行、调试按钮；编译状态、目标、串口、仿真时钟显示。
- **快捷键**：可配置（参考通用 IDE 键位，自定义实现）。
- **向导 UI**：新建项目多步向导（F1.1）。
- **设置页**：全局设置 + 项目设置（`project.toml` 表单编辑）。

### F14 辅助功能

- 内建终端（串口监视器 + 系统 shell 标签）。
- 输出/日志面板、事件日志。
- 全局文件搜索（内容 + 文件名）。
- Git 集成（仓库状态、diff、提交）。
- 静态分析入口（clang-tidy、cargo clippy 等，按工具可用性）。
- 项目设置、全局设置。
- 崩溃恢复（会话状态保存/恢复，预留）。

---

## 5. 里程碑划分（建议实现顺序）

| 阶段 | 目标 | 覆盖模块 |
|---|---|---|
| **M0 脚手架** | Rust + GPUI 空壳可运行，主窗口、Dock、文件树、编辑器标签 | F13（基础）、F3（基础） |
| **M1 项目与配置** | 新建向导（语言→编译器→仿真→调试→烧写）、`project.toml` 读写校验、项目模板 | F1、F2 |
| **M2 语言基础** | C/C++/Rust/Zig 语法高亮、基础诊断；ZR 语法定义 | F3、F4 |
| **M3 构建** | 读取 `project.toml` 调编译器、错误定位、产物生成 | F5 |
| **M4 调试** | GDB 适配、断点增删与命中、寄存器/内存/反汇编视图（宿主机构件切片已完成，见 §5.1；嵌入式 attach 未完成） | F7（含 F7.1） |
| **M5 烧写** | OpenOCD/pyOCD 适配、固件格式转换 | F8 |
| **M6 运行编排** | 编译→烧写→调试一键流程 | F9 |
| **M7 仿真** | 内建指令级仿真器、虚拟外设、仿真视图 | F6 |
| **M8 语言增强** | LSP 补全/跳转、rust-analyzer/clangd/zls 集成 | F3、F4 |
| **M9 设备库** | SVD 解析、设备选择、链接脚本生成 | F11 |
| **M10 ZR 接入** | 外部 `zr` 编译器与 `zr lsp` 接入 | F4.2、F12 |
| **M11 工具链管理** | 自动探测、版本、缺失指引 | F10 |
| **M12 辅助与体验** | 终端、搜索、Git、主题、命令面板、静态分析 | F14 |

> 里程碑顺序可调整；**M1 的 `project.toml` 与向导是全局依赖，建议最先稳定**。

### 5.1 当前实现状态（以本仓库代码为准）

✅ 已实现并验证：

- **M0 脚手架**：GPUI 主窗口、墨金主题、文件树、多标签编辑器、欢迎页、状态栏。
- **M1 项目与配置**：新建向导（语言 → 编译器 → 仿真/调试/烧写 → 生成项目）、`project.toml` 读写/校验、最近项目、工具探测。
- **M2 语言基础**：C/C++/Rust/Zig/ZR 内建词法高亮；gcc/clang/rust 诊断解析并在问题面板定位。
- **M3 构建**：按 `project.toml` 调用编译器、构建输出面板、问题面板点击跳转。
- **M3 构建进度与取消（F5 切片）**：构建输出**逐行流式**追加到输出面板（`ns_build::execute_stream`，stdout/stderr 双线程读，不在进程退出后一次性 dump，长构建可见中间输出）；构建中状态栏显示「■ 取消构建」按钮，点击置位取消标志（`ns_build::execute_stream_cancelable`）终止子进程并在输出面板如实报告「构建已取消」；单测覆盖逐行送达与取消（预置取消标志→失败报告+无后续输出）。
- **F5 并行构建（M3 切片）**：`project.toml` 新增 `[build] jobs`（0=自动检测 CPU 数，上限 32）；`Project::build_jobs` 解析；CMake 后端 `cmake --build ... -j N`、Make 后端 `make -jN`、Rust `cargo build -j N` 全部透传并行作业数；设置页新增「并行作业数」字段（保存校验：0 或正整数，写回 `[build] jobs`）；单测覆盖 cmake/make 透传 `-j` 与 cargo 配置 jobs 生效。
- **F14 全局文件搜索**：`Cmd/Ctrl+Shift+F` 打开搜索面板（类命令面板样式），对项目执行大小写不敏感的**内容 + 文件名**搜索（跳过 .git/build/target/node_modules、跳过二进制与 >1MB 文件），↑↓ 选择、Enter/点击跳转，`Esc` 关闭；支持以当前选区文本作为预填关键词。单位测试覆盖大小写不敏感、目录跳过、上限 200 条、空关键词。
- **编辑器虚拟视口**：编辑器改为手动虚拟滚动（行号槽/断点/命中行/当前行随滚动同步，滚动到底/顶自动夹紧，跳转诊断/断点/搜索结果会自动把目标行滚动进可见区域）。
- **F7.1 编辑器内断点子集**：行号槽点击切换、F9 切换当前行、`Cmd/Ctrl+Shift+F9` 清除全部、断点随项目保存到 `.ns-ide/breakpoints.json`（重新打开恢复）、独立“断点”面板（按文件分组、勾选启用/禁用、点击跳转源码、删除单条、清除全部、启用计数、显示条件表达式）、状态栏启用计数。
- **F7.1 右键菜单**：编辑器内右键 — 添加断点 / 启用·禁用断点 / 编辑条件… / 删除断点 / 清除本文件断点（菜单随该行是否存在断点动态切换）。
- **F7.1 条件断点**：条件随断点持久化并**真正下发到调试器**（`condition`，e2e 用例验证命中与不命中的路径）；面板以「条件 · 表达式」显示。
  - **命中次数断点**：第 N 次命中才暂停（DAP `hitCondition`，e2e 用例 `dap_breakpoint_results_reported_and_hit_condition_pauses` 验证第 2 次命中才停）。
  - **日志断点**：不暂停，将日志消息（支持 `{expr}` 占位）经 DAP `logMessage` 输出到调试控制台。
  - **断点状态（已验证/命中/未绑定/未同步/失效/已禁用）逐行展示**：`setBreakpoints` 响应中每个断点的 `verified`/`reason` 由适配器解析为 `BreakpointResults`，启动与运行时重新同步时写入断点存储并持久化；命中断点后 `mark_hit` 标记为「命中」，继续运行后自动清除；文件被删除/重命名或源码缩短导致行号越界时，同步前自动重新校验并标记「失效」且显示原因（不再下发给适配器，恢复后自动重新同步）；面板新增「状态」列（玉色=已验证、朱色=命中/未绑定、金色=未同步/失效、灰=已禁用），并在属性摘要后显示未绑定/失效原因；右键「编辑断点属性…」可同屏编辑条件/命中次数/日志消息。
  - **观察点（数据断点，DAP `dataBreakpointInfo` + `setDataBreakpoints`）**：断点面板下方「观察点」区新增/编辑/删除/启用·禁用观测表达式（如 `marker`、`obj.field`），点击访问类型徽章在**读取 / 写入 / 读写**间循环；变量面板每行右侧「👁」一键把变量加入观察点，编辑器右键「添加观察点（数据断点）…」以光标所在标识符预填；命令面板「添加观察点（数据断点）…」。实现要点：每次暂停后清空旧的适配器 `dataId` 并重新解析（`dataBreakpointInfo` 只传 `variablesReference`，因此先在 Locals/Globals 变量列表中找到表达式所在作用域，找不到再退回 frame/global 上下文），解析完成后一次性下发 `setDataBreakpoints`；`setDataBreakpoints` 响应按请求顺序回填每个观察点的已验证/未绑定状态与原因；观察点随 `.ns-ide/breakpoints.json` 持久化（`watchpoints` 字段），继续运行/会话结束时清除「命中」标记。e2e 用例 `dap_watchpoint_resolves_and_stops_on_write` 验证解析 dataId、下发写入观察点、继续后在写入处停下。
  - **硬件断点**：断点面板/编辑器右键「改用硬件断点」把断点切换为硬件执行（DAP `setBreakpoints` 的 `mode: "hardware"`，受目标硬件资源限制，绑定失败时适配器回传未绑定原因并如实显示），切换回软件断点保留条件/日志/命中次数属性；`BreakpointStore::set_hardware` 切换 `BreakpointKind::Hardware` 并失效化验证状态；下发时 `DebugBreakpoint`/`DebugBreakpointLocation` 携带 `hardware` 标志，`send_breakpoint_set` 据此写入 `mode`。e2e 用例 `dap_hardware_breakpoint_binds_and_stops` 验证绑定（verified）并实际在源码行停下。
- **M4 调试切片（宿主机构件）**：**LLDB DAP 适配器（lldb-dap）**，`NS_DEBUG_ADAPTER` 可覆盖、自动发现（PATH + xcrun + Command Line Tools 路径）。`F5`/顶部「调试」构建并启动会话；`Shift+F5` 停止；`F10` 步过 / `F11` 步入 / `Shift+F11` 跳出；命中断点后：命中行高亮（jade 底色 + ▶ 标记 + 反汇编中 PC 行高亮）、调用栈面板（点击帧跳转源码）、变量面板（**作用域切换：Locals/Globals/Registers，变量可递归展开**）、调试控制台（程序输出）；**内存视图（hex dump + ASCII，地址输入、64/128/256 字节；**编辑**：hex 字节输入 + 「写入」（DAP `writeMemory`，成功后自动重读）；**多区域**：命名区域栏 + 「＋区域」/「－区域」（`MemoryRegion` 保存/切换/删除，区域随会话内状态切换）；**暂停时实时刷新**（`refresh_memory_on_stop` 在每个停止事件后自动重读当前区域）**）、反汇编源码混排视图（从 PC 读取 24 条指令并高亮当前指令；带源码行映射的指令之间插入可点击的源码行头部——`file:line` + 源码文本，点击跳转到该文件该行的编辑器；映射来源为 lldb-dap `disassemble` 响应的 `line`/`location`，源码文本按需从打开的编辑器文本或磁盘读取；无映射的尾部/溢出指令保持裸行不插头部）**；调试中增删/启停/清除断点**即时同步**到适配器；条件断点经 `condition` 下发并命中（e2e 验证）；会话结束自动清理高亮。
- **F7 反汇编断点（M4 切片）**：反汇编每行左侧新增**断点槽**（`dbg-disasm-bp-*`）——该指令有源码映射（`Instruction.file`/`line`）时点击切换对应源码行断点（复用 `on_breakpoint_toggled` → `BreakpointStore` + 调试器即时同步），槽内圆点颜色随断点启用/禁用（朱=启用、灰=禁用、空环=无断点）；无源码映射的指令（尾部/溢出/纯汇编）点击如实提示「该指令无源码映射，无法设置源码断点」，不静默。单测 `disasm_row_toggles_source_breakpoint` 验证切换添加/删除。
- **跟踪窗口（SWO/SWV）配置面板**：
  - 底层 `ns-debug::swvd`：DWT 比较器编码、ITM 包解码（`decode`）、探针发现（`NS_TRACE_PROBE`/ST-Link GDB server/OpenOCD 端口）、`parse_address`、`validate`/`requests`/`stop_requests`；单测覆盖寄存器写入顺序、包解码、探针支持说明、配置校验与文件回退。
- **构建**：debug 配置自动加 `-g`（native）/`-C debuginfo=2`（rustc），产物调试可用。
- **F14 Git 集成（M12 切片）**：状态栏「Git」项（点击打开面板）显示分支与变更数；Git 面板（输出区新标签）列出变更文件（暂存/取消暂存/删除/未跟踪徽章），选中文件显示 unified diff，提交框输入信息后一键提交；**可折叠「历史 · 分支」区**（提交 hash/subject 列表 + 分支计数，PyCharm 式），**历史·分支区每行本地分支可点击切换**（当前分支玉色标记），**顶栏「＋新建分支」「↓ 拉取」「↑ 推送」按钮**（新建分支弹输入框创建并切换，拉取/推送如实报告错误）；非仓库目录如实提示。底层 `ns-ide::git`：`repo_status`（`status --porcelain` 解析 + `branch --show-current`，含 unborn 分支；**重命名条目解析为变更目标路径**，不再显示 `old -> new` 伪路径）、`diff`/`stage`/`unstage`/`commit`、`log`/`branches`（**过滤分离 HEAD 伪分支**）、`checkout`/`create_branch`/`pull`/`push`；单测覆盖分支/未跟踪/暂存/提交/diff/非仓库/空提交拒绝/新建切换分支/拉取推送错误/重命名目标/分离 HEAD。
  - **F10 工具链管理（M11 切片）**：`ns-project::toolchain` 增加 `Tool::version`（运行 `--version` 探测首行）与 `missing_guide`（按工具给安装建议）；新建项目向导的工具步骤为每个工具显示「路径 · 版本」，缺失则显示**安装命令指引**（`missing_guide`，如 `brew install arm-none-eabi-gcc` / `pip install pyocd` / `cargo install probe-rs-tools`），检测结果按需缓存（不逐帧起子进程）；`missing_guide` 覆盖 Keil/IAR 交叉工具链（`avr-gcc`/`msp430-elf-gcc`/`v850-elf-gcc`/`rx-elf-gcc`/`rl78-elf-gcc`/`sdcc`，SDCC 支持 8051/80251/STM8 目标），缺失时给出对应安装指引；单测覆盖版本探测/安装建议/未知工具与交叉工具链指引。
- **F10 工具链校验（M11 切片）**：`Project::tool_warnings` 校验 `project.toml` 中配置的编译器/仿真器/调试器/烧写器是否在 PATH（空值=自动发现跳过），缺失时给出安装建议；`Project::open` 仅对结构性错误（空名称/版本/输出目录）失败，工具缺失作为警告在打开项目后显示到「问题」面板。
- **F14 静态分析（M12 切片）**：命令面板「静态分析」按语言运行 `clang-tidy`（C/C++，按 `project.toml` 传 `--std`/`-D`/`-I`）、`cargo clippy --message-format=short`（Rust）、`zig build-exe -fno-emit-bin`（Zig）、`zr <entry> -c`（ZR，仅检查不产出），结果经统一诊断解析进「问题」面板；工具缺失时如实提示。底层 `ns_build::plan_static_analysis`；端到端单测用可告警的 Rust 工程跑 `cargo clippy` 并断言 `clone_on_copy` 被解析。
- **F14 终端（M12 切片）**：命令面板「打开终端」启动系统 shell（`Terminal` 基于 `portable-pty`，PTY 100×30，非阻塞 `drain_events` 轮询），实时流式渲染、输入回显、Ctrl+C 中断、清屏、退出码/尺寸显示、`Drop` 时关 PTY；命令面板「打开串口监视器」用 `NS_SERIAL_PORT` 启动串口会话（`TerminalKind::Serial`），未设置时如实提示。端到端单测用真实 `sh` 起 PTY 断言输出；`strip_ansi` 单测覆盖 SGR/光标移动/OSC title。
- **F14 会话恢复（M12 切片）**：打开文件标签 + 活动标签持久化到 `.ns-ide/session.json`（`ns_project::Session`，打开/切换/关闭/退出保存，重开项目恢复，缺失文件安全跳过），往返单测覆盖。
- **F13 设置页（M12 切片）**：`Cmd/Ctrl+,` 或命令面板「项目设置…」打开设置模态，编辑 `project.toml` 常用字段（项目名称/目标 MCU/编译器路径/构建配置/输出目录/调试器路径/烧写器路径/烧写器类型），保存写回 `project.toml`（校验：名称非空、构建配置 debug|release、烧写器类型 openocd|pyocd|jlink，失败如实提示）；`ns_project` 设置保存/重开往返单测覆盖。
- **F8 烧写（M5 切片）**：命令面板「烧写固件」/ 顶部「烧写」/ `Cmd/Ctrl+Shift+F10` 用 `project.toml` 的 `[flasher]`（`kind` openocd|pyocd|jlink + `path` + `interface`）烧写构建产物，结果进输出面板并如实报告退出码。底层 `ns_flash` crate：`Flasher` trait + `adapter_for` + OpenOCD/pyOCD/J-Link 适配器（ELF/Intel HEX/BIN 固件，HEX 解码含校验和/EOF 校验、扩展线性/段地址）；端到端单测用 stub 工具验证命令构造与 `adapter_for` 从 `project.toml` 解析。
- **F9 运行编排（M6 切片）**：顶部「烧写并运行」（编译→烧写→复位运行）与「烧写并调试」（编译→烧写→连接调试器），命令面板「烧写并运行」/「烧写并调试」同源；`pending_flash` 状态机：构建成功才烧写，烧写成功才运行/调试，任一环节失败即停止并如实报告（不静默继续）。
- **F10 工具链全局路径（M11 切片）**：命令面板「工具链路径…」编辑全局覆盖（`ToolchainConfig::set_override`，arm-none-eabi-gcc/gdb、clang、gcc、rustc、zig、openocd、pyocd、JLinkExe），留空=从 PATH 探测；覆盖优先于自动发现，保存在用户配置目录 `toolchain.json` 跨项目共享；`registry`（语言→可用编译器，向导级联）。单测覆盖覆盖优先/清理/注册表/serde 往返。
- **F3.2/F3.3 语言服务（M8 切片）**：`ns_lsp` crate（LSP-over-stdio：Content-Length 帧、后台读写线程、请求/通知路由、`server_for_language`/`server_available`）；项目打开时按语言自动启动（C/C++→clangd、Rust→rust-analyzer、Zig→zls、ZR→`zr lsp`），`didOpen`/`didChange` 实时同步；`publishDiagnostics` 合并进问题面板并点击跳转；`F12` 跳转定义、`Shift+F12` 查找引用、`Cmd/Ctrl+K Cmd/Ctrl+I` 悬停（状态栏）、`Cmd/Ctrl+Space` 补全（浮动面板，↑/↓、Enter/Tab 确认、Esc 关闭）。单测：mock 服务器（initialize/补全/定义/诊断）+ 真实 clangd 端到端。
- **F11 目标设备（M9 切片）**：`ns_device` crate——CMSIS-SVD XML 解析（`parse_svd`：外设→寄存器→位域，`bitRange`/`bitOffset`/`lsb`-`msb`、hex/binary/dec 数字）、内置目录（stm32f103c8/stm32f407vg/nrf52840/rp2040，`CATALOG`/`device_for_target`/`catalog_entry`）、`linker_script`/`startup_file`（flash/RAM 映射 + 向量表/复位处理，Cortex-M0+/M3/M4）、`memory_map`/`registers_by_peripheral`；向导「目标设备」步显示核/Flash/RAM 并在生成时自动写出 `link.ld` 与 `src/startup.s`（`Wizard::generate_device_files`）；调试面板「设备」页按 SVD 展示外设寄存器（地址/复位值/位域）。单测覆盖解析/生成/目录/未知目标。
- **F4.2 ZR 语言（M10 切片）**：ZR 由外部 `zr` 语言提供（https://zrlang.github.io/）。IDE 接入其编译器 `zr` 与语言服务 `zr lsp`：构建后端调用 `zr <entry> --target <triple> -o <artifact>`（`ns_build::plan_zr`，按 MCU 核选 `riscv32-unknown-none`/`arm-none-eabi`），LSP 客户端以 `zr lsp`（stdio）驱动，`didOpen`/`didChange`/`publishDiagnostics`/补全/悬停同构；工具缺失时如实提示。`ns_zr` crate 已移除（不再自研语言）。
- **F6 仿真（M7 切片）**：`ns_sim` crate——内建指令级仿真器（RV32I：`registers[32]`/`pc`/`memory[64KiB]`/`cycle_count`、`load_elf`/`load_image`/`reset`、`step`/`run`/`step_over`/`set_breakpoint`/`clear_breakpoint`、`sign_extend`/`jal_immediate`/`branch_immediate`、`ecall` 停机、`read_reg`/`write_u32`/`snapshot`）；虚拟外设（`VirtualPeripherals`：内存映射 UART `uart_base` + `putchar` 回调 + `outputs`）；IDE：命令面板「仿真运行（内建指令级）」加载构建产物（ELF 优先，否则按 `ENTRY_POINT` 原始加载），调试面板「仿真」页（加载并运行/单步/运行/复位/停止、PC/周期/状态、32 寄存器 ABI 名 + 值、UART 输出）。单测覆盖 ALU（加/减）、循环累加（1..10=55）、断点命中、load/store、ecall 停机、寄存器命名、快照，及真实 ELF32 端到端（入口→循环→a0=55→停机）。
- **F14 设置页扩展（M12 切片）**：设置模态扩展为**全量配置页**——`project.toml` 全部字段（`[project]` 名称/版本/语言/目标/编译器/仿真器/调试器/烧写器、`[compiler]` 路径/标准/优化/宏定义/头文件目录、`[build]` 配置/输出目录、`[debugger]` 路径/接口/服务器/探针/GDB、`[flasher]` 路径/类型/接口、`[sim]` CPU/周期）+ **全局设置**（终端 shell → `terminal.json`、全局配置键 `$config["key"]` → `config.json`）；保存写回整个 `project.toml`（校验：名称非空、构建配置 debug|release、烧写器类型 openocd|pyocd|jlink|stlink|probe-rs、探针类型 openocd|stlink|jlink|pyocd|probe-rs、语言 C|C++|Rust|Zig|ZR）。
- **F14 Agent 面板（参考 Zed agent）**：输出区新增「Agent」标签（命令面板「打开 Agent 面板」/标签页），任务列表（状态点：排队中/运行中/成功/失败/已取消 + 命令标题）、选中任务输出区、底部 shell 命令输入 + 运行按钮；后台线程跑 `sh -c`/`cmd /C`（平台自适应），stdout/stderr 并行读取流式追加，`drain_events` 轮询重渲染（镜像终端面板）；「清空」保留运行中任务、「全部取消」置取消位。
- **F14 串口助手（M12 切片）**：输出区新增「串口」标签（命令面板「打开串口助手」），枚举端口（`NS_SERIAL_PORT` 覆盖）、波特率/校验/数据位/停止位/流控、打开/关闭、hex 发送/接收、时间戳、清空、收发字节计数；后台线程读端口进接收缓冲，UI 轮询重渲染。参考串口助手。
- **F14 终端设置（M12 切片）**：设置页「终端 shell」跨项目保存到 `terminal.json`；留空=平台默认（**Windows: cmd，Linux/macOS: bash**），`default_terminal_shell`/`terminal_label` 自动识别默认终端路径（PATH 探测）。
- **F1.2 项目模板（F1.2 切片）**：C 项目模板——空模板（`main.c` 空循环 + `counter`）、**LED Blink**（GPIOB 端口翻转，GPIO 驱动）、**UART 回环**（USART1 收发回环）——新建向导新增「模板」步（仅 C 语言显示，`CTemplate` 枚举），生成时写入 `src/main.c` 并按模板填充外设初始化；单测覆盖三模板源码生成。
- **F5/F8 烧写扩展（M5 切片）**：`ns_flash::firmware::convert`（ELF→Intel HEX / 纯 BIN 转换）；烧写面板接入**擦除/校验/读回**（`FlashAction`，读回结果写 `out/readback.bin`）；烧写进度显示（工具输出流式 + 完成状态）。
- **F9 运行编排扩展（M6 切片）**：重启运行会话（停止后重新编译→运行）；运行/调试会话的停止与重启状态机完善。
- **F5 构建系统（Make/CMake）**：`[build] system`（`auto`/`direct`/`make`/`cmake`，默认 `auto`）；`plan_cmake`（`sh -c` 包装 `cmake -S . -B <out>/cmake -DCMAKE_BUILD_TYPE=<config>` + `cmake --build`，`CMAKE_RUNTIME/LIBRARY/ARCHIVE_OUTPUT_DIRECTORY` 指向 `artifact_path()` 父目录，`NS_IDE_DEFINES`/`NS_IDE_INCLUDES` 转发 `[compiler]` 宏与头文件）；`plan_make`（`make -f <makefile> BUILD_DIR=… CONFIG=… OUTPUT=… all`）；`auto` 检测优先 `CMakeLists.txt` → `Makefile` → 直接编译；向导为 C/C++ 生成 `CMakeLists.txt`（`file(GLOB)` 收集 `src/*.c`/`.S`，项目名替换，`SUFFIX ".elf"`）；4 项单测含真实 cmake 端到端（产物在预期路径、宏生效），本机真实 `cmake`/`make` 构建验证。
- **F6 周期级外设时序**：`tick_peripherals_for(cycles)` 按周期数推进外设（Timer 每周期 tick、溢出置 IRQ，`tick_peripherals()` 保留为 1 周期兼容包装）；`step()` 全部 12 个架构路径在返回前统一推进外设——修复 11 个非 RISC-V 分支（MCS-51/ARM/80251/C166/Xtensa/AVR/MSP430/V850/STM8/RX/RL78）此前完全跳过外设推进、以及 cycle-accurate 模式外设慢 N 倍（`cycle_count` 按指令延迟前进但 Timer 只走 1 周期）两处缺陷；2 项单测（含 ARM 外设推进回归测试），ns-sim 185 项全绿。
- **F7 调试扩展（F7 切片）**：运行到光标（`RunToCursor`，DAP `continueTo`）；设置变量值（DAP `setVariable`）；监视（watch）表达式面板（变量面板 watch 输入，`evaluate`）；调试控制台表达式求值入口。
- **F3 编辑器扩展（F3 切片）**：当前文件查找/替换（编辑器内查找栏，`Cmd/Ctrl+F` + 替换）；括号匹配高亮（光标处括号配对高亮）；代码折叠（基于缩进/语法，fold 区域）；文件页签编辑保存（`Cmd/Ctrl+S` 写回磁盘，脏标记）。
- **F3.2 编辑体验补充**：跳转返回历史栈（`Alt+←/→`，`push_nav_entry` 记录跳转并按序回退/前进）；跳转到包含头文件（`#include`/`use` 解析定位，`parse_include_directive`）；迷你地图（编辑器右侧缩略渲染，点击/拖拽定位）；多光标编辑（`Cmd/Ctrl+Alt+↑/↓` 添加垂直光标、`Cmd/Ctrl+D` 选中下一个、`Esc` 清空）；单测覆盖 include 解析与多光标清除。
- **F3.2/F3.3 LSP 扩展（M8 切片）**：签名帮助（`signatureHelp`）；inlay hints（`inlayHint` 请求与渲染）；语义高亮叠加（`semanticTokens` 请求与编辑器着色）。
- **F3.3 符号重命名（M8 切片）**：`Cmd/Ctrl+Shift+R` / 命令面板「重命名符号」按光标标识符发起 LSP `textDocument/rename`，响应 `WorkspaceEdit`（`changes` 映射）解析为文件编辑并**跨文件应用**（`parse_workspace_edit`、`apply_edit_to_text`、`Editor::apply_text_edit`，0 基 line/char 范围替换，越界拒绝）；单测覆盖 `parse_workspace_edit` 展平/跳过畸形项与编辑应用越界拒绝。
- **F11 设备库扩展（M9 切片）**：外部 SVD 文件加载（`parse_svd` 文件入口）；启动文件模板库（按设备核选择更多 MCU 模板）；设备选择写回 `project.toml` 的 SVD 路径。
- **F6 仿真扩展（M7 切片）**：周期级/外设时序（GPIO/TIMER/中断控制器）；仿真断点下发到源码（源码行↔地址映射）；C 源码映射（符号表/行号表 → 编辑器定位）。
- **F6 仿真外设状态视图**：仿真页新增 **GPIO 状态视图**（32 引脚方块，按方向/电平着色——输出高=jade、输入高=accent、低=暗，输出高/输出使能计数）与 **TIMER 状态视图**（每定时器一行：基址、启用/禁用、计数器/自动重载、预分频、溢出计数）；状态随 `tick_peripherals` 周期更新；单测覆盖 GPIO 翻转事件记录与 TIMER 计数/IRQ 触发。
- **F4.2 ZR 标准库（M10 切片）**：ZR 标准库由上游 `zr` 语言提供（`zr pkg` 包管理），IDE 不内置 `zr-std`；外设 API（GPIO/UART/TIMER）经 `zr` 的 FFI/裸机目标使用。
- **F14 静态分析增强（M12 切片）**：`plan_static_analysis` 的 **Zig** 分支现传 `-target <triple>`（复用 `zr_target_triple` 按 MCU 核映射：RISC-V→`riscv32-unknown-none`、ARM Cortex-M→`arm-none-eabi`，未知目标保持主机）及 `-Werror`（分析诊断作为错误进问题面板）；单测 `static_analysis_plan_zig_passes_target_and_werror` 用 stub `zig` 捕获 argv 断言透传。静态分析单测共享 `PATH_LOCK` 串行化 PATH 修改（clang-tidy/zig stub 注入与 `find_program` 扫描竞争），消除并发偶发失败。
- **F3.2/F3.3 签名帮助自动消失**：`signature_anchor: Option<(PathBuf, usize, usize)>` 记录签名帮助显示时的光标锚点；编辑器 `observe` 回调比对当前光标位置，移出锚点（或切换/关闭标签）即清空 `signature_help`，弹窗不再滞留。
- **F3.3 inlay hints 去重**：`Editor::set_inlay_hints` 按 `(line, start)` 组合去重后存储，慢 LSP 响应重叠时不再渲染双提示；单测 `inlay_hints_dedup_by_position` 验证重复项折叠。
- **F6 仿真 TIMER 溢出精度**：`Timer::tick()` 返回 `u64` 精确溢出次数，`counter` 越过 `auto_reload` 若干倍时按 `counter / reload` 累计 `update_count`（`saturating_add`），不再视为单事件；`tick_peripherals` 以 `tick() > 0` 判 IRQ；单测 `timer_tick_counts_exact_overflows` 验算 10 倍溢出/正常递增/恰在重载/禁用四种路径。
- **F6 仿真 GPIO 事件对齐 PC**：`gpio_events` 由 `(cycle, pin, value)` 改为 `GpioEvent { cycle, pin, value, pc }`，记录触发翻转的 store 指令 PC；IDE 仿真页新增「GPIO 事件（最近 6 条）」日志视图（pin/电平/PC/cycle）；单测断言事件 PC 与当前 PC 一致。
- **F3.3 LSP 代码片段补全**：`ns_lsp::CompletionItem` 增加 `insert_text: Option<String>` 与 `snippet: bool`；`parse_completion_items` 优先 `textEdit.newText` > `insertText` > `label`，并读 `insertTextFormat: 2` 标记 snippet；`expand_snippet` 展开 `${1:default}`（取 default）、`$0`/`${0}`（标记最终光标）、`$N` 编号制表位（空）、`$TM_FILENAME`/`$TM_DIRECTORY`/`$TM_FILEPATH`；`Editor::insert_completion(text, replace_prefix, cursor_at)` 删除光标前已输入单词前缀并按 `$0` 定位光标（多光标同步）；`completion_confirm` 接入全部路径；单测覆盖片段解析/展开/插入（5 项）与多光标插入。
- **F3 编辑器自动缩进**：`Enter` 换行时自动延续缩进——`auto_indent_for_line` 继承当前行首空白，行（去尾随空白）以 `{`/`(`/`[` 结尾时追加一级（4 空格）；`Editor::newline` 接入；`Tab` 选中多行时 `Editor::indent_lines` 在每行首插入 4 空格（光标同步），单行/无选区插入 4 空格；单测覆盖自动缩进 6 路径与多行/单行缩进 2 项。
- **F11 设备选择器增强**：调试面板「设备」页顶部新增**目标设备选择器**（`device-picker-current` 点击展开内置 `ns_device::CATALOG` 列表，选中项经 `set_project_target` 写回 `project.toml`，含 SVD 源跟随/清理）；设备页改为可切换 MCU，不再仅靠向导初选。
- **F11 外设寄存器实时监视**：调试面板「设备」页每个寄存器现显示**实时值**——仿真激活时经 `Simulator::read_u32` 读内存（玉色=实时），调试会话停止时经 `refresh_device_registers_on_stop` 按外设基址批量 `ReadMemory` 并在 `DebugMessage::Memory` 回包时写入 `debug.device_reg_values`（调试源优先）；无活动源回退复位值（灰）。
- **F11 外设寄存器写入编辑**：调试面板「设备」页寄存器值**可点击编辑**——点击当前值（`dev-reg-val-*`）进入编辑（`begin_device_reg_edit` 预填当前实时值），Enter 提交（`commit_device_reg_edit` 解析 0x/bare hex 或十进制，32 位上限），写回目标：仿真激活走 `Simulator::write_u32`（状态栏「（仿真）」），否则调试会话走 DAP `writeMemory`（4 字节小端，自动重读回填 `device_reg_values`），两者都无时如实提示；单测 `device_reg_edit_parses_32bit_value`（hex/十进制/拒绝超 32 位）与 `device_reg_edit_commits_to_simulator`（写入到达仿真内存）。
- **F11 外设寄存器树过滤**：调试面板「设备」页外设列表上方新增**过滤输入框**（`device-filter`），按外设名或寄存器名做大小写不敏感子串过滤——外设名匹配时显示该外设全部寄存器（保持组上下文），仅寄存器名匹配时保留外设头作为上下文并只列匹配寄存器，两者都不匹配则隐藏；空过滤恢复完整树；单测 `device_filter_matches_peripheral_and_registers`。
- **F11 设备寄存器描述与位域活值（本轮）**：调试面板「设备」页——外设标题下显示 `peripheral.description`（截断 64 字符）、每寄存器行下方显示 `register.description`（截断 80 字符，省略号）；字段摘要改为 `名=0x值[msb:lsb]`，值从 live 寄存器值按 `bit_offset`/`bit_width` 提取（`device_field_value`，单比特/顶位字段正确掩码，零宽/越界防御返回 0）；辅助 `device_description_short`（trim/截断/省略号/空串）。单测 `device_field_value_extracts_shifted_and_masked` + `device_description_short_truncates_and_preserves`。
- **F11 设备寄存器字段展开（本轮）**：调试面板「设备」页有字段的寄存器行前加**展开箭头**（`▸`/`▾`），点击行展开显示每个字段的 `名[msb:lsb] = 0x值` + `field.description`（非零字段高亮金色，含 SVD 字段描述；惰性构建避免未展开路径分配）；寄存器值点击仍进入编辑（`stop_propagation` 防误触展开）；`toggle_device_reg_expand` 维护展开集（`device_reg_expanded: HashSet<u64>`）。单测 `toggle_device_reg_expand_adds_and_removes`；渲染冒烟测试 `renders_device_picker_and_build_config` 扩展为展开一个 GPIOA 寄存器以走通展开渲染路径。
- **F7 寄存器手动刷新（本轮）**：调试「寄存器」列工具栏新增**「刷新」**按钮（`dbg-regs-refresh`），`refresh_registers` 清空 `register_groups`/`register_group_refs` 缓存并重新下发 `GetVariables`（Registers 作用域），与变量面板 `dbg-vars-refresh` 对称；无 Registers 作用域时如实提示。单测 `refresh_registers_clears_and_reports`。
- **F3 保存所有文件（本轮）**：命令面板「保存所有文件」（`save-all`）+ 快捷键 `Cmd/Ctrl+Alt+S`（`SaveAll` action + `cmd-alt-s`/`ctrl-alt-s` 绑定），经既有 `save_all_editors` 写回全部脏标签（hex/文本，外部修改冲突如实提示）；此前只有「保存当前文件」。单测 `save_all_editors_persists_dirty_tabs`。
- **F6 仿真时钟频率显示**：`project.toml` `[sim]` 新增 `clock_hz`（0=未知，设置页「时钟频率(Hz)」可编辑，保存写回）；仿真页 PC/周期行在配置时钟后显示**仿真时间 + 名义频率**（如「时间: 1.00 s @ 72.0 MHz」，`sim_time_readout`），未配置时钟只显示周期数；单测 `sim_time_readout_formats_cycles_and_clock`。
- **F11 设备文件生成**：
- **F3.3 LSP 补全文档预览**：`ns_lsp::CompletionItem` 增加 `documentation: String`；`parse_completion_items` 解析 `documentation` 的字符串或 `MarkupContent.value`；`render_completion_popup` 在选中项有文档时于列表下方显示独立滚动预览区（上边框分隔、最多 120px，随选中项切换更新）；单测覆盖纯字符串与 MarkupContent 两种形式。
- **F3.3 LSP 悬停提示浮窗**：LSP `textDocument/hover` 结果不再仅写状态栏——`App` 新增 `hover_tooltip: Option<String>` 与 `hover_anchor: Option<(PathBuf, usize, usize)>`；`lsp_hover` 成功分支设置两者（path/line/column），空/超时清空；`render_hover_tooltip` 左上绝对定位浮窗（top/left 12、max_w 560/max_h 300、边框/阴影/独立滚动、等宽多行文本，id `hover-tooltip`），编辑器叠加层在签名帮助后叠加；observe 回调镜像签名帮助——光标移动或换文件时自动清空。状态栏文本保留。
- **F6 仿真多架构框架 + RISC-V 接入**：`ns_sim::Arch`（RiscV/Arm/Mcs51/Xtensa）与 `from_elf_machine`（e_machine 243=RISC-V、40=ARM）/`from_target`（riscv/esp32c→RiscV、esp32→Xtensa、8051/mcs51→Mcs51、stm32/nrf/rp2040/cortex→Arm、未知→RiscV）；`Simulator.arch` + `for_arch`（非 RISC-V 返回「尚未实现」错误）；`load_elf` 校验 e_machine 与 `arch` 匹配（不匹配报错，不再静默误解码）；IDE `start_simulation` 按项目 target→ELF e_machine→默认 RISC-V 选架构，`for_arch` 失败如实提示并清空旧内核，仿真视图 PC 行显示架构标签。**RV32I 中断向量**：`Csr`（mstatus/mtvec/mepc/mcause/mie/mhartid）+ CSR 指令（CSRRW/CSRRS/CSRRC/CSRRWI/CSRRSI/CSRRCI）+ `mret`；`take_interrupt` 取最低挂起启用行（MIE 门控、保存 mepc/mcause、direct/vectored 跳转），`step()` 指令边界取中断；单测覆盖 CSR/mret/向量跳转/门控/定时器 IRQ。
- **F6 8051（MCS-51）仿真内核 + SDCC 构建**：`ns_sim::mcs51::Mc51Core` 全 8 位指令集模拟器——ACC/B/PSW（CY/AC/OV/RS1:RS0/P）、DPTR、SP（复位 0x07）、256B 内部 RAM、64KiB 外部数据 RAM（MOVX @DPTR/@Ri）、64KiB 代码 ROM、4 寄存器组（RS1:RS0）、位寻址区（0x20-0x2F + SFR 位）；指令覆盖 MOV/MOVX/ADD/ADDC/SUBB/INC/DEC/DA/RLC/RRC/SWAP/ANL/ORL/XRL/XCH/XCHD/MUL/DIV、JMP/LJMP/AJMP/LCALL/ACALL/RET/RETI、JZ/JNZ/JC/JNC/JB/JNB/JBC/CJNE/DJNZ、SETB/CLR/CPL/MOV C,bit 等；SFR P0-P3/TCON/TMOD/TL0/TL1/TH0/TH1/IE/IP。`Simulator` 集成：`for_arch(Mcs51)`、`step()` 分发（断点转 u16）、`load_mcs51`、`program_counter()`。`ns_build::plan_mcs51`（`sdcc -mmcs51 --model-small`，缺失如实提示）+ `artifact_path` 8051 返回 `.ihx`；`ns_device::CATALOG` `mcs51` 条目；IDE `start_simulation` 8051 走 Intel HEX 解码（`ns_flash::decode_intel_hex`）加载。单测 10 项覆盖 MOV/ADD/DA/位/MOVX/栈/跳转/SFR/寄存器组/断点。
- **F6 ARM Cortex-M（Thumb/Thumb-2）仿真内核**：`ns_sim::arm::ArmCore`——16 个 32 位寄存器 R0-R15（R13=SP、R14=LR、R15=PC）、xPSR（N/Z/C/V + Thumb 位 + IPSR 异常号）、PRIMASK/BASEPRI/FAULTMASK/CONTROL、VTOR 向量表、64MiB 平坦缓冲；地址窗口映射（flash 0x0800_0000→offset 0、SRAM 0x2000_0000→offset 0x0200_0000，未映射丢弃）。指令：Thumb-1 全 16 位（移位/ADD/SUB/MOV/CMP/ALU/高寄存器/BX/PC 相对 LDR/寄存器与立即偏移 load/store 含 LDRSB/LDRSH/SP 相对/ADR/ADD SUB SP/PUSH POP/STM LDM/条件与无条件分支/SVC）+ Thumb-2 常用 32 位（BL/BLX/MOVW/MOVT/ADDW/SUBW/B.W/LDR STR imm12/LDRB/LDRH/DMB DSB ISB）。向量表复位（SP=vector[0]、PC=vector[1]|1）；`load_elf` 记录最低 PT_LOAD 为 flash 基址。`Simulator` 集成：`for_arch(Arm)`、`step()` 分发（断点/PC 同步）、`reset` 走向量表、`load_elf` 写 ARM 缓冲、`read_reg`/`read_u32`/`read_u8`/`write_u32` 路由；IDE 寄存器视图显示 r0-r15（ARM）与内存视图经映射访问。单测 12 项 arm.rs + 3 项集成。
- **F6 80251（MCS-251）仿真内核**：`ns_sim::x51::X51Core`——16 位累加器 A/B、PSW（CY/AC/OV）、SP、24 位 DPTR、16×16 位寄存器组、64KiB 扩展 RAM（`read_xram`/`write_xram`）、24 位程序空间、`load_image`。指令：NOP、MOV Rn/#imm16、MOV A↔Rn、ADD/SUBB A,Rn（进位/借位）、INC/DEC、MUL/DIV AB、MOVX @DPTR/@Ri、MOV DPTR #imm24、LCALL/RET（24 位 PC）、LJMP、SJMP、JZ/JNZ/JC/JNC、JMP @A+DPTR。`Simulator` 集成：`for_arch(Mcs251)`、`step()` 分发、`load_x51`、`program_counter`/`read_reg` 路由。单测 11 项 x51.rs + 2 项集成。
- **F6 C166/XC166/XC2000 仿真内核**：`ns_sim::c166::C166Core`——16×16 位寄存器组、PSW（N/C/V/Z）、SP、DPP0-3、64KiB 数据/程序空间、`load_image`。指令：NOP、MOV Rn↔Rm/#imm16、ADD/SUB（进位/借位）、AND/OR/XOR、MUL（32 位积）、DIV（商/余）、LOAD/STORE 字、JMP rel、JZ/JNZ、CALL/RET（栈）。`Simulator` 集成：`for_arch(C166)`、`step()` 分发、`load_c166`、`program_counter`/`read_reg` 路由。`ns_build`：8051 家族含 80251 默认 SDCC（`-mmcs51` 兼容子集），C166 需供应商工具链（Tasking/Keil）未安装时报清晰错误；`ns_device::CATALOG` 增加 80251/C166/XC166/XC2000 条目；IDE `start_simulation` 80251/C166 走 Intel HEX/raw 加载。单测 11 项 c166.rs + 2 项集成。
- **F6 Xtensa（LX6/LX7, ESP32/ESP32-S3）仿真内核**：`ns_sim::xtensa::Lx6Core`——64 物理寄存器窗口（4×16, `window_base`）、A0-A15 架构寄存器（A0=返回地址、A1=栈指针）、PS、64MiB 平坦缓冲（ESP32 代码 0x4000_0000/数据 0x3FF0_0000 窗口,低位直映）。指令：NOP、RRR 算术（ADD/SUB/AND/OR/XOR,op2 选操作）、MOVI（imm12 符号扩展）、L32I/S32I/L8UI/S8I（imm4）、MOV、EXTUI、BEQ/BNE/BLT/BGE、CALL0/CALLX0、RET（A0 返回）。`Simulator` 集成：`for_arch(Xtensa)`、`step()` 分发（断点/PC 同步）、`load_image`/`write_bytes`/`zero_bytes` 路由、`read_reg`（a0-a15）/内存访问路由。`from_elf_machine` 加 EM_XTENSA（94）。`ns_build::plan_xtensa`（`xtensa-esp32-elf-gcc -mlongcalls`）；`ns_device::CATALOG` 增加 esp32/esp32s3；IDE `start_simulation` ELF 加载与 a0-a15 寄存器视图、默认内存地址 0x3FF0_0000。单测 14 项 xtensa.rs + 3 项集成。
  - **F6 仿真页观测增强**：`ns_sim::reg_count_for`/`reg_label_for` 按架构返回寄存器数量与标签（RISC-V 32 x0-x31、ARM 16 r0-r15、Xtensa 16 a0-a15、8051 13 ACC/B/PSW/SP/DPTR+当前 R0-R7、80251 21 +R0-R15、C166 16 R0-R15、AVR 32 R0-R31、MSP430 16 PC/SP/SR/CG2+R4-R15、V850 32（r/sp/gp/tp/ep/lp）、STM8 5 A/X/Y/SP/CC、RX 16 r0-r15、RL78 16 X/A/C/B/E/D/L/H+bank）；`Simulator::read_reg` 增加 `mcs51` 分支（索引 0-4 映射 ACC/B/PSW/SP/DPTR、5-20 按 PSW RS1:RS0 选 bank 的 R0-R7）；`ns_sim::disassemble_one(arch, pc, read)` 各架构单条指令反汇编（RISC-V 全 opcode/ARM Thumb-1/Xtensa/8051/80251/C166/AVR/MSP430/STM8/RX/RL78/V850）；IDE 仿真页寄存器视图改用新 API 显示专用标签，PC/周期行下显示当前指令（等宽强调色），断点列表显示地址+反汇编并在 PC 命中断点时红色高亮「← 命中」。单测 `disassembles_riscv_instructions`/`disassembles_xtensa_and_8051`/`register_metadata_per_architecture`（验证 11 架构寄存器计数与标签）。
- **F6 仿真调试体验**：IDE 仿真页新增「步过」按钮（id `sim-step-over`，接 `ns_sim::step_over`：运行至调用返回/断点/停机），`sim_step_over` 方法镜像 `sim_step` 状态更新；`sim_run` 断点命中后经 `line_for_address`+`source_files` 打开源文件、`set_cursor_line` 定位、`set_debug_line` 高亮当前行（镜像调试器停靠，借用提前释放避免冲突）。仿真页内存视图新增 hex 编辑框（`sim-mem-write`，Enter/按钮提交）与 `sim_write_memory`（解析 hex → `write_bytes`，写后自动重读）；`ensure_sim_mem_input` 惰性创建字段（Enter 提交）。
- **F3.3 LSP 文档格式化**：`ns_lsp::LspCommand::Formatting` + `textDocument/formatting` 请求（`options` tabSize 4/insertSpaces true）；`parse_format_edits` 解析 `TextEdit[]`（0 基 range+newText）；IDE 命令面板「格式化文档」（`format-document`，`shift+alt+f`）经 `lsp_format_document` 请求并用 `Editor::apply_text_edit` 应用（无变更/超时如实提示）。
- **F3.3 LSP 代码动作**：`ns_lsp::LspCommand::CodeAction` + `textDocument/codeAction` 请求（range 含光标、context 空诊断）；`parse_code_actions` 解析 `CodeAction[]`（title + `edit.changes` 经 `parse_workspace_edit`，跳过 command-only 动作）；IDE 命令面板「快速修复（代码动作）」（`code-action`）经 `lsp_code_action` 应用首个含 edits 的动作（`apply_workspace_edit` 复用，无动作/命令型如实提示）。单测 `parses_format_edits_array`/`parses_code_actions_with_edits`（2 项）。
- **F6 仿真寄存器/PC 编辑与断点管理**：`ns_sim::write_reg(index, value)` 跨架构路由（ARM `regs`、8051 ACC/B/PSW/SP/DPTR+按 PSW 选 bank 的 R0-R7、80251、C166、Xtensa `set_reg`、RISC-V `registers`，x0 只读忽略；`read_reg` 逆操作）；`ns_sim::set_program_counter(pc)` 同步顶层 `pc` 并路由各架构（ARM `set_pc` 保留 Thumb 位、Xtensa `set_pc`、8051/80251/C166 16 位 PC；`program_counter()` 逆操作）。IDE 仿真页寄存器单元格可点击（`sim-reg-N`）→ `begin_reg_edit` 预填当前值并聚焦 → 回车 `sim_write_reg`（hex → `write_reg`）；PC 行可点击（`sim-pc`）→ `begin_pc_edit` → 回车 `sim_set_pc`；断点区新增地址输入（`sim-bp-add`）+「添加」→ `sim_add_breakpoint`，列表每行「✕」（`sim-bp-del-N`）→ `sim_remove_breakpoint`。单测 `write_reg_routes_per_architecture`/`write_reg_mcs51_and_x51_special_registers`/`set_program_counter_routes_per_architecture`（3 项）。
  - **F6 仿真 Keil/IAR 全芯片支持（规划）**：以 Keil 官方产品家族（ARM7/ARM9/Cortex-M、C16x/XC16x/ST10、C251/80C251、Cx51/8051）与 IAR 支持架构（Arm、RISC-V、AVR、MSP430、STM8、Renesas RL78/RX、8051、C166、V850）为范围，对照 `ns_sim::Arch` 逐项梳理。**已支持（全部）**：RISC-V（RV32I）、ARM（A32 ARM7/ARM9/Cortex-A + Thumb/Thumb-2 Cortex-M，`arm_state` 标志切换）、8051/80251（Mcs51/Mcs251）、C166/XC166/XC2000/ST10（C166，ST10 同源可复用）、Xtensa（ESP32/ESP32-S3）、AVR（ATmega/ATtiny/AT90，IAR AVR）、MSP430（IAR MSP430）、STM8（IAR STM8）、Renesas RX（RX600/RX62N）、Renesas RL78（RL78/G10-G14，IAR RL78）、Renesas V850（V850E，IAR V850）。**范围原则**：优先实现纯软件可完整验证的架构（AVR→MSP430→ARM 态→STM8→Renesas）；厂商工具链（Tasking/Keil/IAR）未安装时构建后端如实报错，不假装可用；每个架构新增 `Arch` 变体 + `ns_sim` 内核 + `from_target` 识别 + 设备目录条目 + 反汇编 + 单测。单测 `for_arch_supports_implemented_cores` 验证 11 个架构均可构造、`from_target_maps_every_catalog_key` 验证全部 24 个设备目录键（含 ST10→C166、AT90S8515→AVR）映射到正确架构。
  - **ARM A32（ARM7/ARM9/Cortex-A）**：`ArmCore` 的 `arm_state` 标志选择 A32 解码（`execute_arm`，32 位字）；`reset_from_vector_table` 按向量表类型自动进入正确状态——Cortex-M 数据向量（word0=初始 SP、word1=复位 PC|Thumb 位）走 Thumb，ARM7/ARM9 代码向量（word0 为 `0xE` ARM 分支指令）用 ELF 入口（位 0 清=ARM 态）复位，SP 取默认；`map_addr` 覆盖 ARM7/ARM9 SRAM 窗口（S3C2440 0x3000_0000、LPC2148 0x4000_0000）与 Cortex-M 窗口（0x0800_0000/0x2000_0000）。IDE 仿真页默认内存地址按目标选择：LPC21xx/22xx→0x40000000、S3C24xx→0x30000000、其余 ARM→0x20000000（与设备目录 flash/ram 对齐）。覆盖 A32：条件执行（cond 字段）、分支/BL、BX/BLX（寄存器，自动切换 Thumb）、数据块传输（STM/LDM IA/IB/DA/DB + 回写）、单数据传输（LDR/STR 字/字节，寄存器/立即偏移）、半字/有符号传输（LDRH/STRH/LDRSB/LDRSH）、数据操作（AND/EOR/SUB/RSB/ADD/ADC/SBC/RSC/TST/TEQ/CMP/CMN/ORR/MOV/BIC/MVN，操作数 2 移位）、MUL/MLA、长乘（UMULL/SMULL/UMUAL/SMUAL）、MOVW/MOVT、MRS/MSR（CPSR 标志位）、CLZ、SWP（如实 halt）、SVC（取异常）。`from_target` 识别 ARM7/ARM9/ARM926/ARM920/LPC21x/LPC22x/S3C24xx/AT91/i.MX（`lpc2148`/`s3c2440`/`at91sam7`/`imx6`/`arm926ej-s` → `Arch::Arm`）；设备目录新增 `lpc2148`（ARM7TDMI-S，flash 0x0/RAM 0x4000_0000）与 `s3c2440`（ARM920T，flash 0x0/RAM 0x3000_0000）条目；A32 单测 12 项（块传输/BX 切 Thumb/MLA/MOVW-MOVT/复位向量选 ARM 态等）。**A32 反汇编**：`disassemble_one_state(arch, pc, read, arm_state)` 按 `arm_state` 选 A32/Thumb 解码（`disassemble_a32` 覆盖分支/BL、BX/BLX、块传输、单/半字传输、数据操作、MUL/MLA、长乘、MOVW/MOVT、MRS/MSR、CLZ、SVC、条件后缀）；`Simulator::arm_state()` 暴露状态，IDE 仿真页当前指令与断点列表改用状态感知反汇编；单测 `disassembles_a32_instructions` 验证 A32/Thumb 分流与 MOVW/ADD/LDR/B 解码。**A32 构建后端**：`plan_build` 识别 ARM7/ARM9/ARM926/ARM920/LPC21xx/22xx/S3C24xx/AT91/i.MX 目标，走 `plan_arm_a32`（`arm-none-eabi-gcc -marm -mcpu=<core>`，强制 ARM 态而非 Thumb，`-mcpu` 按核选 `arm7tdmi`/`arm920t`/`arm926ej-s`/`cortex-a7`）；`zr_target_triple` 为 ARM7TDMI-S/ARM920T/ARM926EJ-S/Cortex-A 返回 `arm-none-eabi`；单测 `arm_a32_build_plan_forces_arm_state_and_cpu` 验证 `-marm`/`-mcpu=arm7tdmi`。复位与内存窗口单测：`arm7_code_vector_resets_to_arm_state_entry`/`cortex_m_data_vector_resets_sp_and_pc`。
  - **RL78 内核（本次新增）**：`ns_sim::rl78::Rl78Core`——8 位寄存器 X/A/C/B/E/D/L/H（双 bank）、16 位对 AX/BC/DE/HL、PSW（Z/AC/CY/RBS/IE）、SP、CS/ES 段寄存器（20 位寻址）、1 MiB 平坦内存。覆盖 MOV/MOVW（全部寻址模式）、ADD/ADDC/SUB/SUBC/CMP/AND/OR/XOR（字节+字，reg-reg 与 ES:[HL+imm]/ES:[addr]）、INC/DEC/INCW/DECW、PUSH/POP（rp 与 PSW）、RET/RETI/RETB、BR AX、BRK/BRK1/HALT/STOP、SKc/BH/BNH、XCH、ROL/ROR/ROLC/RORC/ROLWC、MOVS。`Arch::Rl78` 变体 + `from_target`（rl78）+ `from_elf_machine`（EM_RL78=197）+ 设备目录条目（rl78g14）+ 反汇编 + 单测 13 项 + 集成测试 1 项（load/run/read_reg/PC）。构建后端 `plan_rl78`（`rl78-elf-gcc -mmcu=rl78`），IDE 仿真页经 ELF 加载、内存默认 0xFFE00。
  - **Keil/IAR 构建产物与工具链指引**：SDCC 目标（MCS-51/80251/STM8）构建产出 Intel HEX（`.ihx`），ELF 目标产出 `.elf`；`Project::artifact_path` 现按目标识别 HEX 格式（含 80251/STM8），IDE 能正确找到构建产物；`missing_guide` 覆盖 Keil/IAR 交叉工具链（`avr-gcc`/`msp430-elf-gcc`/`v850-elf-gcc`/`rx-elf-gcc`/`rl78-elf-gcc`/`sdcc`），缺失时如实提示安装指引。
- **F1.2 FreeRTOS 模板**：新建向导「模板」步新增 **FreeRTOS** 选项（`CTemplate::Freertos`）——自包含协作式任务调度器 + 两个任务（`task_led`/`task_monitor`），无需外部内核下载即可构建；换成真正 FreeRTOS 内核时把调度循环改为 `vTaskStartScheduler`、任务函数改 `xTaskCreate` 入口即可。源码经 host gcc `-Wall -Wextra` 零警告编译验证；单测 `c_template_generates_matching_source` 扩展覆盖 FreeRTOS 分支。
- **F13 浅色主题**：`theme.rs` 双调色板——默认暖墨金深色 + 浅色（`L_BG_0..L_DARK_TEXT`，暖纸底/深墨字/加深金玉朱强调）；`theme::set_light`/`is_light`/`resolve` 把全部色令牌经 `color`/`alpha` 路由到活动主题（286 处 `rgb(theme::X)` 直调改走 `theme::color`，不再绕过映射）；命令面板「切换主题（深/浅色）」（`toggle-theme`）切换并持久化到 `theme.json`（`ns_project::ThemeSettings`/`ThemeMode`，`App::new` 启动恢复）；单测覆盖 resolve 映射/未知字面量保持与 ThemeSettings JSON 往返。
- **F14 崩溃恢复增强（会话状态）**：`Session` 新增 `views: Vec<FileView>`（每文件 `path`/`cursor`/`scroll_ratio`）与 `saved_at` 时间戳；`Editor::scroll_ratio` 暴露滚动位置，`save_session` 写入每文件光标+滚动，`load_project` 恢复（`set_cursor_line_column`+`scroll_to_ratio`）；`Session::save_stamped` 记录墙钟时间用于崩溃恢复排序；打开项目后每 30s 后台自动快照会话（崩溃最多丢失 30s 标签/光标状态，而非整个编辑器）；单测覆盖往返/时间戳。
- **F13 多窗口/多工作区**：命令面板「新建窗口」（`new-window`）经 GPUI `App::open_window` 打开独立 `IdeApp`（每窗口独立工作区：标签/输出/面板/项目选择互不影响），`open_new_window` 复用主窗口的 `WindowOptions`（1380×860、最小 980×620、毛玻璃标题栏）；palette 单测覆盖命令注册。
- **F1.2 文件树拖拽移动**：`TreeDragPayload`（`source`+`is_dir`）经 GPUI `on_drag`/`on_drop`/`can_drop`——目录项接受放下（`can_drop` 拒绝文件项/自投/目录投入），树容器空放下→项目根；`move_tree_entry` 调 `std::fs::rename` 并刷新打开标签路径；纯函数 `tree_move_plan` 校验（拒绝同父/自身/后代/目标已存在），单测覆盖拒绝与目标计算。
- **F7.1 断点命中计数持久化**：`Breakpoint` 新增持久化字段 `hits: u64`（`mark_hit` 每次命中 `saturating_add(1)`，`#[serde(default)]` 向后兼容旧状态文件）；`BreakpointStore::reset_hits` 清零；断点面板状态徽标旁显示「命中 N」胶囊（命中数>0），随 `.ns-ide/breakpoints.json` 持久化，重开项目恢复；单测 `hit_count_increments_persists_and_resets` 覆盖累加/往返/清零。
- **F6 仿真看门狗外设（本轮新增）**：`ns_sim::Watchdog`——内存映射 WDT（`base+0x00` 喂狗写 `0xAA55` 重载、`base+0x04` 启用/禁用、`base+0x08` 超时周期、`base+0x0C` 读复位计数、`base+0x10` 读复位 PC）；`tick_peripherals_for` 按周期推进，超时 `reset_count+1` 并记录 `last_reset_pc`（固件未喂狗跑飞可见而非静默停滞）；`peripheral_read`/`peripheral_write` 路由 WDT 寄存器；IDE 仿真页新增「看门狗」状态视图（启用/超时/已过/复位次数）；单测 `watchdog_fires_when_unfed_and_resets_on_feed`（启用→超时复位、喂狗重载、非喂狗不重载）。
- **F8 烧写外部 loader 完善（本轮新增）**：`FlashRequest.device` 现由 IDE 传入项目 `target`；pyOCD 适配器用 `request.device`（非空覆盖硬编码 `stm32f103c8`）、J-Link 用 `request.device` + FLM `base_address`/`device_size`（`verifybin`/`savebin` 显式基址与尺寸，替代硬编码 `0x8000000`/`0x8000`）；FLM 几何（`base_address`/`page_size`）此前已接入 OpenOCD，现扩展至全部适配器。
- **F13 快捷键可配置（本轮新增）**：`ns_project::KeybindingOverrides`（`keybindings.json` 用户配置目录，action 名→键位字符串，`BTreeMap` 确定性序列化，空值=恢复默认）；`app::bindings_with(&overrides)` 按 action 名应用覆盖（覆盖时该 action 只保留一条绑定，不再产生重复 cmd/ctrl 对）；`main.rs` 启动时加载覆盖；命令面板「快捷键设置…」（`keybindings`）打开编辑模态（每可重映射 action 一行：命令名+默认键位+编辑框，保存/取消，Enter 确认、Esc 取消）；单测 `overrides_round_trip_via_serde`/`empty_value_is_treated_as_unbound`/`save_drops_empty_values`/`bindings_apply_keystroke_overrides`。
- **F3 编辑器分屏（本轮）**：命令面板「编辑器分屏（右侧）」/ `Cmd/Ctrl+\` 打开**独立第二编辑窗格**（`SplitPane`：独立 `Entity<Editor>` + LSP 同步/断点/右键菜单/定义订阅，`build_split_editor` 复用标签页接线）；分屏关闭 `Cmd/Ctrl+Shift+\` /「关闭分屏」；分屏脏状态经 `Cmd+S` 一并保存（`save_split_pane`，外部修改同样弹窗）；断点增删经 `sync_editor_breakpoints` 同步到分屏；会话持久化（`Session.split` 项目相对路径，重开项目恢复分屏）；单测 `split_view_opens_and_closes`。
- **F4.1 单元测试集成运行器（本轮）**：`ns_build::plan_tests` 按语言/构建系统选测试命令——Rust `cargo test --target-dir <build> -j N`、C/C++ CMake `ctest --test-dir <build>/cmake`、Makefile `make test`、Zig `zig build test`（需 `build.zig`）、ZR/汇编 `None`（无内建运行器，如实提示）；命令面板「运行单元测试」（`run-tests`）→ 输出面板流式结果 + 问题面板诊断；单测 `plans_test_runner_by_language_and_build_system`。
- **F14 静态分析增强——栈使用分析（本轮）**：`ns_build::plan_stack_usage`（C/C++：`-fstack-usage -c` 逐源编译到 `<build>/stack-usage/`）、`parse_stack_usage_su`（`\t<file>\t<function>\t<bytes>\t<static|dynamic>`）、`collect_stack_usage`（扫描 `.su` 并按文件/栈深排序）；命令面板「栈使用分析（-fstack-usage）」（`stack-usage`）→ 输出面板按函数列出静态/动态栈深；单测 `parses_stack_usage_and_plans_only_for_c`。
- **F14 覆盖率工具接入（本轮）**：`ns_build::plan_coverage`（C/C++：`--coverage` 编译+链接单测试二进制）、`run_coverage`（运行二进制 → `gcov -o <dir> <stem>` 汇总）、`parse_gcov_report`（`File '<path>'` + `Lines executed:<pct>% of <n>`）、`CoverageFile::percent`；命令面板「覆盖率分析（gcov）」（`coverage`）→ 输出面板按文件列出行覆盖率；`gcov` 缺失/非 C 如实提示；单测 `parses_gcov_report_and_plans_coverage_only_for_c` + 真实 gcov 端到端 `coverage_runs_end_to_end_with_gcov`。
- **F3.2 全局符号搜索（workspace/symbol，本轮）**：`ns_lsp` 新增 `WorkspaceSymbols` 命令 + `workspace_symbols(query)` + `parse_workspace_symbols`（展平 `SymbolInformation[]` 为 `WorkspaceSymbolInfo`：name/kind/path/line/column）；命令面板「搜索项目符号…」/ `Cmd/Ctrl+Shift+O` 打开浮动弹窗（`workspace-symbol-popup`：查询输入 + 结果列表，↑/↓ 选择、Enter/点击跳转到符号所在文件与行、Esc 关闭，光标移出自动关闭，generation 计数丢弃过期响应）；单测 `parses_workspace_symbols_flat_and_nested`（LSP 侧）与 `workspace_symbol_search_navigates`（跳转）。
- **F3 快速打开文件（Quick Open，本轮）**：`search::collect_files`（展平项目源文件，跳过 `.git`/`build`/`target`/`node_modules`/`.ns-ide`、隐藏项与二进制）；命令面板「快速打开文件…」/ `Cmd/Ctrl+P` 打开弹窗（`quick-open-popup`：输入过滤项目文件（大小写不敏感子串）、Enter/点击打开、Esc 关闭，空查询列出全部）；单测 `collect_files_lists_sources_only`（search 侧）与 `quick_open_filters_and_opens`。
- **F1.2 最近文件列表（本轮）**：`ns_project::RecentFiles`（`.ns-ide/recent_files.json`，项目内最近打开文件，最前=最新，去重，上限 100，`push`/`save`/`load`/`retain_existing`）；`open_file` 时 `remember_recent_file` 记录（相对项目根，canonicalize 归一化 `/var`↔`/private/var`）；命令面板「最近打开的文件…」/ `Cmd/Ctrl+E` 打开弹窗（`recent-files-popup`：列表 + ↑/↓/Enter/Esc）；单测 `push_moves_to_front_and_dedupes`/`save_load_round_trips_and_prunes_missing`（ns-project）与 `recent_files_records_and_reopens`。
- **F1.2 文件树符号过滤（本轮）**：侧栏「项目」标题下新增过滤输入框（`tree_filter_field`，observe 实时更新 `tree_filter`）；非空时 `filtered_tree_entries` 展平为过滤结果（匹配文件 + 其祖先目录为上下文，目录标记展开；过滤态目录点击为无操作）；空时恢复完整树；单测 `tree_filter_narrows_and_clears`。
- **F6 仿真 step_out 返回识别完善（本轮）**：`ns_sim::disassemble_riscv` 对 `funct12==0x302` 的 `0x73` 指令输出 `mret`；`is_return_mnemonic` 增加 `mret` 及 ARM 返回形式（`bx lr`/`bx r14`/`blx lr`，`pop`/`ldm`/`ldmia`/`ldmfd` 含 `pc`/`r15`）；`disassemble_arm_state` Thumb-1 解码补 `bx`/`blx`（0x4700-0x477F）与 `pop`（0xBC00-0xBDFF，含恢复 PC）；`step_out` 现可在 RISC-V `ret`/`mret`、ARM `bx lr`/`pop {..,pc}` 处停靠；单测 `step_out_stops_at_returns`/`step_out_stops_at_mret_after_interrupt` 与 `return_mnemonic_detection` 扩展。
- **F14 覆盖率编辑器内联可视化（本轮）**：编辑器新增 `set_coverage`（按行命中数，0=未覆盖），gutter 覆盖条 + 行背景按覆盖率着色（未覆盖=jade 底、部分/全覆盖渐弱）；`start_coverage` 完成后 `apply_coverage_to_editors` 把 `CoverageFile` 的逐行命中写入每个已打开编辑器；`finish_coverage` 状态行去重；单测 `editor_coverage_stores_and_clears`。
- **F7 寄存器快照导出（本轮）**：寄存器视图标题栏新增「导出快照」按钮（`dbg-regs-export`），`export_register_snapshot` 把当前 `debug.register_groups` 按组写成 `.ns-ide/register-snapshot.json`（名称/值，组排序）；状态栏报告路径或失败原因；空寄存器集如实提示；单测 `export_register_snapshot_writes_json`。
- **F8 固件大小符号级分析（本轮）**：`ns_flash::FirmwareSize` 新增 `symbols: Vec<SymbolSize>`（name/size/address）；`analyze_symbols`（`nm --size-sort --radix=d`，`ns_project::toolchain::find_command` 定位）与 `parse_nm_symbols`（过滤 absolute/undefined/debug/零大小符号，按大小降序）；`render_firmware_size_summary` 追加「最大符号: …」前 6 名；单测 `parses_nm_symbols_and_sorts_by_size`。
- **F5 构建时间逐文件统计（本轮）**：`ns_build::BuildOutcome` 新增 `file_timings: Vec<FileTiming>`（file + elapsed_ms）；`execute_stream_inner` 在逐行流式时用 `file_from_build_line` 识别 CMake/Make `Building …` 与编译器 `-c <file>` 标记，记录真实墙钟间隔；`finish_build` 追加「文件编译耗时（估计）」排序列表到构建输出；非流式 `execute` 返回空（无墙钟跟踪）；单测 `file_from_build_line_extracts_source`。
- **F5 构建错误快速修复（本轮）**：`ns_build::QuickFix` 枚举（`AddInclude`/`AddLinkFlag`/`Hint`）+ `suggest_quick_fix`（缺失头文件→`AddInclude`、未定义引用→符号提示、未声明标识符→提示、缺分号→提示）；问题面板每个有修复的诊断显示「快速修复」按钮（`quick-fix-N`），`apply_quick_fix` 对 `AddInclude` 用 `insert_include_line` 在最后一个 `#include` 后插入并写盘+`reload_text` 刷新编辑器；单测 `suggests_quick_fix_for_common_errors`/`insert_include_line_after_last_include`。
- **F6 trace 触发/统计 + DWT PC 采样布防（本轮）**：`ns_debug::swvd::TraceConfig` 新增 `pc_sample: bool`（持久化到 `.ns-ide/trace.json`）；`requests()` 在启用时写 `DWT_CYCCNT=0`（0xE0001004）、比较器 0 周期模式（`dwt_function_cycles`）、DWT_CTRL 置 CYCCNTENA；`TraceStats`/`trace_stats` 统计 stimulus/DWT/异常/PC 采样；IDE trace 面板新增「PC 采样」开关（`dbg-trace-pc-sample-pill`，`trace_toggle_pc_sample`）与「实时包」统计行；单测 `pc_sampler_arms_cycle_counter`/`trace_stats_counts_kinds_and_pc_samples`。
- **F1.2 厂商工程导入（本轮）**：`ns_project::import`——解析 IAR `.ewp`（`<file><name>` 源码 + `<state>` 包含目录）、Keil `.uvprojx`（`<File><FilePath>` 源码 + `<IncludePath>`）、STM32CubeMX `.ioc`（`Mcu.Name`/`Mcu.IP`），产出 `ImportedProject`（`ProjectConfig` + 源文件绝对路径 + 说明）；命令面板「导入工程（IAR/Keil/STM32CubeMX）…」→ 选厂商文件 → 写 `project.toml`、复制源文件到 `src/`、打开新项目（工具链默认 `arm-none-eabi-gcc`+`openocd`）；单测 5 项（ewp/uvprojx/ioc/目标识别/未知扩展拒绝）。
- **F11 外设初始化代码生成（本轮）**：`ns_device::PeripheralConfig` + `generate_peripheral_init`（按 SVD 外设生成 `xxx_init()` 助手 + `peripheral_init()` 聚合，配置寄存器以复位值打底）+ `peripheral_names`；命令面板「生成外设初始化代码（SVD 配置）…」按项目 target 选 GPIO/UART 外设写 `src/peripheral_init.c` 并打开；单测 4 项（生成/跳过未知/外设列表/未知 target）。
- **F6 profiler 热点聚合（本轮）**：`ns_debug::profile::ProfileAggregator`——按地址归并 DWT PC 采样到 `nm` 符号（二分定位，间隙/未知计入 `<unknown>`），`hotspots()` 按命中降序；IDE trace 面板「热点」开关（`dbg-trace-profile-pill`，`trace_toggle_profile`，开启时用固件符号重建聚合器）→ 面板下方热点直方图（函数名/占比条/命中数）；单测 3 项（归并/未知/空符号）。
- **F5/F11 内存布局可视化（本轮）**：`ns_build::layout`——`parse_linker_memory` 解析 `MEMORY { NAME (attrs) : ORIGIN = .., LENGTH = .. }`（K/M/G 后缀）、`assign_sections` 把固件段按地址归入区域、`MemoryLayout`/`MemoryRegion`/`SectionUsage`；命令面板「内存布局视图（链接脚本区域/段占用）」→ 读 `link.ld` + 固件符号 → 输出面板区域占用条（已用/容量/百分比）+ 前 8 大段；单测 3 项（区域解析/段归入/`size` 报告）。
- **F3.2 include 依赖图（本轮）**：`ns_project::depgraph`——`parse_include` + `build_include_graph`（扫源文件、解析 `#include`、按 `[compiler] includes` + 所在目录解析本地头、记录 deps/dependents）+ `transitive_dependents`；命令面板「include 依赖图（项目级）」→ 输出面板每文件列出直接依赖与反向计数（头文件金色高亮）；单测 3 项（解析/构建+本地解析/传递依赖）。
- **F3.2 依赖图 DAG 图形渲染（本轮）**：`ns_project::depgraph` 新增 `dag_layout`（按最长依赖路径分层：`DagNode{path,column,row}` + `DagEdge{from,to}`，`IncludeGraph` 遍历入度 0 起点、最长路径列、同列行分配）+ `DagLayout`；include 依赖图输出面板头部新增「图形/列表视图」切换（`include-graph-mode`），图形视图用绝对定位节点盒 + 双段水平边渲染分层 DAG（头文件金色、源文件米白）；单测 `dag_layout_layers_by_longest_dependency_path`。
- **F5 宏展开/预处理输出（本轮）**：`ns_build::plan_preprocess`（C/C++：`resolve_compiler` + `-std`/`-D`/`-I` 同 `plan_native`，追加 `-E -P` 对全部 C/C++ 源单次预处理并流式输出；非 C/C++ 或无编译器返回 `None`）；命令面板「宏展开/预处理输出（C/C++ -E）」（`preprocess`）→ 输出面板流式显示展开后的翻译单元（`-P` 去除行号标记，可读源码）；单测 `preprocess_plan_uses_compiler_flags_and_e`/`preprocess_plan_none_for_non_c` + 真实 cc 端到端（`SQUARE(LED)` 展开为 `((7)*(7))`）。
- **对比嵌入式 IDE 补齐（本轮）**：对照 IAR/Keil/STM32CubeIDE/PlatformIO/VSCode 能力模型逐项核对，补齐 11 项：
  - **F3 编辑器基础操作**：行注释切换（`Ctrl+/`，选区多行 `//`）、重复行/删除行（`duplicate line`/`delete line`，`Cmd/Ctrl+Shift+↑/↓` 与 `Cmd/Ctrl+Shift+K`）、移动行（`Alt+↑/↓`）、选中展开/收缩（`Shift+Alt+←/→`，词边界感知：展开跳过分隔符、收缩排除尾部分隔符）。
  - **F7 调试体验**：寄存器值编辑（调试会话中 `✎` 打开 `setVariable` 对话框修改寄存器值）；内存视图搜索/填充（`search_memory`/`fill_memory` 已接入，命中高亮）；watch 表达式持久化（`persist_breakpoints` 把观察点写入 `.ns-ide/breakpoints.json`，重开恢复）。
  - **F8 固件大小摘要增强**：`FirmwareSize` 新增 `text`/`data`/`bss` 字段；`analyze_size_breakdown`（`size` 工具三列解析）+ `parse_size_breakdown`；`render_firmware_size_summary` 追加「分区: text N B · data N B · bss N B」行。
   - **F8 烧写后自动校验（verify 链）**：`FlashMode::Verify`——「烧写并校验」（`flash-and-verify` 命令/`topm-flash-verify` 菜单）在烧写成功后自动链式执行 `FlashAction::Verify`（构建失败即停，不静默继续）。
  - **F3.2 头文件/源文件切换**：命令面板/右键「切换到头文件/源文件」（`goto-header-source`）解析 `#include` 定位对应 `.h`/`.c`（同目录 + `[compiler] includes`），打开并聚焦对应文件；无对应文件时如实提示。
  - **F3 编辑器标签右键菜单**：标签页右键菜单「关闭其他标签」「关闭右侧标签」「关闭全部标签」；`close_tab` 真正移除标签（不只改活动索引），`close_other_tabs`/`close_tabs_to_right` 精确保留目标标签；单测 `tabs_close_and_other_tabs_keep_target` 覆盖关闭中间标签与关闭其他。
  - **F7 调试 watch 表达式持久化**：watch 表达式（非运行值）持久化到 `.ns-ide/watch.json`，重开项目恢复（`persist_watches`/`load_watches`）；跨调试会话保留（`launch_debug` 经 `std::mem::take` 把旧 watches 带入新会话）；单测 `watch_expressions_persist_and_reload` 覆盖往返与去重。
  - **F7 变量面板手动刷新**：变量页标题栏新增「↻ 刷新」按钮（`dbg-vars-refresh`），清除缓存的作用域/变量并对当前栈帧重新下发 `GetScopes`；无栈帧时如实提示；单测 `refresh_variables_clears_and_reports`。
  - **F5 构建配置下拉选择器**：状态栏「构建配置」改为下拉（`build-config-opt-debug/release`），选中即写回活动目标对应 `[[targets]]` 条目（无条目则新建，保存失败按 `RollbackKind` 精确回滚），`toggle_build_config` 拆分为 `set_build_config` 并注册命令面板「切换构建配置（debug ↔ release）」（`toggle-build-config`）；单测 `set_build_config_persists_to_project` 覆盖持久化/幂等/非法拒绝。
  - **F3.3 调试控制台命令历史**：调试控制台输入框回车求值后记录命令（`DebugView::push_console_history`，去重连续重复、上限 100）；`↑`/`↓` 在历史中导航（首次按 `↑` 保存当前草稿，越过最新恢复草稿）；单测 `debug_console_history_navigates_and_dedupes` 覆盖导航/去重/空历史。
  - **F3 跳转到行**：命令面板/`Cmd/Ctrl+G`「跳转到行…」（`go-to-line`）打开对话框输入 1 基行号，回车/「跳转」经 `Editor::set_cursor_line` 移动光标（越界自动夹紧，`Editor::line_count` 计算逻辑行数），非数字如实提示；单测 `go_to_line_dialog_jumps_and_clamps` 覆盖跳转/夹紧/非法输入。
  - **F1.2 文件树右键增强**：文件树右键菜单新增「复制路径」（`tree-menu-copy-path`，经 `cx.write_to_clipboard` 写绝对路径到系统剪贴板）与「在访达中显示」（`tree-menu-reveal`，macOS `open -R` / 其他平台 `xdg-open` 父目录），均关闭菜单并在状态栏报告。
  - **F1.2 文件树复制副本**：文件树右键菜单新增「复制副本…」（`tree-menu-duplicate`）——输入新名称后把源文件/整个目录树复制到目标（文件走 `std::fs::copy`，目录走 `copy_dir_recursive` 递归复制），重名拒绝，完成后展开父目录并在状态栏报告；单测 `duplicate_tree_entry_copies_recursively` 覆盖文件与递归目录复制。
  - **F14 输出面板保存/终端清屏**：输出面板头部新增「保存」按钮（`out-save`，`save_output_to_file` 把当前构建/运行缓冲写入 `<project>/out/build-output-<时间戳>.log` 并在状态栏报告路径，空输出如实拒绝）；终端标签栏新增「清屏」按钮（`term-clear`，`Terminal::clear` 清空当前会话滚动缓冲）；单测 `save_output_to_file_writes_log_and_rejects_empty` 覆盖写入与空拒绝。
  - **F3 编辑器缩放（zoom）**：`Cmd/Ctrl+=` / `Cmd/Ctrl+-` 放大/缩小、`Cmd/Ctrl+0` 重置 100%（`Editor::set_zoom`，`zoom` 字段 0.5–2.0 夹紧；`line_height`/`font_size` 按缩放系数统一驱动全部布局/绘制/命中测试/滚动/最小地图）；命令面板「放大/缩小/重置编辑器缩放」；缩放随会话持久化（`FileView.zoom`，重开项目恢复）；单测 `editor_zoom_scales_and_clamps` 覆盖夹紧/缩放/重置，会话往返单测已扩展。
  - **F3 编辑器缩进宽度配置**：命令面板「缩进宽度：2/4/8 空格」（`indent-2`/`indent-4`/`indent-8`）经 `Editor::set_indent_width`（非法值回退 4）配置 Tab/自动缩进步长——`auto_indent_for_line`/`tab`/`indent_lines` 全部改用 `indent_string()`（`" ".repeat(width)`），光标按宽度推进；缩进宽度随会话持久化（`FileView.indent_width`，重开项目恢复）；单测 `indent_width_configures_tab_and_auto_indent` 覆盖夹紧/按宽度插入/自动缩进，`auto_indent_inherits_leading_whitespace` 扩展 2/8 空格分支。
  - **F3 当前行高亮**：光标所在行显示微妙的背景色带（`theme::alpha(ACCENT, 0.06)`，仅代码区，折叠中隐藏），随光标移动实时跟随，选中/调试高亮绘制在其上不遮挡——便于多行代码中追踪光标；`PrepaintState.current_line` 在 bracket/debug 层之前绘制。
  - **F3.2 调用层级（call hierarchy）**：命令面板「调用层级（被调用/调用）」（`call-hierarchy`）经 LSP `textDocument/prepareCallHierarchy` 解析光标处符号，浮动弹窗显示**被调用/调用**双向可切换（`call-hierarchy-dir-in/out`，复用已准备的 `CallHierarchyItem` 不重复 prepare）；列表项显示 `行:列` + 符号名/签名，Enter/点击经 `call_hierarchy_confirm` 跳转到调用点源码；`↑/↓` 导航、`Esc` 关闭；单测 `parses_call_hierarchy_item_and_array`（单对象/数组/null）与 `parses_call_hierarchy_calls_incoming_and_outgoing`（`from`+`fromRanges` / `to`+`toRanges`）。
- **对比嵌入式 IDE 补齐（本轮完成）**：
  - **F3 编辑器双击/三击选择**：双击选中词（`word_range_at`，字母/数字/下划线边界，点击空白不误选）、三击选中整行（`line_range_at`，含行尾换行），`on_mouse_down` 按 `click_count` 分流；单测 `word_range_at_selects_identifier_edges` / `line_range_at_spans_full_lines` / `double_click_selects_word_and_triple_selects_line`。
  - **F7 内存视图导入/导出**：调试内存视图新增「导出」（把当前区域写入 `out/memory-<addr>-<ts>.bin`，状态栏报告路径，空数据如实拒绝）与「导入」（从 `out/memory-import.bin` 经 DAP `writeMemory` 写入当前地址并自动重读）；单测 `memory_export_writes_bin_and_rejects_empty`。
  - **F3.2 整理导入（organizeImports）**：`ns_lsp` 新增 `LspCommand::OrganizeImports` + `organize_imports()` 客户端方法（`textDocument/codeAction` 请求，`context.only = ["source.organizeImports"]`）；命令面板「整理导入（organizeImports）」经 `lsp_organize_imports` 应用返回的 `WorkspaceEdit`（rust-analyzer / clangd / zls）。
  - **F8 烧写 target 传递修复**：`probe-rs` 适配器硬编码 `--chip stm32f103c8` 已修复为使用 `request.device`（项目 `project.toml` 的 target），`device_or` 回退保留默认；`StLink` 的 `write/verify/read` 基址改用 FLM 几何（`flm.base_address`）而非硬编码 `0x08000000`；单测 `probe_rs_uses_request_device_as_chip`。
  - **F7 日志断点命中联动**：`DebugMessage::Output` 新增 `source`/`line`（DAP `output` 事件携带 `source.path`/`line`，日志断点不暂停仍可定位）；IDE `mark_log_breakpoint_hit` 命中对应断点并持久化（`hits` 计数 + `hit` 标记，`.ns-ide/breakpoints.json`），状态栏报告「日志断点命中」；单测 `output_event_carries_source_and_line_for_log_breakpoints` / `output_event_without_location_has_none_source_and_line`（ns-debug）与 `log_breakpoint_hit_marks_and_persists`（ns-ide）。
  - **F6 设备寄存器写入回填仿真内存**：`commit_device_reg_edit` 经仿真器写 `Simulator::write_u32` 后自动 `sim_read_memory` 刷新仿真内存视图（此前仅更新状态栏，视图不刷新）。
  - **F3 自动闭合括号/引号（输入时）**：`Editor::maybe_auto_close` 在输入 `(`/`[`/`{`/`"`/`'`/`` ` `` 时若光标右侧即为配对闭合符则跳过（不重复），否则输入左符并自动插入右符、光标置于中间；`delete` 在左右符紧邻时成对删除；光标移动/选择/换行不触发；单测 `auto_close_pairs_and_skips_existing`。
  - **F3 软换行（word wrap）**：`Editor` 新增 `word_wrap`（默认关，保留横向滚动）与 `wrap_rows` 缓存（按视口宽度惰性重建，prepaint 计算视觉行→文档行映射）；`set_word_wrap`/`word_wrap()` 暴露，会话持久化（`FileView.word_wrap`，重开项目恢复）；命令面板「切换软换行（word wrap）」（`toggle-word-wrap`）；命中测试/光标移动/滚动按视觉行换算，长行不横向滚动。
  - **F7.1 断点导入/导出（JSON）**：`BreakpointStore::export_to`/`import_from` 把断点+观察点以 1:1 的 `breakpoints.json` 结构导出/导入（导出经保存对话框选路径，导入经打开对话框选文件并合并去重，导入后重新校验文件有效性）；断点面板新增「导入」「导出」按钮（`bp-import`/`bp-export`），单测 `breakpoint_export_round_trip_and_dedupe`/`breakpoint_import_rejects_invalid`。
  - **F3 二进制十六进制编辑器（可编辑）**：二进制/非 UTF-8 文件打开为**可编辑** hex 视图（此前只读）——`parse_hex_dump_bytes` 把编辑后的 dump 文本（`OFFSET HH HH … |ascii|`）解析回原始字节，`save_active` 对 `is_hex` 标签写字节而非文本（无效字节单元格如实报错不保存）；`read_only` 字段/`set_read_only` 已删除（无只读标签）；单测 `hex_dump_round_trips_editable_bytes`/`hex_dump_parse_rejects_bad_cells`。
  - **F1.2 项目管理（关闭/重命名/删除）**：文件菜单与命令面板新增「关闭项目」（`close-project`，停调试/运行、清状态回欢迎页）、「重命名项目…」（`rename-project`，重命名目录 + 同步 `project.toml` name + 更新最近项目）、「删除项目…」（`delete-project`，确认对话框后递归删除目录 + 从最近项目移除）；`ns_project::forget_project` 新增；单测 `remove_from_recents_drops_matching_paths`。
  - **F5 compile_commands.json 刷新命令**：命令面板「生成 compile_commands.json（clangd）」（`compile-commands`）经 `ns_build::generate_compile_commands` 重新生成 clangd 编译数据库（此前仅在项目打开/构建前自动刷新，无用户入口）；`generate_compile_commands` 报告生成条数，非 C/C++ 如实返回 0。
  - **F14 文件树「在终端中打开」**：文件树右键菜单新增「在终端中打开」（`tree-menu-terminal`）——目录直接作为 shell 工作目录，文件取其父目录；`TerminalConfig` 新增 `cwd: Option<String>`（`spawn_shell` 优先使用，缺省回退 IDE 当前目录）；单测 `terminal_config_round_trips_cwd`。
- **F14 外部工具/自定义命令（本轮）**：`ns_project::tools`——`ToolCommand`（id/title/command/cwd）+ `ToolStore`（`.ns-ide/tools.json` 项目级持久化，`load`/`save`/`upsert`/`remove`/`find`/`tools`）；`ns_ide::external_tools::ExternalToolsPanel`（后台线程 `sh -c`/`cmd /C` 流式输出 + 取消，镜像 Agent 面板）；命令面板「运行外部工具…」（`tools`，弹窗选择已配置工具并运行，`↑/↓`/Enter/Esc）与「编辑外部工具…」（`edit-tools`，模态编辑标题/命令/工作目录，保存/删除）；输出区「工具」标签页（`out-tab-tools`）；单测 `external_tools_add_edit_run_delete`（ns-ide，增/改/存/删/运行全链）与 tools 模块 5 项（`load_missing_is_default` 等，ns-project）。
- **F3/F14 诊断导航（本轮）**：`F8` / `Shift+F8`（命令面板「下一个/上一个错误/警告」，`next-diagnostic`/`prev-diagnostic`）跨构建输出 + LSP 诊断（按文件/行排序，路径 canonicalize 归一化 `/var`↔`/private/var`）从当前活动文件+光标行顺序跳转，环回两端；跳转打开文件、定位光标行、切到「问题」面板并在状态栏报告 `错误/警告 path:line:col — message`；单测 `navigate_diagnostic_walks_and_wraps` 覆盖顺序/环回/问题面板显示。
- **F3 编辑器诊断悬停提示（本轮）**：编辑器悬停错误/警告波浪线显示消息浮窗——`Editor::DiagnosticMark`（line/start_col/end_col/message/is_error）替代 `(line,bool)` 行标记，波浪线按标记字符范围绘制（`x_for_index` 定位），`on_mouse_move` 经 `diagnostic_at_position`（视觉行→文档行 + `index_for_x` 列命中，整行标记命中任意列）命中后发 `DiagnosticHover` 事件（message+position，离开清空）；App 订阅显示 `render_diagnostic_hover_tooltip`（锚定鼠标下方，随鼠标跟随），`refresh_diagnostics` 构建富标记（构建诊断划线到行尾、LSP 诊断按列+1）；`set_diagnostics` 重置 hovered 状态；单测 `diagnostic_hover_hit_tests_visual_line`/`set_diagnostics_resets_hovered_mark`/`diagnostic_marks_dedup_and_error_wins`。
- **F7 调试变量悬停求值（本轮）**：调试会话**停止**时悬停编辑器标识符显示当前值——`Editor::HoverWord` 事件（`word_at_position` 视觉行→文档行 + `index_for_x` 列→`word_range_at` 取词，过 gutter/顶部/空白返回 None，仅词变化时发）；App `on_hover_word`：调试停止时记录 `debug_hover=(word,pos)` 并下发 `DebugCommand::Evaluate`（`scope_name:"hover"`），非停止时清空（让 LSP 悬停接管）；`DebugMessage::Evaluated` 响应匹配 `debug_hover` 表达式时写 `debug_hover_value`（`(type) expr = value`），`render_debug_hover_tooltip`（玉色描边浮窗，锚定鼠标）在值到达后显示；离开词清空两字段；单测 `word_at_position_extracts_identifier`（editor，gutter/顶部/无布局→None）与 `debug_hover_evaluates_and_clears`（app，非停止清空/停止记录/离开清空）。
- **F7 内存视图「跟随 PC」（本轮）**：调试内存视图头部新增「跟随 PC」按钮（`dbg-mem-pc`）——把地址输入设为当前 PC（`0x{pc:x}`）并立即 `read_memory`；无 PC（会话未停止/未启动）如实提示，不静默；单测 `jump_memory_to_pc_sets_address_and_reports`（无 PC 报错 / 有 PC 填地址并触发读取）。
- **F7 变量面板「复制值」（本轮）**：变量行新增「📋 复制」按钮（`dbg-var-copy`）——把 `name = value`（值为空时仅 `name`）写系统剪贴板并报告状态栏；单测 `copy_variable_value_writes_name_eq_value`。
- **F6 仿真内存视图「跟随 PC」（本轮）**：仿真页内存视图头部新增「跟随 PC」按钮（`sim-mem-pc`）——把地址输入设为当前仿真 PC（`0x{pc:x}`）并立即重读 dump；仿真器未加载时如实提示；单测 `sim_jump_memory_to_pc_sets_address_and_reads`。
- **F6 仿真「运行到光标」（本轮）**：仿真页控制条新增「运行到光标」（`sim-run-cursor`）——活动编辑器光标行（1 基）经 `address_for_line` 映射到指令地址，临时加入断点后 `run`，执行后恢复用户断点并打开/聚焦命中源码行；无映射行/未加载如实提示；单测 `sim_run_to_cursor_runs_to_mapped_line`。
- **F6 仿真寄存器「复制」（本轮）**：仿真寄存器单元格新增「📋」按钮（`sim-reg-copy-N`）——把 `name = 0xvalue`（如 `t1 = 0xdeadbeef`）写系统剪贴板并报告状态栏；单测 `sim_copy_reg_writes_name_eq_hex`。
- **F7 反汇编「复制指令」（本轮）**：调试反汇编每行新增「📋」按钮（`dbg-disasm-copy`）——把 `address  instruction` 写系统剪贴板；单测 `copy_disasm_instruction_writes_addr_and_insn`。
- **F7 内存视图「复制行」（本轮）**：调试内存每行新增「📋」按钮（`dbg-mem-copy`）——把 `address  hex-bytes  │ ascii` 写系统剪贴板；单测 `copy_memory_row_writes_address_hex_ascii`。
- **F7 watch 表达式「复制」（本轮）**：调试变量页 watch 行新增「📋」按钮（`dbg-watch-copy`）——把 `expression = value`（值为空时仅 `expression`）写系统剪贴板；单测 `copy_watch_value_writes_expr_eq_value`。
- **F3/F14 全局查找替换与搜索选项（本轮）**：搜索面板新增**替换字段 + 「全部替换」**（`search_replace_all` 经 `search::replace_in_files` 跨文件重写磁盘、刷新搜索匹配、重载已打开的干净编辑器标签与分屏、状态栏报告「N 处 / M 个文件」，空关键词/空替换如实处理；单测 `replace_in_files_replaces_across_sources` / `search_replace_all_rewrites_files_and_reloads_editor`）与**搜索选项开关**（`search::SearchOptions{case_sensitive, whole_word}` + `search_project_with_options` 整词/大小写敏感匹配，搜索面板「Aa 区分大小写」「W 全词匹配」胶囊开关即时重跑查询；单测 `search_options_case_sensitive_and_whole_word` / `search_option_toggles_flip_flags_and_refresh`）。
- **F5 构建失败自动定位（本轮）**：构建失败后 `finish_build` 自动 `jump_to_first_build_diagnostic`——打开首个带源码位置的**错误**（无错误则首个**警告**）文件并把光标移到诊断行，状态栏报告「已定位到首个错误/警告: message」；无源码定位诊断（链接/脚本错误）时如实提示；单测 `jump_to_first_build_diagnostic_opens_first_error`。
- **F3 编辑器文本变换（本轮）**：`Editor::transform_selection_case`（`Cmd/Ctrl+Shift+U` 大写、`Cmd/Ctrl+Alt+Shift+L` 小写，选区或无选区时光标词）、`join_lines`（`Cmd/Ctrl+J`，选中行或当前+下一行合并为单行、内部空白折叠为单空格、尾随换行丢弃）、`sort_lines`（`Cmd/Ctrl+Shift+S`，选中行或全文档按字节序排序，尾随 `\n` 不作为空行参与排序）、`trim_trailing_whitespace`（`Cmd/Ctrl+Alt+T`，选区行或全文档去行尾空白，先收集范围再逆序应用避免偏移错乱）；单测 `transform_selection_case_upper_and_lower` / `join_lines_merges_with_spaces` / `sort_lines_orders_and_skips_sorted` / `trim_trailing_whitespace_trims_lines`。
- **F3 编辑器右键菜单编辑项（本轮）**：编辑器右键菜单新增**编辑**分区（分隔线 + 剪切/复制/粘贴/全选/格式化文档/重命名符号…，`ctx-cut`/`ctx-copy`/`ctx-paste`/`ctx-select-all`/`ctx-format-doc`/`ctx-rename-symbol`），经新公开的 `Editor::copy_selection_to_clipboard`/`cut_selection_to_clipboard`/`paste_from_clipboard`/`select_all_document` 包装既有剪贴板/全选操作；单测 `editor_context_menu_helpers`。
- **F3 外部文件变更自动重载（本轮）**：每 2s 轮询所有打开的编辑器标签，磁盘内容与 `saved_text` 不一致时——**干净**缓冲静默 `reload_text` 并 `mark_saved`（「已从磁盘重新加载（外部变更）」状态栏），**脏**缓冲不动并弹既有外部变更对话框（用户选择重载/保留/取消，不静默覆盖）；`Editor::is_dirty` 已存在，`check_external_changes` 在 `load_project` 启动的 interval 任务里运行；单测 `external_change_reloads_clean_and_prompts_dirty`。
- **F3 比较活动文件与磁盘（本轮）**：`diff` 模块（LCS 行级差异，`diff::diff_lines`/`render_diff`，O(n·m) 表 + 超大输入走前后缀粗对齐回退，CRLF/尾随换行归一）；命令面板「比较活动文件与磁盘…」（`compare-active-disk`）对比活动编辑器缓冲与磁盘内容，变更时弹差异模态（`compare-diff`，`+/-` 行标记 + 变更计数），一致时状态栏提示「与磁盘内容一致」；Esc 关闭；单测 `diff::tests`（7 项）与 `compare_active_with_disk_shows_diff_or_reports_consistent`。
- **F14 问题面板严重级别过滤（本轮）**：问题面板顶部新增**严重级别过滤**胶囊（`prob-filter-error/warning/info`，显示各自诊断数）——点击切换该级别显示/隐藏（`toggle_problem_filter`），`build_problem_shown`/`lsp_problem_shown` 分别门控构建与 LSP 诊断行；最后启用的级别不会被移除（面板永不显示为空）；单测 `problem_severity_filter_toggles_and_never_empties`。
- **F14 文件树 Git 变更徽章（本轮）**：文件树每个**文件**条目（非目录）右侧新增 Git 状态徽章（`tree-git-badge`，`M`=修改/`A`=新增/`D`=删除 玉/朱/金着色）——`git_tree_badge` 按项目根相对路径在 `git.status.files` 查找（目录匹配时子路径前缀命中），未变更/非仓库返回 `None` 不显示；单测 `git_tree_badge_labels_changed_paths`。
- **F7 寄存器行复制（本轮）**：调试寄存器视图每行新增「📋」按钮（`dbg-register-copy`）——把 `名称 = 值` 写系统剪贴板并在状态栏报告「已复制寄存器 名称」。
- **F7 变量过滤（本轮）**：调试变量面板新增**过滤输入框**（`过滤变量…`，id `debug-vars-filter`）——按变量名大小写不敏感子串过滤，**递归匹配**（父变量自身不匹配但展开的子变量匹配时仍显示父变量，子变量过滤随展开递归），空查询恢复全部；无匹配时显示「没有变量匹配「…」」而非空白；`variable_matches`/`variable_rows_filtered` 接入渲染；单测 `variable_filter_matches_names_and_children`。
- **F11 图形化外设配置（本轮）**：命令面板「外设配置（图形化，CubeMX 式）…」（`peripheral-config-gui`）打开**外设配置模态**（`periph-config-modal`）——按目标 SVD 列出全部外设，每行勾选启用（`periph-config-check`）；GPIO 端口启用后显示 **16 个引脚**（`periph-config-pin`，点击在 输入(浮空/上拉/下拉)/输出(推挽/开漏)/复用(推挽/开漏) 间循环）；UART 显示波特率。`PeripheralConfig` 新增 `register_values`（寄存器名→值），`generate_peripheral_init` 在提供值时**发出真实寄存器写**（`PERIPH->REG = value;`，按地址排序；未设置仍以复位值注释标注），否则回退旧注释骨架；STM32F1 GPIO 引脚模式经 `stm32f1_gpio_registers` 打包成 `CRL`/`CRH`（4 位/引脚 [3:2]=MODE [1:0]=CNF），UART 按 72MHz/波特率写 `BRR`。生成写入 `src/peripheral_init.c` 并打开。单测 `stm32f1_gpio_registers_pack_nibbles`、`peripheral_config_gui_generates_real_gpio_writes`、`peripheral_init_emits_real_register_writes`（ns-device）。
- **F7 反汇编↔内存联动（本轮）**：调试反汇编视图每行**地址**（`dbg-disasm-addr`）可点击——切换到内存页并把内存视图跳到该指令地址并重读（`jump_memory_to_address`，`jump_memory_to_pc` 复用同一实现）；单测 `jump_memory_to_address_sets_input_and_reports`。
- **F7 RTOS 任务列表（本轮）**：调试面板新增 **RTOS** 标签（`dbg-tab-rtos`，`render_rtos_column`）——「刷新任务列表」（`dbg-rtos-refresh`）经 `refresh_rtos_tasks` 对 FreeRTOS 风格模板的全局量求值并**逐任务收集**：`task_count`/`uxCurrentNumberOfTasks` → 计数，`current_task` → 运行索引，`tasks[i].{name,priority,stack_usage,state}` → 各字段；`handle_rtos_evaluated` 按表达式关联并回填（`PartialRtosTask` 累积，全任务名称齐备后 `build_rtos_tasks` 组装：标记运行任务、映射状态（就绪/阻塞/挂起）、携带优先级与栈使用、按名排序），`debug.rtos_status` 显示计数；任务行（`dbg-rtos-task`）显示 ▶运行标记/名称/状态·优先级/栈使用%；模板 `Task` 结构体扩展 `priority`/`stack_usage`/`state` 字段并在 `main` 填充可观测值（`-Wall -Wextra` 零警告验证）；无会话如实提示「无调试会话」。单测 `build_rtos_tasks_marks_running_and_sorts`、`refresh_rtos_tasks_without_session_reports`、`handle_rtos_evaluated_routes_task_count`。
- **F3/F5 保存选项（本轮）**：设置页新增「保存选项(IDE 全局)」——**格式化保存**（`format_on_save`：保存时先经 LSP `formatting` 请求再写盘，`format_and_save_tab`）与**构建前保存**（`save_before_build`：构建/运行/烧写/调试前经 `save_before_operation` 自动保存全部脏标签），均持久化到用户配置目录 `settings.json`（`ns_project::IdeSettings`，`format_on_save`/`save_before_build` 字段，`#[serde(default)]` 向后兼容）。
- **F7 单指令步进（本轮）**：调试控制条新增「指令」按钮（`dbg-step-insn`）+ 快捷键 `Cmd/Ctrl+Alt+→`（`DebugStepInstruction`）+ 命令面板「单指令步进」（`step-instruction`）——经 `DebugCommand::StepInstruction`（DAP `stepInstruction`）下发，与步过/步入/跳出同构；`DebugStep::Instruction` 枚举分支映射到 `ns_debug::DebugCommand::StepInstruction`；调试面板反汇编视图随 PC 刷新。
- **F14 代码片段管理 UI（本轮）**：命令面板「管理代码片段…」（`edit-snippets`）打开**片段管理模态**（`snippet-dialog`，`SnippetDialog`/`SnippetRow`）——列出 `snippets.json`（用户配置目录全局共享）全部片段，每行编辑触发词/片段体（LSP `${1:占位}`/`$0` 语法）/描述，「＋添加片段」新增空行、「删除」移除该行、「保存」写回磁盘；`save_snippets` 重建存储（丢弃被删行，`SnippetStore::upsert`/`remove` 语义）；单测 `snippet_dialog_add_edit_save_delete`（增/改/存/删全链，自清理）。
- **F7/F6 编译修复与真实内核遍历补齐（本轮）**：工作区此前**无法编译**——`cargo check` 11 错误：真实 FreeRTOS 内核遍历 WIP 引用 3 个未定义符号（`kernel_walk_expr`/`rtos_kernel_send`/`rtos_kernel_advance`）、`ns_debug::profile` 模块缺失、3 处 `RtosRefreshState` 测试初始化器缺 `mode`/`kernel` 字段。本次：实现 `ns_debug::profile::ProfileAggregator`（`new`/`record`/`hotspots`/`total`，PC 采样按 `nm` 符号二分定位、间隙/未知计入 `<unknown>`，单测 3 项）；实现 `kernel_walk_expr`（owner/name/priority/next 步骤按 `RtosLayout` 偏移构造带 `__rtos_` 标签的求值表达式，单测 2 项）；实现 `rtos_kernel_send`/`rtos_kernel_advance` 接入 `handle_rtos_kernel_evaluated` 的 `Next` 分发；删除死代码（5 个 `rtos_kernel_eval_*`、`evaluate_rtos`/`evaluate_rtos_addr`/`kick_rtos_kernel_walk`、`Next::Finish`、`items_remaining`）。验证：`cargo check --workspace` 退出 0 本地零警告；`cargo test --workspace` **635 项全绿**。

- **未完成（本轮对比识别，不得视为已完成）**：
   - **多核/多芯片调试**：单目标单会话；无多核（如双核 STM32H7）调度与核间同步。
  - **RTOS 感知调试（真实内核）**：真实 FreeRTOS 内核链表遍历已接入——`refresh_rtos_tasks` 先求值 `uxCurrentNumberOfTasks`，命中即走 `RtosKernelMode::RealKernel`：`evaluate_rtos_symbol` 取 `pxCurrentTCB` 与 `xReadyTasksLists`/`xDelayedTaskList1/2`/`xPendingReadyList`/`xSuspendedTaskList`/`xTasksWaitingTermination` 链表头，`handle_rtos_kernel_evaluated` 经 `kernel_walk_expr`（owner/name/priority/next 步骤，`__rtos_` 标签路由）遍历每个 `ListItem_t` 的 `pxNext`/`pvOwner`，按 `RtosLayout` 默认偏移（Cortex-M 32 位，`pcTaskName@44`/`uxPriority@36`/`pvOwner@12`/`pxNext@4`）读 TCB 字段，`rtos_kernel_finish` 按 TCB 去重并发布；`ns_debug::rtos` 提供 `walk_free_rtos` 纯函数（`ReadFn` 驱动，被自身单测覆盖）。**仍需**：把 IDE 遍历改为 `ReadMemory` + `walk_free_rtos` 复用（避免 DAP 往返与纯函数两套实现漂移），或为 DAP 往返路径补集成测试；需真实内核符号验证。
  - **图形化外设配置（CubeMX 式交互）**：基本的外设选择 + GPIO 引脚模式 + UART 波特率模态已接入（见上），但无**时钟树/引脚映射图**等图形化配置（当前为列表式勾选 + 引脚模式循环）。
- **M4 调试（嵌入式目标 attach）**：**已实现但未验证**（`ns-debug::embedded`：OpenOCD/ST-Link/J-Link/pyOCD/probe-rs 探针适配器 + `target remote` GDB attach 会话，命令面板「嵌入式调试（目标 attach）」→ 编译→烧写→attach；因需要硬件，尚未在本机验证，勿声称可用）。
---

## 6. 与 IAR 的差异化（只参考能力，不照抄）

| 维度 | IAR | 本项目 |
|---|---|---|
| 配置模型 | 专有 `.ewp/.eww` 工程文件 | 开放、人类可读的 `project.toml` |
| 工具链 | 厂商绑定（IAR 编译器） | 开放适配器：GCC/Clang/Cargo/Zig/zr 可插拔 |
| 语言 | C/C++/汇编 | C/C++/Rust/Zig/**ZR**（新增） |
| 调试 | C-SPY 专有 | GDB/LLDB 标准协议 + 可插拔厂商适配 |
| 许可 | 商业授权 | 开源（LICENSE 见仓库） |
| 界面 | 经典 IDE 布局 | GPUI 现代原生界面，Dock 自由布局 |

---

## 7. 关键风险与约束

1. **GPUI 生态年轻**：组件库、成熟度需验证；界面层抽象隔离，避免阻塞核心逻辑。
2. **ZR 语言从零开始**：规范、编译器、LSP、标准库工作量大；先行定义 CLI/协议契约，再实现 IDE 侧适配。
3. **工具链碎片化**：厂商工具差异大；以适配器 Trait 统一，新增工具只写适配器。
4. **跨平台差异**：Windows 进程/路径/信号与 Unix 不同；用统一 `std::process` 封装 + 路径抽象。
5. **仿真与调试分离**：仿真器独立于调试器，但共享设备库（SVD/寄存器）数据。
---

## 8. 界面风格（设计语言）

> 整体为**暖墨金深色主题**：墨色底、鎏金点缀、玉/朱辅色；宋体/楷体字系、大写字距、圆角卡片、毛玻璃、柔光晕与星点纹理，呈现文人器物气质。以下为设计令牌与组件规范，界面实现遵循此语言。

### 8.1 色彩令牌

| 令牌 | 值 | 用途 |
|---|---|---|
| `--bg-0` | `#0f0d0a` | 最底层背景 |
| `--bg-1` | `#17130d` | 背景渐变起点/面板衬底 |
| `--bg-2` | `#211a11` | 悬浮/抬升层 |
| `--panel` | `rgba(38,29,18,0.6)` | 半透明面板（毛玻璃） |
| `--panel-solid` | `#201a11` | 实体面板（模态等） |
| `--stroke` | `rgba(219,181,121,0.14)` | 默认描边 |
| `--stroke-strong` | `rgba(219,181,121,0.32)` | 强调描边（hover/虚线环） |
| `--text` | `#f0e6d2` | 主文字（暖米白） |
| `--text-dim` | `#b8a98c` | 次文字 |
| `--text-faint` | `#8a7c63` | 弱文字 |
| `--accent` | `#c9a25f` | 主强调（鎏金） |
| `--accent-2` | `#a57a3d` | 强调暗部 |
| `--accent-3` | `#d8b97f` | 强调亮部 |
| `--jade` | `#6f9b8a` | 辅色·玉（成功/状态） |
| `--cinnabar` | `#b0564a` | 辅色·朱（告警/装饰） |
| `--danger` | `#cf6b5e` | 错误 |
| `--grad` | `linear-gradient(135deg,#d8b97f 0%,#c9a25f 48%,#8a6b35 130%)` | 主渐变（按钮/徽标/播放钮） |

### 8.2 字体

- 正文字系：`PingFang SC, Songti SC, STSong, Kaiti SC, STKaiti, KaiTi, Microsoft YaHei, serif`（宋/楷体为首选，衬线文人气质）。
- 标题字距 **4–5px**、正文/按钮字距 **1px**，按钮字号 13.5px、正文 12.5–13.5px，营造呼吸感。
- 时间/数字使用 `tabular-nums` 等宽数字。

### 8.3 形状与投影

- 圆角：卡片 `--radius:14px`，大卡片/模态 `--radius-lg:20px`，输入框/按钮 `12px`，状态徽标/头像 `999px`（胶囊/圆形）。
- 投影：`0 18px 50px -12px rgba(0,0,0,0.7)`（深且柔，悬浮抬升感）；强调元素附加色晕，如金色 `0 6–8px 24px -6px rgba(201,162,95,0.55~0.65)`。

### 8.4 表面与层次

- 顶栏/状态栏：`rgba(23,19,13,0.78)` + `backdrop-filter: blur(18px)` + 1px 描边分隔，毛玻璃。
- 背景：`bg-1 → bg-0` 竖向渐变，叠加三团径向光晕（金 `rgba(201,162,95,0.16)`、玉 `rgba(111,155,138,0.10)`、朱 `rgba(176,86,74,0.06)`）。
- 装饰纹理：固定"墨脉"星点层，`radial-gradient` 点阵（6 处彩色 1–1.4px 点，380px 周期），`mix-blend-mode: screen`、`opacity 0.28`，`pointer-events: none`，位于内容之下。
- 模态遮罩：`rgba(8,6,4,0.62)` + `blur(8px)`。

### 8.5 组件规范

| 组件 | 规范 |
|---|---|
| 主按钮 | 金色 `--grad` 渐变底、深字 `#221407`、字重 600、字距 1px；hover 提亮 `brightness(1.12)` + 上浮 1–2px |
| 幽灵按钮 | `--panel` 底 + 1px `--stroke` 描边、次文字；hover 文字提亮、描边加强、底色 `rgba(219,181,121,0.08)` |
| 圆形控制钮 | 38–46px 圆形，半透明暖底 + 描边；hover 放大 1.06；播放钮用金色渐变 + 色晕 |
| 输入框 | 圆角 12px、底色 `rgba(255,235,200,0.05)` + 描边；聚焦：`--accent` 描边 + 3px 光晕 `rgba(201,162,95,0.25)` |
| 模态卡片 | `--panel-solid` 底、`--radius-lg`、`--shadow`、上下留白约 26px，标题 18px/700/字距 4px |
| 状态徽标 | 胶囊（999px）、同色系半透明底 + 描边，如玉色 `rgba(111,155,138,0.14)`、朱色 `rgba(176,86,74,0.16)` |
| 品牌徽标 | 圆形 `radial-gradient(circle at 35% 30%, 亮, 金 58%, 暗金)` + 1px 亮描边 + 内圈暗环 + 金色投影，中央图形元素 |

### 8.6 动效

- 交互：按钮/控件 `transition: all 0.18s ease`。
- 模态进入：`fadeIn 0.2s ease`（遮罩）+ `popIn 0.22s cubic-bezier(0.2,0.9,0.3,1.2)`（卡片）。
- 空状态图标：`floaty 4s ease-in-out infinite` 悬浮 + 外圈虚线环 `spin 18s linear infinite` 旋转。
- 出现/消失：滑动、淡入淡出约 `0.35s ease`。

> 深色优先；浅色主题预留（见 F13 主题可扩展）。视觉元素（图标、装饰）采用线性/纯色简洁风格，配合墨金配色。
