//! observ-7 — off-hot-path Prometheus exporter + `/healthz`.
//!
//! Renders the cumulative counters + latency quantiles as Prometheus text exposition and the
//! [`HealthSnapshot`] as `/healthz` JSON, served over a dependency-free blocking `std::net` server
//! (run on a dedicated thread — never the sign hot path). The exposition/JSON rendering is pure and
//! unit-tested; the socket loop is a thin wrapper around [`route`].

use std::io::{Read, Write};
use std::net::{TcpListener, ToSocketAddrs};

use super::health::HealthSnapshot;
use super::latency::LatencyStage;
use super::registry::MetricsRegistry;

/// Render the registry + latency book as Prometheus text exposition.
pub fn prometheus_exposition(reg: &MetricsRegistry, health: &HealthSnapshot) -> String {
    let c = reg.snapshot();
    let mut s = String::new();

    let counter = |s: &mut String, name: &str, help: &str, v: u64| {
        s.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} counter\n{name} {v}\n"
        ));
    };
    let gauge = |s: &mut String, name: &str, help: &str, v: f64| {
        s.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {v}\n"
        ));
    };

    counter(
        &mut s,
        "arb_attempts_total",
        "Arb attempts submitted",
        c.attempts,
    );
    counter(&mut s, "arb_lands_total", "Profitable lands", c.lands);
    counter(
        &mut s,
        "arb_reverts_total",
        "Reverted/dropped attempts",
        c.reverts,
    );
    counter(
        &mut s,
        "arb_landed_profit_lamports_total",
        "Cumulative landed gross profit (lamports)",
        c.landed_profit_lamports,
    );
    counter(
        &mut s,
        "arb_burned_lamports_total",
        "Cumulative base+priority burned on reverted losers",
        c.burned_lamports,
    );

    gauge(
        &mut s,
        "arb_revert_rate_pct",
        "Windowed revert-rate percent",
        health.revert_rate_pct,
    );
    gauge(
        &mut s,
        "arb_burn_rate_lamports_per_min",
        "Windowed burn-rate (lamports/min)",
        health.burn_rate_lpm as f64,
    );
    gauge(
        &mut s,
        "arb_realized_loss_sol_per_hour",
        "Windowed realized loss (SOL/hr)",
        health.realized_loss_sol_per_hour,
    );
    // realized PnL can be negative -> emit as a gauge.
    gauge(
        &mut s,
        "arb_realized_pnl_lamports",
        "Lifetime realized PnL (lamports)",
        health.realized_pnl_lamports as f64,
    );

    // Latency quantiles (only when populated).
    s.push_str("# HELP arb_submit_latency_ms Submit→land latency quantiles (ms)\n# TYPE arb_submit_latency_ms gauge\n");
    if let Some(p50) = reg.latency.p50_ms(LatencyStage::SubmitToLand) {
        s.push_str(&format!(
            "arb_submit_latency_ms{{quantile=\"0.5\"}} {p50}\n"
        ));
    }
    if let Some(p95) = reg.latency.p95_ms(LatencyStage::SubmitToLand) {
        s.push_str(&format!(
            "arb_submit_latency_ms{{quantile=\"0.95\"}} {p95}\n"
        ));
    }

    if let Some(rank) = reg.latency.last_confirmation() {
        gauge(
            &mut s,
            "arb_confirmation_slot",
            "Last landed confirmation slot",
            rank.slot as f64,
        );
        gauge(
            &mut s,
            "arb_confirmation_index_in_block",
            "Last landed index in block",
            rank.index_in_block as f64,
        );
    }

    s
}

/// Render the health snapshot as `/healthz` JSON (hand-rolled to avoid serde derives on the metric
/// structs).
pub fn healthz_json(health: &HealthSnapshot) -> String {
    let conf = match health.last_confirmation {
        Some(r) => format!(
            "{{\"slot\":{},\"index_in_block\":{}}}",
            r.slot, r.index_in_block
        ),
        None => "null".to_string(),
    };
    let opt = |v: Option<f64>| {
        v.map(|x| x.to_string())
            .unwrap_or_else(|| "null".to_string())
    };
    format!(
        "{{\"revert_rate_pct\":{},\"burn_rate_lpm\":{},\"realized_loss_sol_per_hour\":{},\
         \"submit_p50_ms\":{},\"submit_p95_ms\":{},\"realized_pnl_lamports\":{},\
         \"attempts\":{},\"lands\":{},\"reverts\":{},\"last_confirmation\":{}}}",
        health.revert_rate_pct,
        health.burn_rate_lpm,
        health.realized_loss_sol_per_hour,
        opt(health.submit_p50_ms),
        opt(health.submit_p95_ms),
        health.realized_pnl_lamports,
        health.attempts,
        health.lands,
        health.reverts,
        conf,
    )
}

/// Map a request path to `(status_code, content_type, body)`.
pub fn route<M, H>(path: &str, render_metrics: M, render_health: H) -> (u16, &'static str, String)
where
    M: Fn() -> String,
    H: Fn() -> String,
{
    match path {
        "/metrics" => (200, "text/plain; version=0.0.4", render_metrics()),
        "/healthz" => (200, "application/json", render_health()),
        _ => (404, "text/plain", "not found\n".to_string()),
    }
}

/// Serve `/metrics` + `/healthz` on a blocking `std::net` loop. Runs forever — spawn on a dedicated
/// thread, NEVER on the sign hot path. `render_metrics`/`render_health` close over the live state.
pub fn serve_blocking<A, M, H>(addr: A, render_metrics: M, render_health: H) -> std::io::Result<()>
where
    A: ToSocketAddrs,
    M: Fn() -> String,
    H: Fn() -> String,
{
    let listener = TcpListener::bind(addr)?;
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        // First line: "GET /path HTTP/1.1".
        let path = req
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("/");
        let (status, content_type, body) = route(path, &render_metrics, &render_health);
        let reason = if status == 200 { "OK" } else { "Not Found" };
        let resp = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::health::HealthEvaluator;
    use crate::metrics::pnl::{PnlLedger, TxOutcome};
    use solana_pubkey::Pubkey;

    fn populated() -> (MetricsRegistry, HealthSnapshot) {
        let reg = MetricsRegistry::new();
        reg.record_attempt();
        reg.record_land(100_000, 555, 3);
        reg.record_revert(
            crate::metrics::types::RevertCause::OnchainUnprofitable,
            7_000,
        );
        reg.latency.record_us(LatencyStage::SubmitToLand, 250_000.0);
        reg.latency
            .record_confirmation(crate::metrics::latency::ConfirmationRank {
                slot: 555,
                index_in_block: 3,
            });
        let pnl = PnlLedger::new();
        pnl.record_outcome(TxOutcome::landed(
            Pubkey::new_from_array([1; 32]),
            100_000,
            0,
            0,
            0,
            1,
        ))
        .unwrap();
        let health = HealthEvaluator::default().snapshot(&pnl, &reg, 2);
        (reg, health)
    }

    #[test]
    fn prometheus_exposition_has_canonical_metrics() {
        let (reg, health) = populated();
        let text = prometheus_exposition(&reg, &health);
        assert!(text.contains("arb_attempts_total 1"));
        assert!(text.contains("arb_lands_total 1"));
        assert!(text.contains("arb_burned_lamports_total 7000"));
        assert!(text.contains("# TYPE arb_attempts_total counter"));
        assert!(text.contains("arb_submit_latency_ms{quantile=\"0.5\"}"));
        assert!(text.contains("arb_confirmation_slot 555"));
    }

    #[test]
    fn healthz_json_is_parseable() {
        let (_reg, health) = populated();
        let json = healthz_json(&health);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["lands"], 1);
        assert_eq!(v["last_confirmation"]["slot"], 555);
    }

    #[test]
    fn healthz_json_handles_missing_latency_and_confirmation() {
        let reg = MetricsRegistry::new();
        let pnl = PnlLedger::new();
        let health = HealthEvaluator::default().snapshot(&pnl, &reg, 0);
        let json = healthz_json(&health);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["submit_p50_ms"].is_null());
        assert!(v["last_confirmation"].is_null());
    }

    #[test]
    fn router_serves_metrics_health_and_404() {
        let (s200, ct, _) = route("/metrics", || "m".into(), || "h".into());
        assert_eq!(s200, 200);
        assert!(ct.contains("text/plain"));
        let (s_health, ct_h, body) = route("/healthz", || "m".into(), || "h".into());
        assert_eq!(s_health, 200);
        assert_eq!(ct_h, "application/json");
        assert_eq!(body, "h");
        let (s404, _, _) = route("/nope", || "m".into(), || "h".into());
        assert_eq!(s404, 404);
    }
}
