#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod font;

use eframe::egui;
use fiberhome_factory::model::{
    Factory, ImportWarning, Part, PartStatus, Result, random_mac, random_sn,
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

enum Pending {
    New,
    Open(PathBuf),
    Close,
}

#[derive(Clone, Copy)]
enum Language {
    English,
    Chinese,
}

impl Language {
    fn text(self, english: &'static str, chinese: &'static str) -> &'static str {
        match self {
            Self::English => english,
            Self::Chinese => chinese,
        }
    }

    fn toggle(&mut self) {
        *self = match self {
            Self::English => Self::Chinese,
            Self::Chinese => Self::English,
        };
    }
}

enum Notice {
    Info(String),
    Error(String),
}

struct Editor {
    factory: Factory,
    source: Option<PathBuf>,
    source_is_stock: bool,
    dirty: bool,
    notice: Option<Notice>,
    import_warning: Option<ImportWarning>,
    pending: Option<Pending>,
    close_allowed: bool,
    language: Language,
}
impl Editor {
    fn new() -> Result<Self> {
        Ok(Self {
            factory: Factory::new()?,
            source: None,
            source_is_stock: false,
            dirty: false,
            notice: None,
            import_warning: None,
            pending: None,
            close_allowed: false,
            language: Language::English,
        })
    }
    fn message(&mut self, r: Result<String>) {
        self.notice = match r {
            Ok(message) if message.is_empty() => None,
            Ok(message) => Some(Notice::Info(message)),
            Err(message) => Some(Notice::Error(message)),
        };
    }
    fn randomize(
        &mut self,
        field: fn(&mut Factory) -> &mut String,
        generate: fn() -> Result<String>,
    ) {
        let result = generate().map(|value| {
            *field(&mut self.factory) = value;
            self.dirty = true;
            String::new()
        });
        self.message(result);
    }
    fn toggle_language(&mut self, ctx: &egui::Context) {
        self.language.toggle();
        self.notice = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
            self.language
                .text("FiberHome Factory Editor", "烽火 Factory 编辑器")
                .into(),
        ));
    }
    fn continue_pending(&mut self, ctx: &egui::Context) {
        if let Some(pending) = self.pending.take() {
            self.perform(pending, ctx);
        }
    }
    fn request(&mut self, p: Pending, ctx: &egui::Context) {
        if self.dirty {
            self.pending = Some(p);
        } else {
            self.perform(p, ctx);
        }
    }
    fn perform(&mut self, p: Pending, ctx: &egui::Context) {
        let language = self.language;
        let r = match p {
            Pending::Close => {
                self.close_allowed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            Pending::New => Factory::new().map(|f| {
                self.factory = f;
                self.source = None;
                self.source_is_stock = false;
                self.import_warning = None;
                self.dirty = false;
                language.text("Created", "已新建").into()
            }),
            Pending::Open(path) => Factory::open(&path).map(|opened| {
                self.factory = opened.factory;
                self.source = Some(path);
                self.source_is_stock = opened.source_is_stock;
                self.import_warning = opened.warning;
                self.dirty = false;
                if opened.source_is_stock {
                    language
                        .text("Imported stock data", "已导入原厂数据")
                        .into()
                } else {
                    language.text("Opened", "已打开").into()
                }
            }),
        };
        self.message(r);
    }
    fn save(&mut self) -> bool {
        let data = match self.factory.encode() {
            Ok(d) => d,
            Err(e) => {
                self.message(Err(e));
                return false;
            }
        };
        let mut dialog = rfd::FileDialog::new()
            .set_file_name("factory-1m.bin")
            .add_filter(
                self.language.text("Factory image", "Factory 镜像"),
                &["bin"],
            );
        if let Some(p) = self.source.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(p);
        }
        let Some(path) = dialog.save_file() else {
            return false;
        };
        if self.source_is_stock && self.source.as_ref().is_some_and(|p| same_path(p, &path)) {
            self.message(Err(self
                .language
                .text(
                    "Choose a new output file for the converted image.",
                    "请选择新的输出文件，保留原厂备份。",
                )
                .into()));
            return false;
        }
        match save_image(&path, &data) {
            Ok(()) => {
                self.dirty = false;
                self.message(Ok(format!(
                    "{}: {}",
                    self.language.text("Saved", "已保存"),
                    path.display()
                )));
                true
            }
            Err(e) => {
                self.message(Err(e));
                false
            }
        }
    }
    fn part_ui(&mut self, ui: &mut egui::Ui, p: Part) {
        let language = self.language;
        let status = self.factory.part_status(p);
        let present = !matches!(&status, PartStatus::Empty);
        ui.group(|ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.strong(part_title(language, p));
                ui.label(part_status_text(language, status));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(present, egui::Button::new(language.text("Clear", "清空")))
                        .clicked()
                    {
                        self.factory.clear(p);
                        self.dirty = true;
                    }
                    if ui
                        .add_enabled(present, egui::Button::new(language.text("Export", "导出")))
                        .clicked()
                    {
                        let filename = match p {
                            Part::Pon => "pon-calibration.bin",
                            Part::Wifi => "mt7916-eeprom.bin",
                        };
                        if let Some(path) =
                            rfd::FileDialog::new().set_file_name(filename).save_file()
                        {
                            let r = if self.source.as_ref().is_some_and(|s| same_path(s, &path)) {
                                Err(language
                                    .text(
                                        "Choose a different component output file.",
                                        "请选择新的组件导出文件。",
                                    )
                                    .into())
                            } else {
                                save_image(&path, self.factory.part(p)).map(|_| {
                                    format!(
                                        "{}: {}",
                                        language.text("Exported", "已导出"),
                                        path.display()
                                    )
                                })
                            };
                            self.message(r);
                        }
                    }
                    if ui.button(language.text("Replace", "替换")).clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_file()
                    {
                        let r = self.factory.replace(p, &path).map(|_| {
                            self.dirty = true;
                            match language {
                                Language::English => {
                                    format!("Replaced {}", part_title(language, p))
                                }
                                Language::Chinese => {
                                    format!("已替换{}", part_title(language, p))
                                }
                            }
                        });
                        self.message(r);
                    }
                });
            });
        });
    }
}

fn part_title(language: Language, part: Part) -> &'static str {
    match part {
        Part::Pon => language.text("PON calibration", "光模块校准"),
        Part::Wifi => "Wi-Fi EEPROM",
    }
}

fn part_status_text(language: Language, status: PartStatus) -> String {
    match status {
        PartStatus::Empty => language.text("Not set", "未设置").into(),
        PartStatus::Pon {
            chip,
            payload_length,
        } => format!("{} · {} B", chip.name(), payload_length),
        PartStatus::Wifi { length } => {
            format!("{} · {} B", language.text("Valid", "有效"), length)
        }
        PartStatus::Invalid => language.text("Invalid", "校验失败").into(),
    }
}

fn warning_text(language: Language, warning: ImportWarning) -> &'static str {
    match warning {
        ImportWarning::DirtyUbifs => language.text(
            "UBIFS has an uncommitted journal; data comes from the last committed index.",
            "UBIFS 存在未提交日志，当前数据来自最后提交的索引。",
        ),
        ImportWarning::DamagedJffs2 => language.text(
            "JFFS2 contains CRC-damaged nodes; valid copies were used.",
            "JFFS2 包含 CRC 错误节点，已使用有效副本。",
        ),
    }
}
fn identity_field(ui: &mut egui::Ui, label: &str, value: &mut String) -> bool {
    ui.add_sized([100.0, 26.0], egui::Label::new(label));
    ui.add_sized([330.0, 26.0], egui::TextEdit::singleline(value))
        .changed()
}
fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
fn save_image(path: &Path, data: &[u8]) -> Result<()> {
    // An atomic rename keeps the previous image intact when a write fails.
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    tmp.write_all(data)
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    tmp.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}
impl eframe::App for Editor {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        if let Some(path) =
            ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()))
        {
            self.request(Pending::Open(path), &ctx);
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.dirty && !self.close_allowed {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending = Some(Pending::Close);
        }
        egui::CentralPanel::default().show(root, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(self.language.text("FiberHome Factory", "烽火 Factory"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(self.language.text("中文", "English")).clicked() {
                            self.toggle_language(&ctx);
                        }
                        if ui
                            .button(self.language.text("Save 1 MiB", "保存 1 MiB"))
                            .clicked()
                        {
                            self.save();
                        }
                        if ui.button(self.language.text("Open", "打开")).clicked()
                            && let Some(p) = rfd::FileDialog::new()
                                .add_filter(
                                    self.language.text("Partition image", "分区镜像"),
                                    &["bin", "img", "ubi"],
                                )
                                .pick_file()
                        {
                            self.request(Pending::Open(p), &ctx);
                        }
                        if ui.button(self.language.text("New", "新建")).clicked() {
                            self.request(Pending::New, &ctx);
                        }
                    });
                });
                ui.add_space(12.0);
                let title = self
                    .source
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| self.language.text("New factory", "新建 Factory").into());
                ui.label(format!(
                    "{}{}",
                    title,
                    if self.dirty {
                        self.language.text("  • Unsaved", "  • 未保存")
                    } else {
                        ""
                    }
                ));
                ui.separator();
                ui.add_space(10.0);
                ui.heading(self.language.text("Identity", "身份信息"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    self.dirty |= identity_field(
                        ui,
                        self.language.text("Base MAC", "基础 MAC"),
                        &mut self.factory.mac,
                    );
                    if ui.button(self.language.text("Random", "随机")).clicked() {
                        self.randomize(|factory| &mut factory.mac, random_mac);
                    }
                });
                ui.add_space(10.0);
                if self.factory.part_present(Part::Wifi) {
                    ui.horizontal(|ui| {
                        self.dirty |= identity_field(ui, "2.4 GHz MAC", &mut self.factory.wifi_mac);
                        if ui.button(self.language.text("Random", "随机")).clicked() {
                            self.randomize(|factory| &mut factory.wifi_mac, random_mac);
                        }
                    });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        self.dirty |= identity_field(ui, "5 GHz MAC", &mut self.factory.wifi_mac2);
                        if ui.button(self.language.text("Random", "随机")).clicked() {
                            self.randomize(|factory| &mut factory.wifi_mac2, random_mac);
                        }
                    });
                    ui.add_space(10.0);
                }
                ui.horizontal(|ui| {
                    self.dirty |= identity_field(ui, "PON SN", &mut self.factory.pon_sn);
                    if ui.button(self.language.text("Random", "随机")).clicked() {
                        self.randomize(|factory| &mut factory.pon_sn, random_sn);
                    }
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    self.dirty |= identity_field(
                        ui,
                        self.language.text("Device serial", "整机序列号"),
                        &mut self.factory.device_sn,
                    );
                });
                ui.add_space(22.0);
                self.part_ui(ui, Part::Pon);
                ui.add_space(10.0);
                self.part_ui(ui, Part::Wifi);
                if let Some(warning) = self.import_warning {
                    ui.add_space(12.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(175, 105, 20),
                        warning_text(self.language, warning),
                    );
                }
                if let Some(notice) = &self.notice {
                    ui.add_space(18.0);
                    let (message, color) = match notice {
                        Notice::Info(message) => (message, ui.visuals().text_color()),
                        Notice::Error(message) => (message, egui::Color32::from_rgb(190, 50, 45)),
                    };
                    ui.colored_label(color, message);
                }
            });
        });
        if self.pending.is_some() {
            egui::Modal::new(egui::Id::new("unsaved")).show(&ctx, |ui| {
                ui.heading(
                    self.language
                        .text("Save current changes?", "保存当前修改？"),
                );
                ui.horizontal(|ui| {
                    if ui.button(self.language.text("Save", "保存")).clicked() && self.save() {
                        self.continue_pending(&ctx);
                    }
                    if ui
                        .button(self.language.text("Discard", "放弃修改"))
                        .clicked()
                    {
                        self.continue_pending(&ctx);
                    }
                    if ui.button(self.language.text("Cancel", "取消")).clicked() {
                        self.pending = None;
                    }
                });
            });
        }
    }
}
fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 470.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "FiberHome Factory Editor",
        options,
        Box::new(|cc| {
            font::install(&cc.egui_ctx);
            let mut editor = Editor::new().map_err(std::io::Error::other)?;
            if let Some(path) = std::env::args_os().nth(1) {
                editor.perform(Pending::Open(path.into()), &cc.egui_ctx);
            }
            Ok(Box::new(editor))
        }),
    )
}
