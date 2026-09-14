use crate::{config::Config, gpu::GpuSampler, io_trace::IoShared, repo::{game_key, Repo}, win};
use anyhow::Result;
use std::{collections::{HashMap, HashSet}, path::PathBuf, sync::{atomic::{AtomicBool, Ordering}, Arc}, thread, time::Duration};
use sysinfo::{Pid, ProcessesToUpdate, System};
use tao::event_loop::EventLoopProxy;

#[derive(Debug, Clone)]
pub enum UiEvent { Status(String) }

pub struct Controls {
    pub paused: AtomicBool,
    pub force_snapshot: AtomicBool,
    pub quit: AtomicBool,
}
impl Default for Controls {
    fn default() -> Self { Self { paused: false.into(), force_snapshot: false.into(), quit: false.into() } }
}

#[derive(Clone)]
struct Session {
    root_pid: u32,
    exe: PathBuf,
    name: String,
    cool: u32,
}

pub fn spawn(cfg: Config, repo: Arc<Repo>, io: IoShared, controls: Arc<Controls>, proxy: EventLoopProxy<UiEvent>) {
    thread::spawn(move || {
        if let Err(e) = run(cfg, repo, io, controls, proxy.clone()) {
            let _ = proxy.send_event(UiEvent::Status(format!("Error: {e:#}")));
        }
    });
}

fn run(cfg: Config, repo: Arc<Repo>, io: IoShared, controls: Arc<Controls>, proxy: EventLoopProxy<UiEvent>) -> Result<()> {
    let mut gpu = GpuSampler::new()?;
    let mut sys = System::new_all();
    let mut candidates = HashMap::<u32, u32>::new();
    let mut session: Option<Session> = None;
    let ignored: HashSet<String> = cfg.ignore_processes.iter().map(|x| x.to_ascii_lowercase()).collect();
    let tick = Duration::from_millis(cfg.poll_ms.max(250));

    while !controls.quit.load(Ordering::Relaxed) {
        if controls.paused.load(Ordering::Relaxed) {
            io.active_pids.lock().clear();
            let _ = proxy.send_event(UiEvent::Status("Paused".into()));
            thread::sleep(tick); continue;
        }

        sys.refresh_processes(ProcessesToUpdate::All, true);
        let samples = gpu.sample().unwrap_or_default();

        if let Some(s) = session.as_mut() {
            let descendants = process_tree(&sys, s.root_pid);
            *io.active_pids.lock() = descendants.clone();
            let usage = descendants.iter().filter_map(|p| samples.get(p)).copied().fold(0.0, f64::max);
            if usage < cfg.gpu_threshold_percent * 0.35 { s.cool += 1; } else { s.cool = 0; }
            let _ = proxy.send_event(UiEvent::Status(format!("Tracking {} — {:.0}% GPU — {} files", s.name, usage, io.touched.lock().len())));

            let forced = controls.force_snapshot.swap(false, Ordering::Relaxed);
            if s.cool >= cfg.end_samples || forced || !sys.process(Pid::from_u32(s.root_pid)).is_some() {
                finish_session(&repo, &io, s)?;
                io.active_pids.lock().clear(); io.touched.lock().clear();
                session = None; candidates.clear();
                let _ = proxy.send_event(UiEvent::Status("Watching for games".into()));
            }
        } else {
            let mut best: Option<(u32, f64)> = None;
            for (&pid, &usage) in &samples {
                if usage < cfg.gpu_threshold_percent { continue; }
                let Some(proc_) = sys.process(Pid::from_u32(pid)) else { continue; };
                let name = proc_.name().to_string_lossy().to_ascii_lowercase();
                if ignored.contains(&name) { continue; }
                let n = candidates.entry(pid).or_default(); *n += 1;
                if *n >= cfg.activation_samples && best.map(|x| usage > x.1).unwrap_or(true) { best = Some((pid, usage)); }
            }
            candidates.retain(|pid, _| samples.get(pid).copied().unwrap_or(0.0) >= cfg.gpu_threshold_percent * 0.5);
            if let Some((pid, _)) = best {
                if let Some(p) = sys.process(Pid::from_u32(pid)) {
                    if let Some(exe) = p.exe() {
                        let name = p.name().to_string_lossy().into_owned();
                        let key = game_key(exe);
                        repo.reconcile(&key, |local, remote| win::prompt_conflict(&name, &local.to_string(), &remote.to_string()))?;
                        io.touched.lock().clear();
                        *io.active_pids.lock() = process_tree(&sys, pid);
                        session = Some(Session { root_pid: pid, exe: exe.to_path_buf(), name: name.clone(), cool: 0 });
                        let _ = proxy.send_event(UiEvent::Status(format!("Tracking {name}")));
                    }
                }
            } else {
                let _ = proxy.send_event(UiEvent::Status("Watching for games".into()));
            }
        }
        thread::sleep(tick);
    }
    Ok(())
}

fn finish_session(repo: &Repo, io: &IoShared, s: &Session) -> Result<()> {
    let touched: Vec<_> = io.touched.lock().iter().cloned().collect();
    let key = game_key(&s.exe);
    if repo.snapshot(&key, &s.exe, &s.name, &touched)?.is_some() {
        repo.reconcile(&key, |local, remote| win::prompt_conflict(&s.name, &local.to_string(), &remote.to_string()))?;
    }
    Ok(())
}

fn process_tree(sys: &System, root: u32) -> HashSet<u32> {
    let mut out = HashSet::from([root]);
    loop {
        let before = out.len();
        for (pid, p) in sys.processes() {
            if let Some(parent) = p.parent() {
                if out.contains(&parent.as_u32()) { out.insert(pid.as_u32()); }
            }
        }
        if out.len() == before { break; }
    }
    out
}
