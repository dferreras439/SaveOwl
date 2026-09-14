use anyhow::{anyhow, Result};
use ferrisetw::{
    parser::{Parser, Pointer},
    provider::Provider,
    schema_locator::SchemaLocator,
    trace::UserTrace,
    EventRecord,
};
use parking_lot::Mutex;
use std::{collections::{HashMap, HashSet}, path::PathBuf, sync::Arc};

#[derive(Clone, Default)]
pub struct IoShared {
    pub active_pids: Arc<Mutex<HashSet<u32>>>,
    pub touched: Arc<Mutex<HashSet<PathBuf>>>,
}

pub struct IoTrace {
    _trace: UserTrace,
}

impl IoTrace {
    pub fn start(shared: IoShared) -> Result<Self> {
        let file_objects = Arc::new(Mutex::new(HashMap::<usize, PathBuf>::new()));
        let objects = file_objects.clone();
        let callback = move |record: &EventRecord, locator: &SchemaLocator| {
            handle_event(record, locator, &shared, &objects);
        };

        // Microsoft-Windows-Kernel-File. Unlike a filesystem watcher, EventRecord carries the PID.
        let provider = Provider::by_guid("edd08927-9cc4-4e65-b970-c2560fb5c289")
            .add_callback(callback)
            .build();
        let trace = UserTrace::new()
            .named(format!("SaveOwl-{}", std::process::id()))
            .enable(provider)
            .start_and_process()
            .map_err(|e| anyhow!("starting ETW file trace: {e:?}"))?;
        Ok(Self { _trace: trace })
    }
}

fn handle_event(
    record: &EventRecord,
    locator: &SchemaLocator,
    shared: &IoShared,
    objects: &Arc<Mutex<HashMap<usize, PathBuf>>>,
) {
    let pid = record.process_id();
    let active = shared.active_pids.lock().contains(&pid);
    let Ok(schema) = locator.event_schema(record) else { return; };
    let parser = Parser::create(record, &schema);

    match record.event_id() {
        // Create / CreateNewFile: associate file object with a path.
        12 | 30 => {
            let Ok(obj) = parser.try_parse::<Pointer>("FileObject") else { return; };
            let name = parser.try_parse::<String>("FileName")
                .or_else(|_| parser.try_parse::<String>("FilePath"));
            if let Ok(name) = name {
                let path = PathBuf::from(name);
                objects.lock().insert(*obj, path.clone());
                if active && is_candidate_path(&path) { shared.touched.lock().insert(path); }
            }
        }
        // Write / metadata mutation / set-delete / rename-ish operations.
        16 | 17 | 18 | 19 | 26 | 27 => {
            if !active { return; }
            if let Ok(obj) = parser.try_parse::<Pointer>("FileObject") {
                if let Some(path) = objects.lock().get(&*obj).cloned() {
                    if is_candidate_path(&path) { shared.touched.lock().insert(path); }
                }
            }
            if let Ok(path) = parser.try_parse::<String>("FileName") {
                let path = PathBuf::from(path);
                if is_candidate_path(&path) { shared.touched.lock().insert(path); }
            }
        }
        14 => {
            if let Ok(obj) = parser.try_parse::<Pointer>("FileObject") { objects.lock().remove(&*obj); }
        }
        _ => {}
    }
}

fn is_candidate_path(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy().to_ascii_lowercase();
    if !s.contains(r"\users\") { return false; }
    let bad = [r"\appdata\local\temp\", r"\cache\", r"\caches\", r"\shadercache\", r"\shader_cache\", r"\logs\", r"\crashdumps\"];
    if bad.iter().any(|x| s.contains(x)) { return false; }
    !matches!(path.extension().and_then(|x| x.to_str()).map(|x| x.to_ascii_lowercase()).as_deref(), Some("log" | "tmp" | "dmp" | "etl"))
}
