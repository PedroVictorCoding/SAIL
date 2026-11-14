use sail_flow::hermes::pyenv::{
    EnvPlan, EnvRecord, EnvState, Locked, Requirement, parse_pyproject_dependencies, parse_report,
    parse_requirements, pip_args, plan, state,
};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

const EMPTY_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn one(line: &str) -> Requirement {
    let (mut requirements, refused) = parse_requirements(line);
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(requirements.len(), 1);
    requirements.remove(0)
}

#[test]
fn requirement_normalization_table() {
    let rows: &[(&str, &str, &[&str], &str, &str)] = &[
        ("requests", "requests", &[], "", ""),
        ("Requests", "requests", &[], "", ""),
        ("Foo_Bar.baz", "foo-bar-baz", &[], "", ""),
        ("foo..bar", "foo-bar", &[], "", ""),
        ("foo--bar", "foo-bar", &[], "", ""),
        ("zope.interface", "zope-interface", &[], "", ""),
        ("My.Package_Name", "my-package-name", &[], "", ""),
        ("foo[security,socks]", "foo", &["security", "socks"], "", ""),
        (
            "foo[ Security , socks ]",
            "foo",
            &["security", "socks"],
            "",
            "",
        ),
        ("pydantic-core==2.*", "pydantic-core", &[], "==2.*", ""),
        ("foo >= 1.0", "foo", &[], ">=1.0", ""),
        ("foo~=1.2", "foo", &[], "~=1.2", ""),
        ("foo!=1.0", "foo", &[], "!=1.0", ""),
        ("foo<=2", "foo", &[], "<=2", ""),
        ("foo>1", "foo", &[], ">1", ""),
        ("foo===1.0", "foo", &[], "===1.0", ""),
        (
            "foo; python_version < \"3.11\"",
            "foo",
            &[],
            "",
            "python_version < \"3.11\"",
        ),
        (
            "foo[bar]>=1; sys_platform == 'linux'",
            "foo",
            &["bar"],
            ">=1",
            "sys_platform == 'linux'",
        ),
        (
            "django>=4.2; python_version < '3.12' and sys_platform != 'win32'",
            "django",
            &[],
            ">=4.2",
            "python_version < '3.12' and sys_platform != 'win32'",
        ),
        ("foo[bar,baz]>=1", "foo", &["bar", "baz"], ">=1", ""),
    ];
    for (line, name, extras, specifier, marker) in rows {
        let requirement = one(line);
        assert_eq!(requirement.name, *name, "{line}");
        assert_eq!(requirement.extras, *extras, "{line}");
        assert_eq!(requirement.specifier, *specifier, "{line}");
        assert_eq!(requirement.marker, *marker, "{line}");
    }
    assert_eq!(one("foo # a comment").name, "foo");
    assert!(parse_requirements("# only a comment\n\n").0.is_empty());
}

#[test]
fn every_refusal_is_reported() {
    for line in [
        "-e .",
        "--editable .",
        "-r reqs.txt",
        "--requirement reqs.txt",
        "-c constraints.txt",
        "--constraint constraints.txt",
        "--index-url https://example.com/simple",
        "https://example.com/pkg.whl",
        "git+https://github.com/a/b",
        "foo @ https://example.com/foo.whl",
        "./local",
        "/tmp/package",
        "../package",
        "package.whl",
        "package.tar.gz",
        "foo/bar",
        "foo bar",
    ] {
        let (requirements, refused) = parse_requirements(line);
        assert!(requirements.is_empty(), "{line}");
        assert_eq!(refused.len(), 1, "{line}");
    }
    let many = (0..51)
        .map(|index| format!("package{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (requirements, refused) = parse_requirements(&many);
    assert_eq!(requirements.len(), 50);
    assert_eq!(refused.len(), 1);
    assert!(refused[0].contains("more than 50 requirements"));
}

#[test]
fn pyproject_dependency_arrays() {
    assert_eq!(
        parse_pyproject_dependencies("[project]\nname = \"x\"\ndependencies = [\"a\", \"b>=1\"]\n")
            .unwrap(),
        ["a", "b>=1"]
    );
    assert_eq!(
        parse_pyproject_dependencies(
            "[project]\nname = \"x\"\ndependencies = [\n  \"requests>=2\",  # http\n  'rich',\n]\n"
        )
        .unwrap(),
        ["requests>=2", "rich"]
    );
    assert_eq!(
        parse_pyproject_dependencies(
            "[build-system]\ndependencies = [\"setuptools\"]\n[project]\ndependencies = [\"a\"]\n[project.optional-dependencies]\ndev = [\"pytest\"]\n"
        )
        .unwrap(),
        ["a"]
    );
    assert!(
        parse_pyproject_dependencies("[project.optional-dependencies]\ndev = [\"pytest\"]\n")
            .unwrap()
            .is_empty()
    );
    assert!(
        parse_pyproject_dependencies("[project]\nname = \"x\"\n")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        parse_pyproject_dependencies("[project]\ndependencies = [\"a#b\"]\n").unwrap(),
        ["a#b"]
    );
    assert!(parse_pyproject_dependencies("[project]\ndependencies = [\"a\"").is_err());
    assert!(parse_pyproject_dependencies("[project]\ndependencies = \"a\"").is_err());
    assert!(parse_pyproject_dependencies("[project]\ndependencies = [1]").is_err());
    assert!(
        parse_pyproject_dependencies("[tool.x]\ndependencies = [\"ignored\"]\n")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn plan_reads_requirements_then_pyproject() {
    let dir = TempDir::new().unwrap();
    let empty = plan(dir.path()).unwrap();
    assert_eq!(empty.source, "");
    assert!(empty.requirements.is_empty());
    assert!(empty.refused.is_empty());
    assert_eq!(empty.hash, EMPTY_HASH);

    fs::write(
        dir.path().join("requirements.txt"),
        "Requests==2.31.0\nfoo[bar]>=1; sys_platform == 'linux'\n",
    )
    .unwrap();
    let first = plan(dir.path()).unwrap();
    let second = plan(dir.path()).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.source, "requirements.txt");
    assert_eq!(first.requirements.len(), 2);
    assert_eq!(first.requirements[0].name, "requests");
    assert_eq!(first.hash.len(), 64);
    assert!(first.refused.is_empty());

    fs::write(
        dir.path().join("pyproject.toml"),
        "[project]\ndependencies = [\"rich>=13\"]\n",
    )
    .unwrap();
    let requirements_wins = plan(dir.path()).unwrap();
    assert_eq!(requirements_wins.source, "requirements.txt");

    fs::remove_file(dir.path().join("requirements.txt")).unwrap();
    let pyproject = plan(dir.path()).unwrap();
    assert_eq!(pyproject.source, "pyproject.toml");
    assert_eq!(pyproject.requirements[0].name, "rich");

    fs::write(
        dir.path().join("requirements.txt"),
        "git+https://github.com/a/b\n",
    )
    .unwrap();
    let refused = plan(dir.path()).unwrap();
    assert_eq!(refused.refused.len(), 1);
}

#[test]
fn pip_args_are_exact() {
    let (requirements, refused) = parse_requirements("requests>=2\nfoo[bar]==1");
    let plan = EnvPlan {
        source: "requirements.txt".into(),
        requirements,
        refused,
        hash: "x".into(),
    };
    assert_eq!(
        pip_args(Path::new("/tmp/venv"), Path::new("/tmp/report.json"), &plan),
        [
            "/tmp/venv/bin/python",
            "-I",
            "-m",
            "pip",
            "install",
            "--only-binary=:all:",
            "--no-input",
            "--disable-pip-version-check",
            "--no-cache-dir",
            "--report",
            "/tmp/report.json",
            "requests>=2",
            "foo[bar]==1",
        ]
    );
}

#[test]
fn parse_report_reads_pips_json() {
    let json = br#"{
      "install": [
        {
          "download_info": {
            "url": "https://files.example/requests-2.31.0-py3-none-any.whl",
            "archive_info": {
              "hash": "sha256=abc",
              "hashes": {"sha256": "abc"}
            }
          },
          "metadata": {"name": "requests", "version": "2.31.0"},
          "requested": true
        },
        {
          "download_info": {
            "url": "https://files.example/rich-13.0.0-py3-none-any.whl",
            "archive_info": {"hashes": {"sha256": "def"}}
          },
          "metadata": {"name": "rich", "version": "13.0.0", "extra": 1}
        }
      ],
      "environment": {"python_version": "3.12"}
    }"#;
    let locked = parse_report(json).unwrap();
    assert_eq!(
        locked,
        [
            Locked {
                name: "requests".into(),
                version: "2.31.0".into(),
                sha256: "abc".into(),
                url: "https://files.example/requests-2.31.0-py3-none-any.whl".into(),
            },
            Locked {
                name: "rich".into(),
                version: "13.0.0".into(),
                sha256: "def".into(),
                url: "https://files.example/rich-13.0.0-py3-none-any.whl".into(),
            },
        ]
    );
    assert!(parse_report(b"{").is_err());
    assert!(parse_report(b"{}").is_err());
    assert!(parse_report(br#"{"install":[{"metadata":{"name":"x","version":"1"}}]}"#).is_err());
}

#[test]
fn state_table() {
    let empty = EnvPlan {
        source: String::new(),
        requirements: Vec::new(),
        refused: Vec::new(),
        hash: EMPTY_HASH.into(),
    };
    let record = EnvRecord {
        plan_hash: EMPTY_HASH.into(),
        built_at: 1,
        python: "/env/bin/python".into(),
        locked: Vec::new(),
    };
    assert_eq!(state(&empty, None), EnvState::NotNeeded);
    assert_eq!(state(&empty, Some(&record)), EnvState::NotNeeded);

    let (requirements, refused) = parse_requirements("requests>=2");
    let plan = EnvPlan {
        source: "requirements.txt".into(),
        requirements,
        refused,
        hash: "plan".into(),
    };
    assert_eq!(state(&plan, None), EnvState::Needed);
    let ready = EnvRecord {
        plan_hash: "plan".into(),
        ..record.clone()
    };
    assert_eq!(state(&plan, Some(&ready)), EnvState::Ready);
    let changed = EnvRecord {
        plan_hash: "other".into(),
        ..record
    };
    assert_eq!(state(&plan, Some(&changed)), EnvState::Changed);

    let refused_plan = EnvPlan {
        source: "requirements.txt".into(),
        requirements: Vec::new(),
        refused: vec!["-e .: editable installs are not supported".into()],
        hash: "x".into(),
    };
    assert_eq!(state(&refused_plan, None), EnvState::Refused);
}

#[test]
fn pure_parsers_are_deterministic() {
    let text = "Foo_Bar[baz]>=1; python_version < \"3.12\"\n-rich\n";
    assert_eq!(parse_requirements(text), parse_requirements(text));
    let pyproject = "[project]\ndependencies = [\"a\", \"b\"]\n";
    assert_eq!(
        parse_pyproject_dependencies(pyproject),
        parse_pyproject_dependencies(pyproject)
    );
    let (requirements, refused) = parse_requirements("requests>=2");
    let plan = EnvPlan {
        source: "requirements.txt".into(),
        requirements,
        refused,
        hash: "x".into(),
    };
    assert_eq!(
        pip_args(Path::new("/v"), Path::new("/r"), &plan),
        pip_args(Path::new("/v"), Path::new("/r"), &plan)
    );
    let report = br#"{"install":[]}"#;
    assert_eq!(parse_report(report), parse_report(report));
    assert_eq!(state(&plan, None), state(&plan, None));
}

#[test]
fn plan_gives_an_error_for_invalid_utf8() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("requirements.txt"), [0xff, 0xfe]).unwrap();
    assert!(plan(dir.path()).is_err());
}
