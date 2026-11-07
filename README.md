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
  - `ns-flash` — 烧写适配器（OpenOCD / pyOCD / J-Link）+ 固件格式转换（ELF/HEX/BIN）
  - `ns-lsp` — LSP 客户端（clangd / rust-analyzer / zls / zr-lsp）
  - `ns-device` — 目标设备库：CMSIS-SVD 解析、设备目录、链接脚本/启动文件生成
  - `ns-sim` — 内建指令级仿真器（RV32I + 虚拟外设）
  - `ns-zr` — ZR 语言分析（词法/诊断）+ `zr-std` 声明库 + `zr-lsp`
  - `ns-ide` — GPUI 应用

## 当前实现状态

已实现并验证（详见 AGENT.md §5.1）：

- 新建项目向导、打开/最近项目、文件树、多标签编辑器（行号槽 + 断点槽）；命令面板 `Cmd/Ctrl+Shift+P`（命令搜索/执行）
- C/C++/Rust/Zig/ZR 内建词法高亮，构建诊断解析与问题面板
- 按 `project.toml` 构建、构建输出面板、错误定位跳转
- 全局文件搜索：`Cmd/Ctrl+Shift+F`，文件名 + 内容大小写不敏感搜索（跳过构建目录/二进制），↑↓ 选择、Enter/点击跳转；编辑器虚拟视口，跳转自动滚动到目标行
- 断点增删：行号槽点击、`F9` 切换当前行、`Cmd/Ctrl+Shift+F9` 清除全部、右键菜单（添加/启用·禁用/编辑属性/删除/清除本文件）、条件·命中次数·日志断点编辑（持久化到 `.ns-ide/breakpoints.json`）；断点面板（按文件分组、启用/禁用、跳转源码、删除、清除全部、条件/命中次数/日志摘要）与逐行状态（已验证/命中/未绑定/未同步/失效/已禁用，含未绑定/失效原因）；源文件被删除/重命名或行号越界时自动标记失效并停止下发，恢复后自动重同步；**硬件断点**（右键「改用硬件断点」，DAP `mode: "hardware"`，绑定失败回传未绑定原因）；**观察点（数据断点）**：断点面板「观察点」区新增/编辑/删除/启停，访问类型在读取/写入/读写间切换，变量行「👁」一键添加、编辑器右键/命令面板按光标标识符预填，随项目持久化并在每次暂停后重新解析地址，已验证/未绑定状态与原因回到面板
- **调试（宿主机构件）**：`F5` 或顶部「调试」构建并启动 `lldb-dap` 会话；`Shift+F5` 停止；`F10` 步过 / `F11` 步入 / `Shift+F11` 跳出；命中断点行高亮（jade + ▶），调用栈（点击跳转源码）、变量面板（作用域切换 + 可递归展开，含 **Registers**）、内存视图（hex dump，地址输入）、反汇编源码混排视图（PC 行高亮 + 可点击源码行映射）、调试控制台；**跟踪窗口（SWO/SWV）配置面板**（「跟踪」页：ITM 端口 / SWO 分频 / DWT 数据跟踪 / 运行模式，配置持久化到 `.ns-ide/trace.json`，启用时下发目标侧寄存器写入；如实显示 SWV 探针与 SWO 流尚未接入的状态，不假装收到包）；增删/启停/清除断点即时同步，并回填每个断点的 DAP `verified`/`reason` 状态；条件断点、命中次数断点（第 N 次命中）、日志断点（`{expr}` 输出到控制台）均下发并生效
- **Git 集成（M12 切片）**：状态栏「Git」项 + Git 面板（变更文件列表、unified diff、暂存/取消暂存、一键提交、分支与变更数），非仓库目录如实提示
- **工具链检测（M11 切片）**：新建项目向导为编译器/仿真器/调试器/烧写器显示「路径 · 版本」（`--version` 探测），缺失工具给出安装建议；`ns-project::toolchain::Tool::version` + `missing_guide`
- **工具链校验（M11 切片）**：打开项目时校验 `project.toml` 配置的工具是否在 PATH，缺失的显示到「问题」面板并附安装建议；`Project::tool_warnings`
- **静态分析（M12 切片）**：命令面板「静态分析」按语言运行 clang-tidy / cargo clippy / zig / zr-lint，结果进「问题」面板；`ns_build::plan_static_analysis`
- **终端（M12）**：命令面板「打开终端」启动系统 shell（`Terminal`，基于 `portable-pty`，PTY 尺寸 100×30），实时输出流式渲染、可输入回显、Ctrl+C 中断、清屏按钮、退出码/尺寸显示、`Drop` 时关闭 PTY；命令面板「打开串口监视器」用 `NS_SERIAL_PORT` 环境变量指定的设备启动串口会话（`TerminalKind::Serial`），未设置时如实提示。
- **会话恢复（M12）**：打开文件标签与当前活动标签持久化到 `.ns-ide/session.json`（打开/切换/关闭/退出时保存，重开项目自动恢复），`ns_project::Session` 往返单测覆盖。
- **项目设置（M12）**：`Cmd/Ctrl+,` 或命令面板「项目设置…」打开设置模态，编辑 `project.toml` 常用字段（名称/目标 MCU/编译器路径/构建配置/输出目录/调试器路径/烧写器路径/烧写器类型），保存即写回 `project.toml`（校验：名称非空、构建配置为 debug/release、烧写器类型为 openocd/pyocd/jlink），`ns_project` 往返单测覆盖。
- **烧写（M5 切片）**：命令面板「烧写固件」/ 顶部「烧写」按钮 / `Cmd/Ctrl+Shift+F10` 用 `project.toml` 的 `[flasher]` 适配器（OpenOCD / pyOCD / J-Link Commander，`kind` + `path` + `interface`）烧写构建产物；固件格式 ELF / Intel HEX / BIN（HEX 解码、校验和/EOF 校验）；`ns_flash` crate：`adapter_for` + `Flasher` trait + 端到端单测（stub 工具验证命令构造）。
- **运行编排（M6 切片）**：顶部「烧写并运行」（编译→烧写→复位运行）与「烧写并调试」（编译→烧写→连接调试器），命令面板「烧写并运行」/「烧写并调试」同源；任一环节失败即停止并如实报告，不静默继续。
- **工具链全局路径（M11 切片）**：命令面板「工具链路径…」编辑全局工具链覆盖（arm-none-eabi-gcc/gdb、clang、gcc、rustc、zig、openocd、pyocd、JLinkExe），留空=从 PATH 探测；覆盖优先于自动发现，保存在用户配置目录（`toolchain.json`），跨项目共享；`ns_project::ToolchainConfig` + `registry`（语言→可用编译器，向导级联）。
- **语言服务 LSP（M8 切片）**：新建 `ns_lsp` crate（LSP-over-stdio 客户端，Content-Length 帧、后台读写线程、请求/通知路由）；项目打开时按语言自动启动服务器（C/C++→clangd、Rust→rust-analyzer、Zig→zls、ZR→zr-lsp），文件打开/编辑实时同步 `didOpen`/`didChange`；诊断 `publishDiagnostics` 合并进问题面板（点击跳转源码）；`F12` 跳转到定义、`Shift+F12` 查找引用、`Cmd/Ctrl+K Cmd/Ctrl+I` 悬停提示（状态栏）、`Cmd/Ctrl+Space` 触发补全（浮动补全面板，↑/↓ 选择、Enter/Tab 确认、Esc 关闭）；单测含 mock 服务器（initialize/补全/定义/诊断）与真实 clangd 端到端（错误文件→诊断到达）。
- **目标设备库（M9 切片）**：新建 `ns_device` crate——CMSIS-SVD XML 解析（`parse_svd`：外设→寄存器→位域，地址/大小/复位值）、内置设备目录（stm32f103c8 / stm32f407vg / nrf52840 / rp2040，`CATALOG`/`device_for_target`）、`linker_script`/`startup_file` 生成（flash/RAM 内存映射 + 向量表/复位处理）、`memory_map`/`registers_by_peripheral`；向导「目标设备」步显示 MCU 核/Flash/RAM 并在生成时自动写出 `link.ld` 与 `src/startup.s`；调试面板新增「设备」页按 SVD 展示外设寄存器（地址/复位值/位域）；单测覆盖解析/生成/目录查找。
- **ZR 语言（M10 切片）**：新建 `ns_zr` crate——ZR 词法分析（关键字/标识符/符号，`->` 复合符号）、诊断分析（不匹配/未闭合定界符、未知标识符）、`zr-lsp` 内建语言服务二进制（LSP-over-stdio：initialize/`didOpen`/`didChange`/`publishDiagnostics`/补全/悬停/shutdown）；IDE 的 LSP 客户端自动发现并驱动 `zr-lsp`（PATH 或 workspace target 目录回退），ZR 项目因此获得诊断、补全与悬停；`zrc build` 构建计划与 `zr-lint` 静态分析计划保留；单测覆盖词法/诊断/偏移与 LSP 端到端（未闭合定界符→诊断、补全、悬停）。
- **仿真（M7 切片）**：新建 `ns_sim` crate——内建指令级仿真器（RV32I 核：寄存器文件、64 KiB 内存、`load_elf`/`load_image`、`step`/`run`/`step_over`、断点、`ecall` 停机、`sign_extend`/`jal`/`branch` 立即数解码）；虚拟外设（内存映射 UART，`putchar` 回调 + 输出缓冲）；`simulate` 命令面板「仿真运行（内建指令级）」加载构建产物（ELF 优先，否则按入口原始加载）到仿真器；调试面板新增「仿真」页：控制条（加载并运行/单步/运行/复位/停止）、PC/周期/状态、32 个寄存器（ABI 名 + 值）、UART 输出；单测覆盖 ALU（加/减）、循环累加（1..10=55）、断点命中、load/store、ecall 停机，以及真实 ELF32 端到端（入口加载→循环→a0=55→停机）。
- **全量配置页（M12）**：`Cmd/Ctrl+,` / 命令面板「项目设置…」打开设置模态，编辑 `project.toml` **全部字段**（project/compiler/debugger/flasher/build/sim 各节）+ 全局设置（终端 shell → `terminal.json`、全局配置键 `$config["key"]` → `config.json`）；保存即写回整个 `project.toml`（校验：名称非空、构建配置 debug|release、烧写器/探针类型枚举、语言 C|C++|Rust|Zig|ZR）。
- **Agent 面板（参考 Zed agent）**：输出区「Agent」标签 / 命令面板「打开 Agent 面板」；任务列表（状态点 + 命令标题）、选中任务输出、底部 shell 命令输入 + 运行；后台线程跑 `sh -c`/`cmd /C`，stdout/stderr 流式追加，「清空」/「全部取消」。
- **串口助手（M12）**：输出区「串口」标签 / 命令面板「打开串口助手」；枚举端口、波特率/校验/数据位/停止位/流控、打开/关闭、hex 收发、时间戳、清空、收发字节计数。
- **终端设置（M12）**：设置页「终端 shell」跨项目保存到 `terminal.json`；留空=平台默认（**Windows: cmd，Linux/macOS: bash**），默认终端路径自动识别。
- **项目模板（F1.2）**：新建向导「模板」步（仅 C 语言）——空模板 / **LED Blink**（GPIO 翻转）/ **UART 回环**；生成时写入 `src/main.c` 并填充外设初始化。
- **烧写扩展（M5）**：烧写面板接入**擦除/校验/读回**（读回写 `out/readback.bin`）+ 烧写进度；`ns_flash::firmware::convert`（ELF→HEX/BIN）。
- **运行编排扩展（M6）**：运行/调试会话的**停止与重启**状态机（重启 = 停止后重新编译→运行）。
- **调试扩展（F7）**：运行到光标（`continueTo`）、设置变量值（`setVariable`）、监视（watch）表达式面板、调试控制台表达式求值。
- **编辑器扩展（F3）**：当前文件查找/替换（`Cmd/Ctrl+F`）、括号匹配高亮、代码折叠、`Cmd/Ctrl+S` 保存（脏标记）。
- **LSP 扩展（M8）**：签名帮助（`signatureHelp`）、inlay hints、语义高亮叠加（`semanticTokens`）。
- **设备库扩展（M9）**：外部 SVD 文件加载、启动文件模板库（按核更多 MCU）、设备选择写回 `project.toml` 的 SVD 路径。
- **仿真扩展（M7）**：周期级/外设时序（GPIO/TIMER/中断控制器）、仿真断点下发到源码、C 源码映射（符号表→编辑器定位）。
- **ZR 标准库（M10）**：`ns_zr::std` —— `zr-std` 声明库（GPIO/UART/TIMER 内存映射外设 + `std_registry`/`peripheral`/`std_api`），供 `zr-lsp` 补全/签名与 ZR 编译绑定。

未完成（实现但未验证）：**嵌入式目标 attach**（OpenOCD / ST-Link / J-Link / pyOCD / probe-rs 探针适配器 + `target remote` GDB 会话，命令面板「嵌入式调试（目标 attach）」→ 编译→烧写→attach；需硬件，尚未在本机验证）；**ZR 真实 `zrc` 编译器集成**（`zr-std` 声明库已提供，编译器本体后续接入），见 AGENT.md §5.1 🚧 清单。

## 构建与运行

```sh
cargo build --workspace          # 或 cargo check --workspace
cargo test --workspace           # 单元测试 + ns-debug DAP 端到端测试（真实 lldb-dap，缺适配器时自动跳过）
./target/debug/ns-embed-ide [project目录]
```

首次运行可直接执行左上角「＋ 新建」向导，或传入一个含 `project.toml` 的项目目录。

## 调试说明

- 适配器：`lldb-dap`（随 Xcode Command Line Tools / Xcode 提供）。自动发现顺序：`NS_DEBUG_ADAPTER` 环境变量 → `PATH` → `xcrun --find lldb-dap` → 常见安装路径。
- 目前支持**宿主机构建产物**的启动调试（`project.toml` 的 `[build] config = "debug"` 即可，无需额外配置）；嵌入式目标 attach 已实现（OpenOCD/ST-Link/J-Link/pyOCD/probe-rs）但**尚未验证**，需硬件。
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
