//! Read-only Capital tools for the voice assistant. They answer from the owner's ledger,
//! the latest signal run and the same projections the Capital page uses; the model may
//! suggest freely but these tools never place an order or change the ledger.

use chrono::Utc;
use sail_capital::AsOf;
use sail_capital::contracts::AccountId;
use sail_capital::dividends::Allocation;
use sail_capital::forecast::ModelId;
use sail_capital::service::CapitalService;
use sail_capital::trade::SignalCard;
use serde_json::{Value, json};

/// Every Capital tool this module answers, checked by the registry tests.
#[cfg(test)]
pub const TOOL_IDS: [&str; 15] = [
    "capital:overview",
    "capital:today",
    "capital:portfolio",
    "capital:signals",
    "capital:health",
    "capital:check",
    "capital:search",
    "capital:watchlist",
    "capital:record",
    "capital:guards",
    "capital:dividends",
    "capital:forecast",
    "capital:policy",
    "capital:filings",
    "capital:income",
];

/// Runs one tool. Arguments are untrusted input from the model; the service validates
/// symbols, accounts and ranges.
pub async fn execute(service: &CapitalService, tool: &str, input: &Value) -> Result<Value, String> {
    match tool {
        "capital:overview" => overview(service, input),
        "capital:today" => today(service, input),
        "capital:portfolio" => portfolio(service, input),
        "capital:signals" => signals(service, input),
        "capital:health" => health(service, input),
        "capital:watchlist" => watchlist(service),
        "capital:record" => record(service, input),
        "capital:guards" => guards(service, input),
        "capital:dividends" => dividends(service, input),
        "capital:forecast" => forecast(service, input),
        "capital:policy" => policy(service, input),
        "capital:filings" => filings(service, input),
        "capital:income" => income_plan(service),
        "capital:search" => search(service, input).await,
        "capital:check" => check(service, input).await,
        other => Err(format!("Unknown Capital tool: {other}")),
    }
}

fn account(service: &CapitalService, input: &Value) -> Result<AccountId, String> {
    service
        .resolve_account(input.get("account").and_then(Value::as_str))
        .map_err(|error| error.to_string())
}

fn value<T: serde::Serialize>(item: T) -> Result<Value, String> {
    serde_json::to_value(item).map_err(|error| error.to_string())
}

fn overview(service: &CapitalService, input: &Value) -> Result<Value, String> {
    let account = account(service, input)?;
    let now = Utc::now();
    let insights = service
        .insights(&account, AsOf::now(), now)
        .map_err(|error| error.to_string())?;
    let today = service
        .today(&account, now)
        .map_err(|error| error.to_string())?;
    let trade = service
        .trade_view(&account)
        .map_err(|error| error.to_string())?;
    let watch = service.watchlist().map_err(|error| error.to_string())?;
    let policy = service
        .policy(&account)
        .map_err(|error| error.to_string())?;
    let accounts = service.accounts().map_err(|error| error.to_string())?;

    let suggestions: Vec<Value> = today
        .suggestions
        .iter()
        .take(3)
        .map(|suggestion| {
            json!({
                "title": &suggestion.title,
                "why": &suggestion.why,
                "severity": suggestion.severity,
                "kind": suggestion.kind,
                "confidence": &suggestion.confidence,
            })
        })
        .collect();
    let cards: Vec<Value> = trade
        .day
        .as_ref()
        .map(|day| day.cards.iter().take(3).map(compact_card).collect())
        .unwrap_or_default();
    let candidates: Vec<Value> = trade
        .day
        .as_ref()
        .map(|day| day.candidates.iter().take(3).map(compact_card).collect())
        .unwrap_or_default();
    let mut notes = trade.notes;
    if let Some(day) = trade.day.as_ref() {
        notes.extend(day.notes.iter().cloned());
    }

    Ok(json!({
        "as_of": now,
        "account": account.as_str(),
        "accounts": accounts,
        "currency": &insights.currency,
        "owned_value": &insights.owned_value,
        "account_view": &insights.account,
        "concentration": &insights.concentration,
        "freshness": &insights.freshness,
        "warnings": &insights.warnings,
        "stance": &today.stance,
        "top_suggestions": suggestions,
        "signals": {
            "run_at": trade.run_at,
            "session": trade.day.as_ref().map(|day| day.session),
            "card_count": trade.day.as_ref().map(|day| day.cards.len()).unwrap_or(0),
            "top": cards,
            "candidate_count": trade.day.as_ref().map(|day| day.candidates.len()).unwrap_or(0),
            "top_candidates": candidates,
            "next_run_at": trade.next_run_at,
            "running": trade.running,
            "last_error": &trade.last_error,
            "notes": notes,
        },
        "watchlist_count": watch.len(),
        "risk_per_trade_pct": policy.current.limits.risk_per_trade_pct,
    }))
}

fn today(service: &CapitalService, input: &Value) -> Result<Value, String> {
    let account = account(service, input)?;
    let today = service
        .today(&account, Utc::now())
        .map_err(|error| error.to_string())?;
    let suggestions: Vec<Value> = today
        .suggestions
        .iter()
        .map(|suggestion| {
            json!({
                "id": &suggestion.id,
                "kind": suggestion.kind,
                "severity": suggestion.severity,
                "title": &suggestion.title,
                "why": &suggestion.why,
                "if_nothing": &suggestion.if_nothing,
                "confidence": &suggestion.confidence,
                "changes": &suggestion.changes,
                "rules": &suggestion.rules,
                "evidence": &suggestion.evidence,
            })
        })
        .collect();
    Ok(json!({
        "as_of": today.as_of,
        "account": &today.account,
        "policy": &today.policy,
        "stance": &today.stance,
        "suggestions": suggestions,
        "hidden": today.hidden,
    }))
}

fn portfolio(service: &CapitalService, input: &Value) -> Result<Value, String> {
    let account = account(service, input)?;
    let now = Utc::now();
    let insights = service
        .insights(&account, AsOf::now(), now)
        .map_err(|error| error.to_string())?;
    let projection = service
        .portfolio(&account, AsOf::now())
        .map_err(|error| error.to_string())?;
    let position = |position: &sail_capital::ledger::Position| {
        json!({
            "symbol": &position.instrument.symbol,
            "name": &position.instrument.name,
            "kind": position.instrument.kind,
            "currency": &position.instrument.currency,
            "quantity": position.quantity,
            "cost_basis": &position.cost_basis,
            "market_value": &position.broker_market_value,
        })
    };
    let positions: Vec<Value> = projection.positions.iter().map(position).collect();
    let cfd_positions: Vec<Value> = projection.cfd_positions.iter().map(position).collect();

    Ok(json!({
        "account": account.as_str(),
        "currency": &insights.currency,
        "owned_value": &insights.owned_value,
        "account_view": &insights.account,
        "allocation": &insights.allocation,
        "by_kind": &insights.by_kind,
        "concentration": &insights.concentration,
        "total_return": &insights.total_return,
        "income": &insights.income,
        "freshness": &insights.freshness,
        "warnings": &insights.warnings,
        "positions": positions,
        "cfd_positions": cfd_positions,
        "cash": &projection.cash,
        "reconciliation": &projection.reconciliation,
        "ledger_warnings": &projection.warnings,
        "records_used": projection.records_used,
    }))
}

fn signals(service: &CapitalService, input: &Value) -> Result<Value, String> {
    let account = account(service, input)?;
    let trade = service
        .trade_view(&account)
        .map_err(|error| error.to_string())?;
    let mut notes = trade.notes;
    let (session, cards, candidates, scoreboard, suppressed) = match trade.day.as_ref() {
        Some(day) => {
            notes.extend(day.notes.iter().cloned());
            let cards: Vec<Value> = day.cards.iter().map(compact_card).collect();
            let candidates: Vec<Value> = day.candidates.iter().map(compact_card).collect();
            let scoreboard: Vec<Value> = day
                .scoreboard
                .iter()
                .map(|verdict| {
                    json!({
                        "group": &verdict.group,
                        "setup": verdict.selection.setup,
                        "side": verdict.selection.side,
                        "publishes": verdict.publishes,
                        "candidate": verdict.candidate,
                        "reason": &verdict.reason,
                        "selection": {
                            "trades": verdict.selection.trades,
                            "hit_target1": verdict.selection.hit_target1,
                            "expectancy_r": verdict.selection.expectancy_r,
                            "expectancy_ci": verdict.selection.expectancy_ci,
                        },
                        "holdout": {
                            "trades": verdict.holdout.trades,
                            "expectancy_r": verdict.holdout.expectancy_r,
                        },
                        "judgement": verdict.judgement.as_ref().map(|judgement| json!({
                            "p_value": judgement.p_value,
                            "fdr_pass": judgement.fdr_pass,
                            "pooled_mean": judgement.pooled.posterior_mean,
                            "pooled_shrinkage": judgement.pooled.shrinkage,
                            "live_trades": judgement.live.trades,
                            "posterior_mean": judgement.posterior_mean,
                            "posterior_lower": judgement.posterior_lower,
                            "oos_trades": judgement.walk_forward.oos_trades,
                            "oos_mean_r": judgement.walk_forward.oos_mean_r,
                            "all_folds_mean_r": judgement.walk_forward.all_mean_r,
                        })),
                    })
                })
                .collect();
            let suppressed: Vec<Value> = day
                .suppressed
                .iter()
                .map(|s| {
                    json!({"symbol": s.symbol, "setup": s.setup, "side": s.side, "reason": s.reason})
                })
                .collect();
            (Some(day.session), cards, candidates, scoreboard, suppressed)
        }
        None => (None, Vec::new(), Vec::new(), Vec::new(), Vec::new()),
    };

    let board = service
        .signal_board(&account)
        .map_err(|error| error.to_string())?;
    let rows: Vec<Value> = board
        .rows
        .iter()
        .map(|row| {
            let three_months = row.outlook.iter().find(|o| o.horizon_sessions == 63);
            json!({
                "symbol": &row.meta.symbol,
                "name": &row.meta.name,
                "held": row.meta.held,
                "lean": row.read.lean,
                "proven": row.read.proven,
                "read": &row.read.sentence,
                "readings": row.readings.iter().map(|r| json!({
                    "signal": r.signal,
                    "percentile": r.percentile.round(),
                    "text": &r.text,
                })).collect::<Vec<_>>(),
                "outlook_3m": three_months.map(|o| json!({
                    "median": o.median,
                    "low_5": o.low,
                    "high_95": o.high,
                    "chance_up": o.p_up,
                })),
                "setup": &row.setup,
            })
        })
        .collect();

    let record = service
        .board_record(&account)
        .map_err(|error| error.to_string())?;

    Ok(json!({
        "board_record": {
            "first_session": record.first_session,
            "sessions": record.sessions,
            "graded": record.graded,
            "by_lean": &record.by_lean,
            "outlook": &record.outlook,
            "notes": &record.notes,
        },
        "board": {
            "session": board.session,
            "validated_signals": &board.passed,
            "outlook_record": &board.outlook_record,
            "rows": rows,
            "notes": &board.notes,
        },
        "run_at": trade.run_at,
        "session": session,
        "cards": cards,
        "candidates": candidates,
        "scoreboard": scoreboard,
        "suppressed": suppressed,
        "notes": notes,
        "running": trade.running,
        "next_run_at": trade.next_run_at,
        "last_error": &trade.last_error,
    }))
}

fn health(service: &CapitalService, input: &Value) -> Result<Value, String> {
    let account = account(service, input)?;
    let health = service
        .trade_health(&account)
        .map_err(|error| error.to_string())?;
    let problems: Vec<Value> = health
        .instruments
        .iter()
        .filter(|instrument| {
            instrument.excluded
                || instrument.fetch_failed
                || instrument.stale_weekdays > 2
                || !instrument.enough_bars
                || instrument.earnings_known == Some(false)
        })
        .map(|instrument| {
            json!({
                "symbol": &instrument.symbol,
                "key": &instrument.key,
                "last_session": instrument.last_session,
                "stale_weekdays": instrument.stale_weekdays,
                "bars": instrument.bars,
                "enough_bars": instrument.enough_bars,
                "excluded": instrument.excluded,
                "gaps": &instrument.gaps,
                "earnings_known": instrument.earnings_known,
                "fetch_failed": instrument.fetch_failed,
            })
        })
        .collect();
    Ok(json!({
        "run_at": health.run_at,
        "run_stats": health.run_stats,
        "last_error": &health.last_error,
        "problem_count": problems.len(),
        "problems": problems,
        "research": &health.research,
        "notes": &health.notes,
    }))
}

fn watchlist(service: &CapitalService) -> Result<Value, String> {
    let items = service.watchlist().map_err(|error| error.to_string())?;
    Ok(json!({"count": items.len(), "instruments": items}))
}

fn record(service: &CapitalService, input: &Value) -> Result<Value, String> {
    let account = account(service, input)?;
    let record = service
        .trade_record(&account)
        .map_err(|error| error.to_string())?;
    let recent: Vec<Value> = record
        .cards
        .iter()
        .take(20)
        .map(|graded| {
            json!({
                "symbol": &graded.card.symbol,
                "setup": graded.card.setup,
                "side": graded.card.side,
                "published_at": graded.published_at,
                "status": &graded.status,
                "signal_session": graded.card.signal_session,
                "tier": graded.card.tier,
                "r_multiple": graded.graded.as_ref().map(|trade| trade.r_multiple),
                "exit": graded.graded.as_ref().map(|trade| trade.exit),
            })
        })
        .collect();
    Ok(json!({
        "totals": record.totals,
        "candidate_totals": record.candidate_totals,
        "recent": recent,
    }))
}

fn guards(service: &CapitalService, input: &Value) -> Result<Value, String> {
    let account = account(service, input)?;
    let guards = service
        .trade_guards(&account)
        .map_err(|error| error.to_string())?;
    value(guards)
}

fn dividends(service: &CapitalService, input: &Value) -> Result<Value, String> {
    let account = account(service, input)?;
    let years = input["years"].as_u64().unwrap_or(5) as u32;
    let reinvest_pct = input["reinvest_pct"].as_f64().unwrap_or(100.0);
    let allocation = match input["allocation"].as_str().unwrap_or("proportional") {
        "same_holding" => Allocation::SameHolding,
        _ => Allocation::Proportional,
    };
    let monthly_contribution = input["monthly_contribution"].as_f64().unwrap_or(0.0);
    let reserve = input["reserve_pt_tax"].as_bool().unwrap_or(false);
    let outlook = service
        .dividend_projection(
            &account,
            years,
            reinvest_pct,
            allocation,
            monthly_contribution,
            reserve,
        )
        .map_err(|error| error.to_string())?;
    let mut payload = value(outlook)?;
    if let Some(scenarios) = payload.get_mut("scenarios").and_then(Value::as_array_mut) {
        for scenario in scenarios {
            if let Some(object) = scenario.as_object_mut() {
                object.remove("monthly_net");
            }
        }
    }
    if let Some(without) = payload
        .get_mut("without_reinvestment")
        .and_then(Value::as_object_mut)
    {
        without.remove("monthly_net");
    }
    Ok(payload)
}

/// Round 24: the last income-plan job and the dividend growth backtest's headline.
fn income_plan(service: &CapitalService) -> Result<Value, String> {
    let job = service.income_plan_job();
    let calibration = service
        .dividend_calibration()
        .map_err(|error| error.to_string())?
        .map(|(computed_at, report)| {
            json!({
                "computed_at": computed_at,
                "symbols": report.symbols,
                "adopted_shrink_k": report.adopted_k,
                "notes": report.notes,
            })
        });
    let summarize = |plan: &sail_capital::dividends::maximize::IncomePlan| {
        json!({
            "objective": plan.objective,
            "objective_label": plan.objective_label,
            "what_it_maximizes": plan.objective_explainer,
            "recommendation": plan.recommendation,
            "this_month": plan.this_month,
            "weights": plan.weights,
            "scores": plan.scores,
            "sensitivity": plan.sensitivity,
            "excluded_by_safety": plan.excluded,
            "notes": plan.notes,
        })
    };
    // Round 25: one entry per objective the job ran (all three by default).
    let plans: Vec<Value> = if job.plans.is_empty() {
        job.plan.iter().map(summarize).collect()
    } else {
        job.plans.iter().map(summarize).collect()
    };
    Ok(json!({
        "state": job.state,
        "finished_at": job.finished_at,
        "error": job.error,
        "objectives_done": plans.len(),
        "objectives_total": job.objectives_total,
        "plans": plans,
        "dividend_growth_backtest": calibration,
        "note": if job.plan.is_none() {
            "No plan has been run since SAIL started; the owner starts one from Income → Maximize income."
        } else {
            "Simulated, not a promise; SAIL never places orders."
        },
    }))
}

fn forecast(service: &CapitalService, input: &Value) -> Result<Value, String> {
    let account = account(service, input)?;
    let horizon = input["horizon"].as_u64().unwrap_or(20) as u32;
    let model = match input["model"].as_str().unwrap_or("no_change") {
        "trailing_drift" => ModelId::TrailingDrift,
        "ewma_volatility" => ModelId::EwmaVolatility,
        "empirical_horizon" => ModelId::EmpiricalHorizon,
        _ => ModelId::NoChange,
    };
    let view = service
        .forecasts(&account, horizon, model)
        .map_err(|error| error.to_string())?;
    let forecasts: Vec<Value> = view
        .forecasts
        .iter()
        .map(|forecast| {
            json!({
                "symbol": &forecast.symbol,
                "as_of_session": forecast.as_of_session,
                "reference_close": forecast.reference_close,
                "status": forecast.status,
                "expected_log_return": forecast.expected_log_return,
                "median_terminal_price": forecast.median_terminal_price,
                "quantiles": &forecast.quantiles,
                "probability_positive": forecast.probability_positive,
            })
        })
        .collect();
    Ok(json!({
        "as_of": view.as_of,
        "horizon_sessions": view.horizon_sessions,
        "model": view.model,
        "forecasts": forecasts,
        "without_prices": view.without_prices,
    }))
}

fn filings(service: &CapitalService, input: &Value) -> Result<Value, String> {
    let account = account(service, input)?;
    let view = service
        .filings(&account, AsOf::now())
        .map_err(|error| error.to_string())?;
    let items: Vec<Value> = view
        .items
        .iter()
        .take(30)
        .map(|item| {
            json!({
                "issuer": &item.issuer_name,
                "form": &item.filing.form,
                "filed": item.filing.filing_date,
                "published_at": item.published_at,
                "report_date": item.filing.report_date,
                "items": &item.filing.items,
                "description": &item.filing.description,
                "url": &item.filing.url,
                "reason": &item.reason,
                "action": item.action,
            })
        })
        .collect();
    Ok(json!({
        "as_of": view.as_of,
        "items": items,
        "matches": view.matches,
    }))
}

fn policy(service: &CapitalService, input: &Value) -> Result<Value, String> {
    let account = account(service, input)?;
    let policy = service
        .policy(&account)
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "current": policy.current,
        "versions": policy.history.len(),
    }))
}

async fn search(service: &CapitalService, input: &Value) -> Result<Value, String> {
    let query = input["query"].as_str().unwrap_or_default().trim();
    if query.is_empty() {
        return Err("query is required".into());
    }
    let results = service
        .search_trade_symbols(query)
        .await
        .map_err(|error| error.to_string())?;
    value(results)
}

async fn check(service: &CapitalService, input: &Value) -> Result<Value, String> {
    let account = account(service, input)?;
    let symbol = input["symbol"].as_str().unwrap_or_default().trim();
    if symbol.is_empty() {
        return Err("symbol is required".into());
    }
    let check = service
        .check_trade_signal(
            &account,
            symbol,
            input["yahoo"].as_str(),
            input["name"].as_str(),
            input["exchange"].as_str(),
            input["class"].as_str(),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut payload = value(check)?;
    if let Some(object) = payload.as_object_mut() {
        object.remove("recent_closes");
    }
    Ok(payload)
}

fn compact_card(card: &SignalCard) -> Value {
    json!({
        "symbol": &card.symbol,
        "side": card.side,
        "setup": card.setup,
        "class": card.class,
        "score": card.score,
        "entry_zone": card.entry_zone,
        "stop": card.stop,
        "targets": card.targets,
        "max_sessions": card.max_sessions,
        "size": &card.size,
        "risk_pct": card.risk_pct,
        "needs_cfd": card.needs_cfd,
        "relation": &card.relation,
        "tier": card.tier,
        "why": &card.verdict.reason,
        "signal_session": card.signal_session,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sail_capital::market::{PriceSeries, PriceSource, PriceSourceError, SymbolMatch};
    use std::sync::Arc;

    struct FakeSearch;

    #[async_trait]
    impl PriceSource for FakeSearch {
        fn source_id(&self) -> &str {
            "yfinance"
        }

        async fn daily(&self, symbol: &str) -> Result<PriceSeries, PriceSourceError> {
            Err(PriceSourceError::UnknownSymbol(symbol.to_string()))
        }

        async fn search(&self, query: &str) -> Result<Vec<SymbolMatch>, PriceSourceError> {
            assert_eq!(query, "apple");
            Ok(vec![SymbolMatch {
                symbol: "AAPL".into(),
                name: "Apple Inc.".into(),
                exchange: "NASDAQ".into(),
                quote_type: "EQUITY".into(),
            }])
        }
    }

    fn service() -> CapitalService {
        let dir = std::env::temp_dir().join(format!("sail-capital-tools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        CapitalService::open(&dir).unwrap()
    }

    #[test]
    fn the_tool_names_survive_the_voice_name_mapping() {
        for id in TOOL_IDS {
            let (prefix, action) = id.split_once(':').expect("capital:action");
            assert_eq!(prefix, "capital");
            assert!(!action.contains('_'), "{id} would be split twice");
        }
    }

    #[tokio::test]
    async fn the_watchlist_tool_answers_without_an_account() {
        let service = service();
        let payload = execute(&service, "capital:watchlist", &json!({}))
            .await
            .unwrap();
        assert_eq!(payload["count"].as_u64(), Some(44));
    }

    #[tokio::test]
    async fn the_search_tool_returns_the_source_matches() {
        let service = service().with_prices(Arc::new(FakeSearch), true);
        let payload = execute(&service, "capital:search", &json!({"query": "apple"}))
            .await
            .unwrap();
        assert_eq!(payload[0]["symbol"], "AAPL");
        assert_eq!(payload[0]["class"], "stock");
    }

    #[tokio::test]
    async fn an_unknown_tool_is_a_plain_error() {
        let service = service();
        let error = execute(&service, "capital:nope", &json!({}))
            .await
            .unwrap_err();
        assert!(error.contains("Unknown Capital tool"));
    }
}
