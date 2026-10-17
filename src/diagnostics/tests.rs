use super::*;

#[test]
fn redaction_keeps_generation_parameters_but_removes_credentials() {
    let mut capture = Capture::default();
    capture.push(br#"{"max_tokens":20000,"usage":{"prompt_tokens":12,"completion_tokens":8},"nested":{"api_key":"do-not-store","refresh_token":"nope"},"messages":[{"content":"known-secret"}]}"#);
    capture.complete = true;
    let detail = capture.snapshot("application/json", &Redactor(vec!["known-secret".into()]));
    assert_eq!(detail["body"]["max_tokens"], 20000);
    assert_eq!(detail["body"]["usage"]["completion_tokens"], 8);
    assert_eq!(detail["body"]["nested"]["api_key"], "[REDACTED]");
    for secret in ["do-not-store", "nope", "known-secret"] {
        assert!(!detail.to_string().contains(secret));
    }
}

#[test]
fn credentials_from_query_and_json_are_removed_when_echoed_in_errors() {
    let mut redactor = Redactor::default();
    redactor.observe_url("https://example.test/v1?api_key=QUERY_SECRET");
    let mut capture = Capture::default();
    capture.push(br#"{"password":"BODY_SECRET","nested":{"access_token":"TOKEN_SECRET"}}"#);
    redactor.observe_capture(&capture);
    let error = redactor.text("upstream echoed QUERY_SECRET BODY_SECRET TOKEN_SECRET");
    assert_eq!(error, "upstream echoed [REDACTED] [REDACTED] [REDACTED]");
}

#[test]
fn binary_oversized_and_malformed_sensitive_bodies_are_explicitly_omitted() {
    let mut capture = Capture::default();
    capture.push(&vec![b'x'; BODY_LIMIT + 100]);
    let value = capture.snapshot("application/json", &Redactor::default());
    assert_eq!(value["truncated"], true);
    assert_eq!(value["bytes"], BODY_LIMIT + 100);
    assert_eq!(capture.bytes.len(), BODY_LIMIT);
    assert!(value["body"]["omitted"].as_str().unwrap().contains("limit"));
    let mut capture = Capture::default();
    capture.push(b"\xff\x00");
    assert_eq!(
        capture.snapshot("text/plain", &Redactor::default())["body"]["omitted"],
        "binary body"
    );
    let mut capture = Capture::default();
    capture.push(br#"{"password":"secret","bad": "#);
    assert!(
        !capture
            .snapshot("application/json", &Redactor::default())
            .to_string()
            .contains("secret")
    );
}

#[tokio::test]
async fn crash_leftovers_are_distinguishable_from_live_or_successful_requests() {
    let store = Store::open_in_memory().await.unwrap();
    sqlx::query("INSERT INTO request_logs(id,request_id,direction,started_at,request) VALUES('crash','crash','inbound','2026-01-01','{}')").execute(store.pool()).await.unwrap();
    interrupt_leftovers(&store).await.unwrap();
    let (outcome, error): (String, String) =
        sqlx::query_as("SELECT outcome,error FROM request_logs WHERE id='crash'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(outcome, "interrupted");
    assert!(error.contains("unknown"));
}
