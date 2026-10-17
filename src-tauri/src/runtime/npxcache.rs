//! What npx already has on disk, and how to warm it.
//!
//! dshell always runs npx with `npm_config_cache` pointed at its own data root,
//! so "does the local npx already have it" is a question about a directory tree,
//! not about the network. npx keeps one private tree per resolved spec at
//! `<cache>/_npx/<hash>/`, whose `node_modules/@deepseek-ai/dsh/package.json`
//! carries the **resolved** version — the manifest one level up only records the
//! range, which is exactly the thing that cannot answer "which version is here".
//!
//! The user's own npm cache is scanned as well: a `npx @deepseek-ai/dsh` run
//! from their terminal leaves a tree there, and claiming "还需要下载" while that
//! tree exists would be wrong in the one direction that costs 450 MB.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Action, Failure, FailureCode};

/// How long a first-time npx resolution may take.
///
/// A cold tree is ~450 MB / 26k files; the design measured ~3 minutes on a warm
/// network, so the budget is generous rather than tight.
pub const WARM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

pub const PACKAGE: &str = "@deepseek-ai/dsh";

/// One `@deepseek-ai/dsh` that npx has already unpacked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedPackage {
    pub version: String,
    /// The tree's root (`<cache>/_npx/<hash>`) — shown so the user can look.
    pub path: String,
    /// `true` when the tree is the one dshell's own npx runs use.
    pub managed: bool,
}

/// dshell's own npx cache.
pub fn managed_cache(data_root: &Path) -> PathBuf {
    data_root.join("runtimes").join("npx-cache")
}

/// Every cache worth looking at, dshell's own first.
fn cache_dirs(data_root: &Path) -> Vec<(PathBuf, bool)> {
    let mut dirs = vec![(managed_cache(data_root), true)];
    for var in ["LOCALAPPDATA", "APPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            dirs.push((PathBuf::from(base).join("npm-cache"), false));
        }
    }
    dirs
}

/// All cached builds, newest-looking first (highest version first), de-duplicated.
pub fn cached(data_root: &Path) -> Vec<CachedPackage> {
    cached_in(&cache_dirs(data_root))
}

/// [`cached`] over an explicit list of `(cache dir, managed)` pairs.
///
/// Split out so the tests can look at a temp directory instead of this machine's
/// real npm caches — which is also the honest statement that the search list is
/// data, not a constant.
pub fn cached_in(dirs: &[(PathBuf, bool)]) -> Vec<CachedPackage> {
    let mut found: Vec<CachedPackage> = Vec::new();
    for (cache, managed) in dirs {
        scan_cache(cache, *managed, &mut found);
    }
    found.sort_by(|a, b| {
        b.version
            .cmp(&a.version)
            .then_with(|| b.managed.cmp(&a.managed))
    });
    found.dedup_by(|a, b| a.version == b.version && a.managed == b.managed);
    found
}

fn scan_cache(cache: &Path, managed: bool, out: &mut Vec<CachedPackage>) {
    let root = cache.join("_npx");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let tree = entry.path();
        if !tree.is_dir() {
            continue;
        }
        let manifest = tree.join("node_modules").join(PACKAGE).join("package.json");
        let Some(version) = read_version(&manifest) else {
            continue;
        };
        out.push(CachedPackage {
            version,
            path: tree.to_string_lossy().to_string(),
            managed,
        });
    }
}

fn read_version(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let version = value.get("version")?.as_str()?.trim().to_string();
    (!version.is_empty()).then_some(version)
}

/// The version a cache hit would give, if any.
pub fn version_of<'a>(cached: &'a [CachedPackage], wanted: &str) -> Option<&'a CachedPackage> {
    cached.iter().find(|entry| entry.version == wanted)
}

/// Unpack `package_spec` into the cache before a launch needs it.
///
/// The command is `npx --yes <spec> --version`: it resolves or downloads the
/// tree, prints the version it ended up with, and exits — i.e. exactly the
/// network half of a launch, with nothing bound to a port. `--yes` is not
/// optional: without a TTY npx would block forever on its "Ok to proceed?".
///
/// `on_line` receives stdout/stderr lines as they arrive, so the page waiting on
/// this can show something truer than a spinner.
pub async fn warm(
    npx: &Path,
    package_spec: &str,
    cache: &Path,
    on_line: &(dyn Fn(String) + Send + Sync),
) -> Result<String, Failure> {
    let args = vec![
        "--yes".to_string(),
        package_spec.to_string(),
        "--version".to_string(),
    ];
    let env = vec![
        (
            "npm_config_cache".to_string(),
            cache.to_string_lossy().to_string(),
        ),
        ("npm_config_update_notifier".to_string(), "false".to_string()),
    ];
    let output = crate::runtime::exec::run(
        npx,
        &args,
        None,
        &env,
        WARM_TIMEOUT,
        400,
        on_line,
    )
    .await
    .map_err(|err| {
        Failure::new(
            FailureCode::NpxInstallFailed,
            format!("无法运行 npx：{err}"),
        )
        .with_detail(format!("npx: {}", npx.display()))
    })?;

    if output.timed_out {
        return Err(Failure::new(
            FailureCode::NpxInstallFailed,
            format!("npx 超时（{} 秒）", WARM_TIMEOUT.as_secs()),
        )
        .with_detail(output.tail())
        .with_action(Action::primary("retry", "重试")));
    }
    if !output.success() {
        return Err(Failure::new(
            FailureCode::NpxInstallFailed,
            format!(
                "npx 未能下载 {package_spec}（{}）",
                output
                    .code
                    .map(|code| format!("退出码 {code}"))
                    .unwrap_or_else(|| "被信号终止".into())
            ),
        )
        .with_detail(output.tail()));
    }

    Ok(version_from_output(&output.tail()).unwrap_or_else(|| package_spec.to_string()))
}

/// The registry's dist-tags for the package.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DistTags {
    pub latest: Option<String>,
    pub next: Option<String>,
    pub alpha: Option<String>,
}

/// `npm view @deepseek-ai/dsh dist-tags --json`.
///
/// The only way to answer "is there a newer version" without guessing: DSH's own
/// channels (`latest`, `alpha`) are exactly these tags. Failures are silent — the
/// card simply keeps saying what the disk says.
pub async fn dist_tags(npm: &Path) -> Option<DistTags> {
    let mut cmd = tokio::process::Command::new(npm);
    cmd.args(["view", PACKAGE, "dist-tags", "--json"]);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.env("NO_COLOR", "1");
    #[cfg(windows)]
    cmd.creation_flags(crate::win::CREATE_NO_WINDOW);

    let output = tokio::time::timeout(std::time::Duration::from_secs(25), cmd.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let value: serde_json::Value = serde_json::from_str(&text[start..=end]).ok()?;
    let tag = |name: &str| {
        value
            .get(name)
            .and_then(|value| value.as_str())
            .map(str::to_string)
    };
    let tags = DistTags {
        latest: tag("latest"),
        next: tag("next"),
        alpha: tag("alpha"),
    };
    (tags.latest.is_some() || tags.alpha.is_some()).then_some(tags)
}

/// The version npx printed. `--version` output also carries npm notices, so the
/// first `x.y.z` token anywhere in the output wins.
fn version_from_output(text: &str) -> Option<String> {
    for token in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-')) {
        if is_version(token) {
            return Some(token.trim_start_matches('v').to_string());
        }
    }
    None
}

fn is_version(token: &str) -> bool {
    let token = token.trim_start_matches('v');
    let mut parts = token.split('.');
    let (Some(major), Some(minor), Some(patch)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let numeric = |value: &str| !value.is_empty() && value.chars().next().is_some_and(|c| c.is_ascii_digit());
    numeric(major) && numeric(minor) && numeric(patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_resolved_version_from_a_cached_tree() {
        let dir = std::env::temp_dir().join(format!("dshell-npx-{}", std::process::id()));
        let tree = dir.join("_npx").join("abc123");
        let manifest = tree.join("node_modules").join(PACKAGE).join("package.json");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, r#"{"name":"@deepseek-ai/dsh","version":"0.2.0-rc.2"}"#).unwrap();

        let found = cached_in(&[(dir.clone(), true)]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].version, "0.2.0-rc.2");
        assert!(found[0].managed);
        assert!(version_of(&found, "0.2.0-rc.2").is_some());
        assert!(version_of(&found, "0.2.1-alpha.2").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A half-written tree (no manifest, or a manifest without a version) must
    /// not count as "already here" — that is how a launch would skip a download
    /// that is actually needed.
    #[test]
    fn incomplete_trees_are_not_cache_hits() {
        let dir = std::env::temp_dir().join(format!("dshell-npx-bad-{}", std::process::id()));
        let tree = dir.join("_npx").join("deadbeef");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("package.json"), r#"{"dependencies":{}}"#).unwrap();

        assert!(cached_in(&[(dir.clone(), true)]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same version in two caches is one entry, and dshell's own cache wins
    /// the tie because that is the tree a launch will actually run.
    #[test]
    fn the_same_version_from_two_caches_collapses_to_the_managed_one() {
        let root = std::env::temp_dir().join(format!("dshell-npx-both-{}", std::process::id()));
        let managed = root.join("managed");
        let user = root.join("user");
        for (cache, marker) in [(&managed, "a"), (&user, "b")] {
            let tree = cache.join("_npx").join(marker);
            let manifest = tree.join("node_modules").join(PACKAGE).join("package.json");
            std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
            std::fs::write(&manifest, r#"{"version":"0.2.0-rc.2"}"#).unwrap();
        }
        let found = cached_in(&[(managed.clone(), true), (user.clone(), false)]);
        assert_eq!(found.len(), 2, "one entry per cache, both kept: {found:?}");
        assert!(found[0].managed, "the managed tree sorts first");
        let versions: Vec<&str> = found.iter().map(|entry| entry.version.as_str()).collect();
        assert_eq!(versions, vec!["0.2.0-rc.2", "0.2.0-rc.2"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn picks_the_version_out_of_noisy_output() {
        let text = "npm warn exec The following package was not found and will be installed\n0.2.0-rc.2\n";
        assert_eq!(version_from_output(text).as_deref(), Some("0.2.0-rc.2"));
        assert_eq!(version_from_output("v0.2.1-alpha.2").as_deref(), Some("0.2.1-alpha.2"));
        assert!(version_from_output("npm ERR! 404 Not Found").is_none());
        assert!(!is_version("1.2"));
        assert!(!is_version("a.b.c"));
    }

    /// Live checks against this machine. Opt-in because they read the real npm
    /// caches and (for dist-tags) the network:
    ///
    /// ```powershell
    /// cargo test live_ -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "reads this machine's real npm caches"]
    fn live_cache_scan_finds_a_real_npx_tree() {
        let data_root = std::env::temp_dir();
        let found = cached(&data_root);
        println!("cached: {found:#?}");
        for (dir, _) in cache_dirs(&data_root) {
            println!("cache: {} exists={}", dir.display(), dir.exists());
        }
        assert!(
            found.iter().all(|entry| !entry.version.is_empty()),
            "a cache hit without a version would make the card lie"
        );
    }

    #[tokio::test]
    #[ignore = "needs npm and the network"]
    async fn live_dist_tags_resolve() {
        let node = crate::runtime::node::detect_node(&std::env::temp_dir()).await;
        let npm = crate::runtime::node::find_npm(node.as_ref()).expect("npm");
        let tags = dist_tags(&npm).await.expect("dist-tags");
        println!("dist-tags: {tags:?}");
        assert!(tags.latest.is_some(), "npm publishes a latest tag");
    }

    /// The whole warm-up path: spawn npx, stream its output, parse the version it
    /// printed, land the tree in the cache the caller named.
    ///
    /// Defaults to a deliberately tiny package that *has a binary* (npx runs the
    /// package's executable, so a library would fail with "could not determine
    /// executable to run" — which is also how a mistyped spec fails in the app);
    /// `DSHELL_NPX_SPEC` can point it at anything already cached.
    #[tokio::test]
    #[ignore = "spawns npx and downloads a package"]
    async fn live_warm_installs_and_reports_a_version() {
        let spec = std::env::var("DSHELL_NPX_SPEC").unwrap_or_else(|_| "cowsay@1.6.0".to_string());
        let data_root = std::env::temp_dir().join(format!("dshell-warm-{}", std::process::id()));
        let node = crate::runtime::node::detect_node(&data_root).await;
        let npx = node.expect("node on this machine").npx.expect("npx");
        let cache = managed_cache(&data_root);

        let seen = std::sync::Mutex::new(Vec::<String>::new());
        let collect = |line: String| seen.lock().unwrap().push(line);
        let version = warm(&npx, &spec, &cache, &collect).await.expect("warm");

        // One lock acquisition per statement: two guards inside a single
        // `println!` argument list would park the second one forever, because
        // `std::sync::Mutex` is not reentrant.
        let output = {
            let seen = seen.lock().unwrap();
            (seen.len(), seen.join("\n"))
        };
        println!("warm {spec} → {version}, {} lines:\n{}", output.0, output.1);
        assert!(is_version(&version), "unusable version: {version:?}");
        // The tree lands in the cache the caller named — an entry under
        // `<cache>/_npx/`, which is the directory `cached()` scans for DSH.
        let trees = std::fs::read_dir(cache.join("_npx"))
            .map(|entries| entries.flatten().count())
            .unwrap_or(0);
        assert!(trees > 0, "nothing was installed under {}", cache.display());

        let _ = std::fs::remove_dir_all(&data_root);
    }
}
