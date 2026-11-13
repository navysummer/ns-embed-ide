# ns-embed-ide — 对比常用嵌入式 IDE 的缺口分析与任务清单

> 对照 IAR EW / Keil µVision / STM32CubeIDE / PlatformIO / VS Code（C/C++/Embedded 扩展）的能力模型，
> 逐项核对本仓库当前实现（AGENTS.md §5.1 与 README「当前实现状态」），列出：
> **需要修复**（已存在但未接通/有缺陷）、**需要完善**（已有基础但覆盖面不足）、**需要新增**（能力缺失）。

> **更新记录**（2026-11-13）：上一版清单（R1–R4 / E1–E4 / N1–N2）经核对**已全部在代码中落地**，
> 本版重写为真实剩余状态。核心修复：工作区此前**无法编译**（`cargo check` 11 错误），本次已修复。

## 0. 结论摘要

本仓库已实现非常广泛的能力（12 架构仿真、LSP、DAP 调试、烧写、Git、静态分析、覆盖率、
RTOS 任务列表、profiler 热点聚合等）。上一版缺口清单（快速修复 `AddLinkFlag`、Format on Save、
保存选项、单指令步进、片段管理 UI、`ns_sim` 注释）**均已实现并验证**。

**本次发现并修复的真实问题**：

1. **工作区无法编译**：`cargo check` 报 11 个错误——未完成的真实 FreeRTOS 内核遍历 WIP 引用了
   3 个从未定义的符号（`kernel_walk_expr` / `rtos_kernel_send` / `rtos_kernel_advance`），
   `ns_debug::profile` 模块缺失（`ProfileAggregator` 从未创建），3 处测试初始化器过期
   （`RtosRefreshState` 缺 `mode`/`kernel` 字段）。
2. **死代码**：5 个 `rtos_kernel_eval_*` 方法、`evaluate_rtos`/`evaluate_rtos_addr`/`kick_rtos_kernel_walk`、
   `Next::Finish` 变体、`RtosKernelWalk::items_remaining` 字段均不再被引用（编译零警告规则）。

## 1. 本次已修复

| # | 位置 | 问题 | 处理 |
|---|---|---|---|
| FIX-1 | `ns-debug/src/profile.rs`（新增） | `ns_debug::profile::ProfileAggregator` 缺失，IDE 引用无法编译 | 实现模块：`new`/`record`/`hotspots`/`total`，PC 采样按 `nm` 符号二分定位，间隙/未知计入 `<unknown>`；3 项单测全绿 |
| FIX-2 | `ns-ide/src/app.rs` `kernel_walk_expr` | 未定义 | 实现：按 walk 步骤（owner/name/priority/next）构造带 `__rtos_` 标签的求值表达式；2 项单测全绿 |
| FIX-3 | `ns-ide/src/app.rs` `rtos_kernel_send`/`rtos_kernel_advance` | 未定义 | 实现：发送带标签表达式 / 推进到下一调度链表；接入 `handle_rtos_kernel_evaluated` 的 `Next` 分发 |
| FIX-4 | `ns-ide/src/app.rs` 测试初始化器 ×3 | `RtosRefreshState` 缺 `mode`/`kernel` | 补 `mode: RtosKernelMode::Template, kernel: None` |
| FIX-5 | `ns-ide/src/app.rs` 死代码 | 5 方法 + 3 辅助 + `Next::Finish` + `items_remaining` 字段 | 全部删除（零警告规则） |

**验证**：`cargo check --workspace` 退出 0、本地零警告（仅保留 AGENTS.md §1.2.1 允许的上游
`future-incompat` note：`block v0.1.6`/`proc-macro-error2 v2.0.1`）；`cargo test --workspace`
**637 项全绿**（含新增 profile 3 项 + kernel_walk_expr 2 项 + device 字段/描述 2 项）。

## 1b. 设备寄存器视图增强（本轮）

对照 Keil / IAR / STM32CubeIDE / VS Code Cortex-Debug 的寄存器查看器，设备视图此前只显示
寄存器名/地址/值与字段名范围，**SVD 描述（`peripheral.description` / `register.description`）虽被解析却从不显示**，
位域活值（从寄存器值解码出的字段值）也未呈现。本次补齐：

| # | 变更 | 说明 |
|---|---|---|
| DEV-1 | 外设描述 | 设备视图外设标题下显示 `peripheral.description`（截断至 64 字符，省略号） |
| DEV-2 | 寄存器描述 | 每个寄存器行下方显示 `register.description`（截断至 80 字符，省略号） |
| DEV-3 | 位域活值分解 | 字段摘要改为 `名=0x值[msb:lsb]`，值从 live 寄存器值按 `bit_offset`/`bit_width` 提取（`device_field_value`，单比特与顶位字段正确掩码） |
| DEV-4 | 单测 | `device_field_value_extracts_shifted_and_masked`（低/高/单比特/零宽字段）+ `device_description_short_truncates_and_preserves`（截断/省略号/空串） |
| DEV-5 | 寄存器字段展开 | 有字段的寄存器行前加展开箭头（▸/▾），点击行展开显示每个字段的 `名[msb:lsb] = 0x值` + `field.description`（非零字段高亮金色）；值点击仍进入编辑（`stop_propagation` 避免误触展开）；`toggle_device_reg_expand` 维护展开集；单测 `toggle_device_reg_expand_adds_and_removes` |

## 1c. 调试寄存器刷新 + 保存所有文件（本轮）

对照 VS Code Cortex-Debug / Keil 的寄存器查看器与常用 IDE 的保存语义，补齐两处对称缺口：

| # | 变更 | 说明 |
|---|---|---|
| G1 | 寄存器手动刷新 | 调试「寄存器」列工具栏新增**「刷新」**按钮（`dbg-regs-refresh`），`refresh_registers` 清空缓存并重新下发 `GetVariables`（Registers 作用域），与变量面板 `dbg-vars-refresh` 对称；无作用域如实提示。单测 `refresh_registers_clears_and_reports` |
| G2 | 保存所有文件 | 命令面板「保存所有文件」+ 快捷键 `Cmd/Ctrl+Alt+S`（`SaveAll` action + 绑定），经既有 `save_all_editors` 写回全部脏标签（hex/文本）。单测 `save_all_editors_persists_dirty_tabs` |

## 2. 需要修复（已存在但未接通 / 有缺陷）

| # | 位置 | 问题 | 影响 |
|---|---|---|---|
| R1 | `ns-debug/src/embedded.rs` | 嵌入式目标 attach（OpenOCD/ST-Link/J-Link/pyOCD/probe-rs + `target remote` GDB）**已实现但未验证** | 需真实硬件，本机无法确认行为正确 |

## 3. 需要完善（已有基础但覆盖面不足）

| # | 模块 | 现状 | 期望 |
|---|---|---|---|
| E1 | RTOS 感知调试（F7） | FreeRTOS 模板 `tasks[i]` 求值 + **真实内核链表遍历**（`pxCurrentTCB`/`xReadyTasksLists` 等，DAP `evaluate` 往返）已接入；`ns_debug::rtos::walk_free_rtos` 纯函数仅被自身单测覆盖 | IDE 遍历改用 `ReadMemory` + `walk_free_rtos`（复用已测纯函数，避免两套实现漂移）或至少为 DAP 往返路径补集成测试 |
| E2 | 图形化外设配置（F11） | 列表式外设勾选 + GPIO 引脚模式循环 + UART 波特率已接入 | 时钟树/引脚映射图等图形化配置（CubeMX 式） |
| E3 | 多核/多芯片调试（F7） | 单目标单会话，`select_debug_thread` 已支持线程切换 | 多核（如双核 STM32H7）调度与核间同步 |

## 4. 需要新增（能力缺失）

| # | 模块 | 功能 | 说明 |
|---|---|---|---|
| N1 | F14 | 外部工具管理 UI | `ns_project::tools` + `ExternalToolsPanel` 已实现（命令面板「运行外部工具…」/「编辑外部工具…」），无剩余缺口 |
| N2 | — | — | 上一版 N1/N2（片段管理 UI、保存选项）已落地，无新增缺口 |

> 注：上一版清单中 R1–R4（`AddLinkFlag` 死代码、`apply_quick_fix` 提示、`ns_sim` 注释、`CompilerSection` 无链接字段）、
> E1–E4（快速修复、Format on Save、构建前保存、单指令步进）、N1–N2（片段管理、保存选项）均已实现；
> 终端多会话、线程选择器、调试控制台历史、断点导入/导出、编辑器缩放/缩进/软换行等也已实现，**不重复**。

## 5. 执行顺序（剩余）

本轮已完成：**G1 寄存器手动刷新**、**G2 保存所有文件**（见 §1c，均为低风险、可无硬件验证的常用 IDE 缺口）。
剩余项不变：

1. **R1**：嵌入式目标 attach 需硬件验证（标注「未验证」），不阻塞。
2. **E1**：真实 FreeRTOS 内核遍历改为复用 `walk_free_rtos`（`ReadMemory` 驱动）或补集成测试——需真实内核符号，本机无法验证。
3. **E2/E3**：图形化外设配置、多核调试为长期增强，当前版本如实标注为未完成。
4. 验证：`cargo check` 零警告（退出 0、本地 0 警告）、`cargo test --workspace` **640 项全绿**、启动应用走通关键路径。
