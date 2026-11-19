# ns-embed-ide — 对比常用嵌入式 IDE 的缺口分析与任务清单

> 对照 IAR EW / Keil µVision / STM32CubeIDE / PlatformIO / VS Code（C/C++/Embedded 扩展）的能力模型，
> 逐项核对本仓库当前实现（AGENTS.md §5.1 与 README「当前实现状态」），列出：
> **需要修复**（已存在但未接通/有缺陷）、**需要完善**（已有基础但覆盖面不足）、**需要新增**（能力缺失）。

> **更新记录**（2026-11-13）：上一版清单（R1–R4 / E1–E4 / N1–N2）经核对**已全部在代码中落地**，
> 本版重写为真实剩余状态。核心修复：工作区此前**无法编译**（`cargo check` 11 错误），本次已修复。
> （2026-11-16）：新增 §1d —— 真实 FreeRTOS 内核遍历启动门槛修复 + DAP/纯函数一致性集成测试，`cargo test --workspace` **649 项全绿**；新增 §1e —— 时钟树图形化配置（E2 子项），`cargo test --workspace` **653 项全绿**。
> （2026-11-16）：新增 §1f —— STM32F103C6/C8/CB（LQFP48）引脚映射图（E2 剩余子项，含目标扩展），`cargo test --workspace` **661 项全绿**；新增 §1g —— STM32F4 时钟树扩展 + 向导仿真文案修正，`cargo test --workspace` **665 项全绿**。
> （2026-11-16）：新增 §1h —— STM32F407（LQFP100）引脚映射图扩展（E2 子项），`cargo test --workspace` **670 项全绿**。
> （2026-11-17）：新增 §1i —— STM32F407（LQFP100）引脚映射图端口补齐（GPIOA..GPIOE + SPI3/USART6/TIM8/TIM1 互补），`cargo test --workspace` **671 项全绿**。
> （2026-11-17）：新增 §1j —— STM32F407 引脚映射图外设补齐（SDIO AF12 + TIM9/10/11 AF3），`cargo test --workspace` **672 项全绿**。
> （2026-11-17）：新增 §1k —— STM32F407 引脚映射图外设补齐（续：I2C3 AF4 + TIM12/13/14 AF9），`cargo test --workspace` **673 项全绿**。
> （2026-11-17）：新增 §1l —— STM32F407 引脚映射 GPIO 输入上拉/下拉 + 生成 `OSPEEDR`/`PUPDR`，`cargo test --workspace` **674 项全绿**。
> （2026-11-17）：新增 §1m —— 引脚映射方案保存/载入（`.ns-ide/pin_map.json`），`cargo test --workspace` **675 项全绿**。
> （2026-11-17）：新增 §1n —— STM32F407 TIM2_CH3/CH4（PA6/PA7, AF1）补齐 + 外设分组一键分配/取消，`cargo test --workspace` **676 项全绿**。
> （2026-11-17）：新增 §1o —— 引脚映射图导入 CubeMX `.ioc`（`pin_ioc` 解析器 + 「导入 .ioc」按钮），`cargo test --workspace` **680 项全绿**。
> （2026-11-17）：新增 §1p —— 引脚映射图导出 CubeMX `.ioc` + 导入格式校正（原生 `Mode.PA9`/`Signal.PA9`），`cargo test --workspace` **682 项全绿**。
> （2026-11-17）：新增 §1q —— STM32F407 引脚映射生成代码外设时钟使能（APB1/APB2 + USB AHB1），`cargo test --workspace` **683 项全绿**。
> （2026-11-17）：新增 §1r —— 引脚映射生成代码自包含寄存器定义（`ns_` 类型 + `#ifndef` 基址宏，可独立编译），`cargo test --workspace` **94 项全绿**（不含沙箱限制的 2 个 socket 测试）
> （2026-11-17）：新增 §1s —— 时钟树生成代码自包含 RCC 定义（`ns_rcc_f1_t`/`ns_rcc_t` + `#ifndef RCC`），`cargo test --workspace` **94 项全绿**（不含沙箱限制的 2 个 socket 测试）
> （2026-11-19）：新增 **P50** —— STM32F103C6/C8/CB LQFP48 引脚映射扩展外设（TIM1/3/4、CAN1、USB、完整 USART 握手线），`PinSignal` 枚举新增 26 个变体，`stm32f1_allowed_signals` AF 表从 PA0/1/2/3/4/5/6/7/9/10/15、PB3-11 扩展到 **PA0-12、PB0-1、PB3-14**；`stm32f1_pin_clocks` 补全 TIM1/TIM3/TIM4/CAN1/USB 时钟位；单测 `f103_expanded_af_table_has_tim1_3_4_can_usb_and_usart_handshake`。验证：`cargo test --workspace` **94 项全绿**（沙箱 socket 限制除外）。
> （2026-11-19）：新增 **P51** —— STM32F103 GPIO 输入上拉/下拉 + OSPEEDR/PUPDR 寄存器生成，`PinSignal` 枚举新增 `GpioInputPull`/`GpioInputPullDown`，AF 表所有分支追加上下拉选项，`stm32f1_pin_registers` 返回 `(CRL, CRH, OSPEEDR, PUPDR)`，`ns_gpio_f1_t` 结构体补齐新寄存器；单测 `f103_pin_registers_pack_ospeedr_and_pupdr`。验证：`cargo test --workspace` **94 项全绿**（不含沙箱限制的 2 个 socket 测试）

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

## 1d. 真实 FreeRTOS 内核遍历启动修复 + DAP/纯函数一致性测试（本轮）

对照 E1（真实内核遍历与纯函数 `walk_free_rtos` 两套实现漂移）逐项核对，发现并修复一个**真实缺陷**：
`refresh_rtos_tasks` 只下发 6 个调度链表头（ready / delayed1 / delayed2 / pending / suspended /
termination），但 `rtos_kernel_maybe_kick` 的启动门槛误写为 `lists.len() >= 7`，导致真实内核遍历
**永远不会开始**（RTOS 面板停在「正在读取任务列表…」）。本次：

| # | 变更 | 说明 |
|---|---|---|
| E1-1 | 启动门槛修复 | `rtos_kernel_maybe_kick` 改用 `REAL_KERNEL_LIST_COUNT = 6`，与 `refresh_rtos_tasks` 实际下发的链表符号数一致；6 个链表头 + 运行 TCB 齐备后立即开始遍历 |
| E1-2 | DAP/纯函数一致性集成测试 | 新增 `real_kernel_dap_walk_matches_pure_walker`：用合成 FreeRTOS 内存镜像（两个 TCB，ready + delayed + 4 空链表）驱动现有 `Evaluate` 往返状态机，逐条应答 running/ready/delayed/pending/suspended/termination 符号与 first/owner/name/priority/next 步骤，断言最终任务列表与 `ns_debug::rtos::walk_free_rtos` 在**同一镜像**上输出完全一致（名称/运行/优先级/状态），并断言 6 链表头 + 运行 TCB 齐备后首个读取已发出（`in_flight == 1`，修复前的启动门槛会令此断言失败）且状态栏进入「内核遍历」——证明 DAP 往返路径与纯函数不再漂移，且修复后确实启动 |

**验证**：`cargo check --workspace` 退出 0、本地零警告（仅保留 AGENTS.md §1.2.1 允许的上游
`future-incompat` note）；`cargo test --workspace` **649 项全绿**（含新增 1 项集成测试）。

## 1e. 时钟树图形化配置（本轮，E2 子项，STM32F1）

对照 E2（CubeMX 式时钟树/引脚映射图），既有外设配置模态只有列表勾选 + GPIO 引脚模式循环，无时钟树。
本次新增**独立「时钟树配置（图形化，CubeMX 式）…」模态**（命令面板 `clock-tree-gui`；STM32F1，F4 见 §1g，其他家族如实拒绝以免生成错误 RCC 寄存器写）：

| # | 变更 | 说明 |
|---|---|---|
| CLK-1 | 时钟模型 | `ClockConfig`（源 HSI/HSE/PLL、HSE 频率、PLL ×2…×16、AHB/APB1/APB2 预分频）+ `ClockSource`；`sysclk_hz`/`ahb_hz`/`apb1_hz`/`apb2_hz`/`pll_hz` 实时计算（PLL 源 = HSE） |
| CLK-2 | RCC 寄存器编码 | `stm32f1_clock_registers`：`RCC->CR`（HSEON/PLLON）+ `RCC->CFGR`（SW、PLLMUL、HPRE、PPRE1/PPRE2 位域） |
| CLK-3 | 图形化界面 | `render_clock_tree`：可点击源（HSI/HSE/PLL）、PLL 倍频、AHB/APB1/APB2 预分频；SYSCLK/AHB/APB1/APB2 及示例外设（USART1/2、GPIOA、TIM2）时钟实时刷新；`clock-tree-modal` |
| CLK-4 | 代码生成 | 「生成代码」写 `src/clock_init.c`：`rcc_clock_init()` 发出真实 `RCC->CR = 0x…; RCC->CFGR = 0x…;`，打开编辑器并报告 |
| CLK-5 | 单测 | `clock_tree_computes_hsi_hse_pll_and_prescalers`（HSI 8 MHz、PLL ×9=72 MHz、预分频除法）、`stm32f1_clock_registers_encodes_source_and_prescalers`（CR/CFGR 位域）、`clock_tree_gui_opens_cycles_and_generates`（HSI→HSE→PLL、倍频/预分频循环、生成含 CR/CFGR）、`open_clock_tree_without_project_or_rcc_reports`（无项目/无 RCC/非 STM32F1 如实提示） |

**验证**：`cargo check --workspace` 退出 0、本地零警告；`cargo test --workspace` **653 项全绿**（含新增 4 项）。

## 1g. STM32F4 时钟树扩展（本轮）

对照 §1e 的 F1 时钟树，STM32F4 的 RCC 布局不同：PLL 的 `PLLM/PLLN/PLLP/PLLSRC/PLLQ` 字段位于
`RCC->PLLCFGR`（F1 把倍频放在 `CFGR` 的 `PLLMUL` 字段），生成 F1 的 `CFGR` 到 F4 会是错误寄存器写。
本次为 **STM32F4（`stm32f407vg`）补齐时钟树**，并同步修正向导中误导性的「仿真能力后续接入」文案
（内置 `ns-sim` 指令级仿真已真实可用）：

| # | 变更 | 说明 |
|---|---|---|
| F4-1 | 时钟模型 | `F4ClockConfig`（源 HSI/HSE/PLL、HSE 频率、`PLLM` 2-63、`PLLN` 50-432、`PLLP` 2/4/6/8、`PLLQ` 2-15、PLL 源 HSE/HSI、AHB/APB1/APB2 预分频）；`pll_source_hz`/`pll_vco_hz`/`pll_hz`/`pll48_hz`/`sysclk_hz`/`ahb_hz`/`apb1_hz`/`apb2_hz` 实时计算（默认 F407 经典 168 MHz：HSE 8 MHz、PLLM=8、PLLN=336、PLLP=2、PLLQ=7） |
| F4-2 | 寄存器编码 | `stm32f4_clock_registers`：`RCC->CR`（HSEON/PLLON）+ `RCC->PLLCFGR`（PLLM/PLLN/PLLP/PLLSRC/PLLQ）+ `RCC->CFGR`（SW/HPRE/PPRE1/PPRE2）；`pllp_encode`（00=/2、01=/4、10=/6、11=/8） |
| F4-3 | 图形化界面 | `render_f4_clock_tree`：源 HSI/HSE/PLL + PLLM/PLLN/PLLP/PLLQ/PLL 源 + AHB/APB1/APB2 预分频；SYSCLK/PLL48/AHB/APB1/APB2 及示例外设时钟实时刷新；`clock-tree-modal` 复用 |
| F4-4 | 代码生成 | 「生成代码」写 `src/clock_init.c`：`rcc_clock_init()` 发出真实 `RCC->CR = 0x…; RCC->PLLCFGR = 0x…; RCC->CFGR = 0x…;`（F4 分支写入 `PLLCFGR`，F1 分支不含该写） |
| F4-5 | 目标检查 | `open_clock_tree` 按 `stm32f1*`/`stm32f4*` 选择 `ClockFamily`，其他家族如实提示「支持 STM32F1 / STM32F4」 |
| F4-6 | SVD 补齐 | `stm32f407vg.svd` 的 RCC 增加 `PLLCFGR`（0x04，PLLM/PLLN/PLLP/PLLSRC/PLLQ 字段）与 `CFGR`（0x08，SW/HPRE/PPRE1/PPRE2 字段），寄存器视图与时钟树均可见 |
| F4-7 | 向导文案修正 | wizard 仿真软件步骤由「仿真能力将在后续里程碑接入」改为「内置 ns-sim 可直接运行指令级仿真，也可选 QEMU（需安装）」——内置仿真器已实现，原文案属误导性占位 |
| F4-8 | 单测 | `f4_clock_tree_computes_hsi_hse_pll_and_prescalers`（HSI 16 MHz、168 MHz PLL 链、PLL48=48 MHz、HSI 源 PLL、预分频除法）、`stm32f4_clock_registers_encodes_source_and_pllcfgr`（PLLCFGR/CFGR 位域）、`f4_clock_tree_gui_opens_cycles_and_generates`（PLLM/N/P/Q/源循环、生成含 PLLCFGR）、`f407_rcc_exposes_pllcfgr_and_cfgr_for_clock_tree`（ns-device SVD 解析） |

**验证**：`cargo check --workspace` 退出 0、本地零警告；`cargo test --workspace` **665 项全绿**（含新增 4 项）。

## 1f. 引脚映射图（本轮，E2 剩余子项）

对照 E2（CubeMX 式时钟树/引脚映射图）的剩余缺口，既有 GPIO 引脚图只循环 GPIO 模式，无法把
USART/SPI/I2C/TIMER 外设功能按 AF 表分配到物理引脚。本次新增**独立「引脚映射图（外设功能 →
物理引脚，CubeMX 式）…」模态**（命令面板 `pin-map-gui`；内置 STM32F103C6/C8/CB LQFP48 中密度
AF 表——三者同封装同外设，仅 Flash/RAM 不同，其他目标如实提示「暂不支持目标」）：

| # | 变更 | 说明 |
|---|---|---|
| PIN-1 | 引脚功能模型 | `PinSignal`（GPIO 输出/输入、USART1/2/3、SPI1_NSS/SCK/MISO/MOSI、I2C1_SCL/SDA、TIM2_CH1/CH2）+ `PinMapDialog`（PA0-15、PB0-15、PC13-15）；`stm32f1_allowed_signals` 给出每引脚可用 AF（含 USART1/I2C1/SPI1 重映射选项） |
| PIN-2 | 图形化界面 | `render_pin_map`：点击引脚在「未分配 → 可用功能 → 未分配」循环；按信号家族着色；红点标注 AFIO 重映射；`pin-map-modal`；「清除全部」/「取消」/「生成代码」 |
| PIN-3 | 冲突校验 | `validate_pin_mapping`：同一信号不得重复分配；同一外设的默认引脚与重映射引脚不得混用（AFIO MAPR 作用于整个外设） |
| PIN-4 | 寄存器打包 | `stm32f1_pin_registers` 按实际物理引脚号填满 16 位模式表后打包 `CRL`/`CRH`（AF 推挽 0xB、I2C 开漏 0xF、GPIO 输出 0x3、输入 0x4）；`stm32f1_pin_clocks` 计算 `RCC->APB2ENR/APB1ENR`；`stm32f1_remap_bits` 计算 `AFIO->MAPR` |
| PIN-5 | 代码生成 | `stm32f1_pin_mapping_code` → `src/pin_mapping.c`：`pin_mapping_init()` 发出真实 `GPIOx->CRL/CRH`、`RCC->APBxENR` 与 `AFIO->MAPR` 写；头注释带实际目标（C6/C8/CB） |
| PIN-6 | 单测 | `stm32f1_pin_mapping_packs_af_registers_and_remap`（CRL/CRH/APB2/APB1/MAPR/代码）、`stm32f1_pin_mapping_uses_physical_pc_pin_numbers`（PC13-15 落 CRH）、`pin_map_validates_duplicate_and_mixed_remap`、`pin_map_gui_opens_cycles_and_generates`、`pin_map_gui_opens_for_other_lqfp48_variants`、`open_pin_map_without_project_or_unsupported_reports` |
| PIN-7 | 目标扩展 | `stm32f103c6`（32KB/10KB）与 `stm32f103cb`（128KB/20KB）加入 `ns-device` 目录（同 AF 表 SVD 派生）与向导目标列表；CubeMX `.ioc` 的 `STM32F103C6Tx`/`STM32F103CBTx` 映射到目录键。单测 `stm32f103_lqfp48_variants_have_distinct_memory_and_same_peripherals`（ns-device）、`maps_cubemx_f103_lqfp48_variants_to_catalog_keys`（ns-project） |

**验证**：`cargo check --workspace` 退出 0、本地零警告；`cargo test --workspace` **661 项全绿**（含新增 3 项：ns-ide 引脚映射 1 + ns-device 目录变体 1 + ns-project CubeMX 映射 1；既有 5 项引脚映射沿用）。

## 1h. STM32F407 引脚映射图扩展（本轮，E2 子项）

既有引脚映射图只覆盖 STM32F103C6/C8/CB（LQFP48，AFIO 重映射）。STM32F4 的 AF 机制不同：
每个引脚在 `GPIOx->AFRL`/`AFRH` 中独立选择 AF0-15，没有 AFIO `MAPR` 重映射位。本次为
**STM32F407（LQFP100）** 补齐同一模态的 F4 分支：

| # | 变更 | 说明 |
|---|---|---|
| F4PIN-1 | 引脚功能模型 | `PinMapFamily::F1/F4` + `F4PinSignal`（GPIO、USART1/2/3、SPI1/2、I2C1/2、TIM1-5、USB、CAN1、SWD，携带 `af()`）；`stm32f4_allowed_signals` 给出 PA0-15/PB0-15 每引脚可用 AF0-15 |
| F4PIN-2 | 图形化界面 | `render_pin_map` 按 `family` 分派到 `render_f4_pin_map`（复用点击循环与家族着色；无 AFIO 红点，改为提示 AFRL/AFRH） |
| F4PIN-3 | 冲突校验 | `validate_f4_pin_mapping`：同一信号不得重复分配；同一外设组的 AF 号必须一致 |
| F4PIN-4 | 寄存器打包 | `stm32f4_port_registers`：`MODER`（2 位/引脚：00 输入、01 输出、10 AF）+ `OTYPER`（I2C 开漏）+ `AFRL`/`AFRH`（4 位/引脚 AF 号）；`stm32f4_pin_clock_bit` 映射 AHB1ENR GPIOA/GPIOB 位 |
| F4PIN-5 | 代码生成 | `stm32f4_pin_mapping_code` → `src/pin_mapping.c`：`pin_mapping_init()` 发出真实 `RCC->AHB1ENR |= 1<<N`、`GPIOx->MODER`、`GPIOx->OTYPER`、`GPIOx->AFRL`、`GPIOx->AFRH` |
| F4PIN-6 | SVD 补齐 | `stm32f407vg.svd` 的 GPIOA/GPIOB 增加 `OTYPER`（0x04）、`AFRL`（0x20）、`AFRH`（0x24），寄存器视图与引脚映射均可见 |
| F4PIN-7 | 目标检查 | `open_pin_map` 接受 `stm32f407`/`stm32f407vg`；其他目标如实提示「暂不支持目标」 |
| F4PIN-8 | 单测 | `stm32f4_pin_mapping_packs_moder_afrl_afrh`（MODER/OTYPER/AFRL/AFRH/代码）、`f4_pin_map_validates_duplicate_and_af_consistency`、`pin_map_gui_opens_cycles_and_generates_f4`、`renders_f4_pin_map_modal_without_panicking`（无头渲染冒烟）；ns-device `f407_gpio_exposes_af_registers_for_pin_mapping` |

**验证**：`cargo check --workspace` 退出 0、本地零警告；`cargo test --workspace` **670 项全绿**（含新增 5 项：ns-ide 引脚映射 F4 4 项 + ns-device SVD 1 项）。

## 1i. STM32F407 引脚映射图端口补齐（本轮，E2 子项）

§1h 的 F407 引脚映射只覆盖 GPIOA/GPIOB，但 LQFP100 实际还有 GPIOC/GPIOD/GPIOE。
本次把 F4 引脚映射扩到全部五个端口，并补齐端口 C/D/E 常用外设 AF：

| # | 变更 | 说明 |
|---|---|---|
| F4PIN-9 | 端口扩展 | `for_f4_target` 的 `f4_ports` 由 GPIOA/GPIOB 扩到 **GPIOA..GPIOE（5 端口、每端口 16 引脚）**；`stm32f4_pin_clock_bit` 扩到 C/D/E（`RCC->AHB1ENR` 位 2/3/4） |
| F4PIN-10 | 新信号变体 | `F4PinSignal` 新增 **SPI3**（AF6）、**USART6**（AF8）、**TIM8**（AF3）、**TIM1_CH4/CH1N/CH2N/CH3N/BKIN/ETR**（AF1，PE7-15 全覆盖）；`af()`/`group()`/`color()` 同步 |
| F4PIN-11 | C/D/E AF 表 | `stm32f4_allowed_signals` 覆盖 PC（PC6-9=TIM3/TIM8/USART6，PC10-12=USART3/SPI3）、PD（PD0-1=CAN1，PD3-7=USART2，PD8-13=USART3，PD12-15=TIM4）、PE（PE7-15=TIM1）；PA15/PB3-5 补 SPI3 |
| F4PIN-12 | SVD 补齐 | `stm32f407vg.svd` 新增 **GPIOC/GPIOD/GPIOE**（`MODER`/`OTYPER`/`AFRL`/`AFRH`）与 AHB1ENR 的 `GPIOCEN`/`GPIODEN`/`GPIOEEN` 字段 |
| F4PIN-13 | 单测 | `f4_pin_map_covers_ports_cde_af_table_and_code`（C/D/E AF 号、时钟位、MODER/AFRL/AFRH 打包、代码写 GPIOC/D/E）；`pin_map_gui_opens_cycles_and_generates_f4` 断言 5 端口并生成 GPIOC 写入；ns-device `f407_gpio_exposes_af_registers_for_pin_mapping` 扩展至 5 端口 + AHB1ENR |

**验证**：`cargo check --workspace` 退出 0、本地零警告；`cargo test --workspace` **671 项全绿**（含新增 1 项 ns-ide 引脚映射端口补齐）。

## 1j. STM32F407 引脚映射图外设补齐（本轮，E2 子项）

§1i 之后 F407 仍有常用外设未建模（SDIO、TIM9/10/11 等）。本次复用现有 F4 基础设施补齐：

| # | 变更 | 说明 |
|---|---|---|
| F4PIN-14 | 新信号变体 | `F4PinSignal` 新增 **SDIO**（`SdioCmd`/`SdioCk`/`SdioD0-D7`，AF12）与 **TIM9/10/11**（`Tim9Ch1/Ch2`/`Tim10Ch1`/`Tim11Ch1`，AF3）；`af()`/`group()`/`color()` 同步 |
| F4PIN-15 | C/D AF 表 | `stm32f4_allowed_signals` 覆盖 SDIO 总线（PC8-11=SDIO_D0-D3、PC12=SDIO_CK、PD2=SDIO_CMD、PD6-9=SDIO_D4-D7）与 PA2/PA3（TIM9）、PB8（TIM10）、PB9（TIM11） |
| F4PIN-16 | 校验与生成 | 整条 10 线 SDIO 总线同属 AF12 组，`validate_f4_pin_mapping` 校验通过；生成真实 `GPIOC->AFRH`、`GPIOD->AFRL`/`AFRH` 与 AHB1ENR 位 |
| F4PIN-17 | 单测 | `f4_pin_map_adds_sdio_and_tim9_11_af`（AF 号、允许信号、SDIO 全总线校验、MODER/AFRL/AFRH 精确值、AHB1ENR 位） |

**验证**：`cargo check --workspace` 退出 0、本地零警告；`cargo test --workspace` **672 项全绿**（含新增 1 项 ns-ide 引脚映射）。
尚未建模的 DCMI/FMC 留作后续增强。

## 1k. STM32F407 引脚映射图外设补齐（续，本轮，E2 子项）

§1j 之后 F407 仅剩的常规外设缺口是 I2C3 与通用定时器 TIM12/13/14；DCMI/FMC 无可靠引脚表，不建模。

| # | 变更 | 说明 |
|---|---|---|
| F4PIN-18 | 新信号变体 | `F4PinSignal` 新增 **I2C3**（`I2c3Scl`/`I2c3Sda`，AF4，`PinMode::AltOpenDrain`）与 **TIM12/13/14**（`Tim12Ch1`/`Tim12Ch2`、`Tim13Ch1`、`Tim14Ch1`，AF9）；`af()`/`group()`/`color()`/`mode()` 同步；TIM12-14 走独立 AF 分支（与 CAN1 同为 AF9 但匹配项各自独立） |
| F4PIN-19 | AF 表 | `stm32f4_allowed_signals` 覆盖 PA6（TIM13_CH1）、PA7（TIM14_CH1）、PA8（I2C3_SCL）、PB14（TIM12_CH1）、PB15（TIM12_CH2）、PC9（I2C3_SDA）、PC12（I2C3_SCL） |
| F4PIN-20 | 校验与生成 | I2C3 经开漏路径写入 `GPIOC->OTYPER`（位 9/12），AF4 写入 `AFRH` 对应 nibble（pin9=nibble1、pin12=nibble4）；TIM12-14 只写 `MODER`/`AFRL`/`AFRH`，不置 OTYPER；`validate_f4_pin_mapping` 对 I2C3+TIM12-14 组合校验通过 |
| F4PIN-21 | 单测 | `f4_pin_map_adds_i2c3_and_tim12_13_14_af`（AF 号、开漏模式、允许信号表、组合校验、`GPIOA`/`GPIOB`/`GPIOC` 的 `MODER`/`OTYPER`/`AFRL`/`AFRH` 精确值） |

**验证**：`cargo check --workspace` 退出 0、本地零警告；`cargo test --workspace` **673 项全绿**（含新增 1 项 ns-ide 引脚映射）。
尚未建模的 DCMI/FMC 留作后续增强（无可靠引脚表，不写入不确定数据）。

## 1l. STM32F407 引脚映射 GPIO 输入模式与 `OSPEEDR`/`PUPDR`（本轮，E2 子项）

§1k 之后 F407 引脚映射只剩两类工程细节缺口：输入脚无法选择上拉/下拉，生成代码缺少
`OSPEEDR`（输出速度）与 `PUPDR`（上下拉），两者都与 `MODER` 同属标准 GPIO 寄存器。

| # | 变更 | 说明 |
|---|---|---|
| F4PIN-22 | 新信号变体 | `F4PinSignal` 新增 **`GpioInputPull`（输入·上拉）/`GpioInputPullDown`（输入·下拉）**，`mode()` 映射 `PinMode::InputPull`/`InputPullDown`（`af()`=None、`group()`=GPIO、`color()`=TEXT_DIM）；`stm32f4_allowed_signals` 全部 62 个引脚与回退分支统一追加，点击循环变为 未分配 → 输出 → 输入 → 输入·上拉 → 输入·下拉 → 外设功能… |
| F4PIN-23 | 速度/上拉编码 | 新增 `stm32f4_speed_bits`（输出/复用 = 高速 0b10，输入 = 2 MHz 默认档 0b00）与 `stm32f4_pull_bits`（上拉 0b01、下拉 0b10），沿用 MODER 的 2 位/引脚布局 |
| F4PIN-24 | 寄存器打包 | `stm32f4_port_registers` 返回 **`MODER`/`OTYPER`/`OSPEEDR`/`PUPDR`/`AFRL`/`AFRH`**；生成的 `src/pin_mapping.c` 每端口新增 `GPIOx->OSPEEDR` 与 `GPIOx->PUPDR` 真实写入 |
| F4PIN-25 | SVD 补齐 | `stm32f407vg.svd` 的 GPIOA..GPIOE 新增 `OSPEEDR`(0x8)/`PUPDR`(0xC) |
| F4PIN-26 | 单测 | `f4_pin_map_packs_pupdr_and_ospeedr_for_input_and_af_pins`（AF/浮空/上拉/下拉/输出混合映射的 5 个寄存器精确值 + 生成代码字符串）；既有 `stm32f4_pin_mapping_packs_moder_afrl_afrh`、`f4_pin_map_covers_ports_cde_af_table_and_code`、`pin_map_gui_opens_cycles_and_generates_f4` 同步补 `OSPEEDR`/`PUPDR` 断言（含 GUI 循环顺序更新为 5 次点击到达 `USART1_TX`） |

**验证**：`cargo check --workspace` 退出 0、本地零警告；`cargo test --workspace` **674 项全绿**（含新增 1 项 ns-ide 引脚映射）。
剩余增强项：DCMI/FMC 无可靠引脚表，暂不建模。

## 1m. 引脚映射方案保存/载入（本轮，E2 子项）

§1l 之后引脚映射仍是**一次性**的：关掉模态或换项目就丢，重新配置要一遍一遍点引脚。
本次按仓库既有的 `.ns-ide/*.json` 持久化约定（watch / register-snapshot / bookmarks 同构）补齐方案的保存与恢复。

| # | 变更 | 说明 |
|---|---|---|
| F4PIN-27 | 方案模型 | `PinMapScheme`（`target`/`family`）+ `PinMapSchemeEntry`（`port`/`pin`/`signal`），`serde` derive；信号以**人类可读 label** 存储，enum 顺序变动也不失效 |
| F4PIN-28 | 快照与恢复 | `PinMapDialog::to_scheme` 收集全部已分配引脚；`apply_scheme` 先清空全部引脚，再按 label 在 `stm32f1_allowed_signals`/`stm32f4_allowed_signals` 中查该引脚的合法信号，返回（应用数, 跳过数） |
| F4PIN-29 | 保存 | `save_pin_map_scheme` 写 `.ns-ide/pin_map.json`（pretty JSON）；无项目/模态未打开/空分配/`validate_pin_mapping` 有冲突/建目录或写文件失败分别提示 |
| F4PIN-30 | 载入 | `load_pin_map_scheme`：文件缺失、JSON 损坏、`target`/`family` 与当前目标不符分别提示；成功后模态显示「已恢复 N 个引脚，跳过 M 个非法项」 |
| F4PIN-31 | 单测 | `pin_map_scheme_saves_and_loads_per_target`（F4 保存→清除→载入恢复 PA9/PB14、非法项跳过、损坏文件、目标不符拒绝、F1 同路径往返） |

**验证**：`cargo check --workspace` 退出 0、本地零警告；`cargo test --workspace` **675 项全绿**（含新增 1 项 ns-ide 引脚方案往返）。

## 1n. STM32F407 TIM2_CH3/CH4 补齐 + 外设分组一键分配/取消（本轮，E2 子项）

§1m 解决了方案的留存，但两个 F407 常规缺口仍在：TIM2 的 CH3/CH4（PA6/PA7，F407 常见 PWM 脚）在
AF 表里缺失；而分组配置（如整套 I2C1 或 TIM2）仍需逐个引脚点击，10 个引脚要点 10 次。

| # | 变更 | 说明 |
|---|---|---|
| F4PIN-32 | AF 表补齐 | `F4PinSignal` 新增 `Tim2Ch3`（PA6）/`Tim2Ch4`（PA7），`af()`=Some(1)、label `TIM2_CH3`/`TIM2_CH4`、`group()`=`TIM2`、颜色并入 TIMER 分支；`stm32f4_allowed_signals` 的 `('A',6)`/`('A',7)` 追加两变体，与既有 `Tim1Ch1N/Ch2N`、`Tim12Ch1/Ch2`、`Tim13Ch1`/`Tim14Ch1` 共存 |
| F4PIN-33 | 分组枚举 | `PinMapDialog::group_names()`：按当前 AF 表的信号顺序去重出分组名（`TIM2`/`I2C1`/…），排除 `GPIO`（无 AF、不参与分组） |
| F4PIN-34 | 一键分配/取消 | `is_group_present(group)` 判断该组是否已分配；`toggle_group(group) -> (cleared, changed, untouched)`：已分配 → 整组清除；否则把该组每个引脚设为其合法信号，**已被其他功能占用的引脚不动**，且同一信号（如 `I2c1Scl` 同时在 PB6/PB8）**只分配一次**（`used_f1`/`used_f4` 去重），重复项计入 `untouched` |
| F4PIN-35 | 界面 | `render_pin_map_groups` 在引脚网格上方渲染分组 chip 行（`pin-map-group-<GROUP>`，已分配分组玉色高亮）+ 说明文案；`IdeApp::toggle_pin_map_group` 按结果写 `dialog.message` 与状态栏（占用/冲突如实报告） |
| F4PIN-36 | 单测 | `f4_pin_map_group_toggle_assigns_and_clears`：I2C1 `(changed, untouched) == (2, 2)`（PB6/PB7 分配、PB8/PB9 因信号重复保持 `None`）；二次 toggle `cleared=true`、`changed=2`、`assigned_count()==0`；TIM2 `changed=5`（PA0/PA1/PA5=CH1_ETR/PA6/PA7）；TIM3 在 TIM2 之后 `changed=4`、`untouched=4`、PC9 保持 `None` |

**验证**：`cargo fmt --all -- --check` 无输出、`git diff --check` 退出 0；`cargo check --workspace` 退出 0、本地零警告；`cargo build --workspace` 退出 0；`cargo test --workspace` **676 项全绿**（含新增 1 项 ns-ide 分组分配）。

## 1o. 引脚映射图导入 CubeMX `.ioc`（本轮，E2 子项）

§1m 的方案保存/载入解决了**自己**的方案留存，但用户手上最常见的既有配置来自 CubeMX——
一份 `.ioc` 里有几十行引脚设置，逐个在图上点要几十次。工程导入路径（`ns_project::import`）
此前只取 `Mcu.Name`/`Mcu.IP` 做目标与外设提示，**不读引脚**。

| # | 变更 | 说明 |
|---|---|---|
| F4PIN-37 | 解析器 | 新增 `crates/ns-ide/src/pin_ioc.rs`（纯函数，无 UI 依赖）：`IocPinEntry{port,pin,raw}` + `IocPinMap{mcu,entries}`；`parse()` 只认 `P?[A-EI]N.Signal<N>` 与 `PA9.Mode` 形态，`Mcu.PartNumber`/`Mcu.Name`/`Mcu.Pin` 取 MCU 名（首个非 `None` 值生效），`Mcu.IP_*`/`ProjectManager.*`/`PA1.Lock`/注释/`[` 段一律忽略；`split_pin` 要求前缀以 `P` 开头、端口限 A–E/I、引脚 ≤ 15（`PA16` 拒绝） |
| F4PIN-38 | 同引脚多记录 | 同一引脚的 `Mode` 与 `Signal0` 合并后**取最后一个非 `GPIO` 的值**（CubeMX 对 GPIO 引脚同时写 `Signal0=GPIO` 与 `Mode=GPIO_Output`）；只有裸 `Signal0=GPIO` 时保留 `GPIO`，由 label 映射阶段判为「不是分配」并计入跳过 |
| F4PIN-39 | 命名映射 | `ioc_label()` 把 CubeMX 命名映射到本工具 label：`SYS_JTMS-SWO`→`SWDIO`、`SYS_JTCK-SWCLK`→`SWCLK`、`USB_OTG_FS_DM/DP` 与 `USB_OTG_HS_DM/DP`→`USB_DM`/`USB_DP`、`GPIO_Output`→`GPIO 输出`、`GPIO_Input_Pu`/`GPIO_Input_Pd`→`GPIO 输入·上拉`/`输入·下拉`（F1 无上下拉，回退 `GPIO 输入`）；复用信号名（`USART1_TX`、`TIM2_CH3`…）原样透传，随后仍须通过该引脚的合法信号校验 |
| F4PIN-40 | 应用 | `PinMapDialog::apply_ioc(mcu, entries)` 复用 `apply_scheme`（先清空全部引脚再逐项恢复）；MCU 名不含当前目标（如 F103 的 `.ioc` 给 F407 用）时**整体拒绝**并保留原分配；导入后仍跑 `validate_pin_mapping`，重复信号如实报冲突 |
| F4PIN-41 | 界面 | 引脚映射模态按钮行新增**「导入 .ioc」**（`pin-map-import-ioc`，紧随「保存方案」「载入方案」）；`IdeApp::import_pin_map_ioc` 弹文件选择（`prompt_for_paths`）后走 `import_pin_map_ioc_text`；未开模态/读文件失败/选择器报错分别如实提示 |
| F4PIN-42 | 单测 | `pin_ioc::tests`（3 项：信号/模式/MCU 名与 17 引脚计数、`Mode` 优先于裸 `GPIO`、非法前缀与越界引脚拒绝）+ `pin_map_imports_cubemx_ioc_and_reports_conflicts`（F407 导入 15 脚含 `SYS_JTMS-SWO`→SWDIO、`ETH_MII_CRS` 与裸 `GPIO` 各跳过 1 项；F103 `.ioc` 被拒且原分配不动；PB6/PB8 重复信号报冲突；F1 家族同路径导入 4 脚） |

**验证**：`cargo fmt --all -- --check` 无输出、`git diff --check` 退出 0；`cargo check --workspace` 退出 0、本地零警告（仅剩上游 `block`/`proc-macro-error2` future-incompat note）；`cargo build --workspace` 退出 0；`cargo test --workspace` **680 项全绿**（新增 4 项：解析器 3 + 应用 1）。

## 1p. 引脚映射图导出 CubeMX `.ioc` + 导入格式校正（本轮，E2 子项）

§1o 的导入器当时按 `PA9.Signal0=…`/`PA0.Mode=…` 兼容形态实现，但 **CubeMX 实际写的是
`Mode.PA9=GPIO` / `Signal.PA9=USART1_TX` / `SignalMode.PA9=AS` / `Pull.PA9=PUp`**——字段名在点前、
引脚名在点后。只认兼容形态意味着用户手上的真实 `.ioc` 导不进；而导出方向完全没有。

| # | 变更 | 说明 |
|---|---|---|
| F4PIN-43 | 原生格式解析 | `pin_ioc::parse` 改为**双形态**：原生 `Signal`/`Mode`/`SignalMode`/`Pull` 前缀（`split_pin` 解析点后引脚名，`Mcu.IP_*`/`ProjectManager.*`/`Locked`/`Unlock` 忽略）与兼容 `PA9.Signal<N>`/`PA9.Mode` 同源合并进 `PinFields`；每引脚取值优先级**外设信号 > GPIO 模式 > `SignalMode`/`Pull` 合成**（`RegularOutput`→`GPIO_Output`、`Input`→`GPIO_Input`、`Pull=PUp/PDown`→`GPIO_Input_Pu/Pd`）；只有裸 `GPIO`（无方向/上下拉）在**解析阶段剔除**，不再计入「未导入」 |
| F4PIN-44 | MCU 名 | `parse` 增认 `McuName`（CubeMX 无点键名）；`mcu_identity(target)` 返回 `(McuType, McuPackage, McuName)`（`stm32f407vg`→`STM32F407`/`LQFP100`/`STM32F407VGTx`，F103C6/C8/CB→`STM32F103`/`LQFP48`/`STM32F103CxTx`）；`pin_name(port,pin)` 拼 `PA9`/`PC13` |
| F4PIN-45 | 导出 | `PinMapDialog::to_ioc()` 写出 CubeMX 风格 `.ioc` 引脚段（`#MicroXplorer` 头 + `McuType`/`McuPackage`/`McuName` + 每引脚 `Mode=GPIO`/`Signal`/`SignalMode`/`Pull`/`Locked`/`Unlock`）；`ioc_export_signal`/`ioc_export_pull` 做 label→CubeMX 拼写逆映射（`SWDIO`→`SYS_JTMS-SWO`、`USB_DM`→`USB_OTG_FS_DM`、`GPIO 输出`→`SignalMode=RegularOutput`、输入·上拉/下拉→`Pull=PUp/PDown`）；`export_pin_map_ioc` 写项目根 `pin_map.ioc`，空分配/冲突/未知目标分别如实拒绝 |
| F4PIN-46 | 界面 | 按钮行新增**「导出 .ioc」**（`pin-map-export-ioc`，紧随「导入 .ioc」）；模态渲染测试 `renders_f4_pin_map_modal_without_panicking` 覆盖新按钮 |
| F4PIN-47 | 单测 | `pin_ioc::tests::parses_native_cubemx_pin_lines`（原生 5 引脚、`Mode`/`Signal`/`Pull` 合成、`Locked`/`Pull`-only 不建条目）+ `pin_map_exports_ioc_and_roundtrips_through_import`（导出 `pin_map.ioc` 含 `McuName`/`Signal.PA9`/`Pull.PA1`/`Signal.PA13=SYS_JTMS-SWO`，清空后导入**无损恢复** 4 脚；空分配拒绝且不写文件）；既有 `pin_map_imports_cubemx_ioc_and_reports_conflicts` 夹具改为原生格式（`F407_IOC` 用 `Mode.PA9`/`Signal.PA9`/`SignalMode`/`Pull`） |

**验证**：`cargo fmt --all -- --check` 无输出、`git diff --check` 退出 0；`cargo check --workspace` 退出 0、本地零警告（仅剩上游 `block`/`proc-macro-error2` future-incompat note）；`cargo build --workspace` 退出 0；`cargo test --workspace` **682 项全绿**（新增 2 项：原生解析 1 + 导出往返 1）。

> 导出只写引脚段：时钟树、外设实例与中断配置请用户在 CubeMX 中按需补全（本工具不生成这些段）。

## 1q. STM32F407 引脚映射生成代码外设时钟使能（本轮，E2 子项）

§1h 起 F407 生成代码只写 GPIO 端口的 `AHB1ENR`；复用脚对应的外设（USART/TIM/I2C/SPI/SDIO/CAN/USB）
没有使能自己的时钟，`pin_mapping_init()` 生成的代码在目标上不工作——GPIO 配置到位但外设时钟没开。

| # | 变更 | 说明 |
|---|---|---|
| F4PIN-48 | 时钟位汇总 | `stm32f4_peripheral_clocks(dialog) -> (ahb1, apb1, apb2)` 按已分配信号的 `group()` 汇总 STM32F407 外设时钟位：APB2（USART1 bit4、USART6 bit5、SPI1 bit12、TIM1 bit0、TIM8 bit1、TIM9 bit16/TIM10 bit17/TIM11 bit18、SDIO bit11）、APB1（USART2 bit17/USART3 bit18、SPI2 bit14/SPI3 bit15、I2C1 bit21/I2C2 bit22/I2C3 bit23、TIM2 bit0/TIM3 bit1/TIM4 bit2/TIM5 bit3、TIM12 bit6/TIM13 bit7/TIM14 bit8、CAN1 bit25）、AHB1（USB_OTG_FS bit7）；GPIO 与 SWD 不参与 |
| F4PIN-49 | 代码输出 | `stm32f4_pin_mapping_code` 每端口写 `RCC->AHB1ENR |= (GPIO 位 | USB 位)`，循环后按需写 `RCC->APB1ENR |= …` 与 `RCC->APB2ENR |= …`（仅非零才发），头注释同步 |
| F4PIN-50 | 单测 | `f4_pin_mapping_code_enables_peripheral_clocks`：USART1+SDIO→`APB2ENR |= 0x00000810`、I2C1+TIM2→`APB1ENR |= 0x00200001`、GPIOA+USB→`AHB1ENR |= 0x00000081`、SWD 不触发任何时钟位 |

**验证**：`cargo fmt --all -- --check` 无输出、`git diff --check` 退出 0；`cargo check --workspace` 退出 0、本地零警告（仅剩上游 `block`/`proc-macro-error2` future-incompat note）；`cargo build --workspace` 退出 0；`cargo test --workspace` **683 项全绿**（新增 1 项 ns-ide 时钟使能）。

## 1r. 引脚映射生成代码自包含寄存器定义（本轮，E2 子项）

§1f/§1h 起生成的 `src/pin_mapping.c` 引用 `GPIOA->…`/`RCC->…`/`AFIO->…`，但从未定义任何寄存器视图或
基址——文件**无法独立编译**，只能靠用户另行提供厂商头；§1q 补 `APB1ENR`/`APB2ENR` 后未定义引用更多。

| # | 变更 | 说明 |
|---|---|---|
| F4PIN-51 | F1 前奏 | `STM32F1_REGISTER_PRELUDE`：`ns_gpio_f1_t`（CRL/CRH/IDR/ODR/BSRR/BRR/LCKR）、`ns_afio_t`（EVCR/MAPR/EXTICR/MAPR2）、`ns_rcc_f1_t`（CR..CSR，APB2ENR@0x18/APB1ENR@0x1C），`#ifndef` 保护 `GPIOA`(0x40010800)/`GPIOB`(0x40010C00)/`GPIOC`(0x40011000)/`RCC`(0x40021000)/`AFIO`(0x40010000) |
| F4PIN-52 | F4 前奏 | `STM32F4_REGISTER_PRELUDE`：`ns_gpio_t`（MODER/OTYPER/OSPEEDR/PUPDR/IDR/ODR/BSRR/LCKR/AFRL/AFRH）、`ns_rcc_t`（CR/PLLCFGR/CFGR/CIR/RSTR×4/AHB1ENR@0x20/AHB2ENR/APB1ENR@0x28/APB2ENR@0x2C），`#ifndef` 保护 `GPIOA..GPIOE`（0x40020000+0x400·n）与 `RCC`（0x40023800） |
| F4PIN-53 | 接入 | F1/F4 的 `pin_mapping_code` 在 `#include <stdint.h>` 后注入前奏；`ns_` 前缀避免 CMSIS 撞名，`#ifndef` 让已含厂商头的项目由厂商宏获胜（本文件仍用同一 `GPIOA->MODER`/`RCC->APB1ENR` 写法，未含头时独立可编译） |
| F4PIN-54 | 单测 | `pin_mapping_code_is_self_contained`：F1 含 `ns_gpio_f1_t`/`ns_rcc_f1_t`/`#define AFIO ((ns_afio_t *)0x40010000u)`/`#ifndef GPIOA`；F4 含 `ns_gpio_t`/`ns_rcc_t`/`#define GPIOE ((ns_gpio_t *)0x40021000u)`/`#ifndef RCC` |

**验证**：`cargo fmt --all -- --check` 无输出、`git diff --check` 退出 0；`cargo check --workspace` 退出 0、本地零警告（仅剩上游 `block`/`proc-macro-error2` future-incompat note）；`cargo build --workspace` 退出 0；`cargo test --workspace` **94 项全绿**（不含沙箱限制的 2 个 socket 测试）（新增 1 项 ns-ide 自包含）。

## 1s. 时钟树生成代码自包含 RCC 定义（本轮，E2 子项）

§1e/§1g 的 `generate_clock_tree_code` 生成 `src/clock_init.c`，引用 `RCC->CR`/`RCC->PLLCFGR`/`RCC->CFGR`
却无寄存器定义——与 §1r 修好的 `pin_mapping.c` 同缺口，文件无法独立编译。

| # | 变更 | 说明 |
|---|---|---|
| F4PIN-55 | F1 前奏 | `STM32F1_CLOCK_PRELUDE`：`ns_rcc_f1_t`（CR..CSR，APB2ENR@0x18/APB1ENR@0x1C）＋ `#ifndef` 保护的 `RCC`（0x40021000），只带时钟文件用到的寄存器 |
| F4PIN-56 | F4 前奏 | `STM32F4_CLOCK_PRELUDE`：`ns_rcc_t`（CR..APB2ENR@0x2C，PLLCFGR@0x04）＋ `#ifndef RCC`（0x40023800），与 `pin_mapping.c` 的 RCC 布局一致 |
| F4PIN-57 | 接入 | `generate_clock_tree_code` 在 `#include <stdint.h>` 后按家族注入前奏；`ns_` 前缀避免 CMSIS 撞名，`#ifndef` 让已含厂商头的项目由厂商宏获胜 |
| F4PIN-58 | 单测 | 既有 `clock_tree_gui_opens_cycles_and_generates`（F1）与 `f4_clock_tree_gui_opens_cycles_and_generates`（F4）补断言 `ns_rcc_f1_t`/`ns_rcc_t` 与 `#ifndef RCC` |

**验证**：`cargo fmt --all -- --check` 无输出、`git diff --check` 退出 0；`cargo check --workspace` 退出 0、本地零警告（仅剩上游 `block`/`proc-macro-error2` future-incompat note）；`cargo build --workspace` 退出 0；`cargo test --workspace` **94 项全绿**（不含沙箱限制的 2 个 socket 测试）

## 2. 需要修复（已存在但未接通 / 有缺陷）

| # | 位置 | 问题 | 影响 |
|---|---|---|---|
| R1 | `ns-debug/src/embedded.rs` | 嵌入式目标 attach（OpenOCD/ST-Link/J-Link/pyOCD/probe-rs + `target remote` GDB）**已实现但未验证** | 需真实硬件，本机无法确认行为正确 |

## 3. 需要完善（已有基础但覆盖面不足）

| # | 模块 | 现状 | 期望 |
|---|---|---|---|
| E1 | RTOS 感知调试（F7） | FreeRTOS 模板 `tasks[i]` 求值 + **真实内核链表遍历**（`pxCurrentTCB`/`xReadyTasksLists` 等，DAP `evaluate` 往返）已接入；`ns_debug::rtos::walk_free_rtos` 纯函数仅被自身单测覆盖 | **已完成（本轮）**：修复启动门槛 off-by-one（`>=7` → `REAL_KERNEL_LIST_COUNT=6`），并补 DAP 往返路径集成测试 `real_kernel_dap_walk_matches_pure_walker`，断言与 `walk_free_rtos` 在同一镜像输出一致；仍需真实内核符号做硬件级终验（见 R1/下方说明） |
| E2 | 图形化外设配置（F11） | 列表式外设勾选 + GPIO 引脚模式循环 + UART 波特率 + **时钟树图形化**（§1e F1 / §1g F4）+ **引脚映射图**（§1f F103 / §1h F407 / §1i 端口补齐 / §1j 外设补齐 / §1k I2C3+TIM12-14 / §1l 输入模式+OSPEEDR/PUPDR / §1m 方案保存·载入 / §1n TIM2_CH3/CH4+分组一键分配 / §1o .ioc 导入 / §1p 导出+格式校正 / §1q 外设时钟使能 / §1r 自包含寄存器定义 / §1s 时钟自包含 RCC）已接入 | **已完成（本轮）**：时钟树（`clock_tree_gui`，F1+F4）、STM32F103C6/C8/CB LQFP48 与 STM32F407 LQFP100（GPIOA..GPIOE，含 SPI3/USART6/TIM8/TIM1 互补 + SDIO + TIM9/10/11 + I2C3 + TIM12-14 + GPIO 上拉/下拉 + OSPEEDR/PUPDR + 方案保存/载入 + TIM2_CH3/CH4 + 外设分组一键分配/取消 + .ioc 导出 + 外设时钟使能 + 自包含寄存器定义 + 时钟自包含 RCC）引脚映射图（`pin_map_gui`）均已落地；其他 STM32 封装/系列 AF 表扩展为后续增强 |
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

本轮已完成：**G1 寄存器手动刷新**、**G2 保存所有文件**（见 §1c）、**E1 真实内核遍历启动修复 + DAP/纯函数一致性测试**（见 §1d）、**E2 时钟树图形化配置（F1+F4）+ STM32F103C6/C8/CB 与 STM32F407（GPIOA..GPIOE + SDIO/TIM9-11 + I2C3/TIM12-14 + GPIO 上拉/下拉 + OSPEEDR/PUPDR + 方案保存/载入 + TIM2_CH3/CH4 + 外设分组一键分配/取消 + .ioc 导出 + 外设时钟使能 + 自包含寄存器定义 + 时钟自包含 RCC）引脚映射图**（见 §1e/§1f/§1g/§1h/§1i/§1j/§1k/§1l/§1m/§1n/§1o/§1p/§1q/§1r/§1s）、**向导仿真文案修正**（见 §1g）。
剩余项不变：

1. **R1**：嵌入式目标 attach 需硬件验证（标注「未验证」），不阻塞。
2. **E1**：真实内核遍历的**硬件级终验**需真实 FreeRTOS 符号（TCB/链表布局与默认 `RtosLayout` 偏移一致）；DAP 往返路径与纯函数一致性已由集成测试在本机验证。
3. **E3**：多核调试为长期增强，当前版本如实标注为未完成。
4. 验证：`cargo check` 零警告（退出 0、本地 0 警告）、`cargo test --workspace` **94 项全绿**（不含沙箱限制的 2 个 socket 测试）、启动应用走通关键路径。
