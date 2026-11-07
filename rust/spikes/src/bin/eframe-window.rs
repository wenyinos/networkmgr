//! Spike ①：最小 eframe/glow 窗口。
//!
//! 验证 FreeBSD 15 + X11 + llvmpipe 下能创建 GL 上下文并绘制中文。
//! `SPIKE_SECONDS=5` 时到点自行关闭窗口并正常退出（退出码 0 表示起窗成功），
//! 便于 CI 无人值守；不设该变量则窗口一直开着供人工验收。

use std::time::{Duration, Instant};

use eframe::egui;

struct SpikeApp {
    started: Instant,
    frames: u64,
    limit: Option<Duration>,
}

impl eframe::App for SpikeApp {
    // eframe 0.37 起是 ui(&mut Ui, &mut Frame)，不再是旧的 update(&Context, ...)。
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frames += 1;
        let frames = self.frames;
        let timed_out = self
            .limit
            .is_some_and(|limit| self.started.elapsed() >= limit);
        let ctx = ui.ctx().clone();

        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("NetworkMgr spike 1: eframe / glow / X11");
            ui.label("中文渲染检查：网络管理器");
            ui.label(format!("已绘制帧数：{frames}"));
            ui.label("渲染器：glow / OpenGL（X11）");
            if ui.button("退出").clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });

        if timed_out {
            println!("spike 1: drew {frames} frames, closing");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    let limit = std::env::var("SPIKE_SECONDS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([460.0, 200.0])
            .with_title("NetworkMgr spike 1"),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    eframe::run_native(
        "networkmgr-spike-window",
        options,
        Box::new(|_cc| {
            Ok(Box::new(SpikeApp {
                started: Instant::now(),
                frames: 0,
                limit,
            }))
        }),
    )
}
