use anyhow::{anyhow, Context, Result};
use blake3::Hasher;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::Read, path::{Path, PathBuf}};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: Uuid,
    pub parent: Option<Uuid>,
    pub game_key: String,
    pub exe: PathBuf,
    pub process_name: String,
    pub machine_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub files: BTreeMap<String, FileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub hash: Option<String>,
    pub size: u64,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GameState {
    pub local_head: Option<Uuid>,
    pub last_seen_upstream: Option<Uuid>,
}

#[derive(Debug, Clone, Copy)]
pub enum ConflictChoice { KeepLocal, UseUpstream, KeepBoth }

pub struct Repo {
    root: PathBuf,
    upstream: Option<PathBuf>,
    pub machine_id: Uuid,
    max_file_bytes: u64,
}

impl Repo {
    pub fn open(root: PathBuf, upstream: Option<PathBuf>, max_mb: u64) -> Result<Self> {
        fs::create_dir_all(root.join("blobs"))?;
        fs::create_dir_all(root.join("games"))?;
        let machine_path = root.join("machine-id");
        let machine_id = if machine_path.exists() {
            fs::read_to_string(&machine_path)?.trim().parse()?
        } else {
            let id = Uuid::new_v4(); fs::write(&machine_path, id.to_string())?; id
        };
        if let Some(u) = &upstream { fs::create_dir_all(u)?; }
        Ok(Self { root, upstream, machine_id, max_file_bytes: max_mb * 1024 * 1024 })
    }

    pub fn snapshot(&self, game_key: &str, exe: &Path, name: &str, touched: &[PathBuf]) -> Result<Option<Snapshot>> {
        let state = self.load_state(game_key)?;
        let parent = state.local_head;
        let previous = parent.and_then(|id| self.load_snapshot(game_key, id).ok());
        let mut files = previous.as_ref().map(|x| x.files.clone()).unwrap_or_default();
        let mut any = false;

        for path in touched {
            let key = canonical_key(path);
            if path.exists() && path.is_file() {
                let meta = fs::metadata(path)?;
                if meta.len() > self.max_file_bytes { continue; }
                let hash = self.store_blob(path)?;
                let next = FileEntry { hash: Some(hash), size: meta.len(), deleted: false };
                if files.get(&key).map(|x| (x.hash.as_deref(), x.deleted)) != Some((next.hash.as_deref(), false)) {
                    files.insert(key, next); any = true;
                }
            } else if files.contains_key(&key) {
                files.insert(key, FileEntry { hash: None, size: 0, deleted: true }); any = true;
            }
        }
        if !any { return Ok(None); }
        let snap = Snapshot {
            id: Uuid::new_v4(), parent, game_key: game_key.into(), exe: exe.to_path_buf(), process_name: name.into(),
            machine_id: self.machine_id, created_at: Utc::now(), files,
        };
        self.write_snapshot(&snap)?;
        let mut state = state; state.local_head = Some(snap.id); self.save_state(game_key, &state)?;
        Ok(Some(snap))
    }

    pub fn reconcile<F>(&self, game_key: &str, mut choose: F) -> Result<()>
    where F: FnMut(Uuid, Uuid) -> ConflictChoice {
        let Some(upstream_root) = &self.upstream else { return Ok(()); };
        let mut state = self.load_state(game_key)?;
        let upstream_head = read_head(&upstream_root.join(game_key).join("HEAD"))?;
        let local_head = state.local_head;

        match (local_head, upstream_head) {
            (None, None) => return Ok(()),
            (Some(local), None) => {
                self.publish_snapshot_chain(game_key, local, upstream_root)?;
                write_head(&upstream_root.join(game_key).join("HEAD"), local)?;
                state.last_seen_upstream = Some(local);
            }
            (None, Some(remote)) => {
                self.import_snapshot_chain(game_key, remote, upstream_root)?;
                self.restore(game_key, remote)?;
                state.local_head = Some(remote); state.last_seen_upstream = Some(remote);
            }
            (Some(local), Some(remote)) if local == remote => state.last_seen_upstream = Some(remote),
            (Some(local), Some(remote)) if state.last_seen_upstream == Some(remote) => {
                self.publish_snapshot_chain(game_key, local, upstream_root)?;
                write_head(&upstream_root.join(game_key).join("HEAD"), local)?;
                state.last_seen_upstream = Some(local);
            }
            (Some(local), Some(remote)) if state.last_seen_upstream == Some(local) => {
                self.import_snapshot_chain(game_key, remote, upstream_root)?;
                self.restore(game_key, remote)?;
                state.local_head = Some(remote); state.last_seen_upstream = Some(remote);
            }
            (Some(local), Some(remote)) => {
                self.import_snapshot_chain(game_key, remote, upstream_root)?;
                match choose(local, remote) {
                    ConflictChoice::KeepLocal => {
                        self.publish_snapshot_chain(game_key, local, upstream_root)?;
                        write_head(&upstream_root.join(game_key).join("HEAD"), local)?;
                        state.last_seen_upstream = Some(local);
                    }
                    ConflictChoice::UseUpstream => {
                        self.restore(game_key, remote)?;
                        state.local_head = Some(remote); state.last_seen_upstream = Some(remote);
                    }
                    ConflictChoice::KeepBoth => {
                        // Never overwrite either branch. Leave the divergence unresolved so a
                        // later reconcile prompts again instead of silently publishing one side.
                        let cdir = self.root.join("games").join(game_key).join("conflicts");
                        fs::create_dir_all(&cdir)?;
                        fs::write(cdir.join(format!("remote-{remote}.txt")), remote.to_string())?;
                    }
                }
            }
        }
        self.save_state(game_key, &state)
    }

    pub fn force_snapshot_marker(&self) -> Result<()> {
        fs::write(self.root.join("last-force-snapshot"), Utc::now().to_rfc3339())?; Ok(())
    }

    pub fn data_root(&self) -> &Path { &self.root }

    fn restore(&self, game_key: &str, id: Uuid) -> Result<()> {
        let snap = self.load_snapshot(game_key, id)?;
        for (path, entry) in snap.files {
            let path = PathBuf::from(path);
            if entry.deleted { let _ = fs::remove_file(&path); continue; }
            let Some(hash) = entry.hash else { continue; };
            let src = self.blob_path(&hash);
            if !src.exists() { return Err(anyhow!("missing blob {hash}")); }
            if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
            fs::copy(src, path)?;
        }
        Ok(())
    }

    fn store_blob(&self, path: &Path) -> Result<String> {
        let mut f = fs::File::open(path)?;
        let mut hasher = Hasher::new(); let mut buf = [0u8; 1024 * 1024];
        loop { let n = f.read(&mut buf)?; if n == 0 { break; } hasher.update(&buf[..n]); }
        let hash = hasher.finalize().to_hex().to_string(); let dest = self.blob_path(&hash);
        if !dest.exists() { if let Some(p) = dest.parent() { fs::create_dir_all(p)?; } fs::copy(path, &dest)?; }
        Ok(hash)
    }

    fn blob_path(&self, hash: &str) -> PathBuf { self.root.join("blobs").join(&hash[..2]).join(hash) }
    fn game_dir(&self, key: &str) -> PathBuf { self.root.join("games").join(key) }
    fn state_path(&self, key: &str) -> PathBuf { self.game_dir(key).join("state.json") }
    fn snapshot_path(&self, key: &str, id: Uuid) -> PathBuf { self.game_dir(key).join("snapshots").join(format!("{id}.json")) }

    fn load_state(&self, key: &str) -> Result<GameState> {
        let p = self.state_path(key); if !p.exists() { return Ok(GameState::default()); }
        Ok(serde_json::from_slice(&fs::read(p)?)?)
    }
    fn save_state(&self, key: &str, s: &GameState) -> Result<()> {
        let p = self.state_path(key); if let Some(parent) = p.parent() { fs::create_dir_all(parent)?; }
        atomic_write_json(&p, s)
    }
    fn write_snapshot(&self, s: &Snapshot) -> Result<()> {
        let p = self.snapshot_path(&s.game_key, s.id); if let Some(parent) = p.parent() { fs::create_dir_all(parent)?; }
        atomic_write_json(&p, s)
    }
    fn load_snapshot(&self, key: &str, id: Uuid) -> Result<Snapshot> {
        Ok(serde_json::from_slice(&fs::read(self.snapshot_path(key, id))?)?)
    }

    fn publish_snapshot_chain(&self, key: &str, mut id: Uuid, upstream: &Path) -> Result<()> {
        let game = upstream.join(key); fs::create_dir_all(game.join("snapshots"))?; fs::create_dir_all(upstream.join("blobs"))?;
        loop {
            let snap = self.load_snapshot(key, id)?;
            let dst = game.join("snapshots").join(format!("{}.json", snap.id));
            if !dst.exists() { fs::copy(self.snapshot_path(key, snap.id), &dst)?; }
            for entry in snap.files.values() {
                if let Some(hash) = &entry.hash {
                    let src = self.blob_path(hash); let dst = upstream.join("blobs").join(&hash[..2]).join(hash);
                    if !dst.exists() { if let Some(p)=dst.parent(){fs::create_dir_all(p)?;} fs::copy(src,dst)?; }
                }
            }
            let Some(parent) = snap.parent else { break; }; id = parent;
        }
        Ok(())
    }

    fn import_snapshot_chain(&self, key: &str, mut id: Uuid, upstream: &Path) -> Result<()> {
        loop {
            let src = upstream.join(key).join("snapshots").join(format!("{id}.json"));
            if !src.exists() { return Err(anyhow!("upstream snapshot missing: {id}")); }
            let snap: Snapshot = serde_json::from_slice(&fs::read(&src)?)?;
            let dst = self.snapshot_path(key, id); if !dst.exists() { if let Some(p)=dst.parent(){fs::create_dir_all(p)?;} fs::copy(&src,&dst)?; }
            for entry in snap.files.values() {
                if let Some(hash) = &entry.hash {
                    let srcb = upstream.join("blobs").join(&hash[..2]).join(hash); let dstb = self.blob_path(hash);
                    if !dstb.exists() { if let Some(p)=dstb.parent(){fs::create_dir_all(p)?;} fs::copy(srcb,dstb)?; }
                }
            }
            let Some(parent) = snap.parent else { break; }; id = parent;
        }
        Ok(())
    }
}

pub fn game_key(exe: &Path) -> String {
    let normalized = exe.to_string_lossy().to_ascii_lowercase();
    blake3::hash(normalized.as_bytes()).to_hex()[..16].to_string()
}

fn canonical_key(path: &Path) -> String {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().into_owned()
}
fn replace_with_temp(tmp: &Path, path: &Path) -> Result<()> {
    // std::fs::rename does not replace an existing destination on Windows.
    // The repository's immutable snapshots are unaffected; this helper is only
    // used for small mutable metadata files (state/HEAD).
    if path.exists() { fs::remove_file(path)?; }
    fs::rename(tmp, path)?;
    Ok(())
}
fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    replace_with_temp(&tmp, path)
}
fn read_head(path: &Path) -> Result<Option<Uuid>> {
    if !path.exists() { return Ok(None); } Ok(Some(fs::read_to_string(path)?.trim().parse().context("invalid upstream HEAD")?))
}
fn write_head(path: &Path, id: Uuid) -> Result<()> {
    if let Some(p)=path.parent(){fs::create_dir_all(p)?;}
    let tmp=path.with_extension("tmp");
    fs::write(&tmp,id.to_string())?;
    replace_with_temp(&tmp, path)
}
