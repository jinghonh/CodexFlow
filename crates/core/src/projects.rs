use codexflow_domain::{LocalProject, ThreadAttribution, ThreadMetadata};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

struct GitWorkspace {
    root: PathBuf,
    common_dir: PathBuf,
}

fn git_workspace(path: &Path) -> Option<GitWorkspace> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args([
            "rev-parse",
            "--show-toplevel",
            "--path-format=absolute",
            "--git-common-dir",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let mut lines = text.lines();
    let root = fs::canonicalize(lines.next()?).ok()?;
    let common_dir = fs::canonicalize(lines.next()?).ok()?;
    Some(GitWorkspace { root, common_dir })
}

fn has_git_marker(path: &Path) -> bool {
    path.ancestors()
        .any(|ancestor| fs::symlink_metadata(ancestor.join(".git")).is_ok())
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn project_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_text(path))
}

fn git_project(workspace: &GitWorkspace) -> LocalProject {
    let id = format!("git:{}", path_text(&workspace.common_dir));
    let main_root = workspace
        .common_dir
        .file_name()
        .filter(|name| *name == ".git")
        .and_then(|_| workspace.common_dir.parent())
        .unwrap_or(&workspace.root);
    LocalProject {
        id,
        name: project_name(main_root),
        root: path_text(main_root),
        git_common_dir: Some(path_text(&workspace.common_dir)),
    }
}

pub fn selected_project(path: &str) -> Result<LocalProject, String> {
    if !Path::new(path).is_absolute() {
        return Err("请输入项目目录的绝对路径。".into());
    }
    let root = fs::canonicalize(path).map_err(|_| "项目目录不存在或无法读取。".to_owned())?;
    if !root.is_dir() {
        return Err("请选择一个目录。".to_owned());
    }
    if let Some(workspace) = git_workspace(&root) {
        return Ok(git_project(&workspace));
    }
    if has_git_marker(&root) {
        return Err("检测到 Git 工作区，但无法验证共享 Git 目录。".into());
    }
    Ok(LocalProject {
        id: format!("dir:{}", path_text(&root)),
        name: project_name(&root),
        root: path_text(&root),
        git_common_dir: None,
    })
}

fn attribution(
    thread: &ThreadMetadata,
    project_id: Option<String>,
    workspace_root: Option<String>,
    basis: &str,
    detail: String,
) -> ThreadAttribution {
    ThreadAttribution {
        thread_id: thread.id.clone(),
        project_id,
        workspace_root,
        basis: basis.to_owned(),
        detail,
        diagnostic: None,
        source_project_id: thread.project_id.clone(),
    }
}

pub fn reconcile(
    threads: &[ThreadMetadata],
    known_projects: Vec<LocalProject>,
    previous: &[ThreadAttribution],
) -> (Vec<LocalProject>, Vec<ThreadAttribution>) {
    let mut projects: BTreeMap<String, LocalProject> = known_projects
        .into_iter()
        .map(|project| (project.id.clone(), project))
        .collect();
    let previous_by_thread: BTreeMap<&str, &ThreadAttribution> = previous
        .iter()
        .map(|item| (item.thread_id.as_str(), item))
        .collect();
    let mut results = Vec::with_capacity(threads.len());
    let mut missing_paths = BTreeSet::new();

    for thread in threads {
        let old = previous_by_thread.get(thread.id.as_str()).copied();
        let real = if Path::new(&thread.cwd).is_absolute() {
            fs::canonicalize(&thread.cwd)
                .ok()
                .filter(|path| path.is_dir())
        } else {
            None
        };
        if real.is_none() {
            missing_paths.insert(thread.id.clone());
        }
        let direct = real.as_ref().and_then(|path| {
            if let Some(workspace) = git_workspace(path) {
                let project = git_project(&workspace);
                let id = project.id.clone();
                projects.entry(id.clone()).or_insert(project);
                return Some(attribution(
                    thread,
                    Some(id),
                    Some(path_text(&workspace.root)),
                    "gitCommonDir",
                    format!("共享 Git 目录：{}", path_text(&workspace.common_dir)),
                ));
            }
            if has_git_marker(path) {
                return None;
            }
            projects
                .values()
                .filter(|project| {
                    project.git_common_dir.is_none() && path.starts_with(&project.root)
                })
                .max_by_key(|project| project.root.len())
                .map(|project| {
                    attribution(
                        thread,
                        Some(project.id.clone()),
                        Some(project.root.clone()),
                        "directoryRoot",
                        format!("位于已选择的目录：{}", project.root),
                    )
                })
        });
        let result = match direct {
            Some(mut direct) => {
                if let Some(old) = old
                    .filter(|old| old.project_id.is_some() && old.project_id != direct.project_id)
                {
                    let current_project_id = direct.project_id.clone().unwrap_or_default();
                    direct.project_id = old.project_id.clone();
                    direct.workspace_root = old.workspace_root.clone();
                    direct.basis = "previousVerified".into();
                    direct.detail = "沿用此会话此前已验证的项目归属。".into();
                    direct.diagnostic = Some(format!("当前路径指向另一个项目（{current_project_id}）；请检查工作区迁移或路径复用。"));
                }
                direct
            }
            None => {
                if let Some(old) = old.filter(|old| {
                    old.project_id
                        .as_ref()
                        .is_some_and(|id| projects.contains_key(id))
                }) {
                    let mut kept = attribution(
                        thread,
                        old.project_id.clone(),
                        old.workspace_root.clone(),
                        "previousVerified",
                        "当前工作目录无法验证，沿用此会话此前已验证的归属。".into(),
                    );
                    kept.diagnostic = Some("工作目录已失效或不再处于此前的项目范围。".into());
                    kept
                } else {
                    attribution(
                        thread,
                        None,
                        None,
                        "unassigned",
                        if !Path::new(&thread.cwd).is_absolute() {
                            "来源工作目录不是绝对路径，无法确认本地归属。".into()
                        } else if real.is_none() {
                            "工作目录不存在或无法读取，尚无可复用的已验证归属。".into()
                        } else if real.as_deref().is_some_and(has_git_marker) {
                            "检测到 Git 工作区，但无法验证仓库身份；保持未归属。".into()
                        } else {
                            "工作目录不属于已选择的非 Git 项目；请选择其真实根目录。".into()
                        },
                    )
                }
            }
        };
        results.push(result);
    }

    let mut source_projects: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for item in previous.iter().chain(results.iter()) {
        if item.basis == "gitCommonDir"
            || item.basis == "directoryRoot"
            || item.basis == "previousVerified"
        {
            if let (Some(source), Some(project)) = (&item.source_project_id, &item.project_id) {
                source_projects
                    .entry(source.clone())
                    .or_default()
                    .insert(project.clone());
            }
        }
    }
    for item in &mut results {
        let Some(source) = item.source_project_id.as_deref() else {
            continue;
        };
        let Some(mapped) = source_projects.get(source) else {
            continue;
        };
        if mapped.len() > 1 {
            item.diagnostic = Some(format!(
                "来源项目标识 {source} 对应多个已验证的本地项目；已按路径证据保持仓库边界。"
            ));
        } else if item.project_id.is_none()
            && item.basis == "unassigned"
            && missing_paths.contains(&item.thread_id)
        {
            let project_id = mapped.iter().next().unwrap().clone();
            if projects.contains_key(&project_id) {
                item.project_id = Some(project_id);
                item.basis = "sourceProjectId".into();
                item.detail = format!("路径失效；复用来源项目标识 {source} 此前唯一的已验证映射。");
            }
        } else if item
            .project_id
            .as_deref()
            .is_some_and(|id| !mapped.contains(id))
        {
            item.diagnostic = Some(format!(
                "来源项目标识 {source} 与当前路径的项目归属不一致；已按可验证路径保留仓库边界。"
            ));
        }
    }
    (projects.into_values().collect(), results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexflow_domain::{GitMetadata, ThreadMetadata};
    use std::{
        os::unix::fs::symlink,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn git(args: &[&str]) {
        let output = Command::new("git").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn thread(id: &str, cwd: &Path, source_project_id: Option<&str>) -> ThreadMetadata {
        ThreadMetadata {
            id: id.into(),
            session_id: id.into(),
            title: None,
            preview: String::new(),
            cwd: path_text(cwd),
            project_id: source_project_id.map(str::to_owned),
            source_kind: "cli".into(),
            source_detail: None,
            thread_source: None,
            parent_thread_id: None,
            forked_from_id: None,
            git: None,
            created_at: 0,
            updated_at: 0,
            archived: false,
            metadata_complete: true,
            turns_complete: false,
            items_complete: false,
            missing_from_source: false,
            content_complete: false,
            read_error: None,
            observed_at_unix_ms: 0,
        }
    }

    #[test]
    fn real_repositories_worktrees_clones_nested_repos_and_missing_paths() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base =
            std::env::temp_dir().join(format!("codexflow-projects-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        let repo = base.join("repo");
        let worktree = base.join("worktree");
        let clone = base.join("clone");
        let nested = repo.join("nested");
        let plain = base.join("plain");
        let plain_sibling = base.join("plain-other");
        let plain_nested_git = plain.join("nested-git");
        let broken_git = plain.join("broken-git");
        let alias = base.join("alias");
        fs::create_dir_all(&repo).unwrap();
        git(&["init", "-q", repo.to_str().unwrap()]);
        git(&[
            "-C",
            repo.to_str().unwrap(),
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ]);
        git(&[
            "-C",
            repo.to_str().unwrap(),
            "worktree",
            "add",
            "-qb",
            "other",
            worktree.to_str().unwrap(),
        ]);
        git(&[
            "clone",
            "-q",
            repo.to_str().unwrap(),
            clone.to_str().unwrap(),
        ]);
        let worktree_child = worktree.join("src");
        fs::create_dir_all(&worktree_child).unwrap();
        fs::create_dir_all(&nested).unwrap();
        git(&["init", "-q", nested.to_str().unwrap()]);
        fs::create_dir_all(&plain).unwrap();
        fs::create_dir_all(&plain_sibling).unwrap();
        fs::create_dir_all(&plain_nested_git).unwrap();
        git(&["init", "-q", plain_nested_git.to_str().unwrap()]);
        fs::create_dir_all(broken_git.join(".git")).unwrap();
        symlink(&repo, &alias).unwrap();

        let repo_project = selected_project(repo.to_str().unwrap()).unwrap();
        assert!(selected_project(".").is_err());
        assert_eq!(
            repo_project.id,
            selected_project(worktree.to_str().unwrap()).unwrap().id
        );
        assert_eq!(
            repo_project.id,
            selected_project(alias.to_str().unwrap()).unwrap().id
        );
        assert_ne!(
            repo_project.id,
            selected_project(clone.to_str().unwrap()).unwrap().id
        );
        assert_ne!(
            repo_project.id,
            selected_project(nested.to_str().unwrap()).unwrap().id
        );
        let plain_project = selected_project(plain.to_str().unwrap()).unwrap();
        let missing = base.join("removed");
        let mut threads = vec![
            thread("main", &repo, Some("native-shared")),
            thread("tree", &worktree_child, Some("native-shared")),
            thread("clone", &clone, Some("native-shared")),
            thread("nested", &nested, None),
            thread("alias", &alias, None),
            thread("plain", &plain, None),
            thread("sibling", &plain_sibling, None),
            thread("plain-nested-git", &plain_nested_git, None),
            thread("broken-git", &broken_git, None),
            thread("missing", &missing, Some("native-shared")),
            thread("relative", Path::new("."), None),
        ];
        for thread in &mut threads {
            if thread.id == "main" || thread.id == "clone" {
                thread.git = Some(GitMetadata {
                    branch: None,
                    sha: None,
                    origin_url: Some("https://example.invalid/same.git".into()),
                });
            }
        }
        let (projects, attributions) = reconcile(&threads, vec![plain_project.clone()], &[]);
        let by_id: BTreeMap<_, _> = attributions
            .iter()
            .map(|item| (item.thread_id.as_str(), item))
            .collect();
        assert_eq!(
            by_id["main"].project_id.as_deref(),
            Some(repo_project.id.as_str())
        );
        assert_eq!(
            by_id["tree"].project_id.as_deref(),
            Some(repo_project.id.as_str())
        );
        assert_eq!(
            by_id["tree"].workspace_root.as_deref(),
            Some(fs::canonicalize(&worktree).unwrap().to_str().unwrap())
        );
        assert_eq!(
            by_id["alias"].project_id.as_deref(),
            Some(repo_project.id.as_str())
        );
        assert_ne!(by_id["clone"].project_id, by_id["main"].project_id);
        assert_ne!(by_id["nested"].project_id, by_id["main"].project_id);
        assert_eq!(
            by_id["plain"].project_id.as_deref(),
            Some(plain_project.id.as_str())
        );
        assert!(by_id["sibling"].project_id.is_none());
        assert_ne!(
            by_id["plain-nested-git"].project_id,
            by_id["plain"].project_id
        );
        assert!(by_id["broken-git"].project_id.is_none());
        assert!(selected_project(broken_git.to_str().unwrap()).is_err());
        assert!(by_id["missing"].project_id.is_none());
        assert!(by_id["relative"].project_id.is_none());
        assert!(by_id["main"].diagnostic.is_some());
        assert!(by_id["clone"].diagnostic.is_some());
        assert!(projects.iter().any(|project| project.id == repo_project.id));

        let old = vec![by_id["main"].clone()];
        fs::remove_dir_all(&repo).unwrap();
        let (_, kept) = reconcile(
            &[thread("main", &repo, Some("native-shared"))],
            projects,
            &old,
        );
        assert_eq!(
            kept[0].project_id.as_deref(),
            Some(repo_project.id.as_str())
        );
        assert_eq!(kept[0].basis, "previousVerified");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn unique_native_mapping_only_recovers_a_missing_path() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("codexflow-project-map-{nonce}"));
        fs::create_dir_all(&base).unwrap();
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&["init", "-q", repo.to_str().unwrap()]);
        let threads = vec![
            thread("known", &repo, Some("native")),
            thread("missing", &base.join("gone"), Some("native")),
        ];
        let (projects, mapped) = reconcile(&threads, vec![], &[]);
        assert_eq!(mapped[1].project_id, mapped[0].project_id);
        assert_eq!(mapped[1].basis, "sourceProjectId");
        let other = base.join("other-repo");
        fs::create_dir_all(&other).unwrap();
        git(&["init", "-q", other.to_str().unwrap()]);
        let (_, moved) = reconcile(
            &[thread("known", &other, Some("native"))],
            projects,
            &[mapped[0].clone()],
        );
        assert_eq!(moved[0].project_id, mapped[0].project_id);
        assert_eq!(moved[0].basis, "previousVerified");
        assert!(moved[0].diagnostic.is_some());
        let _ = fs::remove_dir_all(base);
    }
}
