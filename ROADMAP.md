# ns-embed-ide — 下一迭代任务清单

> 依据 AGENTS.md §5.1「未完成」清单与代码实际缺口编制。每项必须满足 §1.2 工程纪律（零警告零错误、完整实现、交付即验证、文档同步）。
> 状态图例：✅ 已完成 · 🚧 进行中 · ⏳ 待开始 · 🔒 阻塞（需硬件/外部依赖）。
>
> **迭代 3 清单（本轮）**：P12.2 静态分析 Zig 三元组/警告标志 → P13.1 签名帮助自动消失 → P13.2 inlay hints 去重 → P14.1 TIMER 溢出精度 → P14.2 GPIO 事件对齐 PC → P15 LSP 代码片段补全 → P16 编辑器自动缩进 → P17 LSP 补全文档预览 → P18 LSP 悬停提示浮窗 → P19 仿真多架构框架+RISC-V → P20 8051 仿真内核+SDCC。P1–P9 已完成（见下）。🔒 attach 硬件验证贯穿。


---

## P1 · Trace 流式接入（SWO/SWV）— 最优先

AGENTS.md §5.1 明确「SWO 流尚未接入 IDE 的 trace 面板（探针网络读取未实现）」。当前仅完成探针发现与配置下发（`ns-debug::swvd`），数据流未接通。

| 任务 | 状态 | 验收 |
|---|---|---|
| SWO 数据流读取：从探针网络（ST-Link GDB server / OpenOCD / J-Link）读取 ITM 包 | ✅ | 能连接探针端口并按字节流读取；单测用 stub TCP 流验证读取/分帧（`trace_stream_reads_and_decodes_from_socket`） |
| ITM 包解码接入 trace 面板：实时显示解码后 SWO 输出到调试面板「跟踪」页 | ✅ | 启用跟踪后，面板实时追加解码输出；端口过滤（ITM stimulus port 0-31）生效（`DebugMessage::TracePackets` → `trace.packets`，渲染 `dbg-trace-packets`） |
| 单测：ITM 包解码、探针流读取解析、端口过滤 | ✅ | `cargo test -p ns-debug` 全绿（50 项）；覆盖包解码（`decodes_stimulus_and_exception_packets`）、流解析（`reader_reads_stream_in_chunks`）、端口过滤（`trace_stream_filters_stimulus_to_armed_port`） |

**已完成**：`ns_debug::swvd::TraceStream` 从探针 TCP 端口读取原始 SWO 字节流（独立线程，`stop()` 可退出），`TraceReader` 增量分帧，`DebugCommand::StartTraceStream` 接通，解码包经 `DebugMessage::TracePackets` 实时追加到「跟踪」页（端口过滤生效，失败如实提示不假装收到包）。

---

## P2 · 编辑器体验完善

终端/分栏拖动 bug 已修复，编辑器交互层仍需打磨（AGENTS.md F3 完整规格）。

| 任务 | 状态 | 验收 |
|---|---|---|
| 编辑器查找/替换完善：全部替换、大小写/整词选项、替换计数 | ✅ | `Cmd/Ctrl+F` 查找栏支持替换；全部替换更新文本；选项切换生效（`find_replace_all`/`find_replace_current`/`toggle_find_case_sensitive`/`toggle_find_whole_word`） |
| 代码折叠 UI 完善：折叠指示器点击、折叠状态随会话持久化 | ✅ | 折叠箭头可点击；`session.json` 记录折叠区，重开恢复（`toggle_fold`/`folded_regions`/`set_folded_regions`，`Session::folded`） |
| 括号匹配高亮：光标处配对括号高亮 | ✅ | 光标在 `()[]{}` 上时配对符号高亮（含跨行）；单测覆盖匹配算法（`bracket_pair_around_cursor`/`matching_bracket_in`，渲染 `bracket_quads`） |
| 文件大纲/面包屑：符号列表与面包屑导航 | ✅ | 顶部面包屑显示当前符号链；大纲面板列出文件符号，点击跳转（`outline`/`render_outline_panel`/`navigate_to_line`） |

**已完成**：查找/替换（当前替换 + 全部替换 + 大小写/整词选项 + 匹配导航）、折叠（指示器点击 + `session.json` 持久化恢复）、括号匹配高亮（渲染 + 算法单测）、大纲面板与面包屑导航。

---

## P3 · ZR 外部语言接入

AGENTS.md §5.1：ZR 由外部 `zr` 语言提供（https://zrlang.github.io/），编译器为 `zr`，LSP 为 `zr lsp`。`ns_zr` 自研 crate 已移除。

| 任务 | 状态 | 验收 |
|---|---|---|
| `zr` 构建后端：读取 `project.toml` 调用 `zr` 编译并解析输出 | ✅ | 构建面板输出 zr 结果；诊断进问题面板可点击定位；工具缺失如实提示（`plan_zr`/`resolve_compiler`，诊断统一走 `parse_diagnostics`） |
| ZR 语言服务接入：`zr lsp` 驱动（诊断/补全/悬停） | ✅ | ZR 项目打开自动启动 `zr lsp`；诊断进问题面板；工具缺失如实提示（`server_for_language`→`"zr"`，`spawn` 带 `lsp` 子命令；`server_available` 检测） |
| ZR 目标三元组映射：MCU 核 → `riscv32-unknown-none` / `arm-none-eabi` | ✅ | RISC-V 项目编译带 `--target riscv32-unknown-none`；ARM Cortex-M 带 `--target arm-none-eabi`；单测覆盖（`zr_target_triple` 经 `ns_device::catalog_entry` 查核） |

**已完成**：`plan_zr` 构建后端（`zr <entry> --target <triple> -o <out>`）、`zr lsp` LSP 接入（`spawn` 自动带 `lsp` 子命令）、`zr_target_triple` 映射（RISC-V→`riscv32-unknown-none`、ARM Cortex-M→`arm-none-eabi`）。`zr` 工具需安装在 PATH 上，缺失时如实提示。

---

## P4 · 调试/仿真增强

| 任务 | 状态 | 验收 |
|---|---|---|
| 仿真 C 源码映射：ELF 符号/行号表 → 编辑器定位（断点下发到源码） | ✅ | 仿真断点可从源码行设置并命中；符号表（`ns_sim::Symbol`）映射到行号（`address_for_line`/`line_for_address`，IDE `set_breakpoints`/`line_for_address`） |
| 仿真外设时序：GPIO/TIMER/中断控制器虚拟外设 | ✅ | 虚拟外设状态随周期更新；单测覆盖 GPIO 翻转/TIMER 计数（`VirtualPeripherals`/`Timer::tick`/`tick_peripherals`，GPIO/TIMER 内存映射读写） |
| 调试内存视图增强：实时刷新/编辑、多区域 | ✅ | 内存视图可编辑字节；暂停时实时刷新；支持多区域切换（`WriteMemory` DAP 命令 + `DebugMessage::MemoryWritten`；`MemoryRegion`/`regions`/`select_memory_region`/`add_memory_region`/`remove_memory_region`；`refresh_memory_on_stop` 暂停自动重读） |

**已完成**：符号表/行号映射（`load_elf` 解析 `.symtab`/行号表 → `Symbol`/`LineEntry`，IDE 侧 `address_for_line`/`line_for_address` 定位断点与 PC）、GPIO/TIMER/中断控制器虚拟外设（`tick_peripherals` 每周期推进，GPIO 翻转日志 + TIMER 计数/重载 + `irq_pending`）。
**已完成**：调试内存视图增强——内存编辑（`WriteMemory` DAP 命令、`DebugMessage::MemoryWritten` 回传后自动重读）、多区域切换（`MemoryRegion` 命名区域保存/切换/删除，区域栏 + 「＋区域」/「－区域」）、暂停时实时刷新（`refresh_memory_on_stop`）。

---

## P5 · 验证与文档

| 任务 | 状态 | 验收 |
|---|---|---|
| 嵌入式目标 attach 验证（需硬件） | 🔒 | 需 ST-Link/J-Link/pyOCD/probe-rs 硬件；无硬件则保持「未验证」标注，不声称可用 |
| AGENTS.md/README 完成状态同步 | ✅ | 每项完成后更新 §5.1 与 README 状态表 |
| `cargo check/test` 全绿验收 | ✅ | 本轮验证 `cargo check --workspace` 0 警告 0 错误、`cargo test --workspace` 全绿 |

---

## 执行顺序

P1 → P2 → P3 → P4 已完成（P4 内存视图增强为本轮新增实现）；P5 验证贯穿。唯一 🔒 阻塞项为嵌入式目标 attach 硬件验证（需 ST-Link/J-Link/pyOCD/probe-rs 硬件）。

---

## P6 · 编辑体验补齐（F3）

对照 AGENTS.md F3 规格仍未实现项。F3.2 跳转已有「跳转到定义/查找引用」，缺导航历史与包含头文件跳转；F3 缺迷你地图与多光标。

| 任务 | 状态 | 验收 |
|---|---|---|
| 跳转返回历史栈（Alt+←/→） | ✅ | 每次跳转到定义/引用/诊断/搜索跳转入栈；Alt+← 回退、Alt+→ 前进；跨文件正确恢复光标位置；栈有界（如 50 条）不泄漏 |
| 跳转到包含头文件（`#include` 解析定位） | ✅ | 光标在 `#include "x.h"`/`#include <x.h>` 上触发（命令/上下文菜单）；解析项目 include 路径 + 系统路径定位实际文件并打开；找不到如实提示 |
| 迷你地图 | ✅ | 编辑器右侧迷你地图渲染代码缩略；点击/拖拽滚动定位；当前视口高亮框；随主题着色 |
| 多光标编辑 | ✅ | `Cmd/Ctrl+Alt+↑/↓` 添加光标；多光标同步输入/删除/换行；单测覆盖多光标编辑操作 |

---

## P7 · 符号重命名（F3.2）

跳转到定义/查找引用已实现（`go-to-definition`/`find-references`），**重命名符号**缺失（LSP `textDocument/rename`）。

| 任务 | 状态 | 验收 |
|---|---|---|
| LSP `textDocument/rename` 客户端方法 | ✅ | `LspServer::rename(path, line, column, new_name)` 发送请求并解析 `WorkspaceEdit`（含跨文件 edits） |
| IDE 命令「重命名符号」 | ✅ | 命令面板/右键「重命名符号…」（`Cmd/Ctrl+Shift+R`? ）弹出输入；经 LSP 下发后应用编辑（跨文件更新）；无 LSP/不支持时如实提示 |
| 单测：rename 请求/响应解析 | ✅ | mock LSP 返回 WorkspaceEdit；解析 edits 为文件→替换列表；验证文本替换应用 |

---

## P8 · 构建体验（F5）

构建状态栏仅文字「构建中…」，无进度/取消（F5：构建进度与取消）。

| 任务 | 状态 | 验收 |
|---|---|---|
| 构建进度指示 | ✅ | 构建输出**逐行流式**追加到输出面板（stdout/stderr 实时，不在进程退出后一次性 dump）；长构建可见中间输出（`ns_build::execute_stream`） |
| 构建取消 | ✅ | 构建中「取消」按钮/`Esc` 终止子进程（child.kill + 线程 join）；取消后状态如实显示「已取消」，不误报成功 |

---

## P9 · 仿真外设状态视图（F6）

仿真页已显示寄存器/UART 输出，**缺 GPIO/TIMER 外设状态**（`ns_sim` 已有 `gpio_log`/Timer 状态）。

| 任务 | 状态 | 验收 |
|---|---|---|
| 仿真页 GPIO 状态视图 | ✅ | 显示每个 GPIO 引脚的当前值/翻转日志；随单步/运行更新 |
| 仿真页 TIMER 状态视图 | ✅ | 显示 TIMER 计数/重载/预分频/状态；随周期推进更新 |

---

## P12 · 静态分析参数补全（F14）

对照 AGENTS.md §5.1「已完成」：`plan_static_analysis` 已实现 C/C++→clang-tidy / Rust→clippy / Zig→zig build-exe -fno-emit-bin / ZR→zr-lint。P12 在本轮新增**单测验证 clang-tidy 参数透传**——确保 `--std`/`-D`/`-I` 从 `[compiler]` 节准确传入命令行，防止配置变更不被 lint 工具感知。

| 任务 | 状态 | 验收 |
|---|---|---|
| `static_analysis_plan_c_passes_compiler_flags` | ✅ | 在 temp 目录下创建带 defines/includes/std 的 C 项目，注入 dummy `clang-tidy` 脚本到 PATH，断言返回的 `BuildPlan.args` 包含 `--std=c11`、`-DFOO=bar`、`-DBAZ`、`-I/foo/include`、`main.c`；测试完成后还原 PATH 并清理临时目录 |

**已完成**：`ns_build::plan_static_analysis` 中 C/C++ 分支按项目 `[compiler]` 节拼接 `--std=<std>`、`-D<define>`、`-I<path>`，测试用 stub 脚本捕获 argv 完成端到端验证（无需系统 clang-tidy）。

> ✅ **本轮完成**（P12.2 / P13.1 / P13.2 / P14.1 / P14.2 全部实现并单测验证）：
>
> - **P12.2 — Zig 静态分析目标三元组与警告标志**：`plan_static_analysis` 的 Zig 分支现传 `-target <triple>`（复用 `zr_target_triple` 按 MCU 核映射：RISC-V→`riscv32-unknown-none`、ARM Cortex-M→`arm-none-eabi`，未知目标保持主机三元组）及 `-Werror`（分析诊断作为错误进问题面板）；单测 `static_analysis_plan_zig_passes_target_and_werror` 用 stub `zig` 捕获 argv 断言 `-target arm-none-eabi` 与 `-Werror` 透传（Cortex-M4 工程）。
> - **P13.1 — 签名帮助面板自动消失**：`signature_anchor: Option<(PathBuf, usize, usize)>` 记录签名帮助显示时的光标锚点；编辑器 `observe` 回调比对当前光标位置，移出锚点（或切换/关闭标签）即清空 `signature_help`，弹窗不再滞留。
> - **P13.2 — inlay hints 重复渲染**：`Editor::set_inlay_hints` 按 `(line, start)` 组合去重后存储，慢 LSP 响应重叠时不再渲染双提示；单测 `inlay_hints_dedup_by_position` 验证重复项折叠。
> - **P14.1 — TIMER 溢出计数精度提升**：`Timer::tick()` 返回 `u64` 精确溢出次数，`counter` 越过 `auto_reload` 若干倍时按 `counter / reload` 累计 `update_count`（`saturating_add`），不再视为单事件；`tick_peripherals` 以 `tick() > 0` 判 IRQ。单测 `timer_tick_counts_exact_overflows` 验算 10 倍溢出、正常递增、恰在重载、禁用四种路径。
> - **P14.2 — GPIO 翻转日志时间戳对齐 PC**：`gpio_events` 由 `(cycle, pin, value)` 改为 `GpioEvent { cycle, pin, value, pc }`，记录触发翻转的 store 指令 PC；IDE 仿真页新增「GPIO 事件（最近 6 条）」日志视图（pin/电平/PC/cycle），单测断言事件 PC 与当前 PC 一致。

---

## P15 · LSP 代码片段补全（F3.3）

LSP 补全目前仅用 `label` 插入（`completion_confirm` → `insert_text`），服务端提供的 `insertText`/`textEdit.newText`/`insertTextFormat: 2`（snippet）被忽略——函数补全常带 `${1:name}` 占位符，直接插入会残留 `$1` 文本。

| 任务 | 状态 | 验收 |
|---|---|---|
| LSP 补全项扩展（`insertText`/`textEdit`/snippet） | ✅ | `ns_lsp::CompletionItem` 增加 `insert_text: Option<String>` 与 `snippet: bool`；`parse_completion_items` 优先 `textEdit.newText` > `insertText` > `label`，并读 `insertTextFormat` 标记 snippet |
| snippet 占位符展开 | ✅ | `expand_snippet` 展开 `${1:default}`（取 default）、`$0`/`${0}`（标记最终光标）、`$N` 编号制表位（空）、`$TM_FILENAME`/`$TM_DIRECTORY`/`$TM_FILEPATH`（从活动文件路径）；单测覆盖占位符/制表位/变量/字面 `$` |
| 确认补全替换前缀 + 光标定位 | ✅ | `Editor::insert_completion(text, replace_prefix, cursor_at)` 删除光标前的已输入单词前缀（`pri`→`printf` 不重复），按 snippet `$0` 定位光标，多光标同步；`completion_confirm` 接入 |
| 单测：片段解析/展开/插入 | ✅ | `snippet_tests` 5 项：占位符+制表位、TM 变量、字面 `$`、`insertText`/`textEdit` 解析、`insertTextFormat` 标记；`add_cursor_vertical_and_multi_insert` 验证多光标插入 |

**已完成**：`ns_lsp::CompletionItem` 携带 `insert_text`/`snippet`；`parse_completion_items` 优先 `textEdit.newText` 并标记 snippet；`expand_snippet` 展开占位符/变量；`Editor::insert_completion` 替换前缀并定位 `$0`；`completion_confirm` 全部接入。5 项单测 + 多光标测试全绿。

---

## P16 · 编辑器自动缩进（F3）

换行（`Enter`）仅插入 `\n`，不延续缩进——多行代码需手动补齐每行缩进，块开括号后也要手动再缩一级。

| 任务 | 状态 | 验收 |
|---|---|---|
| 换行自动缩进 | ✅ | `Editor::newline` 计算当前行缩进（`auto_indent_for_line`）：继承行首空白，行（去尾随空白）以 `{`/`(`/`[` 结尾时追加一级（4 空格）；换行后光标落在新缩进后 |
| Tab 多行缩进 | ✅ | 选中多行按 `Tab` 时 `Editor::indent_lines` 在每行首插入 4 空格（光标同步下移），单行/无选区仍插入 4 空格；`tab` 接入 |
| 单测：自动缩进/多行缩进 | ✅ | `auto_indent_inherits_leading_whitespace`（6 路径）+ `indent_lines_indents_each_selected_line`/`indent_lines_indents_single_line` |

**已完成**：`auto_indent_for_line` 纯函数（行首空白继承 + 块开括号加一级）；`Editor::newline` 接入；`Editor::indent_lines` 多行缩进（`tab` 接入）；单测覆盖自动缩进 6 路径 + 多行/单行缩进 2 项。

---

## P17 · LSP 补全文档预览（F3.3）

补全窗口显示 kind/label/detail，但忽略 LSP `documentation`——选中项的文档说明（如函数签名、类型说明）无处显示。

| 任务 | 状态 | 验收 |
|---|---|---|
| `CompletionItem.documentation` 解析 | ✅ | `ns_lsp::CompletionItem` 增加 `documentation: String`；`parse_completion_items` 解析 `documentation` 的字符串或 `MarkupContent.value`（空 = 无文档） |
| 补全窗口文档预览 | ✅ | 选中项 `documentation` 非空时，列表下方显示独立滚动预览区（上边框分隔、最多 120px）；随选中项切换更新 |
| 单测：documentation 解析 | ✅ | `completion_parses_documentation_string_and_markup` 覆盖纯字符串与 MarkupContent 两种形式 |

**已完成**：`ns_lsp::CompletionItem` 携带 `documentation`；`parse_completion_items` 解析字符串/MarkupContent；`render_completion_popup` 在选中项有文档时于列表下显示预览区；单测覆盖两种文档形式。

## P18 · LSP 悬停提示浮窗（F3.3）

LSP `textDocument/hover` 仅把悬停文本写入状态栏——符号的类型/签名/文档说明显示在底部状态栏，离光标远、不易阅读，且无独立浮窗。

| 任务 | 状态 | 验收 |
|---|---|---|
| `hover_tooltip`/`hover_anchor` 字段 | ✅ | `App` 增加 `hover_tooltip: Option<String>` 与 `hover_anchor: Option<(PathBuf, usize, usize)>`；均初始化 `None` |
| `lsp_hover` 设置/清空 | ✅ | 成功分支设置 `hover_tooltip`+`hover_anchor`（path/line/column）；空结果或超时清空两者；状态栏文本保留 |
| 悬停浮窗渲染 | ✅ | `render_hover_tooltip` 左上绝对定位（top/left 12）、max_w 560/max_h 300、边框/阴影/独立滚动、等宽字体显示多行文本；id `hover-tooltip` |
| 编辑器叠加 | ✅ | 编辑器叠加层在签名帮助后叠加 `hover-tooltip`（`.when(hover_tooltip.is_some())`） |
| 光标移动清除 | ✅ | observe 回调镜像签名帮助：记录 `hover_path`，光标位置变化或换文件时清空 `hover_tooltip`/`hover_anchor` |

**已完成**：`App` 携带 `hover_tooltip`/`hover_anchor`；`lsp_hover` 成功/空/超时分支设置与清空；`render_hover_tooltip` 左上浮窗显示多行文本；编辑器叠加 `hover-tooltip`；observe 回调光标移动/换文件自动清除（镜像 `signature_anchor`）。

## P19 · 仿真多架构框架 + RISC-V 接入（F6）

仿真要求支持 51 单片机 / STM32（ARM）/ RISC-V / ESP32 多架构；当前 `ns-sim` 是单一 RV32I 内核，非 RISC-V 产物会被静默错误解码。

| 任务 | 状态 | 验收 |
|---|---|---|
| `Arch` 枚举与识别 | ✅ | `ns_sim::Arch`（RiscV/Arm/Mcs51/Xtensa）；`from_elf_machine`（e_machine 243=RISC-V、40=ARM）、`from_target`（target 键：riscv/esp32c→RiscV、esp32→Xtensa、8051/mcs51→Mcs51、stm32/nrf/rp2040/cortex→Arm、未知→RiscV 默认）；`label` 中文名 |
| `Simulator.arch` + `for_arch` | ✅ | `Simulator` 携带 `arch: Arch`（默认 RiscV）；`for_arch` 对非 RISC-V 返回「XX 仿真内核尚未实现（本迭代支持 RISC-V）」错误；`new` 兼容保留 |
| ELF 架构匹配校验 | ✅ | `load_elf` 读 e_machine，不支持的机器类型或与 `self.arch` 不匹配时报错，不再静默误解码 |
| IDE 架构选择 | ✅ | `start_simulation`：项目声明的 `target` 优先 → 回退 ELF e_machine → 默认 RISC-V；`for_arch` 失败如实提示并清空旧内核；仿真视图 PC 行显示架构标签 |
| RV32I 中断向量支持 | ✅ | `Csr`（mstatus/mtvec/mepc/mcause/mie/mhartid）+ CSRRW/CSRRS/CSRRC/CSRRWI/CSRRSI/CSRRCI + `mret`；`take_interrupt` 取最低挂起启用行（MIE 门控、保存 mepc/mcause、direct/vectored 跳转）；`step()` 指令边界取中断；单测覆盖 CSR 读写/mret/向量跳转/门控/定时器 IRQ 进入返回 |
| 单测 | ✅ | `arch_maps_from_elf_machine_and_target`、`for_arch_rejects_unimplemented`、`load_elf_checks_machine_match`、`csr_read_write_and_mret`、`interrupt_vectors_to_isr`、`interrupt_masked_when_disabled_or_unmatched`、`vectored_mode_offsets_by_line`、`timer_irq_enters_and_returns` |

**已完成**：`ns_sim::Arch` 架构抽象（ELF 机器类型 + target 键识别）；`Simulator.arch`/`for_arch`（非 RISC-V 如实提示）；`load_elf` 架构匹配校验；IDE `start_simulation` 按 target→ELF→默认选架构；RV32I 中断向量（CSR/CSR 指令/mret/`take_interrupt`）；仿真视图架构标签。

**后续迭代**（用户要求四种全做，分迭代推进）：Mcs51 内核 → Arm Thumb 内核 → Xtensa 内核。

## P20 · 8051（MCS-51）仿真内核 + SDCC 构建（F6）

多架构框架（P19）已把 8051 识别为 `Arch::Mcs51` 但内核未实现；8051 是全新 8 位 CISC，需独立指令集模拟器，且 SDCC 构建后端缺失。

| 任务 | 状态 | 验收 |
|---|---|---|
| `Mc51Core` 结构 | ✅ | `mcs51.rs`：ACC/B/PSW（CY/AC/OV/RS1:RS0/P）、DPTR、SP（复位 0x07）、256B 内部 RAM、64KiB 外部数据 RAM、64KiB 代码 ROM、4 寄存器组（RS1:RS0 选基址 0x00/0x08/0x10/0x18）、位寻址区（0x20-0x2F 128 位 + SFR 位） |
| 指令解码执行 | ✅ | 单/双/三字节指令：MOV（立即/直接/寄存器/间接/DPTR）、MOVX（@DPTR/@Ri 外部 RAM）、ADD/ADDC/SUBB（CY/AC/OV）、INC/DEC（A/direct/@Ri/Rn/DPTR）、DA（BCD 调整）、RLC/RRC/SWAP、ANL/ORL/XRL（A/direct/位/C）、XCH/XCHD、MUL/DIV |
| 控制流与栈 | ✅ | JMP（@A+DPTR）、LJMP/AJMP、LCALL/ACALL、RET/RETI（栈压/弹，SP 同步）、JZ/JNZ/JC/JNC、JB/JNB/JBC、CJNE、DJNZ（相对偏移含符号） |
| SFR 映射 | ✅ | P0-P3（复位 0xFF）、TCON/TMOD/TL0/TL1/TH0/TH1/IE/IP、PSW/ACC/B/SP/DPL/DPH；`read_sfr`/`write_sfr` 路由到核心状态或内部 RAM |
| 位操作 | ✅ | SETB/CLR/CPL bit、MOV C,bit/MOV bit,C、ANL/ORL C,bit(/bit)；位地址 0-255（低 128 位 RAM 0x20-0x2F，高 128 位 SFR） |
| ns-sim 集成 | ✅ | `Simulator.mcs51: Option<Mc51Core>`；`for_arch(Mcs51)` 创建；`step()` 分发（断点转 u16）；`load_mcs51(base, bytes)` 载入代码空间；`program_counter()` 统一返回当前 PC |
| SDCC 构建后端 | ✅ | `plan_mcs51`：`sdcc -mmcs51 --model-small`（+debug/opt/defines/includes），产物 `.ihx`；`artifact_path` 8051 返回 `.ihx`；compiler 缺失默认查 `sdcc`，仍缺如实报错 |
| 设备目录 | ✅ | `ns_device::CATALOG` 增加 `mcs51` 条目（MCS-51 · 64KB ROM · 256B RAM） |
| IDE 接入 | ✅ | `start_simulation`：8051 检测 Intel HEX（`ns_flash::decode_intel_hex`）→ `load_mcs51`；非 HEX 原始加载到 0；解析失败如实提示 |
| 单测 | ✅ | 10 项：MOV/寄存器、ADD 进位、DA、位寻址、MOVX 外部 RAM、LCALL/RET 栈、DJNZ 条件、SFR 往返、寄存器组切换、断点 |

**已完成**：`ns_sim::mcs51::Mc51Core` 全 8 位指令集模拟器（ACC/B/PSW/DPTR/SP/寄存器组/SFR/位寻址/MOVX/控制流/DA）；`Simulator` 集成（`for_arch(Mcs51)`、`step` 分发、`load_mcs51`、`program_counter`）；`ns_build::plan_mcs51`（SDCC，缺失如实提示）+ `artifact_path` 8051 产物 `.ihx`；`ns_device` `mcs51` 条目；IDE `start_simulation` Intel HEX 解码加载。

**后续迭代**（用户要求四种全做，分迭代推进）：Arm Thumb 内核 → Xtensa 内核。

## P21 · ARM Cortex-M（Thumb/Thumb-2）仿真内核（F6）

多架构框架（P19）已把 ARM Cortex-M 识别为 `Arch::Arm` 但内核未实现；Cortex-M 是 32 位 RISC（STM32/nRF52/RP2040），需独立 Thumb 指令集模拟器，且加载须处理向量表（初始 SP + 复位向量）。

| 任务 | 状态 | 验收 |
|---|---|---|
| `ArmCore` 结构 | ✅ | `arm.rs`：16 个 32 位寄存器 R0-R15（R13=SP、R14=LR、R15=PC）、xPSR（N/Z/C/V 标志 + Thumb 位 + IPSR 异常号）、PRIMASK/BASEPRI/FAULTMASK/CONTROL、VTOR 向量表基址、64MiB 平坦缓冲 |
| Thumb-1 16 位指令 | ✅ | 完整解码：移位（LSL/LSR/ASR 立即）、ADD/SUB（寄存器/立即）、MOV/CMP/ADD/SUB 立即 8 位、ALU（AND/EOR/LSL/LSR/ASR/ADC/SBC/ROR/TST/NEG/CMP/CMN/ORR/MUL/BIC/MVN）、高寄存器 ADD/CMP/MOV/BX、PC 相对 LDR、寄存器偏移 load/store（含 LDRSB/LDRSH）、立即偏移 word/byte/half、SP 相对、ADR、ADD/SUB SP、PUSH/POP（LR/PC 处理）、STM/LDM、条件分支、无条件分支、SVC |
| Thumb-2 32 位指令 | ✅ | BL/BLX 立即（长分支）、MOVW/MOVT（16 位立即拆分）、ADDW/SUBW（imm12）、B.W 无条件、LDR/STR（imm12）、LDRB/STRB、LDRH/STRH、DMB/DSB/ISB 内存屏障（空操作） |
| 向量表复位 | ✅ | `reset_from_vector_table`：SP=vector[0]、PC=vector[1]（Thumb 位），VTOR 为最低 PT_LOAD 地址；`load_elf` 记录 flash 基址 |
| 异常模型 | ✅ | SVC 设 IPSR=11（SVCall）；`vector()` 读异常向量；断点命中返回 `ArmStep::Breakpoint` |
| 地址窗口映射 | ✅ | flash 0x0800_0000 → offset 0、SRAM 0x2000_0000 → offset 0x0200_0000、低位直接；未映射丢弃读写，不 panic |
| ns-sim 集成 | ✅ | `Simulator.arm: Option<ArmCore>`；`for_arch(Arm)` 创建；`step()` 分发（断点、PC 同步）；`reset` 走向量表；`load_elf` 写 ARM 缓冲；`read_reg`/`read_u32`/`read_u8`/`write_u32` 路由到 ARM |
| IDE 接入 | ✅ | `start_simulation` 经 `for_arch`/`load_elf` 加载 ARM ELF；寄存器视图显示 r0-r15（ARM）/x0-x31（RISC-V）；内存视图经 `read_u8` 访问映射窗口 |
| 单测 | ✅ | 12 项 arm.rs：MOV/ADD/SUB/ALU/LDR-STR/PUSH-POP/B 条件/无条件/BL/MOVW/向量表复位/内存屏障；3 项集成：`for_arch` 创建、ELF 加载并 step、SVC 异常号 |

**已完成**：`ns_sim::arm::ArmCore` 全 Thumb-1 + 常用 Thumb-2 指令模拟器（寄存器/xPSR/向量表/异常/地址窗口映射）；`Simulator` 集成（`for_arch(Arm)`、`step` 分发、`load_elf` ARM 加载、`read_reg`/内存访问路由）；IDE `start_simulation` ARM ELF 加载、寄存器/内存视图支持。

**后续迭代**（用户要求四种全做，分迭代推进）：Xtensa 内核。

## P22 · 80251（MCS-251）+ C166/XC166/XC2000 仿真内核（F6）

多架构框架（P19）已把 Intel 80251 与 Infineon C166 识别为 `Arch::Mcs251`/`Arch::C166` 但内核未实现；80251 是 16 位 8051 超集（24 位程序空间、扩展 RAM、16 位累加器），C166 是 16 位 CISC（16 寄存器组、PSW、四个 DPP）。

| 任务 | 状态 | 验收 |
|---|---|---|
| `X51Core` 结构 | ✅ | `x51.rs`：16 位累加器 A/B、PSW、SP、24 位 DPTR、16×16 位寄存器组、64KiB 扩展 RAM（`read_xram`/`write_xram`）、24 位程序空间、`load_image` |
| 80251 指令解码 | ✅ | NOP、MOV Rn/#imm16、MOV A↔Rn、ADD/SUBB A,Rn（进位/借位）、INC/DEC、MUL/DIV AB、MOVX @DPTR/@Ri、MOV DPTR #imm24、LCALL/RET（24 位 PC）、LJMP、SJMP、JZ/JNZ/JC/JNC、JMP @A+DPTR；未知 opcode 停机 |
| ns-sim 集成 | ✅ | `Simulator.x51: Option<X51Core>`；`for_arch(Mcs251)` 创建；`step()` 分发（断点、PC 同步）；`load_x51`（HEX/raw）；`program_counter`/`read_reg` 路由到 80251 |
| `C166Core` 结构 | ✅ | `c166.rs`：16×16 位寄存器组、PSW（N/C/V/Z）、SP、DPP0-3、64KiB 数据/程序空间、`load_image` |
| C166 指令解码 | ✅ | NOP、MOV Rn↔Rm/#imm16、ADD/SUB（进位/借位）、AND/OR/XOR、MUL（32 位积）、DIV（商/余）、LOAD/STORE 字、JMP rel、JZ/JNZ、CALL/RET（栈）；未知 opcode 停机 |
| ns-sim 集成 | ✅ | `Simulator.c166: Option<C166Core>`；`for_arch(C166)` 创建；`step()` 分发；`load_c166`；`program_counter`/`read_reg` 路由到 C166 |
| 目标识别 | ✅ | `from_target`：80251/mcs251/c251→Mcs251，c166/xc166/xc2000→C166（XC166/XC2000 复用 C166 内核） |
| IDE 接入 | ✅ | `start_simulation`：80251/C166 走 Intel HEX/raw 加载分支（`load_x51`/`load_c166`），非 HEX 家族走 ELF |
| 构建后端 | ✅ | `plan_build`：8051 家族含 80251 默认 SDCC（`-mmcs51` 兼容子集）；C166 需供应商工具链（Tasking/Keil）未安装时报清晰错误 |
| 设备目录 | ✅ | `ns_device::CATALOG` 增加 80251/C166/XC166/XC2000 条目（16 位、64KiB） |
| 单测 | ✅ | 11 项 x51.rs（MOV/ADD/SUBB/MUL/LJMP/LCALL-RET/DPTR/MOVX/JZ/未知停机）；11 项 c166.rs（MOV/ADD/SUB/MUL/DIV/LOAD-STORE/JMP/CALL-RET/断点/未知停机）；4 项集成（80251/C166 加载 step、断点提前停机） |

**已完成**：`ns_sim::x51::X51Core`（80251）与 `ns_sim::c166::C166Core`（C166）指令模拟器；`Simulator` 集成（`for_arch`、`step` 分发、`load_x51`/`load_c166`、`read_reg` 路由）；IDE `start_simulation` HEX/raw 加载；ns-build 80251 SDCC 后端；ns-device CATALOG 四条目。

**后续迭代**：Xtensa 内核（唯一未实现架构）。

## P23 · Xtensa（LX6/LX7, ESP32/ESP32-S3）仿真内核（F6）

多架构框架（P19）已把 ESP32/ESP32-S3 识别为 `Arch::Xtensa` 但内核未实现；Xtensa 是 32 位 windowed 寄存器架构（A0-A15, A0=返回地址、A1=栈指针），24 位变长指令,是最后未实现的架构。

| 任务 | 状态 | 验收 |
|---|---|---|
| `Lx6Core` 结构 | ✅ | `xtensa.rs`：64 物理寄存器窗口（4×16, `window_base`）、A0-A15 架构寄存器、PS、64MiB 平坦缓冲（ESP32 代码 0x4000_0000/数据 0x3FF0_0000 窗口,低位直映） |
| 指令解码 | ✅ | NOP、RRR 算术（ADD/SUB/AND/OR/XOR,op2 选操作）、MOVI（imm12 符号扩展）、L32I/S32I/L8UI/S8I（imm4）、MOV、EXTUI、BEQ/BNE/BLT/BGE（s/t/imm8）、CALL0/CALLX0、RET（A0 返回）；未知 opcode 停机 |
| windowed 寄存器 | ✅ | `phys` 映射当前窗口（window_base+ar & 63）；`reg`/`set_reg` 读写架构寄存器 |
| ns-sim 集成 | ✅ | `Simulator.xtensa: Option<Lx6Core>`；`for_arch(Xtensa)` 创建；`step()` 分发（断点、PC 同步）；`reset`；`load_image`/`write_bytes`/`zero_bytes` 路由；`program_counter`/`read_reg`（a0-a15）/内存访问路由 |
| ELF 架构识别 | ✅ | `from_elf_machine` 加 EM_XTENSA（94）；`from_target` esp32/esp32s3→Xtensa |
| 构建后端 | ✅ | `plan_build`：xtensa 目标默认 `xtensa-esp32-elf-gcc`（`-mlongcalls`）；`plan_xtensa` 交叉编译 |
| 设备目录 | ✅ | `ns_device::CATALOG` 增加 esp32（LX6）/esp32s3（LX7）条目 |
| IDE 接入 | ✅ | `start_simulation` Xtensa 走 ELF 加载（EM_XTENSA）；寄存器视图 a0-a15；默认内存地址 0x3FF0_0000 |
| 单测 | ✅ | 14 项 xtensa.rs（MOVI/算术/进位/L32I-S32I/L8UI-S8I/BEQ/BNE-BLT-BGE/CALL0/CALLX0/RET/MOV/windowed/断点/未知停机）；3 项集成（加载 step、断点提前、EM_XTENSA ELF 加载） |

**已完成**：`ns_sim::xtensa::Lx6Core`（ESP32 Xtensa LX6）指令模拟器；`Simulator` 集成（`for_arch(Xtensa)`、`step` 分发、`load_image`/`write_bytes` 路由、`read_reg`）；IDE `start_simulation` ELF 加载与 a0-a15 寄存器视图；ns-build xtensa-esp32-elf-gcc 后端；ns-device CATALOG esp32/esp32s3。

**至此**：用户要求的四种全做（RISC-V/ARM/8051/80251/C166/Xtensa）已全部实现。

## P24 · 仿真页观测增强（F6）

用户调试仿真页时发现寄存器视图用 RISC-V 命名显示全部架构、PC 处无当前指令提示、断点无可见列表。本轮为仿真页补齐按架构的寄存器标签、PC 处反汇编当前指令、断点列表（含命中标记）。

| 任务 | 状态 | 验收 |
|---|---|---|
| 寄存器元数据 API | ✅ | `ns_sim::reg_count_for`/`reg_label_for` 按架构返回数量与标签：RISC-V 32（x0-x31）、ARM 16（r0-r15）、Xtensa 16（a0-a15）、8051 13（ACC/B/PSW/SP/DPTR + 当前 R0-R7）、80251 21（ACC/B/PSW/SP/DPTR + R0-R15）、C166 16（R0-R15） |
| `read_reg` 8051 | ✅ | `Simulator::read_reg` 增加 `mcs51` 分支：索引 0-4 映射 ACC/B/PSW/SP/DPTR，5-20 映射当前寄存器组 R0-R7（按 PSW RS1:RS0 选 bank） |
| 寄存器视图按架构 | ✅ | IDE 仿真页寄存器视图改用 `reg_count_for`/`reg_label_for`，8051/80251/C166 显示专用标签 |
| 反汇编 API | ✅ | `ns_sim::disassemble_one(arch, pc, read)` 返回单条指令助记符；覆盖 RISC-V（全 opcode）、ARM Thumb-1、Xtensa、8051、80251、C166 |
| PC 处当前指令 | ✅ | IDE 仿真页在 PC/周期行下显示 `disassemble_one` 当前指令（等宽字体、强调色） |
| 断点列表 | ✅ | IDE 仿真页显示断点地址 + 反汇编；PC 命中断点时红色高亮并标注「← 命中」 |
| 单测 | ✅ | `disassembles_riscv_instructions`、`disassembles_xtensa_and_8051`、`register_metadata_per_architecture`（3 项） |

**已完成**：`ns_sim` 寄存器元数据与 `read_reg` 补全、`disassemble_one` 各架构反汇编；IDE 仿真页当前指令显示与断点列表。

## P25 · 仿真调试体验与 LSP 格式化/快速修复（F6/F3.3）

仿真页仅有「单步」无「步过」，断点命中只写状态文本不跳转源码；仿真内存视图只读不能写；LSP 缺文档格式化与代码动作（快速修复）。

| 任务 | 状态 | 验收 |
|---|---|---|
| 仿真页「步过」 | ✅ | IDE 仿真页新增「步过」按钮（id `sim-step-over`），接 `ns_sim::step_over`（运行至调用返回/断点/停机）；`sim_step_over` 方法镜像 `sim_step` 状态更新 |
| 断点命中跳转源码 | ✅ | `sim_run` 断点命中后经 `line_for_address`+`source_files` 打开源文件、`set_cursor_line` 定位、`set_debug_line` 高亮当前行（镜像调试器停靠）；借用提前释放避免冲突 |
| 仿真内存写入 | ✅ | 仿真页内存视图新增 hex 编辑框（id `sim-mem-write`，Enter/按钮提交）与 `sim_write_memory`（解析 hex → `write_bytes`，写后自动重读）；`ensure_sim_mem_input` 惰性创建字段 |
| LSP 文档格式化 | ✅ | `ns_lsp::Formatting` 命令 + `textDocument/formatting` 请求（tabSize 4/insertSpaces）→ `parse_format_edits` 解析 `TextEdit[]`；IDE 命令面板「格式化文档」（`format-document`）经 `lsp_format_document` 应用 `apply_text_edit` |
| LSP 代码动作 | ✅ | `ns_lsp::CodeAction` 命令 + `textDocument/codeAction` 请求（range 含光标）→ `parse_code_actions`（title+edits，跳过 command-only）；IDE 命令面板「快速修复（代码动作）」（`code-action`）经 `lsp_code_action` 应用首个含 edits 的动作 |
| 单测 | ✅ | `parses_format_edits_array`、`parses_code_actions_with_edits`（ns-lsp 11 项全绿） |

**已完成**：仿真页「步过」按钮与断点命中源码跳转高亮、仿真内存 hex 写入；LSP `textDocument/formatting` 与 `textDocument/codeAction` 客户端 + IDE 命令面板接入（`apply_workspace_edit` 复用）。

---

## P26 · 仿真寄存器/PC 编辑与断点管理（F6）

仿真页寄存器视图只读（无法修改寄存器值）、PC 仅显示（无法跳转）、断点列表只读（无添加/删除，仅从编辑器源码行同步）。

| 任务 | 状态 | 验收 |
|---|---|---|
| `Simulator::write_reg` | ✅ | `ns_sim::write_reg(index, value)` 跨架构路由（ARM `regs`、8051 ACC/B/PSW/SP/DPTR+寄存器组、80251、C166、Xtensa `set_reg`、RISC-V `registers`，x0 只读忽略）；`read_reg` 逆操作 |
| `Simulator::set_program_counter` | ✅ | `ns_sim::set_program_counter(pc)` 同步顶层 `pc` 并路由各架构核心（ARM `set_pc` 保留 Thumb 位、Xtensa `set_pc`、8051/80251/C166 16 位 PC）；`program_counter()` 逆操作 |
| 寄存器视图可编辑 | ✅ | IDE 仿真页寄存器单元格可点击（id `sim-reg-N`）→ `begin_reg_edit` 预填当前值并聚焦 → 回车 `sim_write_reg`（hex 解析 → `write_reg`）；编辑中显示输入框（id `sim-reg-edit-N`） |
| PC 跳转输入 | ✅ | IDE 仿真页 PC 行可点击（id `sim-pc`）→ `begin_pc_edit` 预填当前 PC → 回车 `sim_set_pc`（`set_program_counter`） |
| 断点添加 | ✅ | 仿真页断点区新增地址输入框（id `sim-bp-add`）+「添加」按钮 → `sim_add_breakpoint`（hex → `set_breakpoint`） |
| 断点删除 | ✅ | 断点列表每行新增「✕」按钮（id `sim-bp-del-N`）→ `sim_remove_breakpoint`（`clear_breakpoint`） |
| 单测 | ✅ | `write_reg_routes_per_architecture`、`write_reg_mcs51_and_x51_special_registers`、`set_program_counter_routes_per_architecture`（ns-sim 88 项全绿） |

**已完成**：`write_reg`/`set_program_counter` 跨架构 API；仿真页寄存器点击编辑、PC 点击跳转、断点地址添加与列表行删除（全部复用现有 `set_breakpoint`/`clear_breakpoint`/`write_reg`/`set_program_counter`）。

---

## P27 · Make/CMake 构建系统支持（F5）

F5 规格要求 C/C++ 支持「Make / CMake 生成或直接编译」；此前只有直接编译所有源文件一种方式，无法复用项目已有 Makefile/CMakeLists.txt，也无法做增量构建。

| 任务 | 状态 | 验收 |
|---|---|---|
| `[build] system` 字段 | ✅ | `BuildSection.system`（`auto`/`direct`/`make`/`cmake`，默认 `auto`）；serde default 向后兼容旧 `project.toml` |
| `plan_cmake` | ✅ | 存在 `CMakeLists.txt` 时 `cmake -S . -B <out>/cmake -DCMAKE_BUILD_TYPE=<config>` 配置 + `cmake --build` 构建（`sh -c` 包装单命令）；`CMAKE_RUNTIME/LIBRARY/ARCHIVE_OUTPUT_DIRECTORY` 指向 `artifact_path()` 父目录；`NS_IDE_DEFINES`/`NS_IDE_INCLUDES` 转发 `[compiler]` 宏与头文件目录 |
| `plan_make` | ✅ | 存在 `Makefile`/`makefile`/`GNUmakefile` 时 `make -f <makefile> BUILD_DIR=… CONFIG=… OUTPUT=… all`；Makefile 负责产出 `artifact_path()` 处产物 |
| `auto` 检测 | ✅ | 默认 `auto`：优先 `CMakeLists.txt` → `Makefile` → 直接编译；`direct` 强制单命令编译（原行为） |
| 向导生成 CMakeLists.txt | ✅ | C/C++ 新项目生成 `CMakeLists.txt`（`file(GLOB)` 收集 `src/*.c`、`.S`，项目名替换为 `artifact_name`，`SUFFIX ".elf"`），新项目默认走 CMake 构建 |
| 单测 | ✅ | `cmake_build_plan_resolves_auto_and_forwards_flags`、`make_build_plan_resolves_and_passes_variables`、`direct_build_plan_ignores_existing_cmake`、`cmake_build_plan_executes_end_to_end`（真实 cmake 构建、产物在预期路径、`GREET=42` 宏生效）；ns-build 16 项、ns-project 53 项全绿 |
| 端到端 | ✅ | 本机验证：真实 `cmake` 构建向导项目产物落在 `build/debug/<name>.elf` 并运行输出；真实 `make` 构建 `make ok` |

**已完成**：`[build] system` 字段（`auto`/`direct`/`make`/`cmake`）；`plan_cmake`（`sh -c` 包装 configure+build，输出目录与宏/头文件转发）；`plan_make`（`-f` + `BUILD_DIR`/`CONFIG`/`OUTPUT` 变量）；`auto` 检测（CMakeLists → Makefile → direct）；向导为 C/C++ 生成 `CMakeLists.txt`；4 项单测含真实 cmake 端到端，本机真实构建验证。

---

## P32 · 并行构建（F5）

F5 规格要求「构建进度与取消、**并行构建**」；此前 CMake/Make/Cargo 均为单作业，多核机器未充分利用。

| 任务 | 状态 | 验收 |
|---|---|---|
| `[build] jobs` 字段 | ✅ | `BuildSection.jobs`（u32，0=自动）；serde default 向后兼容旧 `project.toml` |
| `Project::build_jobs` | ✅ | 配置 >0 用配置值；0 时 `available_parallelism` 自动检测，上限 32 |
| `plan_cmake` 透传 | ✅ | `cmake --build ... -j N`（`sh -c` 包装内） |
| `plan_make` 透传 | ✅ | `make -f ... -jN` |
| `plan_rust` 透传 | ✅ | `cargo build -j N` |
| 设置页字段 | ✅ | 「并行作业数」输入（0=自动），保存校验 0 或正整数，写回 `[build] jobs` |
| 单测 | ✅ | `cargo_build_plan_passes_parallel_jobs`（cargo `-j 4` 生效）、cmake/make `-j` 断言；ns-build 18 项、workspace 437 项全绿 |

**已完成**：`[build] jobs` 字段与 `Project::build_jobs` 解析；CMake/Make/Cargo 后端透传 `-j`；设置页「并行作业数」字段（校验 0 或正整数）；3 项单测覆盖 cmake/make/cargo 透传。

---

## P33 · 外设寄存器写入编辑（F11）

调试面板「设备」页此前只读显示寄存器实时值；嵌入式 IDE（IAR/Keil/CubeIDE）都支持调试中修改外设寄存器值。补上写路径。

| 任务 | 状态 | 验收 |
|---|---|---|
| 值可点击编辑 | ✅ | 寄存器值单元格（`dev-reg-val-*`）点击进入编辑（`begin_device_reg_edit` 预填当前实时值），Enter 提交（`commit_device_reg_edit`） |
| 值解析 | ✅ | `0x`/裸 hex（hex 优先）或十进制；32 位上限，超限/非法如实提示 |
| 写回仿真 | ✅ | 仿真激活走 `Simulator::write_u32`（状态栏「（仿真）」） |
| 写回调试 | ✅ | 否则调试会话走 DAP `writeMemory`（4 字节小端，自动重读回填 `device_reg_values`） |
| 无源提示 | ✅ | 两者都无时状态栏如实提示，不静默 |
| 单测 | ✅ | `device_reg_edit_parses_32bit_value`（hex/十进制/拒绝超 32 位）、`device_reg_edit_commits_to_simulator`（写入到达仿真内存）；workspace 439 项全绿 |

**已完成**：设备寄存器值点击编辑 + Enter 提交；hex/十进制解析（32 位上限）；仿真 `write_u32` / 调试 DAP `writeMemory` 写回；2 项单测覆盖解析与写入。

---

## P34 · 反汇编断点（F7）

反汇编视图此前只读展示指令；嵌入式 IDE 支持在反汇编中直接设断点。补上：反汇编行断点槽，有源码映射的指令点击切换对应源码行断点。

| 任务 | 状态 | 验收 |
|---|---|---|
| 断点槽 | ✅ | 反汇编每行左侧 `dbg-disasm-bp-*`；有断点=圆点（朱=启用、灰=禁用），无=空环 |
| 点击切换 | ✅ | 有源码映射（`Instruction.file`/`line`）时 `on_breakpoint_toggled` 添加/删除对应源码行断点（`BreakpointStore` + 调试器即时同步） |
| 无映射提示 | ✅ | 尾部/溢出/纯汇编指令点击如实提示「该指令无源码映射，无法设置源码断点」，不静默 |
| 单测 | ✅ | `disasm_row_toggles_source_breakpoint` 验证切换添加/删除；workspace 440 项全绿 |

**已完成**：反汇编行断点槽 + 点击切换源码行断点（复用 `on_breakpoint_toggled`）；无源码映射如实提示；单测覆盖切换。

---

> ## 执行顺序
>
> P1 → P5 已完成。迭代 2 新增 **P6 → P7 → P8 → P9 → P12** 全部完成并同步文档；本轮迭代 3 新增 **P12.2 → P13.1 → P13.2 → P14.1 → P14.2 → P15 → P16 → P17 → P18 → P19 → P20** 全部完成并同步文档；本轮迭代 4 新增 **P21** 全部完成并同步文档；本轮迭代 5 新增 **P22**（80251 + C166/XC166/XC2000）全部完成并同步文档；本轮迭代 6 新增 **P23**（Xtensa/ESP32）全部完成并同步文档；本轮迭代 7 新增 **P24**（仿真页观测增强）全部完成并同步文档；本轮迭代 8 新增 **P25**（仿真调试体验 + LSP 格式化/快速修复）全部完成并同步文档；本轮迭代 9 新增 **P26**（仿真寄存器/PC 编辑与断点管理）全部完成并同步文档；本轮迭代 10 新增 **P27**（Make/CMake 构建系统）全部完成并同步文档；本轮迭代 11 新增 **P28**（周期级外设时序）全部完成并同步文档；本轮迭代 12 新增 **P29（仿真看门狗外设）→ P30（烧写外部 loader 完善）→ P31（快捷键可配置）** 全部完成并同步文档；本轮迭代 13 新增 **P32（并行构建）** 全部完成并同步文档；本轮迭代 14 新增 **P33（外设寄存器写入编辑）** 全部完成并同步文档；本轮迭代 15 新增 **P34（反汇编断点）** 全部完成并同步文档；🔒 attach 硬件验证贯穿（需硬件，未验证）。

---

## P28 · 周期级外设时序（F6）

AGENTS.md F6 预留「周期级仿真（周期精确建模）」。检查发现两处真实缺陷：所有非 RISC-V 架构分支（8051/ARM/80251/C166/Xtensa/AVR/MSP430/V850/STM8/RX/RL78）在 `step()` 中 **return 前不调用 `tick_peripherals`**，外设（Timer）在这些目标上永不推进；且 `tick_peripherals` 每步只推进 1 周期，cycle-accurate 模式下 `cycle_count` 按指令延迟前进但 Timer 仍走 1 周期——外设相对 CPU 时钟慢 N 倍。

| 任务 | 状态 | 验收 |
|---|---|---|
| `tick_peripherals_for(cycles)` | ✅ | `Simulator` 新增按周期数推进外设（Timer 每周期 tick，溢出置 IRQ）；`tick_peripherals()` 保留为 1 周期的兼容包装 |
| 全架构外设推进 | ✅ | `step()` 的 11 个非 RISC-V 分支（MCS-51/ARM/80251/C166/Xtensa/AVR/MSP430/V850/STM8/RX/RL78）在返回前统一调用 `tick_peripherals_for(last_instruction_cycles)`；主 RISC-V 路径同 |
| cycle-accurate 对齐 | ✅ | `cycle_count` 前进 `last_instruction_cycles`（指令估算延迟）时外设同步前进相同周期数，不再慢 N 倍 |
| 单测 | ✅ | `tick_peripherals_for_advances_timer_by_cycles`（4 周期无溢出 → 第 5 周期溢出 + IRQ）、`arm_step_advances_peripherals`（ARM 每步 tick Timer 并置 IRQ）；ns-sim 185 项全绿 |

**已完成**：`tick_peripherals_for(cycles)` 周期数推进外设；`step()` 全部 12 个架构路径在返回前统一推进外设；cycle-accurate 模式外设与 CPU 时钟同步；2 项单测验证（含 ARM 外设推进回归测试）。

---

## P29 · 仿真看门狗外设（F6）

虚拟外设此前仅 GPIO/UART/TIMER/中断控制器；看门狗（WDT）是嵌入式固件最典型的缺失外设——超时复位可完整验证，让跑飞固件可见而非静默停滞。

| 任务 | 状态 | 验收 |
|---|---|---|
| `ns_sim::Watchdog` | ✅ | 内存映射：`base+0x00` 喂狗（写 `0xAA55` 重载）、`base+0x04` 启用/禁用、`base+0x08` 超时周期、`base+0x0C` 读复位计数、`base+0x10` 读复位 PC |
| 周期推进 | ✅ | `tick_peripherals_for` 按周期推进，超时 `reset_count+1` 并记录 `last_reset_pc` |
| 寄存器路由 | ✅ | `peripheral_read`/`peripheral_write` 路由 WDT 寄存器（读值先于可变借用） |
| IDE 仿真页视图 | ✅ | 「看门狗」状态行：基址/启用/超时/已过/复位次数 |
| 单测 | ✅ | `watchdog_fires_when_unfed_and_resets_on_feed`：启用→超时复位、喂狗重载、非喂狗不重载；ns-sim 177 项全绿 |

**已完成**：`Watchdog` 结构 + `VirtualPeripherals.watchdog` + `tick_peripherals_for` 推进 + `peripheral_read`/`peripheral_write` 路由 + IDE 仿真页视图 + 1 项单测。

---

## P30 · 烧写外部 loader 完善（F8）

FLM 解析此前已接入 OpenOCD；本轮把 FLM 几何与项目 target 扩展至全部适配器，消除硬编码设备名与地址。

| 任务 | 状态 | 验收 |
|---|---|---|
| `FlashRequest.device` | ✅ | IDE `flash_artifact` 传入项目 `target`（非空覆盖适配器硬编码） |
| pyOCD 设备选择 | ✅ | `request.device` 非空覆盖硬编码 `stm32f103c8` |
| J-Link 几何 | ✅ | `request.device` + FLM `base_address`/`device_size`（`verifybin`/`savebin` 显式基址与尺寸，替代硬编码 `0x8000000`/`0x8000`） |
| 单测 | ✅ | ns-flash 12 项全绿（stub 工具验证命令构造） |

**已完成**：FLM 几何（基址/页大小/设备尺寸）接入 OpenOCD/pyOCD/J-Link；项目 target 作为设备名透传；烧写面板如实报告 FLM 加载失败。

---

## P31 · 快捷键可配置（F13）

快捷键此前固定于 `bindings()`；本轮支持用户覆盖（`keybindings.json`），命令面板可编辑。

| 任务 | 状态 | 验收 |
|---|---|---|
| `KeybindingOverrides` | ✅ | `ns_project` 新增 `keybindings_settings.rs`：`keybindings.json`（用户配置目录），action 名→键位字符串，`BTreeMap` 确定性序列化，空值=恢复默认 |
| `bindings_with` | ✅ | `app::bindings_with(&overrides)` 按 action 名应用覆盖；覆盖时该 action 只保留一条绑定（不再产生重复 cmd/ctrl 对） |
| 启动加载 | ✅ | `main.rs` 加载覆盖后注册 keymap |
| 编辑模态 | ✅ | 命令面板「快捷键设置…」（`keybindings`）：每可重映射 action 一行（命令名+默认键位+编辑框），保存/取消，Enter 确认、Esc 取消 |
| 单测 | ✅ | `overrides_round_trip_via_serde`/`empty_value_is_treated_as_unbound`/`save_drops_empty_values`/`bindings_apply_keystroke_overrides`；ns-project 56 项、ns-ide 88 项全绿 |

**已完成**：快捷键可配置（用户级 `keybindings.json` 覆盖 + 编辑模态 + 启动加载）；重映射后重启生效。


## P35 · Git 分支操作与状态解析修正（F14）

对比嵌入式 IDE 的 Git 集成：分支/历史数据已获取但**未渲染**，`checkout`/`create_branch`/`pull`/`push` 底层函数已实现但**未接线**（4 个死代码警告）；`status --porcelain` 的重命名条目（`R  old -> new`）被当作一个伪路径，分离 HEAD 时 `git branch` 的 `(HEAD detached …)` 伪条目被当成真实分支。

| 任务 | 状态 | 验收 |
|---|---|---|
| 分支行渲染 | ✅ | 历史·分支区每行本地分支可点击切换（当前分支玉色 `●` 标记）；`git_checkout` 切换后刷新状态/diff |
| 新建分支对话框 | ✅ | 顶栏「＋新建分支」弹输入框（`GitBranchDialog`），Enter/「创建并切换」调 `git::create_branch`，空名拒绝；Esc/「取消」关闭；Enter 经 `ConditionDialogSave` 接线 |
| 拉取/推送按钮 | ✅ | 顶栏「↓ 拉取」「↑ 推送」调 `git::pull`/`git::push`，失败如实显示到错误行 |
| 重命名解析 | ✅ | `repo_status` 对 `R  old -> new` 取 ` -> ` 后的目标路径，不再显示 `old -> new` 伪路径；diff/暂存/选中均用目标 |
| 分离 HEAD 过滤 | ✅ | `branches` 过滤 `(HEAD detached …)`/`(no branch)` 伪条目，面板不再把分离 HEAD 当真实分支 |
| 单测 | ✅ | `create_and_switch_branch_updates_status`、`pull_and_push_report_errors_honestly`、`rename_reports_target_as_changed_path`；ns-ide git 7 项全绿 |

**已完成**：Git 面板分支操作（切换/新建/拉取/推送）接线；`status --porcelain` 重命名目标解析；分离 HEAD 伪分支过滤；`git.rs` 4 个死代码警告消除（`cargo check --workspace` 0 警告 0 错误）。

---

> 执行顺序：P1 → P31 已完成。迭代 16 新增 **P35（Git 分支操作与状态解析修正）** 全部完成并同步文档；🔒 attach 硬件验证贯穿（需硬件，未验证）。
>

1. `cargo check --workspace` 0 警告 0 错误；禁止 `#[allow(dead_code)]` 等掩盖。
2. 功能完整实现，禁止 `todo!()`/占位/假数据/「仅演示路径」。
3. 提交前本机实际运行验证关键路径。
4. 文档与代码同步。



