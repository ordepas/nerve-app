use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::{DiffResult, FileChange};

fn s(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

fn git(cwd: &Path, args: &[String]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("git {}: {}", args.join(" "), e))?;
    if !out.status.success() {
        return Err(format!(
            "git {} falló: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub fn is_repo(cwd: &Path) -> bool {
    git(cwd, &s(&["rev-parse", "--is-inside-work-tree"]))
        .map(|v| v.trim() == "true")
        .unwrap_or(false)
}

/// true si hay cambios sin confirmar (sin tocar el índice del usuario).
pub fn is_dirty(cwd: &Path) -> bool {
    git(cwd, &s(&["status", "--porcelain"]))
        .map(|o| !o.trim().is_empty())
        .unwrap_or(true)
}

pub fn current_branch(cwd: &Path) -> Result<String, String> {
    Ok(git(cwd, &s(&["branch", "--show-current"]))?.trim().to_string())
}

pub fn head_sha(cwd: &Path) -> Result<String, String> {
    Ok(git(cwd, &s(&["rev-parse", "HEAD"]))?.trim().to_string())
}

pub fn worktree_base(cwd: &Path) -> PathBuf {
    cwd.join(".nerve-worktrees")
}

pub fn nerve_worktree_dir(cwd: &Path, id: &str) -> Option<PathBuf> {
    let dir = worktree_base(cwd).join(id);
    dir.exists().then_some(dir)
}

fn ensure_local_exclude(cwd: &Path) -> Result<(), String> {
    let f = cwd.join(".git").join("info").join("exclude");
    if let Some(parent) = f.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let content = fs::read_to_string(&f).unwrap_or_default();
    if !content.lines().any(|l| l.trim() == ".nerve-worktrees/") {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&f)
            .map_err(|e| e.to_string())?;
        writeln!(file, ".nerve-worktrees/").map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Crea un git worktree aislado en .nerve-worktrees/<id> sobre una rama nueva nerve/<id>.
pub fn add_worktree(cwd: &Path, id: &str) -> Result<PathBuf, String> {
    if !is_repo(cwd) {
        return Err("la carpeta del workspace no es un repositorio git".into());
    }
    let base = worktree_base(cwd);
    fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let dir = base.join(id);
    if dir.exists() {
        return Err(format!("el worktree {} ya existe", id));
    }
    ensure_local_exclude(cwd)?;
    let dir_str = dir.to_string_lossy().to_string();
    let branch = format!("nerve/{}", id);
    git(cwd, &s(&["worktree", "add", &dir_str, "-b", &branch]))?;
    Ok(dir)
}

pub fn remove_worktree(cwd: &Path, id: &str, _force: bool) -> Result<(), String> {
    let dir = worktree_base(cwd).join(id);
    let branch = format!("nerve/{}", id);

    // `git worktree remove` puede quedarse esperando confirmación interactiva
    // cuando OneDrive bloquea archivos; se borra el directorio directamente y
    // se hace prune del registro.
    if dir.exists() {
        remove_dir_retry(&dir, 3)?;
    }
    let _ = git(cwd, &s(&["worktree", "prune", "--verbose"]));
    let _ = git(cwd, &s(&["branch", "-D", &branch]));

    if dir.exists() {
        return Err(format!(
            "no se pudo borrar el worktree {} (archivos bloqueados, p. ej. por OneDrive); reintenta en unos segundos",
            id
        ));
    }
    Ok(())
}

fn remove_dir_retry(dir: &Path, attempts: u32) -> Result<(), String> {
    let mut last = String::new();
    for i in 0..attempts {
        match fs::remove_dir_all(dir) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = e.to_string();
                if i + 1 < attempts {
                    std::thread::sleep(std::time::Duration::from_millis(700));
                }
            }
        }
    }
    Err(format!("no se pudo borrar {}: {}", dir.display(), last))
}

/// Lista los worktrees creados por Nerve: (id, ruta).
pub fn list_nerve_worktrees(cwd: &Path) -> Result<Vec<(String, String)>, String> {
    let out = git(cwd, &s(&["worktree", "list", "--porcelain"]))?;
    let mut v: Vec<(String, String)> = Vec::new();
    let mut path: Option<String> = None;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            path = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("branch ") {
            if let Some(p) = &path {
                if rest.contains("nerve/") {
                    let id = p
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or("")
                        .to_string();
                    v.push((id, p.clone()));
                }
            }
        } else if line.is_empty() {
            path = None;
        }
    }
    Ok(v)
}

pub fn stage_all(cwd: &Path) -> Result<(), String> {
    git(cwd, &s(&["add", "-A"]))?;
    Ok(())
}

/// Commit con identidad local nerve (no depende de la config global del usuario).
pub fn commit_all(cwd: &Path, msg: &str) -> Result<String, String> {
    stage_all(cwd)?;
    git(
        cwd,
        &s(&["-c", "user.name=nerve", "-c", "user.email=nerve@local", "commit", "-m", msg]),
    )?;
    head_sha(cwd)
}

/// Diff completo (incluye archivos nuevos) del árbol actual contra `from`.
pub fn diff(cwd: &Path, from: Option<&str>) -> Result<DiffResult, String> {
    let base = from.unwrap_or("HEAD").to_string();
    stage_all(cwd)?;
    let text = git(
        cwd,
        &s(&["diff", "--cached", "--no-color", &base]),
    )?;
    let numstat = git(
        cwd,
        &s(&["diff", "--cached", "--numstat", &base]),
    )?;
    let mut files = Vec::new();
    for line in numstat.lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() == 3 {
            let adds = parts[0].parse::<i64>().unwrap_or(0);
            let dels = parts[1].parse::<i64>().unwrap_or(0);
            let status = if adds > 0 && dels == 0 {
                "added"
            } else if dels > 0 && adds == 0 {
                "deleted"
            } else {
                "modified"
            };
            files.push(FileChange {
                path: parts[2].to_string(),
                status: status.to_string(),
                additions: adds,
                deletions: dels,
            });
        }
    }
    Ok(DiffResult { diff: text, files })
}

/// Revierte el árbol actual (workspace) al sha indicado.
pub fn reset_hard(cwd: &Path, sha: &str) -> Result<(), String> {
    git(cwd, &s(&["reset", "--hard", sha]))?;
    Ok(())
}

/// Fusiona la rama del worktree (nerve/<id>) en la rama actual del workspace.
pub fn merge_worktree(cwd: &Path, id: &str) -> Result<(), String> {
    let branch = format!("nerve/{}", id);
    git(
        cwd,
        &s(&[
            "-c", "user.name=nerve", "-c", "user.email=nerve@local",
            "merge", "--no-ff", &branch, "-m", &format!("Nerve: merge {}", id),
        ]),
    )?;
    Ok(())
}