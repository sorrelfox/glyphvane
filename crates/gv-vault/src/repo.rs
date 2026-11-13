// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Repository URL mapping: `https://github.com/o/r/blob/main/docs/x.md`
//! opens the local `docs/x.md` when the vault is that repository.
//!
//! Nothing is ever fetched. The mapping needs the repository's remotes and
//! known refs ([`RepoContext`], read from `.git/` by [`RepoContext::read`]).
//! Refs can contain `/`, so the **longest** known ref wins. A full commit
//! SHA maps to the working tree (with a note that it shows the current
//! version).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::link::percent_decode_lossy;
use crate::path::VaultPath;

/// A normalized remote: `git@github.com:o/r.git` and
/// `https://github.com/o/r` both become `github.com` + `o/r`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRemote {
    /// Lower-cased host.
    pub host: String,
    /// The repository path (`owner/repo`, GitLab `group/sub/repo`), without
    /// `.git`.
    pub path: String,
}

impl RepoRemote {
    /// Parses a remote URL in the scp-like (`user@host:path`), `ssh://`,
    /// `git://`, `http(s)://` forms.
    #[must_use]
    pub fn parse(url: &str) -> Option<Self> {
        let url = url.trim();
        let (host, path) = if let Some((_, rest)) = url.split_once("://") {
            let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
            let (hostport, path) = rest.split_once('/')?;
            (hostport.split(':').next().unwrap_or(hostport), path)
        } else {
            // scp-like: [user@]host:path
            let (left, path) = url.split_once(':')?;
            let host = left.rsplit_once('@').map_or(left, |(_, h)| h);
            if host.is_empty() || host.contains('/') {
                return None;
            }
            (host, path)
        };
        let path = path.trim_matches('/');
        let path = path
            .strip_suffix(".git")
            .unwrap_or(path)
            .trim_end_matches('/');
        if host.is_empty() || path.is_empty() {
            return None;
        }
        Some(Self {
            host: host.to_ascii_lowercase(),
            path: path.to_owned(),
        })
    }
}

/// What the resolver knows about the repository the vault is in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepoContext {
    /// Every remote.
    pub remotes: Vec<RepoRemote>,
    /// Known ref names (branches, tags, remote branches without the remote
    /// name), possibly containing `/`.
    pub refs: Vec<String>,
    /// The repository root, relative to the vault root.
    pub repo_root: VaultPath,
}

/// URL forms of repository files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoUrlMode {
    /// `/blob/`: the rendered file.
    Blob,
    /// `/tree/`: a directory.
    Tree,
    /// `/raw/` or `raw.githubusercontent.com`.
    Raw,
}

/// A repository URL mapped to a repository path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedUrl {
    /// The form.
    pub mode: RepoUrlMode,
    /// The ref as matched.
    pub git_ref: String,
    /// The ref is one of the known refs.
    pub known_ref: bool,
    /// The ref is a full commit SHA (mapped to the working tree).
    pub sha: bool,
    /// The path inside the repository (segments, decoded).
    pub path: Vec<String>,
}

impl RepoContext {
    /// Splits `rest` (`<ref segments…>/<path…>`) at the longest known ref.
    fn split_ref(&self, rest: &[String]) -> Option<(String, bool, bool, Vec<String>)> {
        let mut best: Option<(usize, &String)> = None;
        for r in &self.refs {
            let n = r.split('/').count();
            if n <= rest.len()
                && r.split('/').zip(rest).all(|(a, b)| a == b)
                && best.is_none_or(|(m, _)| n > m)
            {
                best = Some((n, r));
            }
        }
        if let Some((n, r)) = best {
            return Some((r.clone(), true, false, rest[n..].to_vec()));
        }
        let first = rest.first()?;
        let sha = first.len() == 40 && first.bytes().all(|b| b.is_ascii_hexdigit());
        Some((first.clone(), false, sha, rest[1..].to_vec()))
    }

    fn mode(seg: &str) -> Option<RepoUrlMode> {
        match seg {
            "blob" => Some(RepoUrlMode::Blob),
            "tree" => Some(RepoUrlMode::Tree),
            "raw" => Some(RepoUrlMode::Raw),
            _ => None,
        }
    }

    /// Maps a web URL of one of this repository's remotes (GitHub `blob`,
    /// `tree`, `raw`, `raw.githubusercontent.com`; GitLab `/-/blob/` …).
    #[must_use]
    pub fn map_web_url(&self, url: &str) -> Option<MappedUrl> {
        let u = url::Url::parse(url).ok()?;
        let host = u.host_str()?.to_ascii_lowercase();
        let segs: Vec<String> = u.path_segments()?.map(percent_decode_lossy).collect();
        if host == "raw.githubusercontent.com" {
            let ours = self.remotes.iter().any(|r| {
                r.host == "github.com" && segs.len() > 2 && eq_repo_path(&r.path, &segs[..2])
            });
            if !ours {
                return None;
            }
            let (git_ref, known_ref, sha, path) = self.split_ref(&segs[2..])?;
            return Some(MappedUrl {
                mode: RepoUrlMode::Raw,
                git_ref,
                known_ref,
                sha,
                path,
            });
        }
        for remote in self.remotes.iter().filter(|r| r.host == host) {
            let n = remote.path.split('/').count();
            if segs.len() <= n || !eq_repo_path(&remote.path, &segs[..n]) {
                continue;
            }
            let mut i = n;
            if segs.get(i).map(String::as_str) == Some("-") {
                i += 1; // GitLab
            }
            let Some(mode) = segs.get(i).and_then(|s| Self::mode(s)) else {
                continue;
            };
            let (git_ref, known_ref, sha, path) = self.split_ref(&segs[i + 1..])?;
            return Some(MappedUrl {
                mode,
                git_ref,
                known_ref,
                sha,
                path,
            });
        }
        None
    }

    /// Maps a relative link that leaves the repository through
    /// `blob/<ref>/` (READMEs use `../../blob/main/docs/x.md`): `segments`
    /// are the link's segments after lexical normalization against a
    /// virtual `blob/<ref>/` base, starting at the mode segment.
    #[must_use]
    pub fn map_escaped(&self, segments: &[String]) -> Option<MappedUrl> {
        let mode = Self::mode(segments.first()?)?;
        let (git_ref, known_ref, sha, path) = self.split_ref(&segments[1..])?;
        Some(MappedUrl {
            mode,
            git_ref,
            known_ref,
            sha,
            path,
        })
    }

    /// Reads remotes and refs from the repository at `worktree` (the
    /// directory containing `.git`, which may be a file for worktrees).
    /// `repo_root` is that directory relative to the vault root.
    pub fn read(worktree: &Path, repo_root: VaultPath) -> io::Result<Self> {
        let git_dir = git_dir(worktree)?;
        let common = match fs::read_to_string(git_dir.join("commondir")) {
            Ok(c) => git_dir.join(c.trim()),
            Err(_) => git_dir.clone(),
        };
        let config = fs::read_to_string(common.join("config")).unwrap_or_default();
        let remotes = parse_remotes(&config);
        let mut refs = Vec::new();
        collect_refs(&common.join("refs/heads"), "", &mut refs);
        collect_refs(&common.join("refs/tags"), "", &mut refs);
        let mut remote_refs = Vec::new();
        collect_refs(&common.join("refs/remotes"), "", &mut remote_refs);
        for r in remote_refs {
            // `origin/feature/x` -> `feature/x`
            if let Some((_, name)) = r.split_once('/')
                && name != "HEAD"
            {
                refs.push(name.to_owned());
            }
        }
        if let Ok(packed) = fs::read_to_string(common.join("packed-refs")) {
            for line in packed.lines() {
                let Some((_, name)) = line.split_once(' ') else {
                    continue;
                };
                if let Some(n) = name
                    .strip_prefix("refs/heads/")
                    .or_else(|| name.strip_prefix("refs/tags/"))
                {
                    refs.push(n.to_owned());
                } else if let Some(n) = name.strip_prefix("refs/remotes/")
                    && let Some((_, b)) = n.split_once('/')
                {
                    refs.push(b.to_owned());
                }
            }
        }
        refs.sort();
        refs.dedup();
        Ok(Self {
            remotes,
            refs,
            repo_root,
        })
    }
}

fn eq_repo_path(remote_path: &str, segs: &[String]) -> bool {
    let mut parts = remote_path.split('/');
    segs.iter()
        .all(|s| parts.next().is_some_and(|p| p.eq_ignore_ascii_case(s)))
        && parts.next().is_none()
}

fn git_dir(worktree: &Path) -> io::Result<PathBuf> {
    let dot_git = worktree.join(".git");
    let meta = fs::metadata(&dot_git)?;
    if meta.is_dir() {
        return Ok(dot_git);
    }
    let text = fs::read_to_string(&dot_git)?;
    let target = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "`.git` file without gitdir"))?;
    Ok(worktree.join(target))
}

fn parse_remotes(config: &str) -> Vec<RepoRemote> {
    let mut out = Vec::new();
    let mut in_remote = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_remote = line.starts_with("[remote ");
            continue;
        }
        if in_remote
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == "url"
            && let Some(r) = RepoRemote::parse(v.trim().trim_matches('"'))
            && !out.contains(&r)
        {
            out.push(r);
        }
    }
    out
}

fn collect_refs(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Some(name) = e.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let full = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        match e.file_type() {
            Ok(t) if t.is_dir() => collect_refs(&e.path(), &full, out),
            Ok(_) => out.push(full),
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_forms_normalize() {
        let a = RepoRemote::parse("git@github.com:o/r.git").unwrap();
        let b = RepoRemote::parse("https://github.com/o/r").unwrap();
        let c = RepoRemote::parse("ssh://git@GitHub.com:22/o/r.git/").unwrap();
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(
            RepoRemote::parse("https://gitlab.com/g/sub/r.git")
                .unwrap()
                .path,
            "g/sub/r"
        );
        assert_eq!(RepoRemote::parse("/local/path"), None);
    }

    fn ctx() -> RepoContext {
        RepoContext {
            remotes: vec![
                RepoRemote::parse("git@github.com:o/r.git").unwrap(),
                RepoRemote::parse("https://gitlab.com/g/sub/r").unwrap(),
            ],
            refs: vec!["main".into(), "feature/x".into(), "feature".into()],
            repo_root: VaultPath::root(),
        }
    }

    #[test]
    fn github_and_gitlab_urls() {
        let c = ctx();
        let m = c
            .map_web_url("https://github.com/o/r/blob/main/docs/x.md")
            .unwrap();
        assert_eq!(
            (m.mode, m.git_ref.as_str(), m.path.join("/")),
            (RepoUrlMode::Blob, "main", "docs/x.md".into())
        );
        let m = c
            .map_web_url("https://github.com/O/R/tree/feature/x/docs")
            .unwrap();
        assert_eq!(
            (m.git_ref.as_str(), m.path.join("/")),
            ("feature/x", "docs".into())
        );
        let m = c
            .map_web_url("https://raw.githubusercontent.com/o/r/main/a%20b.png")
            .unwrap();
        assert_eq!(
            (m.mode, m.path.join("/")),
            (RepoUrlMode::Raw, "a b.png".into())
        );
        let m = c
            .map_web_url("https://gitlab.com/g/sub/r/-/blob/main/README.md")
            .unwrap();
        assert_eq!(m.path, vec!["README.md".to_string()]);
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let m = c
            .map_web_url(&format!("https://github.com/o/r/blob/{sha}/x.md"))
            .unwrap();
        assert!(m.sha && !m.known_ref);
        assert_eq!(
            c.map_web_url("https://github.com/o/other/blob/main/x.md"),
            None
        );
        assert_eq!(c.map_web_url("https://github.com/o/r/issues/1"), None);
    }

    #[test]
    fn config_remotes() {
        let cfg = "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = git@github.com:o/r.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n";
        assert_eq!(
            parse_remotes(cfg),
            vec![RepoRemote {
                host: "github.com".into(),
                path: "o/r".into()
            }]
        );
    }
}
