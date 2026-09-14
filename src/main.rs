#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod engine;
mod gpu;
mod io_trace;
mod repo;
mod win;

use anyhow::{Context, Result};
use config::Config;
use engine::{Controls, UiEvent};
use io_trace::{IoShared, IoTrace};
use repo::Repo;
use std::{path::PathBuf, sync::{atomic::Ordering, Arc}};
use tao::{event::Event, event_loop::{ControlFlow, EventLoopBuilder}};
use tray_icon::{
    Icon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

fn main() -> Result<()> {
    let config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("SaveOwl");
    let data_dir = dirs::data_local_dir().unwrap_or_else(|| config_dir.clone()).join("SaveOwl");
    let config_path = config_dir.join("config.toml");
    let cfg = Config::load_or_create(&config_path)?;
    let repo = Arc::new(Repo::open(data_dir, cfg.upstream_dir.clone(), cfg.max_snapshot_file_mb)?);

    let io = IoShared::default();
    let _io_trace = IoTrace::start(io.clone()).context("ETW file tracing failed; try running SaveOwl as administrator")?;

    let event_loop = EventLoopBuilder::<UiEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let controls = Arc::new(Controls::default());
    engine::spawn(cfg, repo.clone(), io, controls.clone(), proxy.clone());

    let menu = Menu::new();
    let status = MenuItem::with_id("status", "Watching for games", false, None);
    let pause = MenuItem::with_id("pause", "Pause", true, None);
    let force = MenuItem::with_id("force", "Snapshot current session", true, None);
    let data = MenuItem::with_id("data", "Open data folder", true, None);
    let config = MenuItem::with_id("config", "Open config folder", true, None);
    let quit = MenuItem::with_id("quit", "Quit", true, None);
    menu.append_items(&[&status, &PredefinedMenuItem::separator(), &pause, &force, &data, &config, &PredefinedMenuItem::separator(), &quit])?;

    let _tray = TrayIconBuilder::new()
        .with_tooltip("SaveOwl — conflict-safe game saves")
        .with_icon(make_icon()?)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(true)
        .build()?;

    let p = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = p.send_event(UiEvent::Status(format!("__menu:{}", e.id.0)));
    }));

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        if let Event::UserEvent(UiEvent::Status(text)) = event {
            if let Some(id) = text.strip_prefix("__menu:") {
                match id {
                    "pause" => {
                        let next = !controls.paused.load(Ordering::Relaxed);
                        controls.paused.store(next, Ordering::Relaxed);
                        pause.set_text(if next { "Resume" } else { "Pause" });
                    }
                    "force" => controls.force_snapshot.store(true, Ordering::Relaxed),
                    "data" => win::open_folder(repo.data_root()),
                    "config" => win::open_folder(&config_dir),
                    "quit" => { controls.quit.store(true, Ordering::Relaxed); *flow = ControlFlow::Exit; }
                    _ => {}
                }
            } else {
                status.set_text(text);
            }
        }
    });
}

fn make_icon() -> Result<Icon> {
    let w = 32u32; let h = 32u32; let mut rgba = vec![0u8; (w*h*4) as usize];
    for y in 0..h { for x in 0..w {
        let i = ((y*w+x)*4) as usize;
        let dx = x as i32 - 16; let dy = y as i32 - 16;
        let body = dx*dx + dy*dy < 13*13;
        let eye_l = (x as i32-11).pow(2)+(y as i32-13).pow(2) < 10;
        let eye_r = (x as i32-21).pow(2)+(y as i32-13).pow(2) < 10;
        if body { rgba[i..i+4].copy_from_slice(&[50, 70, 95, 255]); }
        if eye_l || eye_r { rgba[i..i+4].copy_from_slice(&[235, 240, 245, 255]); }
        if (x==16 && (16..23).contains(&y)) || (y==20 && (13..20).contains(&x)) { rgba[i..i+4].copy_from_slice(&[235, 170, 55, 255]); }
    }}
    Icon::from_rgba(rgba, w, h).map_err(|e| anyhow::anyhow!(e))
}
