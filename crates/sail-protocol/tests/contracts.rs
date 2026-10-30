use sail_protocol::*;
#[test]
fn request_response_error_and_events_have_explicit_v1_contracts() {
    let request = InferenceRequest::text("Olá SAIL");
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["version"], 1);
    assert_eq!(json["messages"][0]["content"], "Olá SAIL");
    let decoded: InferenceRequest = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(decoded.request_id, request.request_id);
    let mut unknown = json.clone();
    unknown["untrusted_grant"] = serde_json::json!(true);
    assert!(serde_json::from_value::<InferenceRequest>(unknown).is_err());
    let error = InferenceError::new(
        ErrorCode::ForbiddenEndpoint,
        "Cloud inference is not allowed",
    );
    let event = InferenceEvent {
        version: VERSION,
        request_id: request.request_id,
        payload: InferenceEventPayload::Failed { error },
    };
    let json = serde_json::to_value(event).unwrap();
    assert_eq!(json["type"], "failed");
    assert_eq!(json["error"]["code"], "forbidden_endpoint");
    assert_eq!(json["error"]["version"], 1);
}

#[test]
fn documented_request_and_schema_agree_with_rust_v1_types() {
    let request: InferenceRequest =
        serde_json::from_str(include_str!("../examples/inference-request-v1.json")).unwrap();
    assert_eq!(request.version, VERSION);
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../Docs/PROTOCOL/inference-v1.schema.json"
    ))
    .unwrap();
    assert_eq!(
        schema["$defs"]["InferenceRequest"]["properties"]["version"]["const"],
        VERSION
    );
    assert_eq!(
        schema["$defs"]["InferenceRequest"]["additionalProperties"],
        false
    );
}
