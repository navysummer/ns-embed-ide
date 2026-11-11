//! Command palette (F13/F14 命令面板).
//!
//! Static list of IDE commands + a small subsequence fuzzy matcher.
//! Matching is case-insensitive; consecutive matches and prefix matches are
//! scored higher.

/// One entry shown in the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandEntry {
    /// Stable identifier used to dispatch the command.
    pub id: &'static str,
    /// Title shown in the list.
    pub title: &'static str,
    /// Shortcut hint shown on the right.
    pub keys: &'static str,
    /// Extra terms the query may match against.
    pub keywords: &'static str,
}

pub const COMMANDS: &[CommandEntry] = &[
     CommandEntry { id: "new-project", title: "新建项目", keys: "cmd/ctrl+n", keywords: "new create project 新建 创建" },
    CommandEntry { id: "import-project", title: "导入工程（IAR/Keil/STM32CubeMX）…", keys: "", keywords: "import iar keil stm32cubemx ewp uvprojx ioc 导入 工程 迁移" },
     CommandEntry { id: "open-project", title: "打开项目…", keys: "cmd/ctrl+o", keywords: "open project 打开 项目" },
    CommandEntry { id: "save", title: "保存当前文件", keys: "cmd/ctrl+s", keywords: "save 保存" },
    CommandEntry { id: "build", title: "构建项目", keys: "cmd/ctrl+b", keywords: "build compile 构建 编译 b" },
    CommandEntry { id: "rebuild", title: "重新构建（清理后构建）", keys: "", keywords: "rebuild clean rebuild 重新 构建 清理" },
    CommandEntry { id: "run", title: "直接运行", keys: "", keywords: "run 直接运行 run-after-build 运行 启动 执行" },
    CommandEntry { id: "clean", title: "清理构建（删除输出目录）", keys: "", keywords: "clean build output 清理 构建 输出" },
    CommandEntry { id: "stop-run", title: "停止运行", keys: "", keywords: "stop run 停止 运行" },
    CommandEntry { id: "debug", title: "开始调试（构建并启动）", keys: "f5", keywords: "debug f5 调试 启动" },
    CommandEntry { id: "embedded-debug", title: "嵌入式调试（目标 attach）", keys: "", keywords: "embedded debug target attach openocd stlink jlink pyocd probe-rs 嵌入式 调试 目标" },
    CommandEntry { id: "stop-debug", title: "停止调试", keys: "shift+f5", keywords: "stop debug 停止 调试" },
    CommandEntry { id: "restart-debug", title: "重启调试", keys: "", keywords: "restart debug 重启 调试 重新" },
    CommandEntry { id: "run-to-cursor", title: "运行到光标", keys: "cmd/ctrl+f10", keywords: "run to cursor continueTo until 运行到光标 执行到" },
    CommandEntry { id: "find", title: "全局文件搜索…", keys: "cmd/ctrl+shift+f", keywords: "find search 搜索 查找" },
    CommandEntry { id: "toggle-output", title: "显示/隐藏输出面板", keys: "cmd/ctrl+j", keywords: "output 输出 面板 panel" },
    CommandEntry { id: "git", title: "显示 Git 面板", keys: "", keywords: "git version control 版本 控制 提交 提交 vcs" },
    CommandEntry { id: "static-analysis", title: "静态分析", keys: "", keywords: "static analysis lint clang-tidy clippy 静态 分析 检查" },
    CommandEntry { id: "terminal", title: "打开终端", keys: "", keywords: "terminal shell console 终端 控制台 shell" },
    CommandEntry { id: "serial-monitor", title: "打开串口助手", keys: "", keywords: "serial monitor uart 串口 助手 监视器 波特率 端口" },
    CommandEntry { id: "agent", title: "打开 Agent 面板", keys: "", keywords: "agent task subagent 任务 子代理 代理 面板 zed" },
    CommandEntry { id: "breakpoints", title: "显示断点面板", keys: "", keywords: "breakpoints 断点 面板" },
    CommandEntry { id: "clear-breakpoints", title: "清除全部断点", keys: "cmd/ctrl+shift+f9", keywords: "clear breakpoints 清除 断点" },
    CommandEntry { id: "add-watchpoint", title: "添加观察点（数据断点）…", keys: "", keywords: "watchpoint data breakpoint 观察点 数据断点 监视" },
    CommandEntry { id: "close-tab", title: "关闭当前标签页", keys: "cmd/ctrl+w", keywords: "close tab 关闭 标签" },
    CommandEntry { id: "toggle-diagnostics", title: "显示问题面板", keys: "", keywords: "problems diagnostics errors 问题 错误" },
    CommandEntry { id: "focus-next-tab", title: "切换到下一个标签页", keys: "ctrl+tab", keywords: "next tab 下一个 标签" },
    CommandEntry { id: "restart-run", title: "重新运行", keys: "", keywords: "restart rerun run again 重新 运行 重启" },
    CommandEntry { id: "settings", title: "项目设置…", keys: "cmd/ctrl+,", keywords: "settings preferences project 设置 偏好 项目配置" },
    CommandEntry { id: "flash", title: "烧写固件", keys: "cmd/ctrl+shift+f10", keywords: "flash program burn firmware 烧写 固件 下载" },
    CommandEntry { id: "flash-erase", title: "擦除芯片", keys: "", keywords: "flash erase chip 擦除 芯片 烧写" },
    CommandEntry { id: "flash-verify", title: "校验固件", keys: "", keywords: "flash verify 校验 固件 烧写" },
    CommandEntry { id: "flash-read", title: "读回固件（保存 BIN）", keys: "", keywords: "flash read readback dump 读回 固件 保存" },
    CommandEntry { id: "flash-and-run", title: "烧写并运行", keys: "", keywords: "flash run flash-and-run 烧写 运行 复位" },
     CommandEntry { id: "flash-and-debug", title: "烧写并调试", keys: "", keywords: "flash debug flash-and-debug 烧写 调试" },
     CommandEntry { id: "flash-and-verify", title: "烧写并校验", keys: "", keywords: "flash verify flash-and-verify 烧写 校验 链" },
    CommandEntry { id: "toggle-build-config", title: "切换构建配置（debug ↔ release）", keys: "", keywords: "build config toggle debug release 构建 配置 切换" },
     CommandEntry { id: "generate-device-files", title: "生成设备文件（链接脚本/启动文件）", keys: "", keywords: "device startup linker ld svd 设备 启动文件 链接脚本 生成" },
    CommandEntry { id: "include-graph", title: "include 依赖图（项目级）", keys: "", keywords: "include graph dependency deps 依赖 图 头文件 include 包含 分析" },
     CommandEntry { id: "memory-layout", title: "内存布局视图（链接脚本区域/段占用）", keys: "", keywords: "memory layout map linker regions sections 内存 布局 区域 段 占用 可视化" },
     CommandEntry { id: "generate-peripheral-init", title: "生成外设初始化代码（SVD 配置）…", keys: "", keywords: "peripheral init code generate svd cube 外设 初始化 生成 配置 gpio uart" },
     CommandEntry { id: "go-to-definition", title: "跳转到定义", keys: "f12", keywords: "go to definition jump 跳转 定义 声明" },
    CommandEntry { id: "goto-header-source", title: "切换到头文件/源文件", keys: "", keywords: "go to header source switch counterpart 切换 头文件 源文件 .h .c" },
    CommandEntry { id: "signature-help", title: "签名帮助", keys: "cmd/ctrl+shift+space", keywords: "signature help parameter args 签名 帮助 参数" },
    CommandEntry { id: "inlay-hints", title: "内联提示", keys: "", keywords: "inlay hints 内联 提示 type param" },
    CommandEntry { id: "semantic-tokens", title: "语义着色", keys: "", keywords: "semantic tokens color 语义 着色 token" },
    CommandEntry { id: "show-completion", title: "触发代码补全", keys: "cmd/ctrl+space", keywords: "completion autocomplete 补全 自动 完成" },
    CommandEntry { id: "toggle-outline", title: "显示/隐藏大纲（符号列表）", keys: "", keywords: "outline symbols document symbols 大纲 符号 列表 导航 breadcrumb" },
    CommandEntry { id: "go-to-line", title: "跳转到行…", keys: "cmd/ctrl+g", keywords: "go to line goto jump line 跳转 行 定位 gotoLine" },
    CommandEntry { id: "document-symbols", title: "刷新文档符号", keys: "", keywords: "document symbols refresh outline 刷新 文档 符号 大纲" },
    CommandEntry { id: "simulation", title: "仿真运行（内建指令级）", keys: "", keywords: "simulation simulate simulator 仿真 模拟 指令级 run" },
    CommandEntry { id: "format-document", title: "格式化文档", keys: "shift+alt+f", keywords: "format document formatting 格式化 文档 排版" },
    CommandEntry { id: "code-action", title: "快速修复（代码动作）", keys: "", keywords: "code action quickfix fix 快速修复 代码动作 诊断" },
    CommandEntry { id: "toggle-theme", title: "切换主题（深/浅色）", keys: "", keywords: "theme light dark toggle 主题 浅色 深色 切换" },
    CommandEntry { id: "new-window", title: "新建窗口", keys: "", keywords: "new window open 新建 窗口 多窗口 workspace 工作区" },
    CommandEntry { id: "keybindings", title: "快捷键设置…", keys: "", keywords: "keybindings shortcuts keys hotkeys 快捷键 键位 键绑定 自定义" },
    CommandEntry { id: "split-right", title: "编辑器分屏（右侧）", keys: "", keywords: "split pane editor divide 分屏 分割 视图 双栏 side by side" },
    CommandEntry { id: "close-split", title: "关闭分屏", keys: "", keywords: "split close pane 关闭 分屏 分割 视图" },
    CommandEntry { id: "run-tests", title: "运行单元测试", keys: "", keywords: "run test tests unittest cargo test ctest make test 单元测试 测试 运行" },
     CommandEntry { id: "stack-usage", title: "栈使用分析（-fstack-usage）", keys: "", keywords: "stack usage fstack analyze 栈 使用 分析 内存" },
     CommandEntry { id: "coverage", title: "覆盖率分析（gcov）", keys: "", keywords: "coverage gcov line cover 覆盖率 行覆盖 测试 分析" },
    CommandEntry { id: "preprocess", title: "宏展开/预处理输出（C/C++ -E）", keys: "", keywords: "preprocess macro expand cpp e preprocessor 宏 展开 预处理 输出" },
     CommandEntry { id: "recent-files", title: "最近打开的文件…", keys: "cmd/ctrl+e", keywords: "recent files history 最近 打开 文件 历史 导航" },
     CommandEntry { id: "tasks", title: "任务面板（TODO/FIXME 扫描）", keys: "", keywords: "todo fixme xxx hack bug task marker 任务 待办 标记 扫描 面板" },
 ];

/// Rank `COMMANDS` for the given query.
///
/// Returns indices into `COMMANDS`, best first. An empty query returns all
/// commands in declaration order.
pub fn rank(query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return (0..COMMANDS.len()).collect();
    }
    let mut scored: Vec<(usize, i64)> = COMMANDS
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            fuzzy_score(&format!("{} {}", entry.title, entry.keywords), &query).map(|score| (index, score))
        })
        .collect();
    scored.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    scored.into_iter().map(|(index, _)| index).collect()
}

/// Score a candidate against a (already lowercased, trimmed) query.
///
/// Subsequence match: all query chars must appear in order. Consecutive runs
/// and an early/first match are worth more; big gaps between matches are
/// penalized.
fn fuzzy_score(candidate: &str, query: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let candidate = candidate.to_lowercase();
    let candidate_chars: Vec<char> = candidate.chars().collect();
    let query_chars: Vec<char> = query.chars().collect();

    let mut score: i64 = 0;
    let mut q = 0usize;
    let mut previous_match: Option<usize> = None;
    let mut run = 0i64;

    for (index, &character) in candidate_chars.iter().enumerate() {
        if q < query_chars.len() && character == query_chars[q] {
            score += 10;
            if let Some(previous) = previous_match {
                if index == previous + 1 {
                    run += 1;
                    score += 15 + run * 5;
                } else {
                    run = 0;
                    score -= 2;
                }
            } else {
                // First matched char: earlier is better, first char is ideal.
                score += (64 - index.min(64)) as i64;
                if index == 0 {
                    score += 40;
                }
            }
            previous_match = Some(index);
            q += 1;
        }
    }

    if q == query_chars.len() {
        Some(score)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_returns_all_commands() {
        assert_eq!(rank(""), (0..COMMANDS.len()).collect::<Vec<_>>());
        assert_eq!(rank("   "), (0..COMMANDS.len()).collect::<Vec<_>>());
    }

    /// Index of the entry with the given stable id in `COMMANDS`.
    fn index_of(id: &str) -> usize {
        COMMANDS
            .iter()
            .position(|command| command.id == id)
            .expect("command id must exist")
    }

    #[test]
    fn exact_title_wins() {
        let ranked = rank("构建项目");
        assert_eq!(ranked.first().copied(), Some(index_of("build")));
    }

    #[test]
    fn subsequence_matches_multiple() {
        let ranked = rank("断点");
        assert!(ranked.contains(&index_of("breakpoints")));
        assert!(ranked.contains(&index_of("clear-breakpoints")));
    }

    #[test]
    fn keywords_match() {
        let ranked = rank("f5");
        assert!(ranked.contains(&index_of("debug")));
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(rank("SAVE"), rank("save"));
    }

    #[test]
    fn no_match_returns_empty() {
        assert!(rank("zzzqqq无此命令").is_empty());
    }

    #[test]
    fn prefix_beats_scattered() {
        let ranked = rank("run");
        assert_eq!(ranked[0], index_of("run"));
    }

    #[test]
    fn flash_commands_are_ranked() {
        let ranked = rank("烧写");
        assert!(ranked.contains(&index_of("flash")));
         assert!(ranked.contains(&index_of("flash-and-run")));
         assert!(ranked.contains(&index_of("flash-and-debug")));
        assert!(ranked.contains(&index_of("flash-and-verify")));
     }
 }
