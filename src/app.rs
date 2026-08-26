use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::Instant;

use eframe::egui;

use crate::pipeline::{Msg, Options};
use crate::sii::VehicleDef;

#[derive(PartialEq, Clone, Copy)]
enum Phase {
    Idle,
    Scanning,
    Converting,
    Finished,
}

pub struct App {
    archive: Option<PathBuf>,
    outdir: Option<PathBuf>,

    vehicles: Vec<VehicleDef>,
    vehicle_i: usize,
    cabin_i: usize,
    chassis_i: usize,
    interior_variants: Vec<String>,
    interior_i: usize,
    variants_row: bool,
    sounds: bool,
    skip_existing: bool,
    cleanup: bool,

    /// Pressing Convert on a fresh archive scans first and then keeps going, so
    /// the common case is one click rather than two.
    auto_continue: bool,
    show_advanced: bool,
    show_log: bool,

    phase: Phase,
    progress: f32,
    step: String,
    started: Option<Instant>,
    log: Vec<(egui::Color32, String)>,
    warnings: usize,
    outputs: Vec<PathBuf>,
    rx: Option<Receiver<Msg>>,
}

fn settings_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("scs2fbx").join("settings.txt")
}

impl Default for App {
    fn default() -> Self {
        let mut app = Self {
            archive: None,
            outdir: None,
            vehicles: Vec::new(),
            vehicle_i: 0,
            cabin_i: 0,
            chassis_i: 0,
            interior_variants: Vec::new(),
            interior_i: 0,
            variants_row: true,
            sounds: false,
            skip_existing: true,
            cleanup: true,
            auto_continue: false,
            show_advanced: false,
            show_log: false,
            phase: Phase::Idle,
            progress: 0.0,
            step: String::new(),
            started: None,
            log: Vec::new(),
            warnings: 0,
            outputs: Vec::new(),
            rx: None,
        };
        app.load_settings();
        app
    }
}

impl App {
    fn load_settings(&mut self) {
        let Ok(txt) = std::fs::read_to_string(settings_path()) else { return };
        for line in txt.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.trim() {
                // a remembered path only wins if it still exists
                "variants_row" => self.variants_row = v == "1",
                "sounds" => self.sounds = v == "1",
                "skip_existing" => self.skip_existing = v == "1",
                "cleanup" => self.cleanup = v == "1",
                "show_advanced" => self.show_advanced = v == "1",
                _ => {}
            }
        }
    }

    fn save_settings(&self) {
        let p = settings_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(
            p,
            format!(
                "variants_row={}\nsounds={}\nskip_existing={}\ncleanup={}\nshow_advanced={}\n",
                self.variants_row as u8,
                self.sounds as u8,
                self.skip_existing as u8,
                self.cleanup as u8,
                self.show_advanced as u8,
            ),
        );
    }

    fn set_archive(&mut self, p: PathBuf) {
        self.outdir = p.parent().map(|d| {
            d.join(
                p.file_stem()
                    .map(|s| format!("{}_blend", s.to_string_lossy()))
                    .unwrap_or_else(|| "scs2fbx_out".into()),
            )
        });
        self.archive = Some(p);
        self.vehicles.clear();
        self.interior_variants.clear();
        self.outputs.clear();
        self.log.clear();
        self.warnings = 0;
        self.phase = Phase::Idle;
    }

    fn push(&mut self, c: egui::Color32, s: String) {
        self.log.push((c, s));
        if self.log.len() > 800 {
            self.log.drain(0..200);
        }
    }


    fn options(&self) -> Option<Options> {
        let v = self.vehicles.get(self.vehicle_i)?;
        Some(Options {
            archive: self.archive.clone()?,
            outdir: self.outdir.clone()?,
            converter_pix: PathBuf::new(), // resolved from the embedded copy
            vehicle: v.name.clone(),
            cabin: v.cabins.get(self.cabin_i).cloned().unwrap_or_default(),
            chassis: v.chassis.get(self.chassis_i).cloned().unwrap_or_default(),
            interior_variant: self
                .interior_variants
                .get(self.interior_i)
                .cloned()
                .unwrap_or_default(),
            cleanup: self.cleanup,
            variants_row: self.variants_row,
            sounds: self.sounds,
            skip_existing: self.skip_existing,
        })
    }

    fn start_scan(&mut self) {
        let (Some(archive), Some(outdir)) = (self.archive.clone(), self.outdir.clone()) else { return };
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.phase = Phase::Scanning;
        self.progress = 0.0;
        self.warnings = 0;
        self.started = Some(Instant::now());
        std::thread::spawn(move || crate::pipeline::scan(archive, outdir, tx));
    }

    fn start_convert(&mut self) {
        let Some(opt) = self.options() else { return };
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.phase = Phase::Converting;
        self.outputs.clear();
        self.progress = 0.0;
        if self.started.is_none() {
            self.started = Some(Instant::now());
        }
        std::thread::spawn(move || crate::pipeline::run(opt, tx));
    }

    fn go(&mut self) {
        self.save_settings();
        if self.vehicles.is_empty() {
            self.auto_continue = true;
            self.log.clear();
            self.start_scan();
        } else {
            self.start_convert();
        }
    }

    fn drain(&mut self) {
        let mut msgs = Vec::new();
        if let Some(rx) = &self.rx {
            while let Ok(m) = rx.try_recv() {
                msgs.push(m);
            }
        }
        for m in msgs {
            match m {
                Msg::Step(s) => {
                    self.step = s.clone();
                    self.push(egui::Color32::from_rgb(140, 190, 255), s);
                }
                Msg::Info(s) => self.push(egui::Color32::GRAY, s),
                Msg::Warn(s) => {
                    self.warnings += 1;
                    self.push(egui::Color32::from_rgb(240, 190, 90), s);
                }
                Msg::Error(s) => {
                    self.push(egui::Color32::from_rgb(255, 120, 120), s);
                    self.step = "Failed".into();
                    self.phase = Phase::Idle;
                    self.auto_continue = false;
                    self.show_log = true;
                }
                Msg::Progress(p) => self.progress = p,
                Msg::Done(outs) => {
                    self.outputs = outs;
                    self.phase = Phase::Finished;
                    self.progress = 1.0;
                    self.step = "Done".into();
                }
            }
        }

        // the scan thread signals completion with a marker file
        if self.phase == Phase::Scanning {
            if let Some(o) = self.outdir.clone() {
                let marker = o.join("_scan_done");
                if marker.is_file() {
                    let _ = std::fs::remove_file(&marker);
                    self.vehicles = crate::sii::find_vehicles(&crate::pipeline::extracted_dir(&o));
                    self.vehicle_i = 0;
                    self.cabin_i = 0;
                    self.chassis_i = 0;
                    self.interior_variants = self
                        .vehicles
                        .first()
                        .map(|v| {
                            crate::sii::interiors(v)
                                .into_iter()
                                .map(|i| if i.variant.is_empty() { i.stem } else { i.variant })
                                .collect()
                        })
                        .unwrap_or_default();
                    self.interior_i = 0;
                    self.phase = Phase::Idle;
                    let n = self.vehicles.len();
                    self.push(
                        egui::Color32::LIGHT_GREEN,
                        format!("found {n} vehicle definition(s)"),
                    );
                    if self.auto_continue && n > 0 {
                        self.auto_continue = false;
                        self.start_convert();
                    } else if n == 0 {
                        self.auto_continue = false;
                        self.push(
                            egui::Color32::from_rgb(255, 120, 120),
                            "no truck definition in this archive - it may be a map, sound or trailer mod".into(),
                        );
                    }
                }
            }
        }
    }
}

fn open_folder(p: &std::path::Path) {
    let dir = if p.is_dir() { p } else { p.parent().unwrap_or(p) };
    let mut c = std::process::Command::new("explorer");
    crate::pipeline::no_window(&mut c);
    let _ = c.arg(dir).spawn();
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();
        let busy = matches!(self.phase, Phase::Scanning | Phase::Converting);
        if busy {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(120));
        }

        // drag and drop anywhere on the window
        let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !busy {
            if let Some(p) = dropped
                .into_iter()
                .find(|p| p.extension().map(|e| e.eq_ignore_ascii_case("scs")).unwrap_or(false))
            {
                self.set_archive(p);
            }
        }
        let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());

        ui.horizontal(|ui| {
            ui.heading("scs2fbx");
            ui.label(egui::RichText::new("ETS2 / ATS vehicle mod to FBX").weak());
        });
        ui.separator();

        // ---- archive ----------------------------------------------------
        let frame = egui::Frame::group(ui.style()).fill(if hovering {
            ui.visuals().selection.bg_fill.gamma_multiply(0.35)
        } else {
            ui.visuals().faint_bg_color
        });
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical_centered(|ui| {
                match &self.archive {
                    Some(p) => {
                        ui.label(
                            egui::RichText::new(
                                p.file_name().unwrap_or_default().to_string_lossy(),
                            )
                            .strong(),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "output: {}",
                                self.outdir
                                    .as_ref()
                                    .map(|d| d.display().to_string())
                                    .unwrap_or_default()
                            ))
                            .weak()
                            .small(),
                        );
                    }
                    None => {
                        ui.label("Drop a .scs mod file here");
                        ui.label(egui::RichText::new("or use Browse").weak().small());
                    }
                }
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space((ui.available_width() - 170.0).max(0.0) / 2.0);
                    if ui.add_enabled(!busy, egui::Button::new("Browse")).clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("SCS archive", &["scs"])
                            .pick_file()
                        {
                            self.set_archive(p);
                        }
                    }
                    if self.archive.is_some()
                        && ui.add_enabled(!busy, egui::Button::new("Change output")).clicked()
                    {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            self.outdir = Some(p);
                        }
                    }
                });
            });
        });

        // ---- the one button ----------------------------------------------
        ui.add_space(6.0);
        let ready = self.archive.is_some() && !busy;
        let label = if self.vehicles.is_empty() { "Convert" } else { "Convert again" };
        ui.vertical_centered(|ui| {
            if ui
                .add_enabled(ready, egui::Button::new(egui::RichText::new(label).size(17.0))
                    .min_size(egui::vec2(180.0, 34.0)))
                .clicked()
            {
                self.go();
            }
        });

        if busy || self.phase == Phase::Finished {
            ui.add_space(4.0);
            let secs = self.started.map(|s| s.elapsed().as_secs()).unwrap_or(0);
            let text = if busy {
                format!("{}  ({}:{:02})", self.step, secs / 60, secs % 60)
            } else {
                format!("{}  in {}:{:02}", self.step, secs / 60, secs % 60)
            };
            ui.add(egui::ProgressBar::new(self.progress).text(text));
            if self.warnings > 0 {
                ui.label(
                    egui::RichText::new(format!(
                        "{} warning(s) - open the log for details",
                        self.warnings
                    ))
                    .small()
                    .color(egui::Color32::from_rgb(240, 190, 90)),
                );
            }
        }

        // ---- results ------------------------------------------------------
        if !self.outputs.is_empty() {
            ui.add_space(6.0);
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.label(egui::RichText::new("Files written").strong());
                for o in self.outputs.clone() {
                    ui.horizontal(|ui| {
                        let size = std::fs::metadata(&o)
                            .map(|m| format!("{:.1} MB", m.len() as f64 / 1_048_576.0))
                            .unwrap_or_default();
                        ui.label(
                            egui::RichText::new(o.file_name().unwrap_or_default().to_string_lossy())
                                .color(egui::Color32::LIGHT_GREEN),
                        );
                        ui.label(egui::RichText::new(size).weak().small());
                    });
                }
                if let Some(first) = self.outputs.first().cloned() {
                    if ui.button("Open folder").clicked() {
                        open_folder(&first);
                    }
                }
            });
        }

        // ---- advanced ------------------------------------------------------
        ui.add_space(6.0);
        let mut adv = self.show_advanced;
        egui::CollapsingHeader::new("Options")
            .default_open(self.show_advanced)
            .show(ui, |ui| {
                adv = true;
                ui.checkbox(&mut self.variants_row, "include unworn variants, parked in a row beside the vehicle");
                ui.checkbox(&mut self.sounds, "also extract the mod's sound to sounds/ (MP3 when ffmpeg is installed)");
                ui.checkbox(&mut self.skip_existing, "skip an archive that already has an .fbx");
                ui.checkbox(&mut self.cleanup, "delete working files when finished");
                if !self.cleanup {
                    ui.label(
                        egui::RichText::new(
                            "the working folder holds ~5000 extracted files - keep it only for debugging",
                        )
                        .weak()
                        .small(),
                    );
                }

                if !self.vehicles.is_empty() {
                    ui.separator();
                    let v = &self.vehicles[self.vehicle_i.min(self.vehicles.len() - 1)];
                    let (cabins, chassis, name) = (v.cabins.clone(), v.chassis.clone(), v.name.clone());
                    let count = self.vehicles.len();
                    egui::Grid::new("cfg").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                        ui.label("Vehicle");
                        egui::ComboBox::from_id_salt("veh").selected_text(&name).show_ui(ui, |ui| {
                            for i in 0..count {
                                let n = self.vehicles[i].name.clone();
                                ui.selectable_value(&mut self.vehicle_i, i, n);
                            }
                        });
                        ui.end_row();

                        ui.label("Cabin");
                        let cab = cabins.get(self.cabin_i).cloned().unwrap_or_default();
                        egui::ComboBox::from_id_salt("cab").selected_text(cab).show_ui(ui, |ui| {
                            for (i, c) in cabins.iter().enumerate() {
                                ui.selectable_value(&mut self.cabin_i, i, c);
                            }
                        });
                        ui.end_row();

                        ui.label("Chassis");
                        let ch = chassis.get(self.chassis_i).cloned().unwrap_or_default();
                        egui::ComboBox::from_id_salt("chs").selected_text(ch).show_ui(ui, |ui| {
                            for (i, c) in chassis.iter().enumerate() {
                                ui.selectable_value(&mut self.chassis_i, i, c);
                            }
                        });
                        ui.end_row();

                        if !self.interior_variants.is_empty() {
                            ui.label("Interior");
                            let iv = self.interior_variants.get(self.interior_i).cloned().unwrap_or_default();
                            egui::ComboBox::from_id_salt("int").selected_text(iv).show_ui(ui, |ui| {
                                for (i, c) in self.interior_variants.clone().iter().enumerate() {
                                    ui.selectable_value(&mut self.interior_i, i, c);
                                }
                            });
                            ui.end_row();
                        }
                    });
                }

            });
        self.show_advanced = adv;

        if !self.log.is_empty() {
            egui::CollapsingHeader::new(format!("Log ({})", self.log.len()))
                .default_open(self.show_log)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for (c, s) in &self.log {
                                ui.label(egui::RichText::new(s).color(*c).monospace().small());
                            }
                        });
                });
        }
    }
}








