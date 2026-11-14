//! Owner terminal for Hermes plugins and Agent Skills.
//!
//! ```text
//! sail-hermes plugins                     list plugins and their status
//! sail-hermes approve <id> [--network] [--env NAME]... [--set key=value]...
//! sail-hermes revoke <id>
//! sail-hermes describe <id>               run register(ctx) in the sandbox and show it
//! sail-hermes call <plugin>.<tool> [json] run one tool (or <plugin>./<command> [text])
//! sail-hermes skills [query]              the skill index, with rejected folders
//! sail-hermes skill <name> [file]         one skill's body or resource file
//!
//! sail-hermes market refresh [--force]    download the Hermes skill and plugin catalogs
//! sail-hermes market search [text] [--kind skill|plugin] [--trusted]
//! sail-hermes market stage <skill|plugin> <id>   download + scan into quarantine
//! sail-hermes market install <stage_id> [--acknowledge-caution]
//! sail-hermes market discard <stage_id>
//! sail-hermes market installed
//! sail-hermes market uninstall <key> [--force]
//! sail-hermes proposals                   skills the model proposed
//! sail-hermes proposal approve <id> [--acknowledge-caution] | reject <id>
//! ```
//!
//! Approving is the owner's decision to run that exact plugin code in SAIL's sandbox. The
//! daemon reads approvals on every call, so no restart is needed.
use sail_flow::hermes::{self, Status};
use sail_flow::market::{Kind, Query, Trust};
use sail_flow::proposals::Proposals;
use sail_flow::skills::SkillLibrary;
use serde_json::{Value, json};

fn usage() -> ! {
    eprintln!(
        "usage: sail-hermes plugins | approve <id> [--network] [--env NAME]... [--set key=value]... | revoke <id> | describe <id> | call <plugin>.<tool> [json] | skills [query] | skill <name> [file] | market refresh|search|stage|install|discard|installed|uninstall ... | proposals | proposal approve|reject <id>"
    );
    std::process::exit(2)
}

fn print(value: &Value) {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
}

async fn library(manager: &hermes::Manager) -> SkillLibrary {
    let mut library = SkillLibrary::scan(&SkillLibrary::default_roots());
    manager.add_skills(&mut library).await;
    library
}

async fn market(args: &[String]) -> Result<Value, String> {
    let market = sail_flow::market::global();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let positional: Vec<&String> = args
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .collect();
    match args.first().map(String::as_str) {
        Some("refresh") => market.refresh(flag("--force")).await.map(|s| json!(s)),
        Some("search") => {
            let (mut words, mut kind) = (Vec::new(), None);
            let mut rest = args[1..].iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--kind" => {
                        let k = rest.next().unwrap_or_else(|| usage());
                        kind = Some(
                            serde_json::from_value::<Kind>(json!(k)).map_err(|e| e.to_string())?,
                        );
                    }
                    "--trusted" => {}
                    word => words.push(word),
                }
            }
            market
                .search(Query {
                    text: words.join(" "),
                    kind,
                    min_trust: flag("--trusted").then_some(Trust::Trusted),
                    limit: 30,
                    ..Query::default()
                })
                .await
                .map(|p| json!(p))
        }
        Some("stage") => {
            let (Some(kind), Some(id)) = (positional.first(), positional.get(1)) else {
                usage()
            };
            let kind: Kind = serde_json::from_value(json!(kind)).map_err(|e| e.to_string())?;
            market.stage(kind, id).await.map(|r| json!(r))
        }
        Some("install") => {
            let id = positional.first().unwrap_or_else(|| usage());
            market
                .install(id, flag("--acknowledge-caution"))
                .await
                .map(|e| json!(e))
        }
        Some("discard") => {
            let id = positional.first().unwrap_or_else(|| usage());
            market.discard(id).await.map(|_| json!({"ok": true}))
        }
        Some("installed") => market.installed().await.map(|l| json!(l)),
        Some("uninstall") => {
            let key = positional.first().unwrap_or_else(|| usage());
            market
                .uninstall(key, flag("--force"))
                .await
                .map(|_| json!({"ok": true}))
        }
        _ => usage(),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let manager = hermes::global();
    let paths = &manager.paths;
    let result: Result<Value, String> = match args.first().map(String::as_str) {
        Some("plugins") => Ok(json!(paths.discover())),
        Some("approve") => {
            let id = args.get(1).cloned().unwrap_or_else(|| usage());
            let (mut network, mut env, mut settings) = (false, Vec::new(), serde_json::Map::new());
            let mut rest = args[2..].iter();
            while let Some(flag) = rest.next() {
                match flag.as_str() {
                    "--network" => network = true,
                    "--env" => env.push(rest.next().cloned().unwrap_or_else(|| usage())),
                    "--set" => {
                        let pair = rest.next().unwrap_or_else(|| usage());
                        let (k, v) = pair.split_once('=').unwrap_or_else(|| usage());
                        let value = serde_json::from_str(v).unwrap_or_else(|_| json!(v));
                        settings.insert(k.into(), value);
                    }
                    _ => usage(),
                }
            }
            match paths.approve(&id, network, env, settings) {
                Ok(plugin) => {
                    let described = manager.describe(&plugin).await;
                    Ok(
                        json!({"plugin":plugin,"registered":described.as_ref().ok(),"error":described.err()}),
                    )
                }
                Err(e) => Err(e),
            }
        }
        Some("revoke") => {
            let id = args.get(1).unwrap_or_else(|| usage());
            paths.revoke(id).map(|removed| json!({"revoked":removed}))
        }
        Some("describe") => {
            let id = args.get(1).unwrap_or_else(|| usage());
            match paths.discover().into_iter().find(|p| &p.id == id) {
                Some(p) if p.status == Status::Enabled => {
                    manager.describe(&p).await.map(|d| json!(d))
                }
                Some(p) => Err(format!("{id} is not enabled: {:?}", p.status)),
                None => Err(format!("no plugin {id}")),
            }
        }
        Some("call") => {
            let name = args.get(1).unwrap_or_else(|| usage());
            manager
                .call(name, args.get(2).map(String::as_str).unwrap_or(""))
                .await
        }
        Some("skills") => {
            let lib = library(manager).await;
            Ok(
                json!({"skills":lib.index(args.get(1).map(String::as_str), 512),"rejected":lib.rejected}),
            )
        }
        Some("skill") => {
            let name = args.get(1).unwrap_or_else(|| usage());
            library(manager)
                .await
                .view(name, args.get(2).map(String::as_str))
                .map(|v| json!(v))
        }
        Some("market") => market(&args[1..]).await,
        Some("proposals") => Proposals::default_paths().list().map(|p| json!(p)),
        Some("proposal") => {
            let id = args.get(2).unwrap_or_else(|| usage());
            let proposals = Proposals::default_paths();
            match args.get(1).map(String::as_str) {
                Some("approve") => proposals
                    .approve(id, args.iter().any(|a| a == "--acknowledge-caution"))
                    .map(|name| json!({"skill": name})),
                Some("reject") => proposals.reject(id).map(|_| json!({"ok": true})),
                _ => usage(),
            }
        }
        _ => usage(),
    };
    match result {
        Ok(value) => print(&value),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1)
        }
    }
}
