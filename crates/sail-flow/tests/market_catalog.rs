use sail_flow::market::{
    Catalog, HERMES_REPO, Kind, Lock, LockEntry, PluginEntry, Query, Route, SkillEntry, Trust,
    catalog::{self, clean_name, parse_plugin_catalog, parse_skills_index},
};

fn skill(
    id: &str,
    name: &str,
    description: &str,
    source: &str,
    trust: Trust,
    tags: &[&str],
) -> SkillEntry {
    SkillEntry {
        name: name.into(),
        description: description.into(),
        source: source.into(),
        identifier: id.into(),
        trust,
        repo: "owner/repo".into(),
        path: "skills/item".into(),
        tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
        resolved_github_id: None,
        owner: None,
    }
}

fn plugin(name: &str, title: &str, trust: Trust) -> PluginEntry {
    PluginEntry {
        name: name.into(),
        title: title.into(),
        description: format!("{name} plugin"),
        repo: format!("https://github.com/owner/{name}"),
        sha: "a".repeat(40),
        subdir: String::new(),
        version: "1.0.0".into(),
        tier: match trust {
            Trust::Builtin => "official",
            Trust::Trusted => "verified",
            _ => "community",
        }
        .into(),
        trust,
        category: "tools".into(),
        maintainer: "owner".into(),
        docs_url: String::new(),
        requires_env: Vec::new(),
        provides_tools: Vec::new(),
        provides_hooks: Vec::new(),
        platforms: Vec::new(),
    }
}

fn lock_entry(kind: Kind, id: &str, name: &str, path: &str) -> LockEntry {
    LockEntry {
        kind,
        name: name.into(),
        id: id.into(),
        source: "github".into(),
        trust: Trust::Community,
        route: Route::Github {
            repo: "o/r".into(),
            path: String::new(),
            reference: None,
        },
        revision: "rev".into(),
        content_hash: "hash".into(),
        verdict: sail_flow::market::Verdict::Safe,
        scanner: "sail-guard-1".into(),
        findings: 0,
        acknowledged_caution: false,
        install_path: path.into(),
        files: Vec::new(),
        installed_at: 1,
        updated_at: 1,
    }
}

#[test]
fn skills_index_parses_tolerantly() {
    let json = r#"{
      "version": 1,
      "generated_at": "2026-11-14T06:51:42Z",
      "skill_count": 6,
      "skills": [
        {"name":"Alpha","description":"One\n  two","source":"official","identifier":"official/a",
         "trust_level":"builtin","repo":"NousResearch/hermes-agent","path":"optional-skills/a",
         "tags":["X",3,"","Second"],"extra":{},"resolved_github_id":"o/r/p"},
        {"name":"no identifier","source":"github"},
        {"name":"duplicate","source":"github","identifier":"official/a"},
        {"name":7,"source":"github","identifier":"numeric-name"},
        {"name":"\"Dictionary\"","source":"clawhub","identifier":"@owner/dict",
         "trust_level":"trusted","tags":["a","b","c","d","e","f","g","h","i","j","k","l","m"],
         "extra":{"owner":"other"}},
        {"name":"bad trust","source":"lobehub","identifier":"lobe/x","trust_level":"mystery"}
      ]
    }"#;
    let (generated, entries, stats) = parse_skills_index(json.as_bytes()).unwrap();
    assert_eq!(generated, "2026-11-14T06:51:42Z");
    assert_eq!(stats.kept, 3);
    assert_eq!(stats.skipped, 3);
    assert_eq!(entries[0].description, "One two");
    assert_eq!(entries[0].tags, ["X", "Second"]);
    assert_eq!(entries[0].trust, Trust::Builtin);
    assert_eq!(entries[0].identifier, "official/a");
    assert_eq!(entries[0].resolved_github_id.as_deref(), Some("o/r/p"));
    let dictionary = &entries[1];
    assert_eq!(dictionary.name, "Dictionary");
    assert_eq!(dictionary.trust, Trust::Trusted);
    assert_eq!(dictionary.owner.as_deref(), Some("other"));
    assert_eq!(dictionary.tags.len(), 12);
    assert_eq!(entries[2].trust, Trust::Community);
}

#[test]
fn skills_index_rejects_missing_or_wrong_shape() {
    assert!(parse_skills_index(b"[]").is_err());
    assert!(parse_skills_index(b"{}").is_err());
    assert!(parse_skills_index(b"not json").is_err());
    assert!(parse_skills_index(br#"{"skills":{}}"#).is_err());
}

#[test]
fn plugin_catalog_parses_tolerantly() {
    let sha = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
    let json = format!(
        r#"{{
          "generated_at":"2026-11-14T00:00:00Z",
          "removed":[{{"name":"old","reason":"broken","date":"2026-09-28"}},{{"reason":"nameless"}}],
          "entries":[
            {{"name":"ok","title":"Ok","repo":"https://github.com/owner/ok","sha":"{sha}",
             "tier":"official","description":"A  plugin","subdir":"pkg/",
             "capabilities":{{"requires_env":["KEY",{{"name":"OTHER"}},{{"x":1}},7],
              "provides_tools":["tool"],"provides_hooks":["pre_tool_call"]}},
             "platforms":["linux"]}},
            {{"name":"short","repo":"https://github.com/o/r","sha":"abc"}},
            {{"name":"branch","repo":"https://github.com/o/r","sha":"main"}},
            {{"name":"ok","repo":"https://github.com/o/r","sha":"{sha}"}},
            {{"name":"wrong","repo":9,"sha":"{sha}"}}
          ]
        }}"#
    );
    let (generated, entries, removed, stats) = parse_plugin_catalog(json.as_bytes()).unwrap();
    assert_eq!(generated, "2026-11-14T00:00:00Z");
    assert_eq!(stats.kept, 1);
    assert_eq!(stats.skipped, 4);
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].name, "old");
    let entry = &entries[0];
    assert_eq!(entry.sha, sha.to_ascii_lowercase());
    assert_eq!(entry.description, "A plugin");
    assert_eq!(entry.trust, Trust::Builtin);
    assert_eq!(entry.requires_env, ["KEY", "OTHER"]);
    assert_eq!(entry.provides_tools, ["tool"]);
    assert_eq!(entry.provides_hooks, ["pre_tool_call"]);
    assert_eq!(entry.platforms, ["linux"]);
}

#[test]
fn plugin_catalog_requires_entries() {
    assert!(parse_plugin_catalog(b"{}").is_err());
    assert!(parse_plugin_catalog(b"[]").is_err());
    assert!(parse_plugin_catalog(br#"{"entries":{}}"#).is_err());
}

#[test]
fn clean_names() {
    assert_eq!(clean_name("\"Dictionary\""), "Dictionary");
    assert_eq!(clean_name("\"Unterminated"), "Unterminated");
    assert_eq!(clean_name("\"AI 产品…\" metadata:"), "AI 产品…");
    assert_eq!(clean_name("  a\u{0}b\nc\t d  "), "abc d");
    let long = "a".repeat(81);
    let cleaned = clean_name(&long);
    assert_eq!(cleaned.chars().count(), 81);
    assert!(cleaned.ends_with('…'));
    assert_eq!(clean_name(&"a".repeat(80)), "a".repeat(80));
}

#[test]
fn skill_routes_cover_the_table() {
    let mut entry = skill("official/a", "A", "", "official", Trust::Builtin, &[]);
    entry.path = "optional-skills/a".into();
    assert_eq!(
        catalog::skill_route(&entry),
        Route::Github {
            repo: HERMES_REPO.into(),
            path: "optional-skills/a".into(),
            reference: None
        }
    );

    let entry = skill("github/a", "A", "", "github", Trust::Community, &[]);
    assert_eq!(
        catalog::skill_route(&entry),
        Route::Github {
            repo: "owner/repo".into(),
            path: "skills/item".into(),
            reference: None
        }
    );

    let mut entry = skill("skills.sh/a", "A", "", "skills.sh", Trust::Community, &[]);
    entry.resolved_github_id = Some("up/stream/skills/pdf".into());
    assert_eq!(
        catalog::skill_route(&entry),
        Route::Github {
            repo: "up/stream".into(),
            path: "skills/pdf".into(),
            reference: None
        }
    );
    entry.resolved_github_id = Some("broken".into());
    assert_eq!(
        catalog::skill_route(&entry),
        Route::Github {
            repo: "owner/repo".into(),
            path: "skills/item".into(),
            reference: None
        }
    );

    let mut entry = skill(
        "dictionary",
        "Dictionary",
        "",
        "clawhub",
        Trust::Community,
        &[],
    );
    entry.owner = Some("publisher".into());
    assert_eq!(
        catalog::skill_route(&entry),
        Route::Clawhub {
            slug: "dictionary".into(),
            owner: Some("publisher".into())
        }
    );
    entry.identifier = "@publisher/dictionary".into();
    entry.owner = Some("ignored".into());
    assert_eq!(
        catalog::skill_route(&entry),
        Route::Clawhub {
            slug: "dictionary".into(),
            owner: Some("publisher".into())
        }
    );

    let entry = skill("lobe/x", "X", "", "lobehub", Trust::Community, &[]);
    assert!(matches!(
        catalog::skill_route(&entry),
        Route::Unsupported { reason } if reason.contains("lobehub")
    ));
}

#[test]
fn skill_route_validation_failures_are_unsupported() {
    for repo in ["owner/../x", "a/b/c", "/x", "owner/", "o/r/"] {
        let mut entry = skill("github/a", "A", "", "github", Trust::Community, &[]);
        entry.repo = repo.into();
        assert!(
            matches!(catalog::skill_route(&entry), Route::Unsupported { reason } if reason.contains("repo")),
            "{repo}"
        );
    }
    for path in ["../path", "/etc/passwd", "a//b", "a\\b"] {
        let mut entry = skill("github/a", "A", "", "github", Trust::Community, &[]);
        entry.path = path.into();
        assert!(
            matches!(catalog::skill_route(&entry), Route::Unsupported { reason } if reason.contains("path")),
            "{path}"
        );
    }
    let mut entry = skill("@owner/", "A", "", "clawhub", Trust::Community, &[]);
    entry.identifier = "@owner/".into();
    assert!(matches!(
        catalog::skill_route(&entry),
        Route::Unsupported { reason } if reason.contains("identifier")
    ));
    let mut entry = skill("@owner/bad slug", "A", "", "clawhub", Trust::Community, &[]);
    entry.identifier = "@owner/bad slug".into();
    assert!(matches!(
        catalog::skill_route(&entry),
        Route::Unsupported { reason } if reason.contains("slug")
    ));
}

#[test]
fn plugin_routes_cover_github_and_other_hosts() {
    let mut entry = plugin("greeter", "", Trust::Community);
    entry.repo = "https://github.com/owner/greeter.git/".into();
    entry.subdir = "pkg/".into();
    entry.sha = "B".repeat(40);
    assert_eq!(
        catalog::plugin_route(&entry),
        Route::Github {
            repo: "owner/greeter".into(),
            path: "pkg".into(),
            reference: Some("b".repeat(40))
        }
    );

    entry.repo = "https://gitlab.com/owner/greeter".into();
    assert!(matches!(
        catalog::plugin_route(&entry),
        Route::Unsupported { reason } if reason.contains("GitHub")
    ));

    entry.repo = "https://github.com/owner/../x".into();
    assert!(matches!(
        catalog::plugin_route(&entry),
        Route::Unsupported { reason } if reason.contains("repo")
    ));

    entry.repo = "https://github.com/owner/greeter".into();
    entry.subdir = "../x".into();
    assert!(matches!(
        catalog::plugin_route(&entry),
        Route::Unsupported { reason } if reason.contains("path")
    ));
}

#[test]
fn search_scores_and_tie_breaks() {
    let catalog = Catalog {
        skills: vec![
            skill(
                "s1",
                "pdf-tools",
                "read pdf",
                "official",
                Trust::Builtin,
                &["documents"],
            ),
            skill(
                "s2",
                "pdf",
                "the pdf reader",
                "github",
                Trust::Community,
                &[],
            ),
            skill(
                "s3",
                "image",
                "makes a pdf from images",
                "github",
                Trust::Trusted,
                &["pdf"],
            ),
        ],
        plugins: vec![plugin("pdf-helper", "", Trust::Builtin)],
        ..Catalog::default()
    };
    let page = catalog::search(
        &catalog,
        &Query {
            text: "pdf".into(),
            ..Query::default()
        },
        &Lock::default(),
    );
    // s2: name == pdf -> 100. p1 and s1: name starts with pdf -> 60 each, p1 sorts
    // before s1 because "pdf-helper" < "pdf-tools". s3: tag == pdf -> 30.
    let ids: Vec<_> = page.items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["s2", "pdf-helper", "s1", "s3"]);
    assert_eq!(page.total, 4);
}

#[test]
fn search_requires_every_term() {
    let catalog = Catalog {
        skills: vec![
            skill(
                "s1",
                "pdf",
                "the pdf reader",
                "github",
                Trust::Community,
                &[],
            ),
            skill(
                "s2",
                "pdf writer",
                "no such text",
                "github",
                Trust::Community,
                &[],
            ),
            skill(
                "s3",
                "scan",
                "pdf and reader",
                "github",
                Trust::Community,
                &[],
            ),
        ],
        ..Catalog::default()
    };
    let page = catalog::search(
        &catalog,
        &Query {
            text: "pdf reader".into(),
            ..Query::default()
        },
        &Lock::default(),
    );
    let ids: Vec<_> = page.items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["s1", "s3"]);
}

#[test]
fn search_filters() {
    let catalog = Catalog {
        skills: vec![
            skill("s1", "alpha", "", "official", Trust::Builtin, &[]),
            skill("s2", "beta", "", "github", Trust::Community, &[]),
            skill("s3", "gamma", "", "github", Trust::Trusted, &[]),
        ],
        plugins: vec![plugin("delta", "", Trust::Builtin)],
        ..Catalog::default()
    };
    let lock = Lock::default();

    let skills = catalog::search(
        &catalog,
        &Query {
            kind: Some(Kind::Skill),
            ..Query::default()
        },
        &lock,
    );
    assert_eq!(skills.total, 3);

    let official = catalog::search(
        &catalog,
        &Query {
            source: Some("official".into()),
            ..Query::default()
        },
        &lock,
    );
    assert_eq!(official.total, 1);

    let catalog_source = catalog::search(
        &catalog,
        &Query {
            source: Some("catalog".into()),
            ..Query::default()
        },
        &lock,
    );
    assert_eq!(catalog_source.total, 1);
    assert_eq!(catalog_source.items[0].kind, Kind::Plugin);

    let trusted = catalog::search(
        &catalog,
        &Query {
            min_trust: Some(Trust::Trusted),
            ..Query::default()
        },
        &lock,
    );
    let ids: Vec<_> = trusted.items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["s1", "delta", "s3"]);
    assert_eq!(trusted.total, 3);

    let builtin = catalog::search(
        &catalog,
        &Query {
            min_trust: Some(Trust::Builtin),
            ..Query::default()
        },
        &lock,
    );
    assert_eq!(builtin.total, 2);
}

#[test]
fn search_pages_and_defaults() {
    let skills: Vec<_> = (0..205)
        .map(|index| {
            skill(
                &format!("s{index:03}"),
                &format!("item-{index:03}"),
                "",
                "github",
                Trust::Community,
                &[],
            )
        })
        .collect();
    let catalog = Catalog {
        skills,
        ..Catalog::default()
    };
    let lock = Lock::default();
    let empty = Query::default();
    let page = catalog::search(&catalog, &empty, &lock);
    assert_eq!(page.total, 205);
    assert_eq!(page.items.len(), 50);

    let capped = catalog::search(
        &catalog,
        &Query {
            limit: 999,
            ..Query::default()
        },
        &lock,
    );
    assert_eq!(capped.items.len(), 200);

    let paged = catalog::search(
        &catalog,
        &Query {
            offset: 200,
            limit: 50,
            ..Query::default()
        },
        &lock,
    );
    assert_eq!(paged.total, 205);
    assert_eq!(paged.items.len(), 5);
    assert_eq!(paged.items[0].id, "s200");
}

#[test]
fn search_fills_installed_from_the_lock() {
    let catalog = Catalog {
        skills: vec![skill(
            "s1",
            "pdf-tools",
            "",
            "official",
            Trust::Builtin,
            &[],
        )],
        plugins: vec![plugin("greeter", "", Trust::Trusted)],
        ..Catalog::default()
    };
    let mut lock = Lock::default();
    lock.installed.insert(
        "skill:pdf-tools".into(),
        lock_entry(Kind::Skill, "s1", "pdf-tools", "/skills/pdf-tools"),
    );
    lock.installed.insert(
        "plugin:greeter".into(),
        lock_entry(Kind::Plugin, "greeter", "greeter", "/plugins/greeter"),
    );
    let page = catalog::search(&catalog, &Query::default(), &lock);
    let installed: Vec<_> = page
        .items
        .iter()
        .map(|item| item.installed.as_deref())
        .collect();
    assert_eq!(installed, [Some("skill:pdf-tools"), Some("plugin:greeter")]);
}

#[test]
fn parsing_and_search_are_deterministic() {
    let json = br#"{"generated_at":"g","skills":[
      {"name":"A","source":"official","identifier":"a"},
      {"name":"B","source":"github","identifier":"b"}]}"#;
    let first = parse_skills_index(json).unwrap();
    let second = parse_skills_index(json).unwrap();
    assert_eq!(first, second);

    let catalog = Catalog {
        skills: first.1,
        plugins: vec![plugin("greeter", "Greeter", Trust::Trusted)],
        ..Catalog::default()
    };
    let query = Query {
        text: "a b".into(),
        ..Query::default()
    };
    assert_eq!(
        catalog::search(&catalog, &query, &Lock::default()),
        catalog::search(&catalog, &query, &Lock::default())
    );
}

#[test]
#[ignore = "release-mode timing check on the real catalogs"]
fn real_catalog_performance() {
    use std::time::{Duration, Instant};

    let Some(index_path) = std::env::var_os("SAIL_MARKET_INDEX") else {
        eprintln!("SAIL_MARKET_INDEX is unset; skipping the real catalog timing check");
        return;
    };
    let bytes = std::fs::read(&index_path).unwrap();
    let started = Instant::now();
    let (_, skills, stats) = parse_skills_index(&bytes).unwrap();
    let parse_time = started.elapsed();
    eprintln!(
        "parsed {} skills ({} skipped) in {:?}",
        stats.kept, stats.skipped, parse_time
    );
    assert!(
        parse_time < Duration::from_secs(3),
        "parse_skills_index took {parse_time:?}"
    );

    let catalog = Catalog {
        skills,
        ..Catalog::default()
    };
    let started = Instant::now();
    let page = catalog::search(&catalog, &Query::default(), &Lock::default());
    let search_time = started.elapsed();
    eprintln!(
        "empty search returned {} items from {} matches in {:?}",
        page.items.len(),
        page.total,
        search_time
    );
    assert_eq!(page.items.len(), 50);
    assert!(
        search_time < Duration::from_millis(150),
        "empty search took {search_time:?}"
    );

    if let Some(plugin_path) = std::env::var_os("SAIL_MARKET_PLUGINS") {
        let bytes = std::fs::read(plugin_path).unwrap();
        let started = Instant::now();
        let (_, plugins, removed, stats) = parse_plugin_catalog(&bytes).unwrap();
        let parse_time = started.elapsed();
        eprintln!(
            "parsed {} plugins ({} removed, {} skipped) in {:?}",
            stats.kept,
            removed.len(),
            stats.skipped,
            parse_time
        );
        assert!(
            parse_time < Duration::from_secs(3),
            "parse_plugin_catalog took {parse_time:?}"
        );
        assert!(!plugins.is_empty());
    }
}
