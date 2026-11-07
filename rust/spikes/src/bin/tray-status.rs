//! Spike ②：最小 tray-icon 原生菜单托盘。
//!
//! 验证图标能出现在 Xfce 面板、菜单/点击事件能收到。
//! `appindicator` 后端必须有 GTK 主循环；`sni` 后端自带 D-Bus worker 线程，
//! 因此 A 类环境可用 `--no-default-features --features sni` 编译验证。
//! `SPIKE_SECONDS`（默认 20）到点自行退出，便于 CI 无人值守。

use std::time::{Duration, Instant};

use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIconBuilder, TrayIconEvent,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "appindicator")]
    gtk::init()?;

    let menu = Menu::new();
    menu.append(&MenuItem::new("NetworkMgr spike 2", false, None))?;
    menu.append(&PredefinedMenuItem::separator())?;
    let quit = MenuItem::new("退出", true, None);
    menu.append(&quit)?;
    let quit_id = quit.id().clone();

    let icon = Icon::from_rgba(networkmgr_spikes::icon_rgba(), 32, 32)?;
    let _tray = TrayIconBuilder::new()
        .with_tooltip("NetworkMgr spike 2 - tray-icon")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()?;

    let seconds = std::env::var("SPIKE_SECONDS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(20);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    println!("spike 2: tray icon created, waiting {seconds}s for events");

    let menu_rx = MenuEvent::receiver();
    let icon_rx = TrayIconEvent::receiver();
    std::thread::spawn(move || loop {
        if let Ok(event) = menu_rx.try_recv() {
            println!("spike 2: menu event {event:?}");
            if event.id == quit_id {
                std::process::exit(0);
            }
        }
        if let Ok(event) = icon_rx.try_recv() {
            println!("spike 2: tray event {event:?}");
        }
        if Instant::now() >= deadline {
            println!("spike 2: timeout, exiting");
            std::process::exit(0);
        }
        std::thread::sleep(Duration::from_millis(50));
    });

    #[cfg(feature = "appindicator")]
    gtk::main();

    // ksni 后端没有主循环要跑，主线程只需等超时线程结束进程。
    #[cfg(not(feature = "appindicator"))]
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }

    #[cfg(feature = "appindicator")]
    Ok(())
}
