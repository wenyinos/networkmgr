//! 阶段 3 骨架：托盘主程序（tray-icon + 原生菜单）。
//!
//! 目标是把「托盘菜单 → 真实动作」这条链打通，菜单项对齐方案第 1.3 节第 1 条
//! 的骨架：状态标题、`Restart Networking`、`Close Network Manager`。动态子菜单
//! （已保存网络、扫描结果、WireGuard）与 30 秒刷新留到下一轮，代码里用
//! `TODO(stage3)` 标出，**不假装已经实现**。
//!
//! `appindicator` 后端要求调用线程上有 GTK 事件循环，所以 main 里先 `gtk::init()`
//! 再建图标、最后 `gtk::main()`；`sni` 后端不需要 GTK，靠自身的 D-Bus worker。
//!
//! `SPIKE_SECONDS` 到点退出，供无人值守验收；点 `Close Network Manager` 立即退出。

use std::thread;
use std::time::{Duration, Instant};

use networkmgr_core::sys::{RealRunner, Sys};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIconBuilder, TrayIconEvent,
};

const ID_TITLE: &str = "status-title";
const ID_RESTART: &str = "restart-networking";
const ID_CLOSE: &str = "close-network-manager";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "appindicator")]
    gtk::init()?;

    let state = Sys::new(&RealRunner::new()).tray_state();
    let title = match (&state.card, state.connection) {
        (None, _) => "Network card is not enabled".to_string(),
        (Some(card), "Connected") => match &state.ssid {
            Some(ssid) => format!("{ssid} ({card})"),
            None => format!("Wired {card} Connected"),
        },
        (Some(card), other) => format!("{card} {other}"),
    };

    let menu = Menu::new();
    menu.append(&MenuItem::with_id(ID_TITLE, &title, false, None))?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&MenuItem::with_id(
        ID_RESTART,
        "Restart Networking",
        true,
        None,
    ))?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&MenuItem::with_id(
        ID_CLOSE,
        "Close Network Manager",
        true,
        None,
    ))?;

    let _tray = TrayIconBuilder::new()
        .with_tooltip(state.tooltip.clone())
        .with_icon(icon_for(state.signal, state.kind == Some("wifi")))
        .with_menu(Box::new(menu))
        .build()?;

    println!(
        "networkmgr (stage 3 skeleton): tray ready, state = {}",
        state.to_json()
    );

    let seconds = std::env::var("SPIKE_SECONDS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(30);
    let deadline = Instant::now() + Duration::from_secs(seconds);

    let menu_rx = MenuEvent::receiver();
    let icon_rx = TrayIconEvent::receiver();
    thread::spawn(move || loop {
        if let Ok(event) = menu_rx.try_recv() {
            println!("networkmgr: menu event {:?}", event.id);
            match event.id.0.as_str() {
                ID_RESTART => {
                    let runner = RealRunner::new();
                    let sys = Sys::new(&runner);
                    match sys.service("netif", "restart") {
                        Ok(()) => println!("networkmgr: netif restarted"),
                        Err(error) => eprintln!("networkmgr: restart failed: {error}"),
                    }
                }
                ID_CLOSE => std::process::exit(0),
                _ => {}
            }
        }
        if let Ok(event) = icon_rx.try_recv() {
            println!("networkmgr: tray event {event:?}");
        }
        if Instant::now() >= deadline {
            println!("networkmgr: timeout, exiting");
            std::process::exit(0);
        }
        thread::sleep(Duration::from_millis(50));
    });

    #[cfg(feature = "appindicator")]
    gtk::main();

    // sni 后端没有主循环要跑，主线程等超时线程结束进程。
    #[cfg(not(feature = "appindicator"))]
    loop {
        thread::sleep(Duration::from_secs(1));
    }

    #[cfg(feature = "appindicator")]
    Ok(())
}

/// 按信号强度生成 0–4 格的方块图标，颜色随格数变化。
///
/// 阶段 3 复用 `src/icons/` 的 PNG 之前，先用代码画，避免为一个骨架引入
/// `image` 解码依赖。
fn icon_for(signal: Option<i32>, wireless: bool) -> Icon {
    const SIZE: u32 = 32;
    let bars = match signal {
        None => 0,
        Some(value) if value >= 75 => 4,
        Some(value) if value >= 50 => 3,
        Some(value) if value >= 25 => 2,
        Some(_) => 1,
    };
    let color = if !wireless {
        (120, 120, 120)
    } else if bars >= 3 {
        (60, 150, 90)
    } else if bars >= 1 {
        (200, 150, 60)
    } else {
        (170, 70, 70)
    };

    let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            // 4 根竖条，从底部往上点亮 bars 根
            let column = x / 8;
            let filled = column < bars && y >= 8 + (3 - column) * 4;
            let (r, g, b) = if bars == 0 {
                // 无信号/有线：画一个实心浅色方块。全暗的图标在深色面板上
                // 等于没画，第一次 VM 验收就吃了这个亏。
                if (10..22).contains(&x) && (10..22).contains(&y) {
                    (210, 210, 210)
                } else {
                    (70, 70, 70)
                }
            } else if filled {
                color
            } else {
                (40, 40, 40)
            };
            pixels.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Icon::from_rgba(pixels, SIZE, SIZE).expect("32x32 RGBA icon is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_bars_follow_the_signal() {
        // 只是保证不 panic、尺寸正确；颜色分档由上面的阈值决定
        for signal in [None, Some(0), Some(30), Some(60), Some(90)] {
            let icon = icon_for(signal, true);
            // Icon 没有公开尺寸访问器，这里以构造成功为准
            drop(icon);
        }
    }
}
