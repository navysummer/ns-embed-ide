# ns-embed-ide

跨平台嵌入式开发软件（IDE），支持 C、C++、Rust、Zig、ZR 几种语言。
一切工具链行为（编译/仿真/调试/烧写/运行）由项目根目录 `project.toml` 驱动，`project.toml` 是唯一权威配置。

功能规划见 [AGENTS.md](AGENTS.md)；其中 **§1.2 工程纪律**约束所有修改（零警告零错误、完整实现、交付即验证、文档同步）。

## 技术栈

- Rust + [GPUI](https://crates.io/crates/gpui) 0.2.2（Zed 的 GUI 框架）
- 工作区 crate：
  - `ns-project` — `project.toml` 模型/校验/模板/最近项目/断点持久化/最近文件
  - `ns-build` — 构建执行与 gcc/clang/rust 诊断解析（debug 配置自动加 `-g` / `-C debuginfo=2`）
  - `ns-debug` — 调试会话抽象（DAP 客户端）：发现并启动 `lldb-dap`，断点/调用栈/变量/控制台
  - `ns-flash` — 烧写适配器（OpenOCD / pyOCD / J-Link）+ 固件格式转换（ELF/HEX/BIN）
  - `ns-lsp` — LSP 客户端（clangd / rust-analyzer / zls / zr lsp）
  - `ns-device` — 目标设备库：CMSIS-SVD 解析、设备目录、链接脚本/启动文件生成
  - `ns-sim` — 内建指令级仿真器（RV32I + 虚拟外设）
  - `ns-ide` — GPUI 应用

## 当前实现状态

已实现并验证（详见 AGENTS.md §5.1）：

- 新建项目向导、打开/最近项目、文件树、多标签编辑器（行号槽 + 断点槽）；命令面板 `Cmd/Ctrl+Shift+P`（命令搜索/执行）
- C/C++/Rust/Zig/ZR 内建词法高亮，构建诊断解析与问题面板
- 按 `project.toml` 构建、构建输出面板、错误定位跳转；**构建进度**（stdout/stderr 逐行实时追加到输出面板，长构建可见中间输出）、**构建取消**（构建中状态栏「■ 取消构建」，终止子进程并如实报告）、**并行构建**（`[build] jobs`，0=自动；CMake/Make/Cargo 透传 `-j`）。
- 全局文件搜索：`Cmd/Ctrl+Shift+F`，文件名 + 内容大小写不敏感搜索（跳过构建目录/二进制），↑↓ 选择、Enter/点击跳转；编辑器虚拟视口，跳转自动滚动到目标行；**查找替换**：搜索面板「替换为…」字段 + 「全部替换」跨文件重写磁盘并刷新已打开的编辑器；**搜索选项**：「Aa 区分大小写」「W 全词匹配」胶囊开关即时重跑查询；**构建失败自动定位**：构建失败后自动打开首个错误（无错误则首个警告）源码位置并移动光标
- 断点增删：行号槽点击、`F9` 切换当前行、`Cmd/Ctrl+Shift+F9` 清除全部、右键菜单（添加/启用·禁用/编辑属性/删除/清除本文件）、条件·命中次数·日志断点编辑（持久化到 `.ns-ide/breakpoints.json`）；断点面板（按文件分组、启用/禁用、跳转源码、删除、清除全部、条件/命中次数/日志摘要）与逐行状态（已验证/命中/未绑定/未同步/失效/已禁用，含未绑定/失效原因）；源文件被删除/重命名或行号越界时自动标记失效并停止下发，恢复后自动重同步；**硬件断点**（右键「改用硬件断点」，DAP `mode: "hardware"`，绑定失败回传未绑定原因）；**观察点（数据断点）**：断点面板「观察点」区新增/编辑/删除/启停，访问类型在读取/写入/读写间切换，变量行「👁」一键添加、编辑器右键/命令面板按光标标识符预填，随项目持久化并在每次暂停后重新解析地址，已验证/未绑定状态与原因回到面板
- **调试（宿主机构件）**：`F5` 或顶部「调试」构建并启动 `lldb-dap` 会话；`Shift+F5` 停止；`F10` 步过 / `F11` 步入 / `Shift+F11` 跳出；命中断点行高亮（jade + ▶），调用栈（点击跳转源码）、变量面板（作用域切换 + 可递归展开，含 **Registers**）、内存视图（hex dump + ASCII，地址输入、64/128/256 字节；**编辑**：hex 字节输入 + 「写入」经 DAP `writeMemory` 下发并自动重读；**多区域**：命名区域栏保存/切换/删除；**暂停时实时刷新**：每次停止事件后自动重读当前区域）、反汇编源码混排视图（PC 行高亮 + 可点击源码行映射 + **每行断点槽**：有源码映射的指令点击切换对应源码行断点，无映射如实提示）、调试控制台；**跟踪窗口（SWO/SWV）配置面板**（「跟踪」页：ITM 端口 / SWO 分频 / DWT 数据跟踪 / 运行模式，配置持久化到 `.ns-ide/trace.json`，启用时下发目标侧寄存器写入；启用后后台线程连接探针端口读取 SWO 字节流，解码为 ITM 包实时追加到「跟踪」页，端口过滤生效，失败如实提示；面板如实显示 SWV 探针状态，不假装收到包）；增删/启停/清除断点即时同步，并回填每个断点的 DAP `verified`/`reason` 状态；条件断点、命中次数断点（第 N 次命中）、日志断点（`{expr}` 输出到控制台）均下发并生效
- **Git 集成（M12 切片）**：状态栏「Git」项 + Git 面板（变更文件列表、unified diff、暂存/取消暂存、一键提交、分支与变更数、**本地分支切换/新建分支/拉取/推送**——历史·分支区每行分支可点击切换，顶栏「＋新建分支」「↓ 拉取」「↑ 推送」按钮，`checkout`/`create_branch`/`pull`/`push` 底层封装），非仓库目录如实提示；重命名条目解析为变更目标路径，分离 HEAD 伪分支不显示
- **工具链检测（M11 切片）**：新建项目向导为编译器/仿真器/调试器/烧写器显示「路径 · 版本」（`--version` 探测），缺失工具给出安装建议；`ns-project::toolchain::Tool::version` + `missing_guide`
- **工具链校验（M11 切片）**：打开项目时校验 `project.toml` 配置的工具是否在 PATH，缺失的显示到「问题」面板并附安装建议；`Project::tool_warnings`
- **静态分析（M12 切片）**：命令面板「静态分析」按语言运行 clang-tidy / cargo clippy / zig / zr-lint，结果进「问题」面板；`ns_build::plan_static_analysis`
- **终端（M12）**：命令面板「打开终端」启动系统 shell（`Terminal`，基于 `portable-pty`），实时输出流式渲染、可输入回显、Ctrl+C 中断、清屏按钮、退出码/尺寸显示、`Drop` 时关闭 PTY；PTY 网格随面板尺寸自适应（`on_children_prepainted` 量取视口后按字符 advance 与 17px 行高计算列/行并 `resize`，提示符与光标不会被挤出可视区）；提示符由 IDE 注入（bash `--rcfile`、zsh `ZDOTDIR`、POSIX sh `$ENV` 生成临时初始化脚本，先加载用户 rc，不改写用户配置），形如 `目录名# `，git 工作区内为 `目录名(git:分支)# `；新终端默认工作目录为项目根（无项目时 `$HOME`）；新输出自动滚动到底部，用户上滚暂停跟随、回到底部恢复，面板底部「跟随输出 / 已暂停跟随」可手动切换；滚轮/触控板按平台方向滚动回溯（上滚看历史，像素增量累积为整行）。命令面板「打开串口监视器」用 `NS_SERIAL_PORT` 环境变量指定的设备启动串口会话（`TerminalKind::Serial`），未设置时如实提示。
- **会话恢复（M12）**：打开文件标签与当前活动标签持久化到 `.ns-ide/session.json`（打开/切换/关闭/退出时保存，重开项目自动恢复），`ns_project::Session` 往返单测覆盖。
- **项目设置（M12）**：`Cmd/Ctrl+,` 或命令面板「项目设置…」打开设置模态，编辑 `project.toml` 常用字段（名称/目标 MCU/编译器路径/构建配置/输出目录/调试器路径/烧写器路径/烧写器类型），保存即写回 `project.toml`（校验：名称非空、构建配置为 debug/release、烧写器类型为 openocd/pyocd/jlink），`ns_project` 往返单测覆盖。
- **烧写（M5 切片）**：命令面板「烧写固件」/ 顶部「烧写」按钮 / `Cmd/Ctrl+Shift+F10` 用 `project.toml` 的 `[flasher]` 适配器（OpenOCD / pyOCD / J-Link Commander，`kind` + `path` + `interface`）烧写构建产物；固件格式 ELF / Intel HEX / BIN（HEX 解码、校验和/EOF 校验）；`ns_flash` crate：`adapter_for` + `Flasher` trait + 端到端单测（stub 工具验证命令构造）。
- **运行编排（M6 切片）**：顶部「烧写并运行」（编译→烧写→复位运行）与「烧写并调试」（编译→烧写→连接调试器），命令面板「烧写并运行」/「烧写并调试」同源；任一环节失败即停止并如实报告，不静默继续。
- **工具链全局路径（M11 切片）**：命令面板「工具链路径…」编辑全局工具链覆盖（arm-none-eabi-gcc/gdb、clang、gcc、rustc、zig、openocd、pyocd、JLinkExe），留空=从 PATH 探测；覆盖优先于自动发现，保存在用户配置目录（`toolchain.json`），跨项目共享；`ns_project::ToolchainConfig` + `registry`（语言→可用编译器，向导级联）。
- **语言服务 LSP（M8 切片）**：新建 `ns_lsp` crate（LSP-over-stdio 客户端，Content-Length 帧、后台读写线程、请求/通知路由）；项目打开时按语言自动启动服务器（C/C++→clangd、Rust→rust-analyzer、Zig→zls、ZR→zr-lsp），文件打开/编辑实时同步 `didOpen`/`didChange`；诊断 `publishDiagnostics` 合并进问题面板（点击跳转源码）；`F12` 跳转到定义、`Shift+F12` 查找引用、`Cmd/Ctrl+K Cmd/Ctrl+I` 悬停提示（状态栏）、`Cmd/Ctrl+Space` 触发补全（浮动补全面板，↑/↓ 选择、Enter/Tab 确认、Esc 关闭）；单测含 mock 服务器（initialize/补全/定义/诊断）与真实 clangd 端到端（错误文件→诊断到达）。
- **目标设备库（M9 切片）**：新建 `ns_device` crate——CMSIS-SVD XML 解析（`parse_svd`：外设→寄存器→位域，地址/大小/复位值）、内置设备目录（stm32f103c8 / stm32f407vg / nrf52840 / rp2040，`CATALOG`/`device_for_target`）、`linker_script`/`startup_file` 生成（flash/RAM 内存映射 + 向量表/复位处理）、`memory_map`/`registers_by_peripheral`；向导「目标设备」步显示 MCU 核/Flash/RAM 并在生成时自动写出 `link.ld` 与 `src/startup.s`；调试面板新增「设备」页按 SVD 展示外设寄存器（地址/复位值/位域）；**寄存器值点击编辑**：点当前值进入编辑（预填实时值），Enter 提交写回目标（仿真 `write_u32` / 调试 DAP `writeMemory`，4 字节小端）；**外设寄存器树过滤**：过滤输入框按外设/寄存器名大小写不敏感子串过滤（外设名匹配显示整组，仅寄存器匹配保留外设头为上下文）；单测覆盖解析/生成/目录查找与写入提交与过滤。
- **仿真时钟频率显示（F6）**：`project.toml` `[sim]` 新增 `clock_hz`（设置页可编辑）；仿真页在配置时钟后显示**仿真时间 + 名义频率**（`sim_time_readout`，如「时间: 1.00 s @ 72.0 MHz」），未配置只显示周期数。
- **仿真（M7 切片）**：新建 `ns_sim` crate——内建指令级仿真器（RV32I 核：寄存器文件、64 KiB 内存、`load_elf`/`load_image`、`step`/`run`/`step_over`、断点、`ecall` 停机、`sign_extend`/`jal`/`branch` 立即数解码）；虚拟外设（内存映射 UART，`putchar` 回调 + 输出缓冲）；`simulate` 命令面板「仿真运行（内建指令级）」加载构建产物（ELF 优先，否则按入口原始加载）到仿真器；调试面板新增「仿真」页：控制条（加载并运行/单步/运行/复位/停止）、PC/周期/状态、32 个寄存器（ABI 名 + 值）、UART 输出；单测覆盖 ALU（加/减）、循环累加（1..10=55）、断点命中、load/store、ecall 停机，以及真实 ELF32 端到端（入口加载→循环→a0=55→停机）。
- **全量配置页（M12）**：`Cmd/Ctrl+,` / 命令面板「项目设置…」打开设置模态，编辑 `project.toml` **全部字段**（project/compiler/debugger/flasher/build/sim 各节）+ 全局设置（终端 shell → `terminal.json`、全局配置键 `$config["key"]` → `config.json`）；保存即写回整个 `project.toml`（校验：名称非空、构建配置 debug|release、烧写器/探针类型枚举、语言 C|C++|Rust|Zig|ZR）。
- **Agent 面板（参考 Zed agent）**：输出区「Agent」标签 / 命令面板「打开 Agent 面板」；任务列表（状态点 + 命令标题）、选中任务输出、底部 shell 命令输入 + 运行；后台线程跑 `sh -c`/`cmd /C`，stdout/stderr 流式追加，「清空」/「全部取消」。
- **串口助手（M12）**：输出区「串口」标签 / 命令面板「打开串口助手」；枚举端口、波特率/校验/数据位/停止位/流控、打开/关闭、hex 收发、时间戳、清空、收发字节计数。
- **终端设置（M12）**：设置页「终端 shell」跨项目保存到 `terminal.json`；留空=平台默认（**Windows: cmd，Linux/macOS: bash**），默认终端路径自动识别。
- **外部工具/自定义命令（本轮）**：命令面板「运行外部工具…」选择已配置工具（`sh -c`/`cmd /C` 流式输出到「工具」标签页，可取消）；「编辑外部工具…」模态增/改/删，配置保存在 `.ns-ide/tools.json`（项目级）。
- **诊断导航（本轮）**：`F8` / `Shift+F8` 在**构建输出 + LSP 诊断**间跨文件/行顺序跳转（问题面板「上一个/下一个错误/警告」），环回两端，跳转定位源码行并在状态栏报告；路径 canonicalize 归一化（`/var`↔`/private/var`）保证锚点一致。
- **编辑器诊断悬停提示（本轮）**：悬停错误/警告波浪线显示消息浮窗（随鼠标跟随，离开清空）；波浪线按诊断字符范围绘制，构建诊断划线到行尾、LSP 诊断按列宽。
- **调试变量悬停求值（本轮）**：调试会话停止时悬停编辑器标识符显示当前值（玉色浮窗，`Evaluate` 求值，离开词即清空）；非调试态仍走 LSP 悬停。
- **项目模板（F1.2）**：新建向导「模板」步（仅 C 语言）——空模板 / **LED Blink**（GPIO 翻转）/ **UART 回环** / **FreeRTOS**（两任务协作调度）；生成时写入 `src/main.c` 并填充外设初始化。
- **烧写扩展（M5）**：烧写面板接入**擦除/校验/读回**（读回写 `out/readback.bin`）+ 烧写进度；`ns_flash::firmware::convert`（ELF→HEX/BIN）。
- **运行编排扩展（M6）**：运行/调试会话的**停止与重启**状态机（重启 = 停止后重新编译→运行）。
- **调试扩展（F7）**：运行到光标（`continueTo`）、设置变量值（`setVariable`）、监视（watch）表达式面板、调试控制台表达式求值；**内存视图增强**：hex 字节编辑（DAP `writeMemory`）、多区域命名切换、暂停时实时刷新。
- **编辑器扩展（F3）**：当前文件查找/替换（`Cmd/Ctrl+F`）、括号匹配高亮、代码折叠、`Cmd/Ctrl+S` 保存（脏标记）；**跳转返回历史栈**（`Alt+←/→`）、**跳转到包含头文件**（`#include` 解析定位）、**迷你地图**、**多光标编辑**（`Cmd/Ctrl+Alt+↑/↓` 添加、`Cmd/Ctrl+D` 选中下一个）、**符号重命名**（`Cmd/Ctrl+Shift+R`，LSP `textDocument/rename`，跨文件 `WorkspaceEdit` 应用）。
- **编辑器文本变换（本轮）**：`Cmd/Ctrl+Shift+U` 大写、`Cmd/Ctrl+Alt+Shift+L` 小写（选区或无选区时光标词）、`Cmd/Ctrl+J` 合并行、`Cmd/Ctrl+Shift+S` 排序行、`Cmd/Ctrl+Alt+T` 去行尾空白；**编辑器右键菜单编辑项**：右键菜单新增剪切/复制/粘贴/全选/格式化文档/重命名符号…分区。
- **外部文件变更自动重载（本轮）**：每 2s 轮询打开的文件，干净缓冲在磁盘变更时静默重载，脏缓冲弹外部变更对话框（重载/保留/取消）不静默覆盖。
- **比较活动文件与磁盘（本轮）**：命令面板「比较活动文件与磁盘…」对比活动编辑器缓冲与磁盘内容，变更时弹差异模态（`+/-` 行 + 变更计数），一致时提示「与磁盘内容一致」。
- **问题面板严重级别过滤（本轮）**：问题面板顶部「错误/警告/信息」胶囊开关（显示各自计数），点击切换该级别显示/隐藏，最后启用级别不隐藏。
- **文件树 Git 变更徽章（本轮）**：文件树文件条目右侧显示 Git 状态徽章（M=修改/U=未跟踪/D=删除），未变更或非仓库不显示。
- **寄存器行复制（本轮）**：调试寄存器视图每行「📋」复制 `名称 = 值` 到剪贴板。
- **变量面板过滤（本轮）**：调试变量面板「过滤变量…」输入框按名大小写不敏感子串过滤（递归匹配展开子变量），无匹配时提示。
- **图形化外设配置（本轮）**：命令面板「外设配置（图形化，CubeMX 式）…」打开模态，按目标 SVD 列出外设并勾选启用；GPIO 端口 16 个引脚点击循环模式（输入/输出/复用），生成时把引脚模式打包成真实 `CRL`/`CRH` 寄存器写（`PERIPH->REG = value;`）。
- **时钟树图形化配置（本轮）**：命令面板「时钟树配置（图形化，CubeMX 式）…」打开独立模态（`clock-tree-modal`），可点击选择 HSI/HSE/PLL 源、PLL 参数、AHB/APB1/APB2 预分频，SYSCLK/APB1/APB2/PLL48 及示例外设时钟实时刷新；覆盖 **STM32F1**（`RCC->CR`/`RCC->CFGR`）与 **STM32F4**（另写 `RCC->PLLCFGR` 的 PLLM/PLLN/PLLP/PLLSRC/PLLQ，`stm32f407vg.svd` 已补齐寄存器）；「生成代码」写 `src/clock_init.c`，`rcc_clock_init()` 发出真实寄存器写。向导「仿真软件」步骤文案同步修正（内置 `ns-sim` 已实现，不再是「后续里程碑」）。
- **引脚映射图（本轮）**：命令面板「引脚映射图（外设功能 → 物理引脚，CubeMX 式）…」打开独立模态（`pin-map-modal`），按 STM32F103C6/C8/CB（LQFP48 中密度，同封装同 AF 表）把 USART1/2/3、SPI1、I2C1、TIM2 与 GPIO 分配到 PA/PB/PC 物理引脚；点击引脚在「未分配 → 可用功能 → 未分配」循环，红点表示 AFIO 重映射；冲突（重复信号、默认/重映射混用）会阻止生成；「生成代码」写 `src/pin_mapping.c`，`pin_mapping_init()` 发出真实 `GPIOx->CRL/CRH`、`RCC->APBxENR` 与 `AFIO->MAPR` 写；`stm32f103c6`/`stm32f103cb` 已加入设备目录（32KB/10KB、128KB/20KB）与向导目标列表，CubeMX `.ioc` 对应名正确映射。
- **引脚映射图 STM32F4 扩展（本轮）**：同一模态按目标自动切换 `PinMapFamily`；STM32F407（LQFP100）用 `F4PinSignal`（GPIO、USART1/2/3、SPI1/2、I2C1/2、TIM1-5、USB、CAN1、SWD）给出每引脚可用 AF0-15，`validate_f4_pin_mapping` 拒绝重复信号与同外设 AF 号不一致；「生成代码」写真实 `RCC->AHB1ENR`、`GPIOx->MODER`、`GPIOx->OTYPER` 与 `GPIOx->AFRL/AFRH`；`stm32f407vg.svd` 的 GPIOA/GPIOB 已补齐这些寄存器。
- **引脚映射图 STM32F407 端口补齐（本轮）**：LQFP100 的 F407 引脚映射此前只覆盖 GPIOA/GPIOB，本次扩到 **GPIOA..GPIOE（5 端口、每端口 16 引脚）**，新增 **SPI3**（AF6）、**USART6**（AF8）、**TIM8**（AF3）、**TIM1_CH4/CHnN/BKIN/ETR**（AF1）变体，覆盖 PC（PC6-9 TIM3/TIM8/USART6、PC10-12 USART3/SPI3）、PD（PD0-1 CAN1、PD3-7 USART2、PD8-13 USART3、PD12-15 TIM4）、PE（PE7-15 TIM1）；`stm32f4_pin_clock_bit` 扩到 C/D/E（AHB1ENR 位 2/3/4）；`stm32f407vg.svd` 新增 GPIOC/GPIOD/GPIOE 与 AHB1ENR 的 GPIOCEN/GPIODEN/GPIOEEN。
- **引脚映射图 STM32F407 外设补齐（本轮）**：再补 F407 常用外设 AF——**SDIO**（AF12：`SdioCmd`/`SdioCk`/`SdioD0-D7`，PC8-11/PC12/PD2/PD6-9）与 **TIM9/10/11**（AF3：PA2/PA3、PB8、PB9），生成真实 `GPIOC->AFRH`/`GPIOD->AFRL/AFRH` 与 AHB1ENR 位。
- **引脚映射图 STM32F407 外设补齐（续，本轮）**：再补 F407 引脚映射的常规小缺口——**I2C3**（AF4：`I2c3Scl`=PA8/PC12、`I2c3Sda`=PC9，开漏，写入 `GPIOC->OTYPER`）与 **TIM12/13/14**（AF9：`Tim12Ch1`=PB14、`Tim12Ch2`=PB15、`Tim13Ch1`=PA6、`Tim14Ch1`=PA7）。
- **引脚映射图 STM32F407 输入模式与速度（本轮）**：GPIO 新增**输入·上拉 / 输入·下拉**两种模式（点击引脚在 输出 → 输入 → 输入·上拉 → 输入·下拉 → 外设功能 间循环），生成代码除 `MODER`/`OTYPER`/`AFRL`/`AFRH` 外新增真实 `GPIOx->OSPEEDR`（复用/输出脚写高速档）与 `GPIOx->PUPDR`（上拉/下拉）写入；`stm32f407vg.svd` 的 GPIOA..GPIOE 已补齐这两个寄存器。
- **引脚映射方案保存/载入（本轮）**：引脚映射图新增「保存方案」「载入方案」按钮，把当前分配写入 `.ns-ide/pin_map.json`，重新打开后可一键恢复；载入只接受该引脚合法的信号（非法项跳过并计数），跨目标/跨家族的方案、损坏文件、无分配或存在冲突都会被如实拒绝并提示。
- **引脚映射图 STM32F407 TIM2_CH3/CH4 与外设分组一键分配（本轮）**：AF 表补上 PA6=`TIM2_CH3`、PA7=`TIM2_CH4`（AF1）；引脚网格上方新增**外设分组 chip 行**，点击分组即批量分配该外设全部引脚，再次点击整组取消——已被其他功能占用的引脚不会被动，同一信号重复的引脚只分配一次，占用/冲突情况在面板与状态栏如实提示。
- **引脚映射图导入 CubeMX `.ioc`（本轮）**：引脚映射图新增「导入 .ioc」按钮，直接读取 CubeMX 生成的 `.ioc`——`PA9.Signal0=USART1_TX`、`PB6.Signal0=I2C1_SCL`、`PA0.Mode=GPIO_Output`、`PA13.Signal0=SYS_JTMS-SWO` 等按 CubeMX 命名导入（`SYS_JTMS-SWO`/`SYS_JTCK-SWCLK` 自动对应 `SWDIO`/`SWCLK`，`USB_OTG_FS_DM/DP` 对应 `USB_DM`/`USB_DP`，`GPIO_Input_Pu/Pd` 对应输入·上拉/下拉）；`Mcu.IP_*`、`ProjectManager.*`、`PA1.Lock` 等非引脚行一律忽略，越界引脚（如 `PA16`）拒绝；导入前先清空当前分配，未建模或该引脚无此功能的项计数并在面板如实说明，MCU 与当前目标不符整体拒绝，导入后仍跑冲突校验（PB6/PB8 同为 `I2C1_SCL` 会报冲突）。
- **引脚映射图导出 CubeMX `.ioc` 与导入格式校正（本轮）**：把上一轮导入器从只认 `PA9.Signal0=…` 扩展为**同时支持 CubeMX 原生格式**（`Mode.PA9=GPIO`、`Signal.PA9=USART1_TX`、`SignalMode.PA9=AS`、`Pull.PA9=PUp`），GPIO 方向/上下拉由 `SignalMode`/`Pull` 合成，裸 `GPIO` 不算分配；新增「导出 .ioc」按钮，把当前引脚分配写成 CubeMX 风格的 `.ioc` 引脚段（`pin_map.ioc`，含 `McuName`/`Signal`/`SignalMode`/`Pull`），信号名按 CubeMX 拼写回写（`SWDIO`→`SYS_JTMS-SWO`、`GPIO 输出`→`SignalMode=RegularOutput`）；导出→再导入可无损往返。只生成引脚段，时钟/中断/外设实例配置请在 CubeMX 中补全。
- **引脚映射图 STM32F407 生成代码外设时钟使能（本轮）**：F407 引脚映射「生成代码」此前只使能 GPIO 端口的 AHB1 时钟，复用脚对应的外设时钟没开。现按已分配信号自动汇总并写出 `RCC->APB2ENR`（USART1/6、SPI1、TIM1/8/9-11、SDIO）、`RCC->APB1ENR`（USART2/3、SPI2/3、I2C1/2/3、TIM2-5、TIM12-14、CAN1）与 `RCC->AHB1ENR`（USB_OTG_FS 位并入每端口写），使生成的 `pin_mapping_init()` 真正可用。
- **引脚映射生成代码自包含寄存器定义（本轮）**：生成的 `pin_mapping.c` 引用 `GPIOx->…`/`RCC->…`/`AFIO->…` 但没定义寄存器视图或基址，无法独立编译。现 F1/F4 各自注入 `ns_` 前缀的自包含寄存器结构（`ns_gpio_f1_t`/`ns_afio_t`/`ns_rcc_f1_t`、`ns_gpio_t`/`ns_rcc_t`）与 `#ifndef` 保护的 `GPIOA..GPIOE`/`RCC`/`AFIO` 基址宏——项目已含厂商头时厂商宏获胜，未含时本文件独立可编译。
- **时钟树生成代码自包含 RCC 定义（本轮）**：`clock_init.c` 引用 `RCC->CR`/`RCC->PLLCFGR`/`RCC->CFGR` 但没定义寄存器视图，无法独立编译。现 F1/F4 各自注入 `ns_` 前缀的自包含 RCC 结构（`ns_rcc_f1_t`/`ns_rcc_t`）与 `#ifndef` 保护的 `RCC` 基址宏（0x40021000/0x40023800）——项目已含厂商头时厂商宏获胜，未含时独立可编译。
- **反汇编↔内存联动（本轮）**：调试反汇编视图每行地址可点击，切换内存页并跳到该地址重读。
- **RTOS 任务列表（本轮）**：调试面板「RTOS」标签 + 刷新按钮，对 FreeRTOS 风格模板的 `task_count`/`current_task`/`tasks[i].name` 求值并逐任务收集，显示运行标记/名称/优先级/栈使用。
- **RTOS 真实内核遍历启动修复 + DAP/纯函数一致性测试（本轮）**：修复真实 FreeRTOS 内核遍历**永不启动**的缺陷（启动门槛 `lists.len() >= 7`，实际只下发 6 个调度链表头 → 改为 `REAL_KERNEL_LIST_COUNT = 6`）；新增 `real_kernel_dap_walk_matches_pure_walker` 集成测试，用合成 FreeRTOS 内存镜像驱动 `Evaluate` 往返状态机，断言与 `ns_debug::rtos::walk_free_rtos` 在同一镜像输出一致（名称/运行/优先级/状态）。`cargo test --workspace` **649 项全绿**。
- **LSP 扩展（M8）**：签名帮助（`signatureHelp`，**光标移出调用自动消失**）、inlay hints（**按 (line,start) 去重防双渲染**）、语义高亮叠加（`semanticTokens`）。
- **静态分析增强（M12）**：`plan_static_analysis` 的 Zig 分支传 `-target <triple>`（按 MCU 核映射 `riscv32-unknown-none`/`arm-none-eabi`）与 `-Werror`；stub 单测验证参数透传。
- **仿真精度增强（M7）**：`Timer::tick()` 返回精确溢出次数（计数器越过自动重载若干倍时按比例累计）；`gpio_events` 携带触发翻转的 store 指令 `pc`，仿真页新增「GPIO 事件」日志视图对齐源码。
- **设备库扩展（M9）**：外部 SVD 文件加载、启动文件模板库（按核更多 MCU）、设备选择写回 `project.toml` 的 SVD 路径。
- **仿真扩展（M7）**：周期级/外设时序（GPIO/TIMER/中断控制器）、仿真断点下发到源码、C 源码映射（符号表→编辑器定位）；**GPIO 状态视图**（32 引脚方块，按方向/电平着色，输出高计数）、**TIMER 状态视图**（每定时器 启用/计数器/自动重载/预分频/溢出计数）。
- **ZR 标准库（M10）**：ZR 标准库由上游 `zr` 语言提供（`zr pkg` 包管理），IDE 不内置 `zr-std`；外设 API（GPIO/UART/TIMER）经 `zr` 的 FFI/裸机目标使用。
- **仿真看门狗外设（本轮）**：`ns_sim::Watchdog` 内存映射 WDT（喂狗 `0xAA55`、启用/禁用、超时周期、复位计数/PC）；`tick_peripherals_for` 按周期推进，超时记录复位事件；仿真页「看门狗」状态视图；单测覆盖喂狗/超时/非喂狗。
- **烧写外部 loader 完善（本轮）**：`FlashRequest.device` 传入项目 target；pyOCD/J-Link 用设备名 + FLM 几何（基址/尺寸）替代硬编码；FLM 几何接入全部适配器。
- **快捷键可配置（本轮）**：命令面板「快捷键设置…」编辑 `keybindings.json`（用户配置目录）覆盖默认键位（action 名→键位字符串），启动时应用，重映射后重启生效；`app::bindings_with` 支持覆盖（单测验证）。
- **编辑器分屏（本轮）**：命令面板「编辑器分屏（右侧）」/ `Cmd/Ctrl+\` 打开独立第二编辑窗格（独立光标/滚动/断点，LSP 同步）；`Cmd/Ctrl+Shift+\` /「关闭分屏」关闭；分屏脏状态随 `Cmd+S` 一并保存；分屏随会话持久化，重开项目恢复。
- **单元测试集成运行器（本轮）**：命令面板「运行单元测试」按语言/构建系统执行 `cargo test` / `ctest` / `make test` / `zig build test`，结果进输出面板与问题面板；ZR/汇编无内建运行器（如实提示）。
- **栈使用分析（本轮）**：命令面板「栈使用分析（-fstack-usage）」对 C/C++ 项目逐源编译 `-fstack-usage` 并解析 `.su` 文件，按函数列出静态/动态栈深。
- **覆盖率分析（本轮）**：命令面板「覆盖率分析（gcov）」对 C/C++ 项目用 `--coverage` 构建+运行测试二进制，`gcov` 汇总每文件行覆盖率（缺失工具如实提示）。
- **全局符号搜索（本轮）**：`Cmd/Ctrl+Shift+O` 或命令面板「搜索项目符号…」经 LSP `workspace/symbol` 搜索项目内符号（名称/文件/行），↑↓ 选择、Enter/点击跳转，Esc 关闭。
- **快速打开文件（本轮）**：`Cmd/Ctrl+P` 或命令面板「快速打开文件…」列出项目源文件，输入过滤、Enter/点击打开（跳过构建目录/二进制）。
- **最近文件列表（本轮）**：`Cmd/Ctrl+E` 或命令面板「最近打开的文件…」列出项目内最近打开文件（最前=最新，去重，上限 100，随 `.ns-ide/recent_files.json` 持久化）。
- **文件树符号过滤（本轮）**：侧栏「项目」标题下过滤输入框，按输入实时过滤文件树（匹配文件 + 祖先目录上下文），清空恢复完整树。
- **仿真 step_out 返回识别完善（本轮）**：`step_out` 现识别 RISC-V `ret`/`mret` 与 ARM `bx lr`/`pop {..,pc}` 返回指令，步出中断处理函数正确停靠（`disassemble_riscv` 补 `mret`，Thumb 解码补 `bx`/`pop`）。
- **覆盖率编辑器内联可视化（本轮）**：覆盖率分析后按行命中在编辑器内着色（未覆盖=jade 底、部分/全覆盖渐弱），gutter 覆盖条同步；`set_coverage` 清空/更新。
- **寄存器快照导出（本轮）**：寄存器视图「导出快照」按钮把当前寄存器组写成 `.ns-ide/register-snapshot.json`（名称/值），状态栏报告路径。
- **固件大小符号级分析（本轮）**：构建成功后「固件大小」摘要追加最大符号前 6 名（`nm --size-sort` 解析，过滤 absolute/undefined/debug）。
- **构建时间逐文件统计（本轮）**：构建输出末尾追加「文件编译耗时（估计）」，按 CMake/Make `Building …` 与 `-c <file>` 标记测量真实墙钟间隔，降序排列。
- **构建错误快速修复（本轮）**：问题面板对缺失头文件/未定义引用/未声明标识符/缺分号诊断显示「快速修复」；缺失头文件一键在最后一个 `#include` 后插入并重载编辑器。
- **trace 触发/统计 + DWT PC 采样布防（本轮）**：trace 面板新增「PC 采样」开关（DWT 周期计数器布防，`DWT_CYCCNT` 复位 + 比较器 0 周期模式）；「实时包」区显示 ITM/DWT/异常/PC 采样统计。
- **厂商工程导入（本轮）**：命令面板「导入工程（IAR/Keil/STM32CubeMX）…」解析 `.ewp`/`.uvprojx`/`.ioc` 为 `project.toml`（源码/包含目录/目标识别），写项目并打开。
- **外设初始化代码生成（本轮）**：命令面板「生成外设初始化代码（SVD 配置）…」按 SVD 外设生成 `peripheral_init()` + 各外设 `xxx_init()` 助手。
- **性能 profiler 热点聚合（本轮）**：trace 面板「热点」开关开启后把 DWT PC 采样按固件符号归并，下方显示函数热点直方图（占比条 + 命中数）。
- **内存布局可视化（本轮）**：命令面板「内存布局视图（链接脚本区域/段占用）」解析 `link.ld` 的 `MEMORY` 并把固件段按地址归入区域，输出面板显示区域占用条（已用/容量/百分比）+ 最大段。
- **include 依赖图（本轮）**：命令面板「include 依赖图（项目级）」扫源文件解析 `#include`，按包含目录解析本地头，输出面板列出每文件直接依赖与反向计数。
- **依赖图 DAG 图形渲染（本轮）**：include 依赖图头部新增「图形/列表视图」切换；图形视图按最长依赖路径分层（`dag_layout`），绝对定位节点盒 + 水平边渲染分层 DAG（头文件金色、源文件米白）。
- **宏展开/预处理输出（本轮）**：命令面板「宏展开/预处理输出（C/C++ -E）」对 C/C++ 项目运行编译器 `-E -P`，输出面板流式显示宏展开后的翻译单元（`-std`/`-D`/`-I` 同构建，非 C/C++ 或缺失编译器如实提示）。
- **对比嵌入式 IDE 补齐（本轮）**：对照 IAR/Keil/STM32CubeIDE/PlatformIO/VSCode 能力模型补齐 16 项——**编辑器基础操作**（`Ctrl+/` 行注释、重复/删除行、`Alt+↑/↓` 移动行、`Shift+Alt+←/→` 选中展开/收缩）；**头文件/源文件切换**（`goto-header-source` 解析 `#include` 定位对应文件）；**编辑器标签右键菜单**（关闭其他/右侧/全部标签）；**调试体验**（寄存器值编辑经 `setVariable`、内存视图搜索/填充、watch 表达式持久化到 `.ns-ide/watch.json`、变量面板「↻ 刷新」按钮重取作用域/变量）；**固件大小摘要**（`text`/`data`/`bss` 分区，`size` 工具三列解析）；**烧写后自动校验**（「烧写并校验」在烧写成功后链式执行 `FlashAction::Verify`）；**构建配置下拉选择器**（状态栏 debug/release 下拉写回活动目标 `[[targets]]` 条目，命令面板「切换构建配置」）。
- **调试控制台命令历史（本轮）**：调试控制台输入框回车求值后记录命令（去重连续重复、上限 100），`↑`/`↓` 在历史中导航（首次按 `↑` 保存草稿，越过最新恢复草稿）。
- **跳转到行（本轮）**：`Cmd/Ctrl+G` 或命令面板「跳转到行…」输入 1 基行号回车跳转（越界自动夹紧，非法输入如实提示）。
- **文件树右键增强（本轮）**：右键菜单新增「复制路径」（写绝对路径到系统剪贴板）与「在访达中显示」（macOS `open -R` / 其他平台 `xdg-open` 父目录）。
- **文件树复制副本（本轮）**：右键菜单「复制副本…」把文件或整个目录树复制到新名称（重名拒绝，完成后展开父目录）。
- **输出面板保存 / 终端清屏（本轮）**：输出头部「保存」把构建/运行缓冲写入 `<项目>/out/build-output-<时间戳>.log`；终端标签栏「清屏」清空当前会话滚动缓冲。
- **编辑器缩放（本轮）**：`Cmd/Ctrl+=` / `Cmd/Ctrl+-` 放大/缩小、`Cmd/Ctrl+0` 重置 100%（0.5–2.0 夹紧），缩放随会话持久化；命令面板同源入口。
- **编辑器缩进宽度（本轮）**：命令面板「缩进宽度：2/4/8 空格」配置 Tab/自动缩进步长（非法值回退 4），随会话持久化。
- **当前行高亮（本轮）**：光标所在行显示微妙背景色带（折叠中隐藏），随光标实时跟随，选中/调试高亮绘制其上不遮挡。
- **调用层级（本轮）**：命令面板「调用层级」经 LSP 解析光标处符号，弹窗显示被调用/调用双向可切换，Enter/点击跳转到调用点源码。
- **对比嵌入式 IDE 补齐（本轮完成）**：编辑器双击选词/三击选行（`word_range_at`/`line_range_at`，点击空白不误选）；调试内存视图「导出/导入」（`out/memory-<addr>-<ts>.bin` 导出，`out/memory-import.bin` 经 DAP `writeMemory` 导入）；命令面板「整理导入（organizeImports）」经 LSP `codeAction` 应用 `WorkspaceEdit`；烧写修复（`probe-rs` 用项目 target 作为 `--chip`，`StLink` 基址改用 FLM 几何）；日志断点命中联动（DAP `output` 事件携带 `source`/`line`，命中计数持久化）；设备寄存器写入回填仿真内存视图（`sim_read_memory` 刷新）。
- **编辑器补全（本轮）**：输入 `(`/`[`/`{`/`"`/`'`/`` ` `` 自动闭合配对（右符紧邻时跳过不重复，删除成对）；命令面板「切换软换行（word wrap）」折行长行（默认关，随会话持久化）；断点面板「导入/导出」把断点+观察点写/读 JSON 文件（合并去重）；二进制/非 UTF-8 文件打开为**可编辑** hex 视图（编辑后保存写回原始字节，非法单元格如实报错）。
- **项目管理与工具链（本轮）**：文件菜单/命令面板新增「关闭项目」「重命名项目…」（重命名目录 + 同步 `project.toml`）+「删除项目…」（确认后递归删除，从最近项目移除）；命令面板「生成 compile_commands.json（clangd）」手动刷新编译数据库；文件树右键「在终端中打开」以所选目录启动 shell（文件取其父目录）。
- **关闭未保存标签确认（本轮）**：关闭脏标签（单个/其他/右侧/全部）不再静默丢弃——弹「取消 / 不保存 / 全部保存」确认（`Enter`=全部保存并关闭），`Cmd+Q`/「退出」同样先弹窗；保存失败即中止关闭。
- **编辑器右键增强（本轮）**：编辑器内容右键菜单新增「复制文件路径」与「在终端中打开当前目录」（以活动文件父目录启动 shell），与文件树右键动作对齐。
- **内存视图「跟随 PC」（本轮）**：调试内存视图头部新增「跟随 PC」按钮——把地址设为当前 PC 并立即读取；无 PC 时如实提示。
- **变量面板「复制值」（本轮）**：变量行新增「📋 复制」——把 `name = value`（值为空时仅 `name`）写系统剪贴板。
- **仿真内存视图「跟随 PC」（本轮）**：仿真页内存视图新增「跟随 PC」按钮——地址设为当前仿真 PC 并立即重读 dump；仿真器未加载时如实提示。
- **仿真「运行到光标」（本轮）**：仿真页控制条新增「运行到光标」——光标行映射到指令地址临时断点运行，执行后恢复断点并聚焦命中行；无映射行/未加载如实提示。
- **仿真寄存器「复制」（本轮）**：仿真寄存器单元格新增「📋」——把 `name = 0xvalue` 写系统剪贴板。
- **反汇编「复制指令」（本轮）**：调试反汇编每行新增「📋」——把 `address  instruction` 写系统剪贴板。
- **内存视图「复制行」（本轮）**：调试内存每行新增「📋」——把 `address  hex-bytes  │ ascii` 写系统剪贴板。
- **watch 表达式「复制」（本轮）**：调试变量页 watch 行新增「📋」——把 `expression = value` 写系统剪贴板。
未完成（实现但未验证）：**嵌入式目标 attach**（OpenOCD / ST-Link / J-Link / pyOCD / probe-rs 探针适配器 + `target remote` GDB 会话，命令面板「嵌入式调试（目标 attach）」→ 编译→烧写→attach；需硬件，尚未在本机验证）；**ZR 真实 `zrc` 编译器集成**（`zr-std` 声明库已提供，编译器本体后续接入）；**真实 FreeRTOS 内核遍历硬件级终验**（DAP 往返路径与纯函数一致性已由集成测试在本机验证，仍需真实内核符号确认 TCB/链表布局与默认 `RtosLayout` 偏移一致）；**STM32 其他封装/系列引脚映射表扩展**（当前内置 STM32F103C6/C8/CB LQFP48 中密度与 STM32F407 LQFP100 已建模 GPIO+USART1/2/3/6+SPI1/2/3+I2C1/2/3+TIM1-14+SDIO+USB+CAN1+SWD；其他封装/系列目标如实提示暂不支持，且 F407 尚未建模的 DCMI/FMC 等留作后续增强），见 AGENTS.md §5.1 🚧 清单。
- **编译修复与补齐（本轮）**：工作区此前无法编译（`cargo check` 11 错误），本次修复——实现缺失的 `ns_debug::profile::ProfileAggregator`（DWT PC 采样热点聚合，3 项单测）、`kernel_walk_expr`/`rtos_kernel_send`/`rtos_kernel_advance`（真实 FreeRTOS 内核链表遍历，2 项单测）、补齐 3 处过期测试初始化器；删除 8 处死代码。`cargo check --workspace` 退出 0、本地零警告；`cargo test --workspace` **635 项全绿**。详见 [GAP_ANALYSIS.md](GAP_ANALYSIS.md)。
- **UI / 交互测试与 MCP 面板完善（本轮）**：项目此前虽声明 `gpui` 的 `test-support` 依赖，但 `#[gpui::test]` 数量为 0——全部为纯逻辑测试。本轮建立 headless GPUI 测试基线（GPUI 0.2.2 的 `TestAppContext`/`VisualTestContext` 可 headless 构建窗口、派发输入、断言 Entity 状态；无像素快照能力），并为 MCP 面板补齐 16 项测试（9 项纯协议 + 7 项 UI/交互，覆盖初始状态、工具调用、无效 JSON、握手、退出/断开、首帧渲染）。同时完善 MCP 面板真实缺陷：启动后发送 `initialize` 握手并在收到响应后发送 `initialized`（`ready` 状态此前永不置位，UI 永远显示「连接中…」）；JSON-RPC id 自增（此前恒为 1）；输入框绑定 Enter 提交；子进程退出经 wait 线程回收并通知 UI、关闭连接时经关闭 stdin 让其自然退出（避免僵尸进程）；抽出纯协议层 `mcp_proto` 供测试复用。修复两处既有警告：`ns-sim` 中 `usize` 恒真比较 `irq_line >= 0`、两个错误嵌套在函数内部而从未注册的内嵌测试（保留文件末尾的顶层副本）。`cargo fmt --all -- --check` 无输出、`cargo check --workspace` 本地零警告、`cargo test --workspace` **839 项全绿**（其中 7 项为 headless UI 测试）。

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

## MCP（Model Context Protocol）

IDE 内置 MCP 服务器，LLM 可通过标准 JSON-RPC 2.0 over stdio 驱动所有 IDE 操作。

**从 IDE 内部使用**：命令面板「打开 MCP 服务器面板」→ MCP tab，自动启动子进程，输入框直接发工具调用或原始 JSON-RPC。

**从外部 LLM 客户端连接**：将 `ns-mcp` 二进制加入客户端的 `mcpServers` 配置：

```jsonc
// Claude Desktop ~/.claude/settings.json 示例
{
  "mcpServers": {
    "ns-embed-ide": {
      "command": "/path/to/ns-mcp",
      "args": [],
      "cwd": "/path/to/your/embedded-project"
    }
  }
}
```

可用工具（139 个）：`project_info`、`list_devices`、`build`、`clean`、`read_file`、`write_file`、`search_files`、`read_file_lines`、`replace_in_files`、`diff_text`、`list_files`、`get_diagnostics`、`read_binary`、`list_open_files`、`get_session`、`save_session`、`run_tests`、`toggle_build_config`、`write_project_field`、`get_workspace_symbols`、`get_device_info`、`read_project_toml`、`list_build_outputs`、`open_project`、`create_project`、`get_peripherals`、`generate_linker_script`、`flash_firmware`、`run_simulation`、`mem_search`、`run_static_analysis`、`preprocess_source`、`generate_compile_commands`、`read_stack_usage`、`suggest_quick_fix`、`memory_map`、`get_startup_file`、`check_toolchain`、`parse_svd_preview`、`get_register_details`、`decode_number`、`generate_init_code`、`get_project_issues`、`get_linker_script`、`list_catalog_devices`、`read_recent_projects`、`compute_artifact_path`、`list_breakpoints`、`add_breakpoint`、`remove_breakpoint`、`update_breakpoint`、`add_watchpoint`、`remove_watchpoint`、`set_watchpoint_enabled`、`list_external_tools`、`add_external_tool`、`remove_external_tool`、`list_bookmarks`、`toggle_bookmark`、`export_breakpoints`、`debug_adapter_status`、`git_status`、`git_diff`、`git_stage`、`git_unstage`、`git_commit`、`git_log`、`git_branches`、`git_checkout`、`git_create_branch`、`git_pull`、`git_push`、`import_project`、`get_toolchain_config`、`set_toolchain_override`、`list_toolchain_registry`、`sim_session_start`、`sim_session_end`、`sim_step`、`sim_run`、`sim_reset`、`sim_status`、`sim_read_mem`、`sim_write_mem`、`sim_read_reg`、`sim_write_reg`、`sim_disassemble`、`sim_snapshot_save`、`sim_snapshot_load`、`sim_breakpoint_add`、`sim_breakpoint_remove`、`sim_breakpoint_list`、`sim_clear_breakpoints`、`sim_step_over`、`sim_step_out`、`sim_run_to`、`sim_peripherals`、`sim_uart_read`、`sim_load_image`、`debug_session_start`、`debug_session_stop`、`debug_status`、`debug_continue`、`debug_pause`、`debug_step_over`、`debug_step_into`、`debug_step_out`、`debug_step_instruction`、`debug_breakpoint_set`、`debug_breakpoint_clear`、`debug_breakpoint_list`、`debug_read_memory`、`debug_write_memory`、`debug_stacks`、`debug_variables`、`debug_read_registers`、`debug_disassemble`、`debug_evaluate`、`decode_hardfault`、`trace_waveform`、`profile_hotspots`、`rtos_tasks`、`decode_trace`、`run_command`、`run_external_tool`、`lsp_session_start`、`lsp_session_end`、`lsp_hover`、`lsp_definition`、`lsp_references`、`lsp_completion`。每个工具的参数 schema 通过 `tools/list` 请求获取完整信息。

其中两个为**嵌入式故障/波形分析**工具：`decode_hardfault` 把 ARM Cortex-M 故障快照（CFSR/HFSR/MMFAR/BFAR/栈帧）解码为可读的故障报告（含故障类型、强制升级、字段逐位解释、摘要、EXC_RETURN 解释）；`trace_waveform` 把已解码的 SWV/ITM 包流（Stimulus/Dwt/Exception/Timestamp）构建为可绘制的通道（`stimN`/`dwt_pc`/`excN`/`ts`），未指定通道时自动探测。二者底层为纯函数（`ns_debug::fault` / `ns_debug::waveform`），无 UI/硬件依赖，可直接供 LLM 分析现场故障与波形。

`decode_trace` 把原始 SWO/ITM 字节解码为包（输出与 `trace_waveform` 输入同构，可串联）；`profile_hotspots` 把 DWT PC 采样按符号表聚合成热点；`rtos_tasks` 从 RAM 镜像遍历真实 FreeRTOS 链表枚举任务；`run_command` 在项目目录执行 shell 命令并返回 stdout/stderr/退出码——这是“大模型可操作软件”的底层原语（等价于 IDE 的 Agent/外部工具面板）。

断点/观察点编辑也经 MCP 完整暴露：`update_breakpoint` 一次调用设置条件、命中次数、日志消息、硬件/软件类型或启用状态（随 `.ns-ide/breakpoints.json` 持久化）；`add_watchpoint`/`remove_watchpoint`/`set_watchpoint_enabled` 管理数据观察点（读/写/读写访问类型）。

外部工具（`.ns-ide/tools.json`）经 MCP 管理：`list_external_tools` 列出、`add_external_tool` 新增/替换（`id` 缺省由标题 slug 化并去重）、`remove_external_tool` 删除——与 `run_command` 一起构成完整的自定义命令“配置 + 运行”闭环。

源码文本操作也经 MCP 暴露：`read_file_lines` 按 1 基行号高效读取文件的行区间（`start`/`count`，越界自动夹紧并返回 `total_lines`/`truncated`），适合大文件分块读取；`replace_in_files` 跨项目文本替换（`from`/`to` + 可选 `case_sensitive`/`whole_word`/`path` 过滤/`dry_run`，跳过 .git/build/target/node_modules 与二进制/超大文件，返回逐文件替换计数与总数），`dry_run` 先预览不落盘；`diff_text` 计算两个文本块的统一行 diff（LCS，3 行上下文，返回 `added`/`removed`/`hunks`/`diff` 行数组）。三者让 LLM 能对大模型生成的代码做可预览的批量改写与前后对比。

命令执行与会话也可经 MCP 驱动：`run_external_tool` 按 `id` 运行 `.ns-ide/tools.json` 中已配置的外部工具（自动使用其 `cwd`，支持 `args` 追加与 `timeout_ms`），与 `list/add/remove_external_tool` 构成完整闭环；`run_command` 新增可选 `timeout_ms`（超时后终止子进程并报错，避免挂起的构建/脚本无限阻塞 MCP 调用）；`get_session`/`save_session` 读写 IDE 会话（`.ns-ide/session.json` 的打开标签、活动索引、每文件光标/滚动/缩放/缩进/软换行与分屏）。

LSP 语言服务也可经 MCP 驱动：`lsp_session_start` 启动（或复用）某语言的语言服务器会话（C/C++→clangd、Rust→rust-analyzer、Zig→zls、ZR→`zr lsp`，按 `language` 显式指定或回退到项目主语言/文件扩展名），`lsp_session_end` 关闭会话；`lsp_hover` 取类型/签名悬停，`lsp_definition` 跳转定义，`lsp_references` 查找引用，`lsp_completion` 请求补全——均以 1 基 `line`/`column` 定位并自动 `didOpen`/`didChange` 同步文件内容，超时（`timeout_ms`，默认 10s）返回错误而非阻塞。会话句柄在 MCP 调用间保活，同一语言复用同一服务器进程。

其中 19 个为**有状态调试会话（DAP/lldb-dap）工具**：会话启动/停止/状态、继续/暂停/步过/步入/步出/单指令步进、断点增删查（含条件/命中次数/日志/硬件）、内存读/写、调用栈、变量与寄存器、反汇编与表达式求值。会话句柄在 MCP 调用间保持存活，真实 `lldb-dap` 端到端用例覆盖启动、命中断点、栈/变量/寄存器/内存/反汇编/求值、步过、断点清空与停止。

注意：`debug_write_memory` 与 `debug_step_instruction` 依赖 DAP 适配器能力。本机 `lldb-dap` 对写内存与单指令步进会直接退出，工具保留，可接入支持这些能力的 DAP 适配器（如嵌入式 GDB/LLDB 会话）。

详见 [AGENTS.md](AGENTS.md) §5.1 P52。

## 环境备注

- `.cargo/config.toml` 配置了 `rsproxy.cn` 镜像；若网络可达 crates.io，可删除该文件恢复官方源。
- GPUI 以 `runtime_shaders` feature 启用：在本机缺少 `xcrun metal`（Metal 着色器编译器）时，着色器在运行时编译而非构建期。
- 已知上游 `future-incompat` note：`block v0.1.6`、`proc-macro-error2 v2.0.1`（均经 GPUI 的 macOS 依赖引入），本地无法消除，不影响功能。

## 工程纪律

修改本仓库前请先阅读 [AGENTS.md](AGENTS.md) §1.2：

1. 每次修改必须 `cargo check --workspace` 0 警告 0 错误、测试全绿；
2. 功能必须完整实现，禁止假实现/占位/残缺路径；
3. 提交前在本机运行验证关键路径；
4. 文档与实现保持同步。
