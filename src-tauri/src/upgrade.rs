//! Startup DSH version detection for restored local npx tabs.
//!
//! Restoring a tab re-spawns its backend from the `RuntimeSpec` saved in
//! `tabs.json`. For an npx tab that spec names a concrete DSH version, so a
//! newer release is invisible unless something looks for it. This module does
//! that one check, applies the user's three-way startup policy, and (for `ask`)
//! raises the native decision prompt.
//!
//! Scope is deliberately narrow: only npx tabs with a concrete version. The npm
//! runtime has no install/update path (its `package_spec` is ignored by
//! `runtime::resolve`), the source checkout updates itself on every start, and a
//! managed tree or arbitrary binary belongs to the user. Attach/remote tabs have
//! no DSH version of their own. Keeping the check here means none of those paths
//! grow a half-implemented notion of "update".

use std::cmp::Ordering;
use std::time::Duration;

use tauri::AppHandle;

use crate::config::PINNED_DSH_VERSION;
use crate::runtime::npxcache::PACKAGE;
use crate::runtime::RuntimeSpec;
use crate::state::AppState;
use crate::tabs::state::{ConnectionKind, TabSpec};

pub const POLICY_ASK: &str = "ask";
pub const POLICY_KEEP: &str = "keep";
pub const POLICY_UPDATE: &str = "update";

/// How long the startup registry check may take.
///
/// A startup must not hang on a proxy or a dead network: the saved version is
/// always a valid answer, and the user can try again on the next launch.
const CHECK_TIMEOUT: Duration = Duration::from_secs(8);

/// The three values the settings page writes, with an unknown value treated as
/// `ask` rather than rejecting the whole config.
pub fn normalize_policy(raw: &str) -> &'static str {
    match raw.trim() {
        POLICY_KEEP => POLICY_KEEP,
        POLICY_UPDATE => POLICY_UPDATE,
        _ => POLICY_ASK,
    }
}

/// A restored local tab that names a concrete DSH version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub tab_id: String,
    pub current: String,
}

/// The tabs that can move to `latest`, and what the prompt says about them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub latest: String,
    /// Ids of the tabs to retarget.
    pub tab_ids: Vec<String>,
    /// Distinct current versions, for the prompt copy.
    pub currents: Vec<String>,
}

impl Proposal {
    pub fn includes(&self, tab_id: &str) -> bool {
        self.tab_ids.iter().any(|id| id.as_str() == tab_id)
    }
}

/// The npm spec for one concrete version.
pub fn package_spec(version: &str) -> String {
    format!("{PACKAGE}@{version}")
}

/// The concrete version a runtime spec pins, if it pins one.
///
/// Only a bare `@deepseek-ai/dsh@<semver>` counts. A dist-tag (`@latest`,
/// `@alpha`) or a range follows whatever npx resolves and must not be pinned to
/// a concrete version by this check.
pub fn spec_version(spec: &RuntimeSpec) -> Option<String> {
    let RuntimeSpec::Npx { package_spec } = spec else {
        return None;
    };
    let (name, version) = package_spec.rsplit_once('@')?;
    if name != PACKAGE {
        return None;
    }
    parse_version(version).map(|_| version.trim_start_matches('v').to_string())
}

/// The version a startup restore may move a local tab to.
///
/// `published` is npm's `latest` tag when the registry answered; the build's pin
/// is a floor because upgrading dshell itself can introduce a newer validated
/// version than the tag this machine can see. Invalid input is ignored, and
/// `None` means nothing usable was found.
pub fn upgrade_target(published: Option<&str>, pinned: &str) -> Option<String> {
    let pinned = normalized(pinned);
    let published = published.and_then(normalized);
    match (published, pinned) {
        (Some(published), Some(pinned)) => {
            if is_newer(&pinned, &published) {
                Some(pinned)
            } else {
                Some(published)
            }
        }
        (Some(published), None) => Some(published),
        (None, Some(pinned)) => Some(pinned),
        (None, None) => None,
    }
}

/// Which restored tabs are behind `latest`. `None` when there is nothing to do.
///
/// Only a strictly newer version counts: an older target (or an unparseable one)
/// must never turn into a downgrade.
pub fn plan(candidates: &[Candidate], latest: &str) -> Option<Proposal> {
    let latest = normalized(latest)?;
    let mut tab_ids = Vec::new();
    let mut currents: Vec<String> = Vec::new();
    for candidate in candidates {
        if !is_newer(&latest, &candidate.current) {
            continue;
        }
        tab_ids.push(candidate.tab_id.clone());
        if !currents.contains(&candidate.current) {
            currents.push(candidate.current.clone());
        }
    }
    if tab_ids.is_empty() {
        return None;
    }
    Some(Proposal {
        latest,
        tab_ids,
        currents,
    })
}

/// Retarget one restored npx tab to `version`, preserving every other field.
///
/// Returns `None` for a tab whose runtime is not npx: the caller must not create
/// an "updated" spec for a runtime dshell cannot install.
pub fn with_version(spec: &TabSpec, version: &str) -> Option<TabSpec> {
    let ConnectionKind::LocalSpawn {
        runtime: RuntimeSpec::Npx { .. },
        ..
    } = &spec.connection
    else {
        return None;
    };
    let mut upgraded = spec.clone();
    let ConnectionKind::LocalSpawn { runtime, .. } = &mut upgraded.connection else {
        return None;
    };
    *runtime = RuntimeSpec::Npx {
        package_spec: package_spec(version),
    };
    Some(upgraded)
}

/// Candidates among the tabs that were successfully opened for restore.
pub fn candidates(specs: &[TabSpec]) -> Vec<Candidate> {
    specs
        .iter()
        .filter_map(|spec| {
            let ConnectionKind::LocalSpawn { runtime, .. } = &spec.connection else {
                return None;
            };
            let current = spec_version(runtime)?;
            Some(Candidate {
                tab_id: spec.id.clone(),
                current,
            })
        })
        .collect()
}

/// Run the startup policy and, for `ask`, the one native prompt.
///
/// Returns `Some(proposal)` only when the caller should start the affected tabs
/// with the newer version. Everything else — `keep`, no newer version, no
/// candidate, an unreachable registry, a dialog that cannot open, a cancel —
/// means "start the saved versions", which is also why this function never
/// returns an error.
pub async fn decide(app: &AppHandle, state: &AppState, specs: &[TabSpec]) -> Option<Proposal> {
    let config = state.config_snapshot().await;
    let policy = normalize_policy(&config.dsh_startup_policy);
    if policy == POLICY_KEEP {
        return None;
    }

    let candidates = candidates(specs);
    if candidates.is_empty() {
        return None;
    }

    let published = state
        .registry_dist_tags(CHECK_TIMEOUT)
        .await
        .and_then(|tags| tags.latest);
    let Some(latest) = upgrade_target(published.as_deref(), PINNED_DSH_VERSION) else {
        debug(
            state,
            "startup upgrade: no usable version from the registry or the pin",
        );
        return None;
    };
    let proposal = plan(&candidates, &latest)?;

    if policy == POLICY_UPDATE {
        debug(
            state,
            &format!(
                "startup upgrade: policy=update, {} tab(s) {} -> {}",
                proposal.tab_ids.len(),
                proposal.currents.join(", "),
                proposal.latest
            ),
        );
        return Some(proposal);
    }

    // An unowned dialog is not modal to dshell and can open behind the main
    // window, which looks like a hung startup. Fail open to the saved version
    // instead; the settings page still works.
    let Some(owner) = crate::picker::dialog_owner(app) else {
        debug(
            state,
            "startup upgrade: no owner window for the prompt; keeping saved versions",
        );
        return None;
    };
    let prompt = choice_prompt(&proposal);
    let answer =
        tokio::task::spawn_blocking(move || crate::dialog::ask_choice(Some(owner), &prompt)).await;

    match answer {
        Ok(Ok(crate::dialog::ChoiceOutcome::Accept { remember })) => {
            if remember {
                remember_policy(state, POLICY_UPDATE).await;
            }
            debug(
                state,
                &format!(
                    "startup upgrade: accepted {} for {} tab(s)",
                    proposal.latest,
                    proposal.tab_ids.len()
                ),
            );
            Some(proposal)
        }
        Ok(Ok(crate::dialog::ChoiceOutcome::Decline { remember })) => {
            if remember {
                remember_policy(state, POLICY_KEEP).await;
            }
            debug(state, "startup upgrade: kept the saved versions");
            None
        }
        Ok(Ok(crate::dialog::ChoiceOutcome::Cancelled)) => {
            debug(
                state,
                "startup upgrade: prompt cancelled; kept the saved versions",
            );
            None
        }
        Ok(Err(err)) => {
            debug(
                state,
                &format!("startup upgrade: prompt failed ({err}); kept the saved versions"),
            );
            None
        }
        Err(err) => {
            debug(
                state,
                &format!("startup upgrade: prompt task failed ({err}); kept the saved versions"),
            );
            None
        }
    }
}

fn choice_prompt(proposal: &Proposal) -> crate::dialog::ChoicePrompt {
    let count = proposal.tab_ids.len();
    let currents = proposal.currents.join("、");
    crate::dialog::ChoicePrompt {
        window_title: "dshell".into(),
        instruction: "发现 DSH 新版本".into(),
        content: format!(
            "当前版本：{currents}\n可用的新版本：{}\n受影响的本地选项卡：{count} 个\n\n如果本机尚未缓存该版本，启动时会先下载（约 450 MB）。",
            proposal.latest
        ),
        verification: "记住我的选择（以后可在「设置 → 偏好」中更改）".into(),
        footer: "只影响重启后自动恢复的本地选项卡。".into(),
        accept_label: "以新版本启动".into(),
        decline_label: "维持旧版本".into(),
    }
}

async fn remember_policy(state: &AppState, policy: &str) {
    let result = state
        .update_config(|config| config.dsh_startup_policy = policy.to_string())
        .await;
    if let Err(failure) = result {
        debug(
            state,
            &format!("startup upgrade: could not save policy {policy}: {failure}"),
        );
    }
}

fn debug(state: &AppState, message: &str) {
    crate::dsh::process::debug_log(&state.data_root.path, message);
}

// ------------------------------------------------------------------ versions

/// A normalized version string, or `None` when the input is not SemVer.
fn normalized(raw: &str) -> Option<String> {
    parse_version(raw).map(|_| raw.trim().trim_start_matches('v').to_string())
}

fn is_newer(candidate: &str, current: &str) -> bool {
    matches!(
        compare_versions(candidate, current),
        Some(Ordering::Greater)
    )
}

/// SemVer precedence, or `None` when either side is not parseable.
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    Some(compare(&parse_version(a)?, &parse_version(b)?))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PreId {
    Numeric(u64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Vec<PreId>,
}

fn parse_version(raw: &str) -> Option<Version> {
    let raw = raw.trim().trim_start_matches('v');
    let core = raw.split_once('+').map_or(raw, |(core, _)| core);
    let (numbers, pre) = match core.split_once('-') {
        Some((numbers, pre)) => (numbers, Some(pre)),
        None => (core, None),
    };
    let mut parts = numbers.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let pre = match pre {
        None => Vec::new(),
        Some("") => return None,
        Some(text) => {
            let mut ids = Vec::new();
            for id in text.split('.') {
                if id.is_empty() {
                    return None;
                }
                if id.chars().all(|c| c.is_ascii_digit()) {
                    ids.push(PreId::Numeric(id.parse::<u64>().ok()?));
                } else if id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    ids.push(PreId::Text(id.to_string()));
                } else {
                    return None;
                }
            }
            ids
        }
    };
    Some(Version {
        major,
        minor,
        patch,
        pre,
    })
}

fn compare(a: &Version, b: &Version) -> Ordering {
    (a.major, a.minor, a.patch)
        .cmp(&(b.major, b.minor, b.patch))
        .then_with(|| compare_pre(&a.pre, &b.pre))
}

fn compare_pre(a: &[PreId], b: &[PreId]) -> Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => Ordering::Equal,
        // A release outranks any prerelease of the same core version.
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            for (left, right) in a.iter().zip(b) {
                let ordering = match (left, right) {
                    (PreId::Numeric(left), PreId::Numeric(right)) => left.cmp(right),
                    (PreId::Text(left), PreId::Text(right)) => left.cmp(right),
                    // Numeric identifiers always have lower precedence than
                    // alphanumeric ones.
                    (PreId::Numeric(_), PreId::Text(_)) => Ordering::Less,
                    (PreId::Text(_), PreId::Numeric(_)) => Ordering::Greater,
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            // A larger set of fields has higher precedence when all shared
            // fields are equal.
            a.len().cmp(&b.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DshHomeSetting;
    use crate::runtime::ManagedMode;

    fn npx_tab(id: &str, version: &str) -> TabSpec {
        TabSpec {
            id: id.into(),
            title: id.into(),
            custom_title: None,
            display_origin: None,
            connection: ConnectionKind::LocalSpawn {
                workspace: None,
                dsh_home: DshHomeSetting::System,
                dsh_agents_home: None,
                runtime: RuntimeSpec::Npx {
                    package_spec: package_spec(version),
                },
                kill_on_close: true,
            },
        }
    }

    fn candidate(id: &str, version: &str) -> Candidate {
        Candidate {
            tab_id: id.into(),
            current: version.into(),
        }
    }

    #[test]
    fn policies_normalize_unknown_values_to_ask() {
        assert_eq!(normalize_policy("ask"), POLICY_ASK);
        assert_eq!(normalize_policy("keep"), POLICY_KEEP);
        assert_eq!(normalize_policy("update"), POLICY_UPDATE);
        assert_eq!(normalize_policy(" keep "), POLICY_KEEP);
        assert_eq!(normalize_policy("from-a-newer-build"), POLICY_ASK);
        assert_eq!(normalize_policy(""), POLICY_ASK);
    }

    #[test]
    fn semver_ordering_matches_the_spec() {
        let cases = [
            ("1.0.0", "1.0.0", Ordering::Equal),
            ("v1.0.0", "1.0.0", Ordering::Equal),
            ("1.0.0+build.9", "1.0.0", Ordering::Equal),
            ("1.0.0", "0.9.9", Ordering::Greater),
            ("0.2.1-alpha.2", "0.2.0-rc.2", Ordering::Greater),
            ("1.0.0", "1.0.0-rc.1", Ordering::Greater),
            ("1.0.0-rc.2", "1.0.0-rc.1", Ordering::Greater),
            ("1.0.0-alpha", "1.0.0-alpha.1", Ordering::Less),
            ("1.0.0-alpha.10", "1.0.0-alpha.2", Ordering::Greater),
            ("1.0.0-alpha.2", "1.0.0-beta", Ordering::Less),
            ("1.0.0-beta", "1.0.0-2", Ordering::Greater),
        ];
        for (left, right, expected) in cases {
            assert_eq!(
                compare_versions(left, right),
                Some(expected),
                "{left} vs {right}"
            );
        }
    }

    #[test]
    fn unparseable_versions_are_not_compared() {
        for bad in [
            "latest", "alpha", "^0.2.0", "1.2", "1.2.3.4", "1.2.3-", "x.y.z",
        ] {
            assert_eq!(compare_versions(bad, "1.0.0"), None, "{bad}");
            assert_eq!(compare_versions("1.0.0", bad), None, "{bad}");
        }
    }

    #[test]
    fn only_a_concrete_npx_spec_names_a_version() {
        let npx = |spec: &str| RuntimeSpec::Npx {
            package_spec: spec.into(),
        };
        assert_eq!(
            spec_version(&npx("@deepseek-ai/dsh@0.2.0-rc.2")).as_deref(),
            Some("0.2.0-rc.2")
        );
        assert_eq!(
            spec_version(&npx("@deepseek-ai/dsh@v0.2.1")).as_deref(),
            Some("0.2.1")
        );
        for floating in [
            "@deepseek-ai/dsh@latest",
            "@deepseek-ai/dsh@alpha",
            "@deepseek-ai/dsh@^0.2.0",
            "@deepseek-ai/dsh@~0.2.0",
            "@deepseek-ai/other@0.2.0",
            "dsh@0.2.0",
        ] {
            assert_eq!(spec_version(&npx(floating)), None, "{floating}");
        }
        assert_eq!(
            spec_version(&RuntimeSpec::Npm {
                root: "E:\\rt".into(),
                package_spec: "@deepseek-ai/dsh@0.2.0".into(),
            }),
            None,
            "the npm runtime has no update path"
        );
        assert_eq!(
            spec_version(&RuntimeSpec::Managed {
                root: "E:\\rt".into(),
                mode: ManagedMode::Installed,
            }),
            None
        );
    }

    #[test]
    fn the_target_is_the_newer_of_the_registry_and_the_pin() {
        assert_eq!(
            upgrade_target(Some("0.2.1"), "0.2.0-rc.2").as_deref(),
            Some("0.2.1")
        );
        assert_eq!(
            upgrade_target(Some("0.2.0-rc.2"), "0.2.1").as_deref(),
            Some("0.2.1")
        );
        assert_eq!(
            upgrade_target(Some("0.2.1"), "0.2.1").as_deref(),
            Some("0.2.1")
        );
        assert_eq!(
            upgrade_target(None, "0.2.0-rc.2").as_deref(),
            Some("0.2.0-rc.2")
        );
        assert_eq!(upgrade_target(Some("latest"), "nope"), None);
        assert_eq!(upgrade_target(None, "nope"), None);
    }

    #[test]
    fn only_strictly_older_tabs_are_proposed() {
        let candidates = [
            candidate("a", "0.2.0-rc.1"),
            candidate("b", "0.2.0-rc.2"),
            candidate("c", "0.2.1"),
        ];
        let proposal = plan(&candidates, "0.2.1").expect("a and b are behind");
        assert_eq!(proposal.latest, "0.2.1");
        assert_eq!(proposal.tab_ids, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            proposal.currents,
            vec!["0.2.0-rc.1".to_string(), "0.2.0-rc.2".to_string()]
        );
        assert!(proposal.includes("a"));
        assert!(!proposal.includes("c"));

        assert_eq!(plan(&candidates, "0.2.1"), plan(&candidates, "0.2.1"));
        assert!(plan(&[candidate("c", "0.2.1")], "0.2.1").is_none());
        assert!(
            plan(&[candidate("c", "0.2.1")], "0.2.0").is_none(),
            "never downgrade"
        );
        assert!(plan(&[candidate("c", "0.2.1")], "latest").is_none());
    }

    #[test]
    fn current_versions_are_reported_once() {
        let candidates = [
            candidate("a", "0.1.0"),
            candidate("b", "0.2.0"),
            candidate("c", "0.1.0"),
        ];
        let proposal = plan(&candidates, "0.3.0").unwrap();
        assert_eq!(proposal.tab_ids.len(), 3);
        assert_eq!(
            proposal.currents,
            vec!["0.1.0".to_string(), "0.2.0".to_string()]
        );
    }

    #[test]
    fn retargeting_preserves_everything_but_the_npx_spec() {
        let mut spec = npx_tab("a", "0.2.0-rc.2");
        if let ConnectionKind::LocalSpawn {
            workspace,
            dsh_home,
            dsh_agents_home,
            kill_on_close,
            ..
        } = &mut spec.connection
        {
            *workspace = Some("E:\\ws".into());
            *dsh_home = DshHomeSetting::Custom {
                path: "E:\\home".into(),
            };
            *dsh_agents_home = Some(DshHomeSetting::System);
            *kill_on_close = false;
        }
        spec.custom_title = Some("工作台".into());
        spec.display_origin = Some("http://127.0.0.1:1".into());

        let upgraded = with_version(&spec, "0.2.1").expect("npx tab");
        assert_eq!(upgraded.id, spec.id);
        assert_eq!(upgraded.custom_title, spec.custom_title);
        assert_eq!(upgraded.display_origin, spec.display_origin);
        let ConnectionKind::LocalSpawn {
            workspace,
            dsh_home,
            dsh_agents_home,
            runtime,
            kill_on_close,
        } = &upgraded.connection
        else {
            panic!("still a local spawn");
        };
        assert_eq!(workspace.as_deref(), Some("E:\\ws"));
        assert!(matches!(
            dsh_home,
            DshHomeSetting::Custom { path } if path == "E:\\home"
        ));
        assert!(matches!(dsh_agents_home, Some(DshHomeSetting::System)));
        assert!(!kill_on_close);
        assert!(matches!(
            runtime,
            RuntimeSpec::Npx { package_spec } if package_spec == "@deepseek-ai/dsh@0.2.1"
        ));
    }

    #[test]
    fn non_npx_tabs_are_never_retargeted() {
        let managed = TabSpec {
            id: "m".into(),
            title: "managed".into(),
            custom_title: None,
            display_origin: None,
            connection: ConnectionKind::LocalSpawn {
                workspace: None,
                dsh_home: DshHomeSetting::System,
                dsh_agents_home: None,
                runtime: RuntimeSpec::Managed {
                    root: "E:\\rt".into(),
                    mode: ManagedMode::Installed,
                },
                kill_on_close: true,
            },
        };
        assert!(with_version(&managed, "0.2.1").is_none());

        let external = TabSpec {
            connection: ConnectionKind::LocalSpawn {
                workspace: None,
                dsh_home: DshHomeSetting::System,
                dsh_agents_home: None,
                runtime: RuntimeSpec::External {
                    binary: "E:\\dsh.exe".into(),
                    label: None,
                },
                kill_on_close: true,
            },
            ..managed
        };
        assert!(with_version(&external, "0.2.1").is_none());
    }

    #[test]
    fn candidates_skip_tabs_without_a_concrete_npx_version() {
        let local = npx_tab("a", "0.2.0-rc.2");
        let mut floating = npx_tab("b", "0.2.0");
        if let ConnectionKind::LocalSpawn { runtime, .. } = &mut floating.connection {
            *runtime = RuntimeSpec::Npx {
                package_spec: "@deepseek-ai/dsh@latest".into(),
            };
        }
        let attach = TabSpec {
            id: "c".into(),
            title: "attach".into(),
            custom_title: None,
            display_origin: None,
            connection: ConnectionKind::LocalAttach {
                url: "http://127.0.0.1:1/?token=x".into(),
            },
        };
        assert_eq!(
            candidates(&[local, floating, attach]),
            vec![candidate("a", "0.2.0-rc.2")]
        );
    }
}
