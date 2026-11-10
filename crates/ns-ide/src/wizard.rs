use std::path::PathBuf;

use gpui::{
    AnyElement, App, BoxShadow, Context, Entity, Focusable, MouseButton, SharedString, Stateful,
    Window, div, prelude::*, px,
};
use ns_project::{Language, NewProjectSpec, toolchain};

use crate::app::IdeApp;
use crate::text_field::TextField;
use crate::{theme, widgets};

pub const STEPS: [&str; 9] = [
    "项目信息",
    "编程语言",
    "项目模板",
    "编译器",
    "仿真软件",
    "调试软件",
    "烧写软件",
    "目标设备",
    "确认生成",
];

pub struct Wizard {
    pub step: usize,
    pub name: Entity<TextField>,
    pub location: Entity<TextField>,
    pub language: Language,
    pub compiler: String,
    pub simulator: String,
    pub debugger: String,
    pub flasher: String,
    pub target: String,
    /// C starter template (LED Blink / UART Loopback / empty).
    pub c_template: ns_project::CTemplate,
    pub error: Option<String>,
    /// Memoized `tool --version` output per tool name, so the wizard does not
    /// spawn a subprocess on every frame while the toolchain step is visible.
    versions: std::cell::RefCell<std::collections::HashMap<String, Option<String>>>,
    /// Whether the target-device dropdown is open.
    pub target_menu_open: bool,
}

impl Wizard {
    pub fn new(cx: &mut App) -> Self {
        let default_location = default_projects_dir().display().to_string();
        let name = cx.new(|cx| TextField::new("例如 blinky", cx));
        let location = cx.new(|cx| {
            let mut field = TextField::new("项目保存目录", cx);
            field.set_text(default_location, cx);
            field
        });
        let language = Language::C;
        let compiler = toolchain::preferred_or_first(&toolchain::compilers(language))
            .unwrap_or_else(|| "gcc".to_string());
        let simulator =
            toolchain::preferred_or_first(&toolchain::simulators(language)).unwrap_or_default();
        let debugger =
            toolchain::preferred_or_first(&toolchain::debuggers(language)).unwrap_or_default();
        let flasher =
            toolchain::preferred_or_first(&toolchain::flashers(language)).unwrap_or_default();
        let target = toolchain::TARGETS
            .first()
            .map(|(id, _)| (*id).to_string())
            .unwrap_or_default();

        Self {
            step: 0,
            name,
            location,
            language,
            compiler,
            simulator,
            debugger,
            flasher,
            target,
            target_menu_open: false,
            c_template: ns_project::CTemplate::Empty,
            error: None,
            versions: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
    }

    pub fn set_language(&mut self, language: Language) {
        self.language = language;
        self.compiler = toolchain::preferred_or_first(&toolchain::compilers(language))
            .unwrap_or_default();
        self.simulator =
            toolchain::preferred_or_first(&toolchain::simulators(language)).unwrap_or_default();
        self.debugger =
            toolchain::preferred_or_first(&toolchain::debuggers(language)).unwrap_or_default();
        self.flasher =
            toolchain::preferred_or_first(&toolchain::flashers(language)).unwrap_or_default();
        self.error = None;
    }

    /// Display subtitle for a tool: installed path plus detected version (memoized),
    /// or the install hint when the tool is missing.
    fn tool_subtitle(&self, tool: &toolchain::Tool) -> Option<String> {
        let Some(path) = tool.path.as_ref() else {
            return toolchain::missing_guide(&tool.name).map(str::to_string);
        };
        let mut cache = self.versions.borrow_mut();
        let version = cache
            .entry(tool.name.clone())
            .or_insert_with(|| tool.version());
        let version = version.clone().unwrap_or_default();
        if version.is_empty() {
            Some(path.display().to_string())
        } else {
            Some(format!("{} · {}", path.display(), version))
        }
    }

    fn tool_available(&self, tool: &toolchain::Tool) -> bool {
        tool.available()
    }

    pub fn spec(&self, cx: &App) -> NewProjectSpec {
        NewProjectSpec {
            name: self.name.read(cx).text().trim().to_string(),
            location: PathBuf::from(self.location.read(cx).text().trim()),
            language: self.language,
            compiler: self.compiler.clone(),
            simulator: self.simulator.clone(),
            debugger: self.debugger.clone(),
            flasher: self.flasher.clone(),
            target: if self.target == "custom" {
                String::new()
            } else {
                self.target.clone()
            },
            svd: if self.target == "custom" || self.target.is_empty() {
                String::new()
            } else {
                self.target.clone()
            },
            c_template: if self.language == Language::C {
                Some(self.c_template)
            } else {
                None
            },
        }
    }

    pub fn validate(&self, cx: &App) -> Option<String> {
        if self.name.read(cx).text().trim().is_empty() {
            return Some("请填写项目名称".to_string());
        }
        if self.location.read(cx).text().trim().is_empty() {
            return Some("请选择项目保存位置".to_string());
        }
        None
    }

    pub fn render(&self, cx: &mut Context<IdeApp>) -> AnyElement {
        let step = self.step.min(STEPS.len() - 1);
        let content = match step {
            0 => self.render_basics(cx),
            1 => self.render_languages(cx),
            2 => self.render_template(cx),
            3 => self.render_tool_step(
                cx,
                "编译器",
                "按语言列出可用的编译器；未检测到的工具也可以先选择，稍后安装。",
                toolchain::compilers(self.language)
                    .into_iter()
                    .map(|tool| {
                        let subtitle = self.tool_subtitle(&tool);
                        let available = self.tool_available(&tool);
                        (tool.name, subtitle, available)
                    })
                    .collect(),
                StepKind::Compiler,
                false,
            ),
            4 => self.render_tool_step(
                cx,
                "仿真软件",
                "选择仿真器，或暂时跳过。仿真能力将在后续里程碑接入。",
                toolchain::simulators(self.language)
                    .into_iter()
                    .map(|tool| {
                        let subtitle = self.tool_subtitle(&tool);
                        let available = self.tool_available(&tool);
                        (tool.name, subtitle, available)
                    })
                    .collect(),
                StepKind::Simulator,
                true,
            ),
            5 => self.render_tool_step(
                cx,
                "调试软件",
                "选择 GDB / LLDB 或厂商调试器，用于之后的断点与寄存器视图。",
                toolchain::debuggers(self.language)
                    .into_iter()
                    .map(|tool| {
                        let subtitle = self.tool_subtitle(&tool);
                        let available = self.tool_available(&tool);
                        (tool.name, subtitle, available)
                    })
                    .collect(),
                StepKind::Debugger,
                true,
            ),
            6 => self.render_tool_step(
                cx,
                "烧写软件",
                "选择 OpenOCD / pyOCD / J-Link 等烧写器。",
                toolchain::flashers(self.language)
                    .into_iter()
                    .map(|tool| {
                        let subtitle = self.tool_subtitle(&tool);
                        let available = self.tool_available(&tool);
                        (tool.name, subtitle, available)
                    })
                    .collect(),
                StepKind::Flasher,
                true,
            ),
            7 => self.render_targets(cx),
            _ => self.render_summary(cx),
        };

        let footer = div()
            .h(px(64.))
            .px(px(22.))
            .flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(theme::alpha(theme::STROKE, 0.12))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        widgets::ghost_button("wizard-cancel", "取消")
                            .on_click(cx.listener(|this, _, _, cx| this.wizard_cancel(cx))),
                    )
                    .child(
                        widgets::ghost_button("wizard-back", "上一步")
                            .on_click(cx.listener(|this, _, _, cx| this.wizard_back(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .child(format!("第 {} / {} 步", step + 1, STEPS.len())),
                    )
                    .child(
                        widgets::primary_button(
                            "wizard-next",
                            if step == STEPS.len() - 1 {
                                "创建项目"
                            } else {
                                "下一步"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.wizard_next(cx))),
                    ),
            );

        let sidebar = div()
            .w(px(176.))
            .h_full()
            .py(px(18.))
            .px(px(14.))
            .flex()
            .flex_col()
            .gap(px(3.))
            .border_r_1()
            .border_color(theme::alpha(theme::STROKE, 0.1))
            .children(STEPS.iter().enumerate().map(|(index, label)| {
                let active = index == step;
                let done = index < step;
                div()
                    .h(px(34.))
                    .px(px(9.))
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .rounded(px(8.))
                    .bg(if active {
                        theme::alpha(theme::ACCENT, 0.1)
                    } else {
                        theme::alpha(0xffffff, 0.0)
                    })
                    .child(widgets::status_dot(
                        if active {
                            theme::color(theme::ACCENT)
                        } else if done {
                            theme::color(theme::JADE)
                        } else {
                            theme::alpha(theme::TEXT_FAINT, 0.5)
                        },
                        if active { 6. } else { 4. },
                    ))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(if active {
                                theme::color(theme::TEXT)
                            } else if done {
                                theme::color(theme::TEXT_DIM)
                            } else {
                                theme::color(theme::TEXT_FAINT)
                            })
                            .child(format!("{}. {}", index + 1, label)),
                    )
            }));

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::color(0x080604).opacity(0.72))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, _| {})
            .child(
                div()
                    .w(px(790.))
                    .h(px(620.))
                    .flex()
                    .flex_col()
                    .rounded(px(20.))
                    .bg(theme::color(theme::PANEL_SOLID))
                    .border_1()
                    .border_color(theme::alpha(theme::STROKE, 0.22))
                    .shadow(vec![BoxShadow {
                        color: theme::color(0x000000).opacity(0.65),
                        offset: gpui::point(px(0.), px(24.)),
                        blur_radius: px(60.),
                        spread_radius: px(0.),
                    }])
                    // Modal title
                    .child(
                        div()
                            .h(px(84.))
                            .px(px(24.))
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(theme::alpha(theme::STROKE, 0.12))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(5.))
                                    .child(
                                        div()
                                            .text_size(px(19.))
                                            .font_family(theme::SERIF_FONT)
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .text_color(theme::color(theme::TEXT))
                                            .child("新建项目"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child("按照 语言 → 工具链 → 目标 的顺序建立工程"),
                                    ),
                            )
                            .child(widgets::brand_mark(38.)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .child(sidebar)
                            .child(
                                div()
                                    .id("wizard-body")
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .overflow_y_scroll()
                                    .px(px(26.))
                                    .py(px(24.))
                                    .child(content),
                            ),
                    )
                    .child(footer),
            )
            .into_any_element()
    }

    fn render_basics(&self, cx: &mut Context<IdeApp>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(self.heading(
                "先给项目一个名字",
                "项目会生成独立的 project.toml，所有工具链配置都将写入其中。",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(7.))
                    .child(widgets::section_label("项目名称"))
                    .child(self.name.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(7.))
                    .child(widgets::section_label("保存位置"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().child(self.location.clone()))
                            .child(
                                widgets::ghost_button("wizard-pick-location", "浏览…")
                                    .on_click(cx.listener(|this, _, _, cx| this.pick_location(cx))),
                            ),
                    ),
            )
            .child(
                div()
                    .rounded(px(11.))
                    .bg(theme::alpha(theme::ACCENT, 0.06))
                    .border_1()
                    .border_color(theme::alpha(theme::ACCENT, 0.16))
                    .px(px(14.))
                    .py(px(11.))
                    .text_size(px(11.5))
                    .text_color(theme::color(theme::TEXT_DIM))
                    .child(format!(
                        "将创建：{}/{}",
                        self.location.read(cx).text().trim(),
                        self.name.read(cx).text().trim()
                    )),
            )
            .into_any_element()
    }

    fn render_languages(&self, cx: &mut Context<IdeApp>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(self.heading(
                "选择编程语言",
                "语言决定默认编译器、语法高亮与构建后端。",
            ))
            .child(
                div()
                    .grid()
                    .grid_cols(2)
                    .gap(px(10.))
                    .children(Language::ALL.into_iter().enumerate().map(|(index, language)| {
                        let selected = language == self.language;
                        div()
                            .id(("wizard-language", index))
                            .h(px(72.))
                            .px(px(14.))
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .rounded(px(11.))
                            .bg(if selected {
                                theme::alpha(theme::ACCENT, 0.1)
                            } else {
                                theme::alpha(0xffffff, 0.02)
                            })
                            .border_1()
                            .border_color(if selected {
                                theme::alpha(theme::ACCENT, 0.6)
                            } else {
                                theme::alpha(theme::STROKE, 0.12)
                            })
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::alpha(theme::ACCENT, 0.06)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(wizard) = this.wizard.as_mut() {
                                    wizard.set_language(language);
                                }
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .size(px(36.))
                                    .rounded(px(9.))
                                    .bg(if selected {
                                        theme::gold_gradient()
                                    } else {
                                        theme::alpha(theme::STROKE, 0.08).into()
                                    })
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(if selected {
                                        theme::color(theme::DARK_TEXT)
                                    } else {
                                        theme::color(theme::TEXT_DIM)
                                    })
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_size(px(12.))
                                    .child(language_short(language)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.))
                                    .child(
                                        div()
                                            .text_size(px(13.5))
                                            .text_color(theme::color(theme::TEXT))
                                            .child(language.as_str()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child(language_descriptor(language)),
                                    ),
                            )
                    })),
            )
            .into_any_element()
    }

    /// Template selection for C projects: empty / LED Blink / UART Loopback.
    fn render_template(&self, cx: &mut Context<IdeApp>) -> AnyElement {
        if self.language != Language::C {
            return self
                .heading("项目模板", "仅 C 项目可选模板；其他语言使用默认入口文件。")
                .into_any_element();
        }
        div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(self.heading(
                "选择项目模板",
                "模板决定 src/main.c 的初始内容；之后可随时编辑。",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .children(
                        ns_project::CTemplate::ALL
                            .iter()
                            .copied()
                            .enumerate()
                            .map(|(index, template)| {
                                let selected = template == self.c_template;
                                option_row(
                                    ("wizard-template", index),
                                    template.label().to_string(),
                                    String::new(),
                                    selected,
                                    true,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(wizard) = this.wizard.as_mut() {
                                        wizard.c_template = template;
                                    }
                                    cx.notify();
                                }))
                            }),
                    ),
            )
            .into_any_element()
    }


    fn render_tool_step(
        &self,
        cx: &mut Context<IdeApp>,
        title: &str,
        subtitle: &str,
        mut options: Vec<(String, Option<String>, bool)>,
        kind: StepKind,
        allow_skip: bool,
    ) -> AnyElement {
        if allow_skip {
            options.push(("跳过".to_string(), None, true));
        }
        let selected = self.selected_for(kind);
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(self.heading(title, subtitle))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(7.))
                    .children(options.into_iter().enumerate().map(|(index, (name, path, _))| {
                        let is_selected = selected == name;
                        let subtitle = match &path {
                            Some(path) => path.clone(),
                            None if name == "跳过" => "不选择此工具".to_string(),
                            None => {
                                let guide = toolchain::missing_guide(&name)
                                    .unwrap_or("未在 PATH 中找到；请安装后重试");
                                format!("未在 PATH 中找到 · 安装: {guide}")
                            }
                        };
                        option_row(
                            ("wizard-tool", index),
                            name.clone(),
                            subtitle,
                            is_selected,
                            path.is_some() || name == "跳过",
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(wizard) = this.wizard.as_mut() {
                                wizard.set_selection(kind, name.clone());
                            }
                            cx.notify();
                        }))
                    })),
            )
            .into_any_element()
    }





    pub fn generate_device_files(&self, root: &std::path::Path) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        let linker = ns_device::linker_script(&self.target)?;
        let startup = ns_device::startup_file(&self.target)?;
        let linker_path = root.join("link.ld");
        let startup_path = root.join("src").join("startup.s");
        let _ = std::fs::create_dir_all(root.join("src"));
        let _ = std::fs::write(&linker_path, linker);
        let _ = std::fs::write(&startup_path, startup);
        Some((linker_path, startup_path))
    }

    fn render_targets(&self, cx: &mut Context<IdeApp>) -> AnyElement {
        let selected_label = toolchain::TARGETS
            .iter()
            .find(|(id, _)| *id == self.target)
            .map(|(_, description)| (*description).to_string())
            .unwrap_or_else(|| {
                if self.target.is_empty() {
                    "稍后设置（保持 project.toml 中 target 为空）".to_string()
                } else {
                    self.target.clone()
                }
            });

        let trigger = div()
            .id("wizard-target-trigger")
            .w_full()
            .h(px(46.))
            .px(px(13.))
            .flex()
            .items_center()
            .justify_between()
            .rounded(px(10.))
            .bg(theme::alpha(0xffffff, 0.02))
            .border_1()
            .border_color(if self.target_menu_open {
                theme::alpha(theme::ACCENT, 0.55)
            } else {
                theme::alpha(theme::STROKE, 0.11)
            })
            .cursor_pointer()
            .hover(|style| style.border_color(theme::alpha(theme::ACCENT, 0.35)))
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(wizard) = this.wizard.as_mut() {
                    wizard.target_menu_open = !wizard.target_menu_open;
                }
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(theme::color(theme::TEXT))
                            .child(selected_label),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::color(theme::TEXT_FAINT))
                    .child(if self.target_menu_open { "▲" } else { "▼" }),
            );

        let list = if self.target_menu_open {
            div()
                .w_full()
                .mt(px(4.))
                .rounded(px(10.))
                .border_1()
                .border_color(theme::alpha(theme::STROKE, 0.11))
                .bg(theme::alpha(0xffffff, 0.015))
                .p(px(4.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .children(toolchain::TARGETS.iter().enumerate().map(
                    |(index, (id, description))| {
                        let value = (*id).to_string();
                        let selected = self.target == value;
                        div()
                            .id(("wizard-target", index))
                            .h(px(36.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(9.))
                            .rounded(px(7.))
                            .bg(if selected {
                                theme::alpha(theme::ACCENT, 0.09)
                            } else {
                                theme::alpha(0xffffff, 0.0)
                            })
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::alpha(theme::ACCENT, 0.05)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(wizard) = this.wizard.as_mut() {
                                    wizard.target = value.clone();
                                    wizard.target_menu_open = false;
                                }
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .size(px(14.))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(if selected {
                                        theme::color(theme::ACCENT)
                                    } else {
                                        theme::alpha(theme::STROKE, 0.3)
                                    })
                                    .when(selected, |style| {
                                        style
                                            .bg(theme::color(theme::ACCENT))
                                            .child(
                                                div()
                                                    .size_full()
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .child(
                                                        div()
                                                            .size(px(4.))
                                                            .rounded_full()
                                                            .bg(theme::color(theme::DARK_TEXT)),
                                                    ),
                                            )
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(theme::color(theme::TEXT))
                                            .child(*description),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(9.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child(*id),
                                    ),
                            )
                    },
                ))
                .child(
                    div()
                        .h(px(1.))
                        .mx(px(6.))
                        .bg(theme::alpha(theme::STROKE, 0.08)),
                )
                .child(
                    div()
                        .id("wizard-target-skip")
                        .h(px(36.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(9.))
                        .rounded(px(7.))
                        .bg(if self.target.is_empty() {
                            theme::alpha(theme::ACCENT, 0.09)
                        } else {
                            theme::alpha(0xffffff, 0.0)
                        })
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::alpha(theme::ACCENT, 0.05)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(wizard) = this.wizard.as_mut() {
                                wizard.target.clear();
                                wizard.target_menu_open = false;
                            }
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(theme::color(theme::TEXT))
                                .child("稍后设置"),
                        )
                        .child(
                            div()
                                .text_size(px(9.5))
                                .text_color(theme::color(theme::TEXT_FAINT))
                                .child("保持 project.toml 中 target 为空"),
                        ),
                )
                .into_any_element()
        } else {
            div().into_any_element()
        }
        ;


        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(self.heading(
                "选择目标设备",
                "设备型号用于生成 SVD、启动文件与链接脚本；也可以稍后设置。",
            ))
            .child(trigger)
            .child(list)
            .when(ns_device::catalog_entry(&self.target).is_some(), |style| {
                let entry = ns_device::catalog_entry(&self.target).unwrap();
                style.child(
                    div()
                        .mt(px(6.))
                        .px(px(12.))
                        .py(px(9.))
                        .rounded(px(6.))
                        .bg(theme::alpha(theme::ACCENT, 0.07))
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(theme::color(theme::TEXT))
                                .child(format!("{} 核 · Flash {}KB · RAM {}KB", entry.core, entry.flash_size / 1024, entry.ram_size / 1024)),
                        )
                        .child(
                            div()
                                .text_size(px(9.5))
                                .text_color(theme::color(theme::TEXT_FAINT))
                                .child("生成项目时将自动写入链接脚本（link.ld）与启动文件（src/startup.s）；设备寄存器可在调试面板「设备」页查看。"),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_summary(&self, cx: &mut Context<IdeApp>) -> AnyElement {
        let spec = self.spec(cx);
        let mut rows = vec![
            ("项目", if spec.name.is_empty() {
                "（未命名）".to_string()
            } else {
                spec.name.clone()
            }),
            ("位置", spec.location.display().to_string()),
            ("语言", spec.language.to_string()),
            ("编译器", value_or_auto(&spec.compiler)),
            ("仿真器", value_or_none(&spec.simulator)),
            ("调试器", value_or_none(&spec.debugger)),
            ("烧写器", value_or_none(&spec.flasher)),
            ("目标设备", value_or_none(&spec.target)),
        ];
        if let Some(error) = &self.error {
            rows.push(("错误", error.clone()));
        }

        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(self.heading(
                "确认并生成",
                "将写出 project.toml、源码骨架、README 与 .gitignore，并立即打开项目。",
            ))
            .child(
                div()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(theme::alpha(theme::STROKE, 0.14))
                    .bg(theme::alpha(0xffffff, 0.02))
                    .overflow_hidden()
                    .children(rows.into_iter().enumerate().map(|(index, (label, value))| {
                        let is_error = label == "错误";
                        div()
                            .px(px(14.))
                            .py(px(10.))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(20.))
                            .when(index > 0, |style| {
                                style.border_t_1().border_color(theme::alpha(theme::STROKE, 0.08))
                            })
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(theme::color(theme::TEXT_FAINT))
                                    .child(label),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(if is_error {
                                        theme::color(theme::DANGER)
                                    } else {
                                        theme::color(theme::TEXT)
                                    })
                                    .text_align(gpui::TextAlign::Right)
                                    .child(value),
                            )
                    })),
            )
            .into_any_element()
    }

    fn heading(&self, title: &str, subtitle: &str) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(5.))
            .child(
                div()
                    .text_size(px(16.))
                    .font_family(theme::SERIF_FONT)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::color(theme::TEXT))
                    .child(title.to_string()),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme::color(theme::TEXT_FAINT))
                    .child(subtitle.to_string()),
            )
    }

    fn selected_for(&self, kind: StepKind) -> String {
        match kind {
            StepKind::Compiler => self.compiler.clone(),
            StepKind::Simulator => self.simulator.clone(),
            StepKind::Debugger => self.debugger.clone(),
            StepKind::Flasher => self.flasher.clone(),
        }
    }

    fn set_selection(&mut self, kind: StepKind, name: String) {
        let value = if name == "跳过" { String::new() } else { name };
        match kind {
            StepKind::Compiler => self.compiler = value,
            StepKind::Simulator => self.simulator = value,
            StepKind::Debugger => self.debugger = value,
            StepKind::Flasher => self.flasher = value,
        }
        self.error = None;
    }
}

#[derive(Clone, Copy)]
enum StepKind {
    Compiler,
    Simulator,
    Debugger,
    Flasher,
}

fn option_row(
    id: impl Into<gpui::ElementId>,
    title: String,
    subtitle: String,
    selected: bool,
    available: bool,
) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .w_full()
        .px(px(13.))
        .py(px(10.))
        .flex()
        .items_center()
        .gap(px(11.))
        .rounded(px(10.))
        .bg(if selected {
            theme::alpha(theme::ACCENT, 0.09)
        } else {
            theme::alpha(0xffffff, 0.02)
        })
        .border_1()
        .border_color(if selected {
            theme::alpha(theme::ACCENT, 0.55)
        } else {
            theme::alpha(theme::STROKE, 0.11)
        })
        .cursor_pointer()
        .hover(|style| {
            style
                .border_color(theme::alpha(theme::ACCENT, 0.35))
                .bg(theme::alpha(theme::ACCENT, 0.05))
        })
        .child(
            div()
                .size(px(16.))
                .rounded_full()
                .border_1()
                .border_color(if selected {
                    theme::color(theme::ACCENT)
                } else {
                    theme::alpha(theme::STROKE, 0.3)
                })
                .flex()
                .items_center()
                .justify_center()
                .when(selected, |style| {
                    style.child(
                        div()
                            .size(px(7.))
                            .rounded_full()
                            .bg(theme::color(theme::ACCENT)),
                    )
                }),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .flex_1()
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme::color(theme::TEXT))
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme::color(theme::TEXT_FAINT))
                        .child(subtitle),
                ),
        )
        .child(if available {
            widgets::pill("可用", theme::color(theme::JADE))
        } else {
            widgets::pill("未检测到", theme::color(theme::TEXT_FAINT))
        })
}

fn language_short(language: Language) -> SharedString {
    match language {
        Language::C => "C".into(),
        Language::Cpp => "C++".into(),
        Language::Rust => "RS".into(),
        Language::Zig => "ZIG".into(),
        Language::ZR => "ZR".into(),
        // Not selectable in the wizard (excluded from `Language::ALL`).
        Language::Asm => "ASM".into(),
    }
}

fn language_descriptor(language: Language) -> &'static str {
    match language {
        Language::C => "GCC / Clang · 裸机与 RTOS",
        Language::Cpp => "G++ / Clang++ · 现代 C++",
        Language::Rust => "Cargo · 内存安全",
        Language::Zig => "zig build · 简洁系统语言",
        Language::ZR => "zr · 外部系统语言（zrlang.github.io）",
        Language::Asm => "汇编 · 启动文件/中断向量",
    }
}

fn value_or_auto(value: &str) -> String {
    if value.trim().is_empty() {
        "自动发现".to_string()
    } else {
        value.to_string()
    }
}

fn value_or_none(value: &str) -> String {
    if value.trim().is_empty() {
        "跳过".to_string()
    } else {
        value.to_string()
    }
}

pub fn default_projects_dir() -> PathBuf {
    if let Some(dirs) = directories::UserDirs::new() {
        let base = dirs
            .document_dir()
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs.home_dir().to_path_buf());
        return base.join("ns-projects");
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn focus_wizard(wizard: &Wizard, window: &mut Window, cx: &mut App) {
    window.focus(&wizard.name.read(cx).focus_handle(cx));
}
