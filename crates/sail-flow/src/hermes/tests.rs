use super::*;

/// A Hermes plugin as documented upstream: `plugin.yaml` plus `register(ctx)`.
const MANIFEST: &str = "name: calc\nversion: 1.0.0\ndescription: Calculator\nprovides_tools: [calculate]\nrequires_env:\n  - name: CALC_KEY\n    description: demo\n";
const PLUGIN: &str = r#"
import json, os, socket
from . import helpers
from hermes_constants import get_hermes_home
from plugins.calc import helpers as absolute_helpers

def calculate(args, **kwargs):
    return json.dumps({"value": helpers.evaluate(args["expression"])})

def probe(args, **kwargs):
    out = {"key": os.environ.get("CALC_KEY"), "other": os.environ.get("OTHER_SECRET")}
    out["home"] = os.environ["HOME"]
    out["hermes_home"] = str(get_hermes_home())
    out["same_module"] = absolute_helpers is helpers
    out["host_home_visible"] = os.path.exists(args.get("host_path", "/nonexistent"))
    try:
        socket.create_connection(("1.1.1.1", 53), timeout=2).close()
        out["network"] = True
    except OSError:
        out["network"] = False
    return json.dumps(out)

def remember(args, **kwargs):
    import builtins
    ctx = builtins.__sail_ctx
    ctx.state.set("count", ctx.state.get("count", 0) + 1)
    return json.dumps({"count": ctx.state.get("count"), "unit": ctx.get_config("unit")})

def register(ctx):
    import builtins
    builtins.__sail_ctx = ctx
    schema = {"name": "calculate", "description": "Evaluate arithmetic",
              "parameters": {"type": "object", "properties": {"expression": {"type": "string"}},
                             "required": ["expression"]}}
    ctx.register_tool(name="calculate", toolset="calc", schema=schema, handler=calculate)
    ctx.register_tool(name="probe", toolset="calc", schema={"description": "probe"}, handler=probe)
    ctx.register_tool(name="remember", toolset="calc", schema={"description": "state"}, handler=remember)
    ctx.register_hook("post_tool_call", lambda **kw: None)
    ctx.register_command("double", lambda raw: str(int(raw) * 2), description="Double a number")
    ctx.register_skill("calc-help", "skills/calc-help/SKILL.md")
    ctx.register_platform("irc", "IRC", None, None)
    print("noise on stdout must not break the protocol")
"#;
const HELPERS: &str = "def evaluate(expr):\n    return eval(expr, {'__builtins__': {}}, {})\n";
const SKILL: &str = "---\nname: calc-help\ndescription: How to use the calculator plugin\n---\nCall calc.calculate.\n";

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    secret: PathBuf,
    manager: Manager,
}

fn fixture() -> Option<Fixture> {
    let bwrap = PathBuf::from("/usr/bin/bwrap");
    // Hosts without a usable bubblewrap cannot run plugins at all; that is tested below.
    let usable = std::process::Command::new(&bwrap)
        .args(["--ro-bind", "/", "/", "--unshare-all", "--", "/bin/true"])
        .status()
        .is_ok_and(|s| s.success());
    if !usable {
        eprintln!("skipping: bubblewrap unavailable");
        return None;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("plugins");
    let dir = root.join("calc");
    fs::create_dir_all(dir.join("skills/calc-help")).unwrap();
    fs::write(dir.join("plugin.yaml"), MANIFEST).unwrap();
    fs::write(dir.join("__init__.py"), PLUGIN).unwrap();
    fs::write(dir.join("helpers.py"), HELPERS).unwrap();
    fs::write(dir.join("skills/calc-help/SKILL.md"), SKILL).unwrap();
    let secret = tmp.path().join("host-secret.txt");
    fs::write(&secret, "do not read").unwrap();
    let env = tmp.path().join("env");
    fs::create_dir_all(&env).unwrap();
    fs::write(env.join("calc.env"), "CALC_KEY=k-123\nOTHER_SECRET=nope\n").unwrap();
    let paths = Paths {
        roots: vec![("hermes".into(), root.clone())],
        approvals: tmp.path().join("approvals.json"),
        data: tmp.path().join("data"),
        env,
    };
    let sandbox = Sandbox {
        bwrap,
        python: "/usr/bin/python3".into(),
        runtime: tmp.path().join("runtime"),
        timeout: Duration::from_secs(30),
    };
    Some(Fixture {
        root,
        secret,
        manager: Manager::new(paths, sandbox),
        _tmp: tmp,
    })
}

fn settings() -> serde_json::Map<String, Value> {
    json!({"unit":"cm"}).as_object().unwrap().clone()
}

#[tokio::test]
async fn quarantined_until_approved_then_runs_in_the_sandbox() {
    let Some(f) = fixture() else { return };
    let plugin = &f.manager.paths.discover()[0];
    assert_eq!(plugin.status, Status::Quarantined);
    assert_eq!(plugin.manifest.requires_env, ["CALC_KEY"]);
    assert!(f.manager.call("calc.calculate", "{}").await.is_err());
    assert!(f.manager.catalog().await.is_empty());

    f.manager
        .paths
        .approve("calc", false, vec!["CALC_KEY".into()], settings())
        .unwrap();
    let catalog = f.manager.catalog().await;
    let description = catalog[0].1.as_ref().unwrap();
    let tools: Vec<_> = description.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(tools, ["calculate", "probe", "remember"]);
    assert_eq!(description.hooks, ["post_tool_call"]);
    assert_eq!(description.commands[0].name, "double");
    assert_eq!(description.unsupported, ["register_platform"]);
    assert_eq!(description.skills["calc-help"], "skills/calc-help/SKILL.md");

    let out = f
        .manager
        .call("calc.calculate", r#"{"expression":"6*7"}"#)
        .await
        .unwrap();
    assert_eq!(out["result"]["value"], 42);
    let out = f.manager.call("calc./double", "21").await.unwrap();
    assert_eq!(out["result"], 42);

    let mut library = crate::skills::SkillLibrary::default();
    f.manager.add_skills(&mut library).await;
    assert_eq!(library.skills["calc:calc-help"].source, "plugin:calc");
}

#[tokio::test]
async fn the_sandbox_hides_network_host_files_and_unlisted_secrets() {
    let Some(f) = fixture() else { return };
    f.manager
        .paths
        .approve("calc", false, vec!["CALC_KEY".into()], settings())
        .unwrap();
    let args = json!({"host_path": f.secret}).to_string();
    let out = f.manager.call("calc.probe", &args).await.unwrap();
    let probe = &out["result"];
    assert_eq!(probe["key"], "k-123");
    assert_eq!(probe["other"], Value::Null);
    assert_eq!(probe["home"], "/home/plugin");
    assert_eq!(probe["hermes_home"], "/home/plugin");
    assert_eq!(probe["same_module"], true);
    assert_eq!(probe["host_home_visible"], false);
    assert_eq!(probe["network"], false);
}

#[tokio::test]
async fn state_and_settings_persist_in_the_plugin_home() {
    let Some(f) = fixture() else { return };
    f.manager
        .paths
        .approve("calc", false, Vec::new(), settings())
        .unwrap();
    for expected in [1, 2] {
        let out = f.manager.call("calc.remember", "{}").await.unwrap();
        assert_eq!(out["result"]["count"], expected);
        assert_eq!(out["result"]["unit"], "cm");
    }
}

#[tokio::test]
async fn a_changed_plugin_stops_running_until_approved_again() {
    let Some(f) = fixture() else { return };
    f.manager
        .paths
        .approve("calc", false, Vec::new(), settings())
        .unwrap();
    fs::write(
        f.root.join("calc/helpers.py"),
        "def evaluate(e):\n    return 0\n",
    )
    .unwrap();
    assert_eq!(f.manager.paths.discover()[0].status, Status::Changed);
    let error = f
        .manager
        .call("calc.calculate", r#"{"expression":"1"}"#)
        .await;
    assert!(error.unwrap_err().contains("not enabled"));
    // Also refused when the change lands between discovery and execution.
    f.manager
        .paths
        .approve("calc", false, Vec::new(), settings())
        .unwrap();
    let plugin = f.manager.paths.discover().remove(0);
    fs::write(f.root.join("calc/helpers.py"), HELPERS).unwrap();
    let error = f
        .manager
        .sandbox
        .run(&f.manager.paths, &plugin, json!({"op":"describe"}))
        .await;
    assert!(error.unwrap_err().contains("changed since"));
}

#[tokio::test]
async fn plugin_failures_are_reported_as_errors() {
    let Some(f) = fixture() else { return };
    f.manager
        .paths
        .approve("calc", false, Vec::new(), settings())
        .unwrap();
    let err = f
        .manager
        .call("calc.calculate", r#"{"expression":"1/0"}"#)
        .await
        .unwrap_err();
    assert!(err.contains("ZeroDivisionError"), "{err}");
    assert!(f.manager.call("calc.missing", "{}").await.is_err());
    assert!(f.manager.call("calc.calculate", "[1]").await.is_err());
    assert!(f.manager.call("nodot", "{}").await.is_err());
}

#[test]
fn unsupported_kinds_symlinks_and_bad_env_names_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("plugins");
    fs::create_dir_all(root.join("irc")).unwrap();
    fs::write(root.join("irc/plugin.yaml"), "name: irc\nkind: platform\n").unwrap();
    fs::create_dir_all(root.join("linky")).unwrap();
    fs::write(root.join("linky/plugin.yaml"), "name: linky\n").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", root.join("linky/passwd")).unwrap();
    fs::create_dir_all(root.join("tools/nested")).unwrap();
    fs::write(root.join("tools/nested/plugin.yaml"), "name: nested\n").unwrap();
    let paths = Paths {
        roots: vec![("hermes".into(), root)],
        approvals: tmp.path().join("approvals.json"),
        data: tmp.path().join("data"),
        env: tmp.path().join("env"),
    };
    let found = paths.discover();
    let status = |id: &str| found.iter().find(|p| p.id == id).unwrap().status.clone();
    assert!(matches!(status("irc"), Status::Unsupported(_)));
    assert!(matches!(status("linky"), Status::Invalid(_)));
    assert_eq!(status("nested"), Status::Quarantined);
    assert!(
        paths
            .approve("irc", false, vec![], Default::default())
            .is_err()
    );
    assert!(
        paths
            .approve(
                "nested",
                false,
                vec!["LD_PRELOAD".into()],
                Default::default()
            )
            .is_err()
    );
    assert!(
        paths
            .approve("nested", false, vec![], Default::default())
            .is_ok()
    );
    assert!(paths.revoke("nested").unwrap());
}

#[tokio::test]
async fn a_missing_bubblewrap_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plugins/calc");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("plugin.yaml"), MANIFEST).unwrap();
    fs::write(dir.join("__init__.py"), "def register(ctx):\n    pass\n").unwrap();
    let paths = Paths {
        roots: vec![("hermes".into(), tmp.path().join("plugins"))],
        approvals: tmp.path().join("approvals.json"),
        data: tmp.path().join("data"),
        env: tmp.path().join("env"),
    };
    paths
        .approve("calc", false, vec![], Default::default())
        .unwrap();
    let manager = Manager::new(
        paths,
        Sandbox {
            bwrap: tmp.path().join("no-bwrap"),
            python: "/usr/bin/python3".into(),
            runtime: tmp.path().join("runtime"),
            timeout: Duration::from_secs(5),
        },
    );
    let err = manager.call("calc.x", "{}").await.unwrap_err();
    assert!(err.contains("bubblewrap"), "{err}");
}

#[test]
fn network_is_shared_only_when_granted() {
    let plugin = Plugin {
        id: "calc".into(),
        source: "hermes".into(),
        dir: "/p/calc".into(),
        manifest: Manifest::default(),
        digest: String::new(),
        skills_only: false,
        status: Status::Enabled,
        approval: None,
    };
    let sandbox = Sandbox::default_sandbox();
    let args = |network| {
        sandbox.args(
            &plugin,
            &Approval {
                network,
                ..Default::default()
            },
            Path::new("/h.py"),
            Path::new("/d"),
            &[],
        )
    };
    assert!(!args(false).contains(&"--share-net".to_string()));
    assert!(!args(false).contains(&"/etc/resolv.conf".to_string()));
    assert!(args(true).contains(&"--share-net".to_string()));
    assert!(args(false).contains(&"--clearenv".to_string()));
}
