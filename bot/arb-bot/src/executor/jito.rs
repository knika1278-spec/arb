//! landing-2 — the Jito Block Engine JSON-RPC client (`getTipAccounts` TTL-cached, `sendBundle`,
//! `getInflightBundleStatuses`, `getBundleStatuses`, `simulateBundle`).
//!
//! The networked HTTP is abstracted behind [`JitoTransport`] (the real impl POSTs to the region's
//! `/api/v1/bundles` endpoint with the `x-jito-auth` UUID over reqwest-rustls — a follow-up; this
//! module owns the request framing, response parsing, and the typed status enums, all host-tested
//! against canned responses). Connectivity + the 8 tip accounts are live-verified (2026-06-23).
//!
//! **Receipt ≠ confirmation (done-when invariant):** [`JitoClient::send_bundle`] returns a
//! [`BundleReceipt`] — a SHA-256 of the bundle's signatures, NOT proof it landed. Landing is known
//! ONLY by polling [`JitoClient::get_bundle_statuses`] → [`BundleFinalStatus`]. The two are distinct
//! types so a receipt can never be mistaken for a confirmation.

use serde_json::{json, Value};
use solana_pubkey::Pubkey;

use super::setup::{TipAccountError, TipAccountSet};
use super::types::{InflightStatus, Region};

/// Why a Jito RPC call failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JitoError {
    /// The transport (HTTP) layer failed before/around a response.
    Transport(String),
    /// The Block Engine returned a JSON-RPC error object.
    Rpc { code: i64, message: String },
    /// The response could not be parsed into the expected shape.
    Decode(String),
    /// `getTipAccounts` returned a set that failed the 8-distinct-non-default check.
    TipAccounts(TipAccountError),
    /// The local per-region rate limiter denied the send (would exceed 1 req/s).
    RateLimited,
}

/// The opaque receipt `sendBundle` returns — the bundle's signature SHA-256. A RECEIPT, never a
/// landing confirmation (poll [`JitoClient::get_bundle_statuses`] for that).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleReceipt(String);

impl BundleReceipt {
    pub fn id(&self) -> &str {
        &self.0
    }
}

/// Final on-chain confirmation level of a bundle (`getBundleStatuses`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmationLevel {
    Processed,
    Confirmed,
    Finalized,
}

impl ConfirmationLevel {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "processed" => Some(Self::Processed),
            "confirmed" => Some(Self::Confirmed),
            "finalized" => Some(Self::Finalized),
            _ => None,
        }
    }
}

/// A landed bundle's final status (`getBundleStatuses` `value[]` entry).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleFinalStatus {
    pub slot: u64,
    pub confirmation: ConfirmationLevel,
    /// `true` if the bundle's transactions errored on-chain (landed-but-reverted).
    pub errored: bool,
}

/// Summary of `simulateBundle`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleSimSummary {
    pub succeeded: bool,
    pub error: Option<String>,
}

/// Sync HTTP transport seam. The real impl POSTs `body` to `region`'s `/api/v1/bundles` endpoint,
/// attaching `x-jito-auth: <auth>` when present, and returns the raw response body. Kept sync (like
/// [`super::landing_loop::LandingTransport`]) so the client + its tests carry no async runtime; the
/// networked impl hides tokio/reqwest behind this call.
pub trait JitoTransport {
    fn post(&self, region: Region, auth: Option<&str>, body: &str) -> Result<String, JitoError>;
}

/// The Jito Block Engine client over a [`JitoTransport`].
pub struct JitoClient<T: JitoTransport> {
    transport: T,
    /// Allowlisted `x-jito-auth` UUID, if provisioned.
    auth: Option<String>,
    /// `getTipAccounts` TTL cache: `(accounts, fetched_at_millis)`.
    tip_cache: Option<(TipAccountSet, u64)>,
    tip_ttl_millis: u64,
}

/// Parse a JSON-RPC envelope: surface an `error` object, else return the `result` value.
fn rpc_result(body: &str) -> Result<Value, JitoError> {
    let v: Value = serde_json::from_str(body).map_err(|e| JitoError::Decode(e.to_string()))?;
    if let Some(err) = v.get("error") {
        return Err(JitoError::Rpc {
            code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
    }
    v.get("result")
        .cloned()
        .ok_or_else(|| JitoError::Decode("missing `result`".into()))
}

fn req(method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string()
}

impl<T: JitoTransport> JitoClient<T> {
    pub fn new(transport: T, auth: Option<String>, tip_ttl_millis: u64) -> Self {
        Self {
            transport,
            auth,
            tip_cache: None,
            tip_ttl_millis,
        }
    }

    fn post(&self, region: Region, body: &str) -> Result<String, JitoError> {
        self.transport.post(region, self.auth.as_deref(), body)
    }

    /// `getTipAccounts` with a TTL cache — the 8 accounts are resolved at runtime (never hardcoded)
    /// and validated (8 distinct, non-default). Re-fetches only after `tip_ttl_millis`.
    pub fn get_tip_accounts(
        &mut self,
        region: Region,
        now_millis: u64,
    ) -> Result<TipAccountSet, JitoError> {
        if let Some((set, at)) = &self.tip_cache {
            if now_millis.saturating_sub(*at) < self.tip_ttl_millis {
                return Ok(*set);
            }
        }
        let body = self.post(region, &req("getTipAccounts", json!([])))?;
        let result = rpc_result(&body)?;
        let arr = result
            .as_array()
            .ok_or_else(|| JitoError::Decode("getTipAccounts result not an array".into()))?;
        let mut pubkeys = Vec::with_capacity(arr.len());
        for v in arr {
            let s = v
                .as_str()
                .ok_or_else(|| JitoError::Decode("tip account not a string".into()))?;
            pubkeys.push(
                s.parse::<Pubkey>()
                    .map_err(|_| JitoError::Decode(format!("bad tip pubkey {s}")))?,
            );
        }
        let set = TipAccountSet::from_resolved(pubkeys).map_err(JitoError::TipAccounts)?;
        self.tip_cache = Some((set, now_millis));
        Ok(set)
    }

    /// `sendBundle` — returns a [`BundleReceipt`] (NOT a landing confirmation). `txs_base64` is the
    /// fully-signed, base64-encoded transaction list (tip rides inside, invariant #10).
    pub fn send_bundle(
        &self,
        region: Region,
        txs_base64: &[String],
    ) -> Result<BundleReceipt, JitoError> {
        let body = self.post(
            region,
            &req("sendBundle", json!([txs_base64, {"encoding": "base64"}])),
        )?;
        let result = rpc_result(&body)?;
        let id = result
            .as_str()
            .ok_or_else(|| JitoError::Decode("sendBundle result not a string".into()))?;
        Ok(BundleReceipt(id.to_string()))
    }

    /// `getInflightBundleStatuses` → typed [`InflightStatus`] per receipt (~5-min window).
    pub fn get_inflight_bundle_statuses(
        &self,
        region: Region,
        receipts: &[BundleReceipt],
    ) -> Result<Vec<(BundleReceipt, InflightStatus)>, JitoError> {
        let ids: Vec<&str> = receipts.iter().map(BundleReceipt::id).collect();
        let body = self.post(region, &req("getInflightBundleStatuses", json!([ids])))?;
        let result = rpc_result(&body)?;
        let values = result
            .get("value")
            .and_then(Value::as_array)
            .ok_or_else(|| JitoError::Decode("inflight: missing value[]".into()))?;
        let mut out = Vec::with_capacity(values.len());
        for v in values {
            let id = v
                .get("bundle_id")
                .and_then(Value::as_str)
                .ok_or_else(|| JitoError::Decode("inflight: missing bundle_id".into()))?;
            let status = match v.get("status").and_then(Value::as_str) {
                Some("Invalid") => InflightStatus::Invalid,
                Some("Pending") => InflightStatus::Pending,
                Some("Failed") => InflightStatus::Failed,
                Some("Landed") => InflightStatus::Landed,
                _ => InflightStatus::NotFound,
            };
            out.push((BundleReceipt(id.to_string()), status));
        }
        Ok(out)
    }

    /// `getBundleStatuses` → typed final status per receipt; `None` if not yet known.
    pub fn get_bundle_statuses(
        &self,
        region: Region,
        receipts: &[BundleReceipt],
    ) -> Result<Vec<(BundleReceipt, Option<BundleFinalStatus>)>, JitoError> {
        let ids: Vec<&str> = receipts.iter().map(BundleReceipt::id).collect();
        let body = self.post(region, &req("getBundleStatuses", json!([ids])))?;
        let result = rpc_result(&body)?;
        let values = result
            .get("value")
            .and_then(Value::as_array)
            .ok_or_else(|| JitoError::Decode("statuses: missing value[]".into()))?;
        let mut out = Vec::with_capacity(values.len());
        for v in values {
            if v.is_null() {
                continue;
            }
            let id = v
                .get("bundle_id")
                .and_then(Value::as_str)
                .ok_or_else(|| JitoError::Decode("statuses: missing bundle_id".into()))?;
            let conf = v
                .get("confirmation_status")
                .and_then(Value::as_str)
                .and_then(ConfirmationLevel::parse);
            let status = match (v.get("slot").and_then(Value::as_u64), conf) {
                (Some(slot), Some(confirmation)) => Some(BundleFinalStatus {
                    slot,
                    confirmation,
                    errored: !v.get("err").map(Value::is_null).unwrap_or(true),
                }),
                _ => None,
            };
            out.push((BundleReceipt(id.to_string()), status));
        }
        Ok(out)
    }

    /// `simulateBundle` → a pass/fail summary (pre-tip simulation gate, landing-5 consumes this).
    pub fn simulate_bundle(
        &self,
        region: Region,
        txs_base64: &[String],
    ) -> Result<BundleSimSummary, JitoError> {
        let body = self.post(
            region,
            &req(
                "simulateBundle",
                json!([{"encodedTransactions": txs_base64}]),
            ),
        )?;
        let result = rpc_result(&body)?;
        let summary = result.get("summary");
        // `summary == "succeeded"` (string) or `{ "failed": { "error": ... } }`.
        if summary.and_then(Value::as_str) == Some("succeeded") {
            return Ok(BundleSimSummary {
                succeeded: true,
                error: None,
            });
        }
        let error = summary
            .and_then(|s| s.get("failed"))
            .and_then(|f| f.get("error"))
            .map(|e| e.to_string());
        Ok(BundleSimSummary {
            succeeded: false,
            error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A transport that returns a canned response keyed by the method name in the request body, and
    /// records the (region, auth) each call saw.
    struct MockTransport {
        responses: Vec<(&'static str, String)>,
        seen: RefCell<Vec<(Region, Option<String>)>>,
    }
    impl MockTransport {
        fn new(responses: Vec<(&'static str, String)>) -> Self {
            Self {
                responses,
                seen: RefCell::new(Vec::new()),
            }
        }
    }
    impl JitoTransport for MockTransport {
        fn post(
            &self,
            region: Region,
            auth: Option<&str>,
            body: &str,
        ) -> Result<String, JitoError> {
            self.seen
                .borrow_mut()
                .push((region, auth.map(str::to_string)));
            for (method, resp) in &self.responses {
                if body.contains(method) {
                    return Ok(resp.clone());
                }
            }
            Err(JitoError::Transport("no canned response".into()))
        }
    }

    const TIP_8: &str = r#"{"jsonrpc":"2.0","id":1,"result":[
        "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe","ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
        "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY","DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
        "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt","DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
        "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5","3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT"]}"#;

    fn client(responses: Vec<(&'static str, String)>) -> JitoClient<MockTransport> {
        JitoClient::new(
            MockTransport::new(responses),
            Some("uuid-123".into()),
            5_000,
        )
    }

    #[test]
    fn get_tip_accounts_parses_8_and_attaches_auth() {
        let mut c = client(vec![("getTipAccounts", TIP_8.to_string())]);
        let set = c.get_tip_accounts(Region::Frankfurt, 0).unwrap();
        assert_eq!(set.as_slice().len(), 8);
        // The x-jito-auth UUID was forwarded to the transport.
        let seen = c.transport.seen.borrow();
        assert_eq!(seen[0], (Region::Frankfurt, Some("uuid-123".to_string())));
    }

    #[test]
    fn get_tip_accounts_uses_ttl_cache() {
        let mut c = client(vec![("getTipAccounts", TIP_8.to_string())]);
        c.get_tip_accounts(Region::Frankfurt, 0).unwrap();
        c.get_tip_accounts(Region::Frankfurt, 4_999).unwrap(); // within TTL => cached
        c.get_tip_accounts(Region::Frankfurt, 6_000).unwrap(); // past TTL => refetch
        assert_eq!(c.transport.seen.borrow().len(), 2); // only 2 network calls
    }

    #[test]
    fn send_bundle_returns_receipt_not_confirmation() {
        let c = client(vec![(
            "sendBundle",
            r#"{"jsonrpc":"2.0","id":1,"result":"abc123bundleid"}"#.to_string(),
        )]);
        let receipt = c.send_bundle(Region::Ny, &["dHg=".to_string()]).unwrap();
        assert_eq!(receipt.id(), "abc123bundleid");
        // BundleReceipt carries no landing/slot field — landing is only knowable via
        // get_bundle_statuses, so a receipt cannot be mistaken for a confirmation.
    }

    #[test]
    fn inflight_statuses_parse_into_typed_enums() {
        let c = client(vec![("getInflightBundleStatuses", r#"{"jsonrpc":"2.0","id":1,"result":{"context":{"slot":1},"value":[
            {"bundle_id":"b1","status":"Landed"},{"bundle_id":"b2","status":"Pending"},{"bundle_id":"b3","status":"Failed"}]}}"#.to_string())]);
        let r = c
            .get_inflight_bundle_statuses(
                Region::Ny,
                &[
                    BundleReceipt("b1".into()),
                    BundleReceipt("b2".into()),
                    BundleReceipt("b3".into()),
                ],
            )
            .unwrap();
        assert_eq!(r[0].1, InflightStatus::Landed);
        assert_eq!(r[1].1, InflightStatus::Pending);
        assert_eq!(r[2].1, InflightStatus::Failed);
    }

    #[test]
    fn bundle_statuses_parse_final_status() {
        let c = client(vec![("getBundleStatuses", r#"{"jsonrpc":"2.0","id":1,"result":{"context":{"slot":9},"value":[
            {"bundle_id":"b1","slot":12345,"confirmation_status":"confirmed","err":null},
            {"bundle_id":"b2","slot":12346,"confirmation_status":"finalized","err":{"InstructionError":[0,"Custom"]}},
            null]}}"#.to_string())]);
        let r = c
            .get_bundle_statuses(
                Region::Ny,
                &[BundleReceipt("b1".into()), BundleReceipt("b2".into())],
            )
            .unwrap();
        let s1 = r[0].1.as_ref().unwrap();
        assert_eq!(s1.slot, 12345);
        assert_eq!(s1.confirmation, ConfirmationLevel::Confirmed);
        assert!(!s1.errored);
        let s2 = r[1].1.as_ref().unwrap();
        assert_eq!(s2.confirmation, ConfirmationLevel::Finalized);
        assert!(s2.errored); // landed-but-reverted
    }

    #[test]
    fn rpc_error_object_surfaces_as_jito_error() {
        let c = client(vec![(
            "sendBundle",
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"bad params"}}"#
                .to_string(),
        )]);
        assert_eq!(
            c.send_bundle(Region::Ny, &["x".into()]).unwrap_err(),
            JitoError::Rpc {
                code: -32602,
                message: "bad params".into()
            }
        );
    }

    #[test]
    fn simulate_bundle_summary() {
        let ok = client(vec![(
            "simulateBundle",
            r#"{"jsonrpc":"2.0","id":1,"result":{"summary":"succeeded","transactionResults":[]}}"#
                .to_string(),
        )]);
        assert!(
            ok.simulate_bundle(Region::Ny, &["x".into()])
                .unwrap()
                .succeeded
        );
        let fail = client(vec![("simulateBundle", r#"{"jsonrpc":"2.0","id":1,"result":{"summary":{"failed":{"error":"AccountNotFound"}}}}"#.to_string())]);
        let s = fail.simulate_bundle(Region::Ny, &["x".into()]).unwrap();
        assert!(!s.succeeded);
        assert!(s.error.is_some());
    }
}
